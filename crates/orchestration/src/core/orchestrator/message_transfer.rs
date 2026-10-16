//! Port of the second start branch of upstream `Orchestrator.ts`
//! `dispatchMessage` (5300-6600): a start that carries context between
//! providers or threads. It runs when a fork or merge-back transfer is
//! pending for the thread, or when the thread's provider thread belongs to
//! another provider instance (a provider switch).
//!
//! It prepares the handoffs (`ContextHandoffService`), records the transfers
//! they consume, and starts the run. Upstream ignores `defer_start` here, and
//! so does this port: the run starts at once.

use std::collections::HashSet;

use turin_contracts::{
    ContextHandoffId, ContextTransferId, ProviderThreadId, RunId, ThreadId,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ContextHandoffStrategy, OrchestrationV2ContextTransfer,
        OrchestrationV2ContextTransferResolution, OrchestrationV2ContextTransferStatus,
        OrchestrationV2ContextTransferType, OrchestrationV2CoveredRunOrdinals,
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2HandoffStrategy,
        OrchestrationV2NativeRefStrength, OrchestrationV2ProviderThread,
        OrchestrationV2ProviderThreadForkedFrom, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RunAttemptReason,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus, OrchestrationV2ServerCommand,
        OrchestrationV2ThreadProjection, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
        OrchestrationV2UserMessageInputIntent, latest_provider_turn_for_attempt,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive as ids},
    notification::{NotificationMessage, notification_turn_item},
    turn_item::turn_item_common,
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{
    StartInput, conversation_message, next_run_ordinal, prepare_root_scope,
    provider_session_id_for, root_provider_threads_for_provider, run_scoped_event,
    wake_work_started_at,
};
use super::thread_fork::{context_source_point_for_run, provider_thread_for_run};
use crate::core::context_handoff_service::{
    ContextHandoffService, PrepareForkDeltaInput as ForkDeltaInput,
    PrepareLegacyImportInput as LegacyImportInput,
    PrepareProviderHandoffInput as ProviderHandoffInput, ProviderHandoffStrategy,
};
use crate::core::provider_session_transition_policy::ProviderSessionTransition;
use crate::pipeline::command_policy::{
    CapabilityCheck, ForkExecutionPolicy, HandoffStrategy, decide_fork_execution,
    ensure_context_handoff, ensure_native_fork,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// `isHandoffSourceRun`: failed and interrupted turns still contain
/// conversation the next provider needs. Queued, cancelled and rolled-back
/// runs must not be replayed as conversation.
pub(super) fn is_handoff_source_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Interrupted
    )
}

/// `lastDeliveredRunForProviderThread`.
pub(super) fn last_delivered_run_for_provider_thread<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    provider_thread_id: &ProviderThreadId,
) -> Option<&'a OrchestrationV2Run> {
    projection.runs.iter().rev().find(|run| {
        is_handoff_source_run(run)
            && run.provider_thread_id.as_ref() == Some(provider_thread_id)
            && (run.status == OrchestrationV2RunStatus::Completed
                || projection
                    .provider_turns
                    .iter()
                    .any(|turn| turn.run_attempt_id == run.active_attempt_id))
    })
}

/// `readHandoffItems`: the turn items of the given runs (`None` entries are
/// the runless imported items), or of the whole thread.
pub(super) async fn read_handoff_items(
    orchestrator: &Orchestrator,
    thread_id: &ThreadId,
    run_ids: Option<Vec<Option<RunId>>>,
) -> Result<Vec<OrchestrationV2TurnItem>, OrchestratorError> {
    Ok(orchestrator
        .get_thread_records(
            thread_id,
            &[RecordField::TurnItems],
            &RecordFilter {
                turn_item_run_ids: run_ids,
                ..RecordFilter::default()
            },
        )
        .await?
        .turn_items)
}

/// `visibleDeltaRunOrdinals`: the run ordinals the items span.
fn visible_delta_run_ordinals(
    projection: &OrchestrationV2ThreadProjection,
    items: &[OrchestrationV2TurnItem],
) -> OrchestrationV2CoveredRunOrdinals {
    let ordinals: Vec<u64> = items
        .iter()
        .filter_map(|item| {
            let run_id = turn_item_common(item).run_id?;
            projection
                .runs
                .iter()
                .find(|run| &run.id == run_id)
                .map(|run| run.ordinal.get())
        })
        .collect();
    let positive = |value: u64| turin_contracts::PositiveInt::new(value).expect("positive");
    match (ordinals.iter().min(), ordinals.iter().max()) {
        (Some(min), Some(max)) => OrchestrationV2CoveredRunOrdinals {
            from: positive(*min),
            to: positive(*max),
        },
        _ => OrchestrationV2CoveredRunOrdinals {
            from: positive(1),
            to: positive(1),
        },
    }
}

/// `shouldPrepareLegacyImportHandoff`.
pub(super) fn should_prepare_legacy_import_handoff(
    imported: bool,
    has_completed_run: bool,
    legacy_import_item_count: usize,
) -> bool {
    imported && !has_completed_run && legacy_import_item_count > 0
}

fn contextual_failure(
    command: &OrchestrationV2ServerCommand,
    cause: impl std::fmt::Display,
) -> OrchestratorError {
    dispatch_failure(command, cause.to_string())
}

pub(super) fn transfer_strategy_name(
    strategy: OrchestrationV2ContextHandoffStrategy,
) -> OrchestrationV2HandoffStrategy {
    match strategy {
        OrchestrationV2ContextHandoffStrategy::DeltaSinceTargetLastSeen => {
            OrchestrationV2HandoffStrategy::DeltaSinceTargetLastSeen
        }
        OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary => {
            OrchestrationV2HandoffStrategy::ForkDeltaSummary
        }
        OrchestrationV2ContextHandoffStrategy::FullThreadSummary => {
            OrchestrationV2HandoffStrategy::FullThreadSummary
        }
        OrchestrationV2ContextHandoffStrategy::CheckpointSummary => {
            OrchestrationV2HandoffStrategy::CheckpointSummary
        }
        OrchestrationV2ContextHandoffStrategy::ManualContext => {
            OrchestrationV2HandoffStrategy::ManualContext
        }
    }
}

/// The latest transfer by `updated_at` (ties go to the later one).
fn latest_context_transfer<'a>(
    transfers: &[&'a OrchestrationV2ContextTransfer],
) -> Option<&'a OrchestrationV2ContextTransfer> {
    transfers
        .iter()
        .copied()
        .fold(None, |latest, transfer| match latest {
            None => Some(transfer),
            Some(latest)
                if transfer.updated_at.as_datetime() >= latest.updated_at.as_datetime() =>
            {
                Some(transfer)
            }
            keep => keep,
        })
}

/// The transfer events share one envelope.
macro_rules! transfer_event {
    ($decision:expr, $variant:ident, $thread:expr, $run:expr, $instance:expr, $now:expr, $payload:expr) => {{
        let id = $decision.event_id($thread);
        $decision.emit(OrchestrationV2DomainEvent::$variant {
            id,
            thread_id: $thread.clone(),
            run_id: Some($run.clone()),
            node_id: None,
            driver: None,
            provider_instance_id: Some($instance.clone()),
            raw_event_id: None,
            occurred_at: $now,
            payload: $payload,
        });
    }};
}

/// The start branch for a provider switch, a pending fork or merge-back.
pub(super) async fn start_run_with_transfers(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    projection: &OrchestrationV2ThreadProjection,
    input: StartInput<'_>,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let StartInput {
        client,
        requested_model,
        resolved,
        ..
    } = input;
    let thread = &projection.thread;
    let thread_id = &thread.id;
    let instance = &requested_model.instance_id;
    let handoffs = ContextHandoffService::default();
    let now = decision.now();

    let pending_fork_transfer = projection.context_transfers.iter().find(|transfer| {
        transfer.r#type == OrchestrationV2ContextTransferType::Fork
            && &transfer.target_thread_id == thread_id
            && transfer.status == OrchestrationV2ContextTransferStatus::Pending
    });
    let pending_merge_backs: Vec<&OrchestrationV2ContextTransfer> = projection
        .context_transfers
        .iter()
        .filter(|transfer| {
            transfer.r#type == OrchestrationV2ContextTransferType::MergeBack
                && &transfer.target_thread_id == thread_id
                && transfer.status == OrchestrationV2ContextTransferStatus::Pending
        })
        .collect();
    let merge_back_sources: HashSet<&ThreadId> = pending_merge_backs
        .iter()
        .map(|transfer| &transfer.source_thread_id)
        .collect();
    if merge_back_sources.len() > 1 {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} has pending merge-back transfers from multiple forks."),
        ));
    }
    let pending_merge_back = latest_context_transfer(&pending_merge_backs);
    let superseded_merge_backs: Vec<&OrchestrationV2ContextTransfer> = pending_merge_backs
        .iter()
        .copied()
        .filter(|transfer| Some(&transfer.id) != pending_merge_back.map(|latest| &latest.id))
        .collect();

    let ordinal = next_run_ordinal(projection);
    let run_id = ids::run(thread_id, ordinal);
    let latest_completed_run = projection
        .runs
        .iter()
        .rev()
        .find(|run| run.status == OrchestrationV2RunStatus::Completed);
    let latest_handoff_run = projection
        .runs
        .iter()
        .rev()
        .find(|run| is_handoff_source_run(run));
    let imported = thread.history_origin
        == Some(turin_contracts::orchestration_v2::OrchestrationV2ThreadHistoryOrigin::V1Import);
    let legacy_import_items = if imported {
        read_handoff_items(orchestrator, thread_id, Some(vec![None])).await?
    } else {
        Vec::new()
    };
    let active_provider_thread = thread.active_provider_thread_id.as_ref().and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let is_provider_switch =
        active_provider_thread.is_some_and(|active| &active.provider_instance_id != instance);
    // Account overlays share native history. Selection commands may already
    // have updated the app thread, so classify against the native thread's owner.
    let can_resume_across_instances = if let (true, Some(active)) =
        (is_provider_switch, active_provider_thread)
        && active.native_thread_ref.is_some()
    {
        let mut planning = projection.clone();
        planning.thread.model_selection.instance_id = active.provider_instance_id.clone();
        orchestrator
            .seams
            .switch_planner
            .plan(&planning, requested_model)
            .await
            .map_err(|cause| contextual_failure(command, cause))?
            .transition
            == ProviderSessionTransition::RestartAndResume
    } else {
        false
    };

    // The pending fork's source.
    let source_projection = match pending_fork_transfer {
        None => None,
        Some(transfer) => Some(
            orchestrator
                .get_thread_records(
                    &transfer.source_thread_id,
                    &[
                        RecordField::Runs,
                        RecordField::Attempts,
                        RecordField::ProviderThreads,
                        RecordField::ProviderTurns,
                    ],
                    &RecordFilter::default(),
                )
                .await?,
        ),
    };
    let source_run = pending_fork_transfer
        .and_then(|transfer| transfer.source_point.run_id.as_ref())
        .zip(source_projection.as_ref())
        .and_then(|(run_id, source)| source.runs.iter().find(|run| &run.id == run_id));
    let source_provider_thread = source_projection
        .as_ref()
        .zip(source_run)
        .and_then(|(source, run)| provider_thread_for_run(source, run));
    let source_provider_turn_id =
        source_projection
            .as_ref()
            .zip(source_run)
            .and_then(|(source, run)| {
                let attempt_id = run.active_attempt_id.as_ref()?;
                latest_provider_turn_for_attempt(&source.provider_turns, Some(attempt_id))
                    .map(|turn| turn.id.clone())
                    .or_else(|| {
                        source
                            .attempts
                            .iter()
                            .find(|candidate| &candidate.id == attempt_id)
                            .and_then(|attempt| attempt.provider_turn_id.clone())
                    })
            });
    if let Some(transfer) = pending_fork_transfer {
        if source_run.is_none() || source_provider_thread.is_none() {
            return Err(dispatch_failure(
                command,
                format!(
                    "Pending fork transfer {} has no resolvable source provider thread.",
                    transfer.id
                ),
            ));
        }
        if transfer.source_provider_instance_id.is_none() {
            return Err(dispatch_failure(
                command,
                format!(
                    "Pending fork transfer {} has no source provider.",
                    transfer.id
                ),
            ));
        }
    }

    let adapter = orchestrator
        .seams
        .adapters
        .adapter(instance)
        .ok_or_else(|| OrchestratorError::ProviderAdapter {
            command_id: command_id(command).clone(),
            provider_instance_id: instance.clone(),
            cause: format!("Provider instance {instance} is not available.").into(),
        })?;
    let target_provider_thread: Option<OrchestrationV2ProviderThread> =
        if is_provider_switch && !can_resume_across_instances {
            root_provider_threads_for_provider(projection, instance)
                .into_iter()
                .next()
        } else {
            active_provider_thread.cloned()
        };
    let provider_session_id = match target_provider_thread
        .as_ref()
        .filter(|_| !can_resume_across_instances)
        .and_then(|row| row.provider_session_id.clone())
    {
        Some(existing) => existing,
        None => provider_session_id_for(adapter.as_ref(), instance, thread_id, command).await?,
    };
    let capabilities =
        adapter
            .get_capabilities()
            .await
            .map_err(|cause| OrchestratorError::ProviderAdapter {
                command_id: command_id(command).clone(),
                provider_instance_id: instance.clone(),
                cause: Box::new(cause),
            })?;
    let capability_check = |_unused: ()| CapabilityCheck {
        command_id: command_id(command),
        thread_id,
        provider_instance_id: instance,
        capabilities: &capabilities,
    };
    let fork_execution = match (pending_fork_transfer, source_run) {
        (Some(transfer), Some(source_run)) => Some(
            decide_fork_execution(
                &capability_check(()),
                transfer.source_provider_instance_id.as_ref() == Some(instance),
                source_provider_thread
                    .and_then(|row| row.native_thread_ref.as_ref())
                    .is_some_and(|reference| {
                        reference.strength == OrchestrationV2NativeRefStrength::Strong
                    }),
                source_run.status,
                true,
            )
            .map_err(|cause| contextual_failure(command, cause))?,
        ),
        _ => None,
    };
    let can_resolve_fork_natively = fork_execution == Some(ForkExecutionPolicy::NativeFork);
    let requires_portable_fork = fork_execution == Some(ForkExecutionPolicy::PortableContext);
    if can_resolve_fork_natively {
        ensure_native_fork(&capability_check(()), source_run.is_some())
            .map_err(|cause| contextual_failure(command, cause))?;
    }

    let ensured_provider_thread = match &target_provider_thread {
        None => OrchestrationV2ProviderThread {
            id: ids::provider_thread(adapter.driver(), None, &format!("pending:{run_id}")),
            driver: adapter.driver().clone(),
            provider_instance_id: instance.clone(),
            provider_session_id: Some(provider_session_id.clone()),
            app_thread_id: Some(thread_id.clone()),
            owner_node_id: None,
            native_thread_ref: None,
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::NotLoaded,
            first_run_ordinal: turin_contracts::PositiveInt::new(ordinal),
            last_run_ordinal: turin_contracts::PositiveInt::new(ordinal),
            handoff_ids: Vec::new(),
            forked_from: if can_resolve_fork_natively {
                source_provider_thread.map(|row| OrchestrationV2ProviderThreadForkedFrom {
                    provider_thread_id: row.id.clone(),
                    provider_turn_id: source_provider_turn_id.clone(),
                    checkpoint_id: None,
                })
            } else {
                None
            },
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now,
            updated_at: now,
        },
        Some(existing) => OrchestrationV2ProviderThread {
            provider_instance_id: instance.clone(),
            provider_session_id: Some(provider_session_id.clone()),
            updated_at: now,
            ..existing.clone()
        },
    };

    // The fork's portable handoff.
    let portable_fork_items = match (
        requires_portable_fork,
        source_projection.as_ref(),
        source_run,
    ) {
        (true, Some(source), Some(source_run)) => {
            let mut run_ids: Vec<Option<RunId>> = source
                .runs
                .iter()
                .filter(|run| run.ordinal <= source_run.ordinal)
                .map(|run| Some(run.id.clone()))
                .collect();
            let source_thread = orchestrator
                .projection
                .get_thread(&pending_fork_transfer.expect("a fork").source_thread_id)
                .await
                .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
            if source_thread.history_origin
                == Some(
                    turin_contracts::orchestration_v2::OrchestrationV2ThreadHistoryOrigin::V1Import,
                )
            {
                run_ids.push(None);
            }
            read_handoff_items(orchestrator, &source_thread.id, Some(run_ids)).await?
        }
        _ => Vec::new(),
    };
    let portable_fork_handoff = match (
        requires_portable_fork,
        pending_fork_transfer,
        source_projection.as_ref(),
        source_run,
    ) {
        (true, Some(transfer), Some(source), Some(source_run)) => Some(
            handoffs.prepare_provider_handoff(&ProviderHandoffInput {
                thread_id,
                target_run_id: &run_id,
                transfer_id: Some(transfer.id.clone()),
                from_provider_thread_ids: &source_provider_thread
                    .map(|row| vec![row.id.clone()])
                    .unwrap_or_default(),
                to_provider_thread_id: &ensured_provider_thread.id,
                from_provider_instance_id: &source_run.provider_instance_id,
                to_provider_instance_id: instance,
                covered_run_ordinals: visible_delta_run_ordinals(source, &portable_fork_items),
                runs: Some(&source.runs),
                strategy: ProviderHandoffStrategy::FullThreadSummary,
                items: &portable_fork_items,
                created_at: now,
            }),
        ),
        _ => None,
    };

    // Provider switch: the conversation the target has not seen.
    let requires_full_context = is_provider_switch && pending_merge_back.is_some();
    let target_last_completed_run = target_provider_thread
        .as_ref()
        .and_then(|row| last_delivered_run_for_provider_thread(projection, &row.id));
    let switch_covered_runs: Vec<&OrchestrationV2Run> = match (
        is_provider_switch && !can_resume_across_instances,
        latest_handoff_run,
    ) {
        (true, Some(latest)) => projection
            .runs
            .iter()
            .filter(|run| {
                is_handoff_source_run(run)
                    && run.ordinal.get()
                        > if requires_full_context {
                            0
                        } else {
                            target_last_completed_run.map_or(0, |run| run.ordinal.get())
                        }
                    && run.ordinal.get() <= latest.ordinal.get()
            })
            .collect(),
        _ => Vec::new(),
    };
    let full_summary = target_provider_thread.is_none() || requires_full_context;
    let switch_items = if switch_covered_runs.is_empty() {
        Vec::new()
    } else {
        let mut items = if latest_completed_run.is_some() && full_summary {
            legacy_import_items.clone()
        } else {
            Vec::new()
        };
        items.extend(
            read_handoff_items(
                orchestrator,
                thread_id,
                Some(
                    switch_covered_runs
                        .iter()
                        .map(|run| Some(run.id.clone()))
                        .collect(),
                ),
            )
            .await?,
        );
        items
    };
    let switch_transfer_id: Option<ContextTransferId> =
        if switch_covered_runs.is_empty() || latest_handoff_run.is_none() {
            None
        } else {
            Some(IdAllocator.context_transfer(thread_id, thread_id, "provider_handoff"))
        };
    if switch_transfer_id.is_some() {
        ensure_context_handoff(
            &capability_check(()),
            if full_summary {
                HandoffStrategy::FullThreadSummary
            } else {
                HandoffStrategy::DeltaContext
            },
        )
        .map_err(|cause| contextual_failure(command, cause))?;
    }
    let provider_switch_handoff = match (&switch_transfer_id, latest_handoff_run) {
        (Some(transfer_id), Some(latest)) => {
            let from_threads: Vec<ProviderThreadId> = {
                let mut seen = HashSet::new();
                switch_covered_runs
                    .iter()
                    .filter_map(|run| run.provider_thread_id.clone())
                    .filter(|id| seen.insert(id.clone()))
                    .collect()
            };
            Some(handoffs.prepare_provider_handoff(&ProviderHandoffInput {
                thread_id,
                target_run_id: &run_id,
                transfer_id: Some(transfer_id.clone()),
                from_provider_thread_ids: &from_threads,
                to_provider_thread_id: &ensured_provider_thread.id,
                from_provider_instance_id: &latest.provider_instance_id,
                to_provider_instance_id: instance,
                covered_run_ordinals: OrchestrationV2CoveredRunOrdinals {
                    from: switch_covered_runs[0].ordinal,
                    to: switch_covered_runs[switch_covered_runs.len() - 1].ordinal,
                },
                runs: Some(&projection.runs),
                strategy: if full_summary {
                    ProviderHandoffStrategy::FullThreadSummary
                } else {
                    ProviderHandoffStrategy::DeltaSinceTargetLastSeen
                },
                items: &switch_items,
                created_at: now,
            }))
        }
        _ => None,
    };
    let legacy_import_recovery_handoff = if is_provider_switch
        && !can_resume_across_instances
        && latest_completed_run.is_none()
        && !legacy_import_items.is_empty()
    {
        Some(handoffs.prepare_legacy_import(&LegacyImportInput {
            thread_id,
            target_run_id: &run_id,
            to_provider_thread_id: &ensured_provider_thread.id,
            to_provider_instance_id: instance,
            items: &legacy_import_items,
            created_at: now,
        }))
    } else {
        None
    };
    let handoff_ids: Vec<ContextHandoffId> = {
        let mut ids_out = ensured_provider_thread.handoff_ids.clone();
        for handoff in [
            portable_fork_handoff.as_ref(),
            provider_switch_handoff.as_ref(),
            legacy_import_recovery_handoff.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            ids_out.push(handoff.id.clone());
        }
        ids_out
    };
    let provider_thread = OrchestrationV2ProviderThread {
        status: OrchestrationV2ProviderThreadStatus::Active,
        first_run_ordinal: ensured_provider_thread
            .first_run_ordinal
            .or(turin_contracts::PositiveInt::new(ordinal)),
        last_run_ordinal: turin_contracts::PositiveInt::new(ordinal),
        handoff_ids,
        updated_at: now,
        ..ensured_provider_thread.clone()
    };

    let attempt_id = ids::run_attempt(&run_id, 1);
    let root_node_id = ids::root_node(&run_id);

    // The merge-back's source.
    let merge_back_source = match pending_merge_back {
        None => None,
        Some(transfer) => Some(
            orchestrator
                .get_thread_records(
                    &transfer.source_thread_id,
                    &[
                        RecordField::Runs,
                        RecordField::Attempts,
                        RecordField::ProviderThreads,
                        RecordField::ProviderTurns,
                    ],
                    &RecordFilter::default(),
                )
                .await?,
        ),
    };
    let merge_back_source_run = pending_merge_back
        .and_then(|transfer| transfer.source_point.run_id.as_ref())
        .zip(merge_back_source.as_ref())
        .and_then(|(run_id, source)| source.runs.iter().find(|run| &run.id == run_id));
    if let (Some(transfer), None) = (pending_merge_back, merge_back_source_run) {
        return Err(dispatch_failure(
            command,
            format!(
                "Pending merge-back transfer {} has no resolvable source run.",
                transfer.id
            ),
        ));
    }
    let merge_back_source_provider_thread = merge_back_source
        .as_ref()
        .zip(merge_back_source_run)
        .and_then(|(source, run)| provider_thread_for_run(source, run));
    if let (Some(transfer), None) = (pending_merge_back, merge_back_source_provider_thread) {
        return Err(dispatch_failure(
            command,
            format!(
                "Pending merge-back transfer {} has no resolvable source provider thread.",
                transfer.id
            ),
        ));
    }
    if pending_merge_back.is_some() {
        ensure_context_handoff(&capability_check(()), HandoffStrategy::ForkDeltaContext)
            .map_err(|cause| contextual_failure(command, cause))?;
    }
    let merge_back_delta_items = match (merge_back_source.as_ref(), merge_back_source_run) {
        (Some(source), Some(source_run)) => {
            read_handoff_items(
                orchestrator,
                &source.thread.id,
                Some(
                    source
                        .runs
                        .iter()
                        .filter(|run| run.ordinal <= source_run.ordinal)
                        .map(|run| Some(run.id.clone()))
                        .collect(),
                ),
            )
            .await?
        }
        _ => Vec::new(),
    };
    let merge_back_handoff = match (
        pending_merge_back,
        merge_back_source.as_ref(),
        merge_back_source_run,
        merge_back_source_provider_thread,
    ) {
        (Some(transfer), Some(source), Some(source_run), Some(source_thread)) => {
            Some(handoffs.prepare_fork_delta(&ForkDeltaInput {
                source_thread_id: &transfer.source_thread_id,
                target_thread_id: thread_id,
                target_run_id: &run_id,
                transfer_id: Some(transfer.id.clone()),
                from_provider_thread_ids: std::slice::from_ref(&source_thread.id),
                to_provider_thread_id: &provider_thread.id,
                from_provider_instance_id: &source_run.provider_instance_id,
                to_provider_instance_id: instance,
                covered_run_ordinals: visible_delta_run_ordinals(source, &merge_back_delta_items),
                delta_items: &merge_back_delta_items,
                created_at: now,
            }))
        }
        _ => None,
    };
    let checkpoint_scope = prepare_root_scope(
        orchestrator,
        command,
        projection,
        &run_id,
        &root_node_id,
        &provider_thread.id,
        now,
    )
    .await?;
    let ensured_scope = orchestrator
        .seams
        .checkpoints
        .ensure_scope(checkpoint_scope)
        .await
        .map_err(|cause| contextual_failure(command, cause))?;

    let run = OrchestrationV2Run {
        id: run_id.clone(),
        thread_id: thread_id.clone(),
        ordinal: turin_contracts::PositiveInt::new(ordinal).expect("run ordinals start at 1"),
        provider_instance_id: instance.clone(),
        model_selection: requested_model.clone(),
        provider_thread_id: Some(provider_thread.id.clone()),
        user_message_id: super::message_dispatch::message_id_of(client),
        root_node_id: Some(root_node_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status: OrchestrationV2RunStatus::Starting,
        queue_position: None,
        queue_held: None,
        requested_at: now,
        started_at: None,
        completed_at: None,
        checkpoint_id: None,
        context_handoff_id: portable_fork_handoff
            .as_ref()
            .or(provider_switch_handoff.as_ref())
            .or(merge_back_handoff.as_ref())
            .or(legacy_import_recovery_handoff.as_ref())
            .map(|handoff| handoff.id.clone()),
        restart_continuation_of_run_id: super::message_dispatch::restart_of(client),
        delegated_task_id: resolved.continued_task_id.clone(),
        work_started_at: wake_work_started_at(
            &projection.runs,
            super::message_dispatch::is_wake(client),
        ),
        restart_cancelled_background_work: None,
        source_plan_ref: super::message_dispatch::source_plan_ref_of(client),
        delegated_completion: None,
        workspace_preparation: None,
    };
    let attempt = OrchestrationV2RunAttempt {
        id: attempt_id,
        native_thread_id: None,
        run_id: run_id.clone(),
        attempt_ordinal: turin_contracts::PositiveInt::new(1).expect("1 is positive"),
        root_node_id: root_node_id.clone(),
        provider_instance_id: instance.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: None,
        reason: OrchestrationV2RunAttemptReason::Initial,
        status: OrchestrationV2RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let root_node = OrchestrationV2ExecutionNode {
        id: root_node_id.clone(),
        thread_id: thread_id.clone(),
        run_id: Some(run_id.clone()),
        parent_node_id: None,
        root_node_id: root_node_id.clone(),
        kind: OrchestrationV2ExecutionNodeKind::RootTurn,
        status: OrchestrationV2ExecutionNodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: Some(ensured_scope.id.clone()),
        started_at: None,
        completed_at: None,
    };
    let mut message = conversation_message(client, thread_id, &run_id, &root_node_id, now);
    resolved.apply(&mut message);
    let user_item = OrchestrationV2TurnItem::UserMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::user_turn_item(&message.id),
        thread_id: thread_id.clone(),
        run_id: Some(run_id.clone()),
        node_id: Some(root_node_id.clone()),
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: ordinal * 100,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        created_by: message.created_by,
        creation_source: message.creation_source,
        message_id: message.id.clone(),
        scheduled_task_id: message.scheduled_task_id.clone(),
        sender_thread_id: message.sender_thread_id.clone(),
        input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
        text: message.text.clone(),
        context: message.context.clone(),
        attachments: message.attachments.clone(),
    };
    let active_handoff = portable_fork_handoff
        .as_ref()
        .or(merge_back_handoff.as_ref())
        .or(provider_switch_handoff.as_ref());
    let handoff_source_runs: Vec<&OrchestrationV2Run> = if portable_fork_handoff.is_some() {
        source_run.into_iter().collect()
    } else if provider_switch_handoff.is_none() {
        merge_back_source_run.into_iter().collect()
    } else {
        switch_covered_runs.clone()
    };
    let handoff_turn_item = active_handoff.map(|handoff| {
        let mut from_instances: Vec<ProviderInstanceId> = Vec::new();
        let mut from_models: Vec<ModelSelection> = Vec::new();
        for run in &handoff_source_runs {
            if !from_instances.contains(&run.provider_instance_id) {
                from_instances.push(run.provider_instance_id.clone());
            }
            if !from_models.iter().any(|model| {
                model.instance_id == run.model_selection.instance_id
                    && model.model == run.model_selection.model
            }) {
                from_models.push(run.model_selection.clone());
            }
        }
        OrchestrationV2TurnItem::Handoff {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: ids::run_signal_turn_item(&run_id, &format!("context-handoff:{}", handoff.id)),
            thread_id: thread_id.clone(),
            run_id: Some(run_id.clone()),
            node_id: Some(root_node_id.clone()),
            provider_thread_id: Some(provider_thread.id.clone()),
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal: ordinal * 100 - 1,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: Some(
                if portable_fork_handoff.is_some() {
                    "Fork context"
                } else if provider_switch_handoff.is_some() {
                    "Provider handoff"
                } else {
                    "Merge-back context"
                }
                .to_owned(),
            ),
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            context_handoff_id: handoff.id.clone(),
            from_provider_thread_ids: handoff.from_provider_thread_ids.clone(),
            to_provider_thread_id: handoff.to_provider_thread_id.clone(),
            from_provider_instance_ids: from_instances,
            to_provider_instance_id: instance.clone(),
            from_model_selections: Some(from_models),
            to_model: Some(requested_model.model.to_string()),
            strategy: transfer_strategy_name(handoff.strategy),
            summary: Some(handoff.summary_text.clone()),
        }
    });
    let native_fork_resolution = (can_resolve_fork_natively)
        .then(|| provider_thread.native_thread_ref.clone())
        .flatten()
        .map(
            |reference| OrchestrationV2ContextTransferResolution::NativeFork {
                provider_thread_ref: reference,
            },
        );
    let portable_fork_resolution = portable_fork_handoff.as_ref().map(|handoff| {
        OrchestrationV2ContextTransferResolution::PortableContext {
            context_handoff_id: handoff.id.clone(),
        }
    });
    let merge_back_resolution = merge_back_handoff.as_ref().map(|handoff| {
        OrchestrationV2ContextTransferResolution::ForkDeltaContext {
            context_handoff_id: handoff.id.clone(),
        }
    });

    // Events, in upstream's order.
    if let (Some(transfer), true) = (pending_fork_transfer, can_resolve_fork_natively) {
        transfer_event!(
            decision,
            ContextTransferUpdated,
            thread_id,
            run_id,
            instance,
            now,
            OrchestrationV2ContextTransfer {
                target_provider_instance_id: Some(instance.clone()),
                target_run_id: Some(run_id.clone()),
                status: OrchestrationV2ContextTransferStatus::Pending,
                resolution: None,
                error: None,
                updated_at: now,
                ..transfer.clone()
            }
        );
    }
    if let (Some(transfer), Some(resolution)) = (pending_fork_transfer, &portable_fork_resolution) {
        transfer_event!(
            decision,
            ContextTransferUpdated,
            thread_id,
            run_id,
            instance,
            now,
            OrchestrationV2ContextTransfer {
                target_provider_instance_id: Some(instance.clone()),
                target_run_id: Some(run_id.clone()),
                status: OrchestrationV2ContextTransferStatus::ResolvedPortable,
                resolution: Some(resolution.clone()),
                error: None,
                updated_at: now,
                ..transfer.clone()
            }
        );
    }
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: provider_thread.clone(),
    });
    if let Some(handoff) = &portable_fork_handoff {
        transfer_event!(
            decision,
            ContextHandoffUpdated,
            thread_id,
            run_id,
            instance,
            now,
            handoff.clone()
        );
    }
    if let Some(handoff) = &legacy_import_recovery_handoff {
        transfer_event!(
            decision,
            ContextHandoffUpdated,
            thread_id,
            run_id,
            instance,
            now,
            handoff.clone()
        );
    }
    if let (Some(transfer_id), Some(handoff), Some(latest)) = (
        &switch_transfer_id,
        &provider_switch_handoff,
        latest_handoff_run,
    ) {
        let transfer = OrchestrationV2ContextTransfer {
            id: transfer_id.clone(),
            r#type: OrchestrationV2ContextTransferType::ProviderHandoff,
            source_thread_id: thread_id.clone(),
            target_thread_id: thread_id.clone(),
            source_point: context_source_point_for_run(projection, latest),
            base_point: if requires_full_context || target_last_completed_run.is_none() {
                None
            } else {
                target_last_completed_run.map(|run| context_source_point_for_run(projection, run))
            },
            source_provider_instance_id: Some(latest.provider_instance_id.clone()),
            target_provider_instance_id: Some(instance.clone()),
            target_run_id: Some(run_id.clone()),
            status: OrchestrationV2ContextTransferStatus::Consumed,
            resolution: Some(
                if handoff.strategy == OrchestrationV2ContextHandoffStrategy::FullThreadSummary {
                    OrchestrationV2ContextTransferResolution::PortableContext {
                        context_handoff_id: handoff.id.clone(),
                    }
                } else {
                    OrchestrationV2ContextTransferResolution::DeltaContext {
                        context_handoff_id: handoff.id.clone(),
                    }
                },
            ),
            created_by: message.created_by,
            error: None,
            created_at: now,
            updated_at: now,
            consumed_at: Some(now),
        };
        transfer_event!(
            decision,
            ContextTransferCreated,
            thread_id,
            run_id,
            instance,
            now,
            transfer
        );
        transfer_event!(
            decision,
            ContextHandoffUpdated,
            thread_id,
            run_id,
            instance,
            now,
            handoff.clone()
        );
    }
    if let Some(handoff) = &merge_back_handoff {
        transfer_event!(
            decision,
            ContextHandoffUpdated,
            thread_id,
            run_id,
            instance,
            now,
            handoff.clone()
        );
    }
    for superseded in &superseded_merge_backs {
        transfer_event!(
            decision,
            ContextTransferUpdated,
            thread_id,
            run_id,
            instance,
            now,
            OrchestrationV2ContextTransfer {
                status: OrchestrationV2ContextTransferStatus::Superseded,
                error: Some(match pending_merge_back {
                    None => "Superseded while consuming merge-back transfer.".to_owned(),
                    Some(latest) => format!("Superseded by merge-back transfer {}.", latest.id),
                }),
                updated_at: now,
                ..(*superseded).clone()
            }
        );
    }
    if let (Some(transfer), Some(resolution)) = (pending_merge_back, &merge_back_resolution) {
        transfer_event!(
            decision,
            ContextTransferUpdated,
            thread_id,
            run_id,
            instance,
            now,
            OrchestrationV2ContextTransfer {
                target_provider_instance_id: Some(instance.clone()),
                target_run_id: Some(run_id.clone()),
                status: OrchestrationV2ContextTransferStatus::Consumed,
                resolution: Some(resolution.clone()),
                error: None,
                updated_at: now,
                consumed_at: Some(now),
                ..transfer.clone()
            }
        );
    }
    run_scoped_event!(
        decision,
        RunCreated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        run
    );
    resolved.complete_source_plan(decision, now);
    run_scoped_event!(
        decision,
        RunAttemptCreated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        attempt
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        root_node
    );
    run_scoped_event!(
        decision,
        CheckpointScopeCreated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        ensured_scope
    );
    if let Some(item) = handoff_turn_item {
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            thread_id,
            &run_id,
            &root_node_id,
            instance,
            item
        );
    }
    let notification_item = notification_turn_item(
        &user_item,
        NotificationMessage::from(&message),
        &projection.subagents,
    );
    run_scoped_event!(
        decision,
        MessageUpdated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        message
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &run_id,
        &root_node_id,
        instance,
        notification_item
    );
    if let Some(forked) = pending_fork_transfer
        && let Some(resolution) = native_fork_resolution.or(portable_fork_resolution)
    {
        transfer_event!(
            decision,
            ContextTransferUpdated,
            thread_id,
            run_id,
            instance,
            now,
            OrchestrationV2ContextTransfer {
                target_provider_instance_id: Some(instance.clone()),
                target_run_id: Some(run_id.clone()),
                status: OrchestrationV2ContextTransferStatus::Consumed,
                resolution: Some(resolution),
                error: None,
                updated_at: now,
                consumed_at: Some(now),
                ..forked.clone()
            }
        );
    }
    if let (true, Some(active)) = (can_resume_across_instances, active_provider_thread)
        && let Some(previous_session_id) = active.provider_session_id.clone()
    {
        const REASON: &str = "Provider account changed; continuing the native thread.";
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::ProviderSessionDetached {
            id,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: Some(active.driver.clone()),
            provider_instance_id: Some(active.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2ProviderSessionDetached {
                provider_session_id: previous_session_id.clone(),
                detached_at: now,
                reason: Some(REASON.to_owned()),
            },
        });
        decision.effects.push(PendingEffect {
            id: format!(
                "effect:{}:provider-session.detach:{previous_session_id}",
                command_id(command)
            ),
            command_id: command_id(command).clone(),
            thread_id: thread_id.clone(),
            request: EffectRequest::ProviderSessionDetach {
                provider_session_id: previous_session_id,
                detail: Some(REASON.to_owned()),
                revoke_mcp_credential: None,
            },
            available_at: None,
        });
    }
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-turn.start:{run_id}",
            command_id(command)
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderTurnStart {
            run_id: run_id.clone(),
        },
        available_at: None,
    });
    Ok(())
}
