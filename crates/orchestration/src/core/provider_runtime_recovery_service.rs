//! Port of upstream/apps/server/src/orchestration-v2/ProviderRuntimeRecoveryService.ts.
//!
//! No provider process outlives the server, so on startup and on shutdown this
//! service closes what those processes left open: nonterminal runs and their
//! attempts, nodes, turns, streaming messages and items; runtime requests that
//! needed a live callback; provider sessions; and the roster of background
//! work. Work that outlived its settled turn is recorded on the thread's latest
//! run, so the next provider turn is told. One command per thread commits it
//! all, and cancels that thread's process-bound effects with it.

use std::{collections::HashSet, sync::Arc};
use turin_shared::orchestration_v2_thread_error::run_ran_after;

use async_trait::async_trait;
use turin_contracts::{
    CommandId, ProviderThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2CompletionWake, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ResponseCapability, OrchestrationV2RestartCancelledBackgroundWork as Work,
        OrchestrationV2Run, OrchestrationV2RunAttemptStatus,
        OrchestrationV2RunBackgroundWorkCancelled, OrchestrationV2RunStatus,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2SecretRequestStatus,
        OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::{
    id_allocator::IdAllocator,
    subagent_projection::{
        EndOrphanedNativeSubagentInput, EndRunlessRootTurnsInput, EndedWorkStatus,
        end_orphaned_native_subagent, end_runless_root_turns,
    },
};

use super::{
    orchestrator::services::{ProviderRuntimeRecovery, ReconcileTrigger, RecoverySummary},
    restart_background_note::{
        cancelled_roster_task_work, cancelled_turn_item_work,
        merge_restart_cancelled_background_work,
    },
    restart_continuation::restart_continuation_run,
};
use crate::{
    pipeline::{
        effect_outbox::{
            Effect, EffectOutbox, EffectRequest, EffectStatus, PROCESS_BOUND_EFFECT_TYPES,
            PendingEffect,
        },
        event_sink::{CancelUnsettled, CommandCommit, EventSink, WriteInput},
        orchestrator_port::BoxError,
        seams::ServerSettingsSource,
    },
    projection::{
        store::{
            ProjectionStore,
            canonical::{ProjectionRecordField, ProjectionRecordFilter},
            recovery::ProjectionRecoveryKind,
            runtime_recovery::RuntimeRecoveryState,
        },
        turn_item,
    },
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `ProviderRuntimeRecoveryError.operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOperation {
    ReadProjections,
    Reconcile,
    DrainOutbox,
}

impl RecoveryOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadProjections => "read-projections",
            Self::Reconcile => "reconcile",
            Self::DrainOutbox => "drain-outbox",
        }
    }
}

/// `ProviderRuntimeRecoveryError`.
#[derive(Debug, thiserror::Error)]
#[error("Provider runtime recovery failed during {}.", .operation.as_str())]
pub struct ProviderRuntimeRecoveryError {
    pub operation: RecoveryOperation,
    pub thread_id: Option<turin_contracts::ThreadId>,
    #[source]
    pub cause: BoxError,
}

fn recovery_error(
    operation: RecoveryOperation,
    thread_id: Option<&turin_contracts::ThreadId>,
) -> impl FnOnce(BoxError) -> ProviderRuntimeRecoveryError {
    let thread_id = thread_id.cloned();
    move |cause| ProviderRuntimeRecoveryError {
        operation,
        thread_id,
        cause,
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(error: E) -> BoxError {
    Box::new(error)
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

fn nonterminal_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

fn background_capable(item: &OrchestrationV2TurnItem) -> bool {
    matches!(
        item,
        OrchestrationV2TurnItem::CommandExecution { .. }
            | OrchestrationV2TurnItem::DynamicTool { .. }
            | OrchestrationV2TurnItem::Subagent { .. }
    )
}

fn open_item_status(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

fn open_node_status(status: OrchestrationV2ExecutionNodeStatus) -> bool {
    matches!(
        status,
        OrchestrationV2ExecutionNodeStatus::Pending
            | OrchestrationV2ExecutionNodeStatus::Running
            | OrchestrationV2ExecutionNodeStatus::Waiting
    )
}

/// A delegate_task child. Its own thread is reconciled and continued on its own
/// and reports back through the app, so it is not provider background work.
fn app_owned_delegation_item(item: &OrchestrationV2TurnItem) -> bool {
    matches!(
        item,
        OrchestrationV2TurnItem::Subagent {
            origin: OrchestrationV2SubagentOrigin::AppOwned,
            child_thread_id: Some(_),
            ..
        }
    )
}

/// An open item closed by recovery. A secret card also closes its form, so it
/// takes no answer.
fn cancelled_item(item: &OrchestrationV2TurnItem, now: UtcDateTime) -> OrchestrationV2TurnItem {
    let mut value = serde_json::to_value(item).expect("turn item encodes");
    let fields = value.as_object_mut().expect("turn item is an object");
    let stamp = serde_json::to_value(now).expect("timestamp encodes");
    fields.insert("status".to_owned(), "cancelled".into());
    fields.insert("completedAt".to_owned(), stamp.clone());
    fields.insert("updatedAt".to_owned(), stamp);
    if matches!(
        item,
        OrchestrationV2TurnItem::SecretRequest {
            secret_status: OrchestrationV2SecretRequestStatus::Pending,
            ..
        }
    ) {
        fields.insert("secretStatus".to_owned(), "cancelled".into());
    }
    serde_json::from_value(value).expect("cancelled turn item decodes")
}

/// The providerInstanceId for a stale background-capable turn item whose run is
/// missing or null: an existing run, then a subagent item's own instance, then
/// the item's provider thread, then the first provider thread, then the
/// thread's selected provider.
fn stale_item_provider_instance_id(
    item: &OrchestrationV2TurnItem,
    projection: &RuntimeRecoveryState,
) -> turin_contracts::provider_instance::ProviderInstanceId {
    let common = turn_item::common(item);
    if let Some(run) = common
        .run_id
        .and_then(|run_id| projection.runs.iter().find(|run| &run.id == run_id))
    {
        return run.provider_instance_id.clone();
    }
    if let OrchestrationV2TurnItem::Subagent {
        provider_instance_id,
        ..
    } = item
    {
        return provider_instance_id.clone();
    }
    if let Some(provider_thread) = common.provider_thread_id.and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|candidate| &candidate.id == id)
    }) {
        return provider_thread.provider_instance_id.clone();
    }
    projection
        .provider_threads
        .first()
        .map(|thread| thread.provider_instance_id.clone())
        .unwrap_or_else(|| projection.thread.provider_instance_id.clone())
}

/// A provider thread's latest started run: the last turn that provider saw.
/// Restart recovery records the thread's cancelled background work on it, and
/// the next run on the same provider thread delivers it with its input.
fn latest_started_run<'a>(
    projection: &'a RuntimeRecoveryState,
    provider_thread_id: &ProviderThreadId,
) -> Option<&'a OrchestrationV2Run> {
    projection.runs.iter().fold(None, |latest, run| {
        if run.provider_thread_id.as_ref() == Some(provider_thread_id)
            && run.status != OrchestrationV2RunStatus::Queued
            && run.status != OrchestrationV2RunStatus::RolledBack
            && latest.is_none_or(|latest| run_ran_after(run, latest))
        {
            Some(run)
        } else {
            latest
        }
    })
}

/// Background work that outlived its settled turn, keyed by the provider thread
/// that lost it. The provider transcript cannot record its death, so the next
/// provider turn is told instead. Insertion order is kept: it is event order.
#[derive(Default)]
struct CancelledBackgroundWork {
    by_thread: Vec<(ProviderThreadId, Vec<Work>)>,
    native_ids: HashSet<String>,
}

impl CancelledBackgroundWork {
    fn record(&mut self, provider_thread_id: Option<&ProviderThreadId>, work: Work) {
        let Some(provider_thread_id) = provider_thread_id else {
            return;
        };
        match self
            .by_thread
            .iter_mut()
            .find(|(candidate, _)| candidate == provider_thread_id)
        {
            Some((_, existing)) => existing.push(work),
            None => self
                .by_thread
                .push((provider_thread_id.clone(), vec![work])),
        }
    }

    fn record_item(&mut self, projection: &RuntimeRecoveryState, item: &OrchestrationV2TurnItem) {
        if !background_capable(item) {
            return;
        }
        let Some(work) = cancelled_turn_item_work(item) else {
            return;
        };
        let common = turn_item::common(item);
        let provider_thread_id = common.provider_thread_id.or_else(|| {
            projection
                .runs
                .iter()
                .find(|run| Some(&run.id) == common.run_id)
                .and_then(|run| run.provider_thread_id.as_ref())
        });
        self.record(provider_thread_id, work);
        if let Some(native_id) = common
            .native_item_ref
            .and_then(|native| native.native_id.as_ref())
        {
            self.native_ids.insert(native_id.as_str().to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// `ProviderRuntimeRecoveryService`. The projection store, event sink and
/// outbox are the real ones: the recovery contract is the rows they hold.
pub struct ProviderRuntimeRecoveryService {
    settings: Arc<dyn ServerSettingsSource>,
    projections: Arc<ProjectionStore>,
    event_sink: Arc<EventSink>,
}

#[derive(Default)]
struct ThreadResult {
    terminalized_runs: usize,
    stopped_sessions: usize,
    closed_requests: usize,
    retired_effects: usize,
}

fn trigger_name(trigger: ReconcileTrigger) -> &'static str {
    match trigger {
        ReconcileTrigger::Startup => "startup",
        ReconcileTrigger::Shutdown => "shutdown",
    }
}

impl ProviderRuntimeRecoveryService {
    pub fn new(
        settings: Arc<dyn ServerSettingsSource>,
        projections: Arc<ProjectionStore>,
        event_sink: Arc<EventSink>,
    ) -> Self {
        Self {
            settings,
            projections,
            event_sink,
        }
    }

    fn outbox(&self) -> &EffectOutbox {
        self.event_sink.outbox()
    }

    /// Whether this thread's project wants its run continued after a restart.
    fn continues_after_restart(
        settings: Option<&turin_contracts::settings::ServerSettings>,
        projection: &RuntimeRecoveryState,
    ) -> bool {
        settings.is_some_and(|settings| {
            turin_shared::project_settings::resolve_project_settings(
                settings,
                Some(&projection.thread.project_id),
                None,
                &|_, _, _| None,
            )
            .settings
            .continue_threads_after_server_update
        })
    }

    async fn reconcile_projection(
        &self,
        projection: &RuntimeRecoveryState,
        trigger: ReconcileTrigger,
        continue_after_restart: bool,
    ) -> Result<ThreadResult, ProviderRuntimeRecoveryError> {
        let thread_id = &projection.thread.id;
        let reconcile_error = |cause: BoxError| ProviderRuntimeRecoveryError {
            operation: RecoveryOperation::Reconcile,
            thread_id: Some(thread_id.clone()),
            cause,
        };
        let now = UtcDateTime::now();
        let startup = trigger == ReconcileTrigger::Startup;

        let mut runs: Vec<&OrchestrationV2Run> = Vec::new();
        for run in projection.runs.iter().filter(|run| nonterminal_run(run)) {
            if run.status == OrchestrationV2RunStatus::Waiting {
                let capture_command =
                    CommandId::new(format!("command:effect:checkpoint.capture:{}", run.id))
                        .expect("effect command id is non-blank");
                let effects = self
                    .outbox()
                    .list_by_command_id(capture_command)
                    .await
                    .map_err(|cause| {
                        recovery_error(RecoveryOperation::Reconcile, Some(thread_id))(boxed(cause))
                    })?;
                let replayable = effects.iter().any(|effect: &Effect| {
                    matches!(
                        &effect.request,
                        EffectRequest::CheckpointCapture { run_id, .. } if run_id == &run.id
                    ) && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
                });
                if replayable {
                    continue;
                }
            }
            runs.push(run);
        }

        let message_request_node_ids: HashSet<_> = projection
            .runtime_requests
            .iter()
            .filter(|request| {
                request.status == OrchestrationV2RuntimeRequestStatus::Pending
                    && matches!(
                        request.response_capability,
                        OrchestrationV2ResponseCapability::Message
                    )
            })
            .map(|request| &request.node_id)
            .collect();
        let requests: Vec<_> = projection
            .runtime_requests
            .iter()
            .filter(|request| {
                request.status == OrchestrationV2RuntimeRequestStatus::Pending
                    && !matches!(
                        request.response_capability,
                        OrchestrationV2ResponseCapability::Message
                    )
            })
            .collect();
        // Delegated task rows, items and nodes stay open: the child settles them.
        let delegated_task_node_ids: HashSet<String> = projection
            .subagents
            .iter()
            .filter(|task| {
                task.origin == OrchestrationV2SubagentOrigin::AppOwned
                    && task.child_thread_id.is_some()
            })
            .map(|task| task.id.as_str().to_owned())
            .chain(projection.turn_items.iter().filter_map(|item| match item {
                OrchestrationV2TurnItem::Subagent { subagent_id, .. }
                    if app_owned_delegation_item(item) =>
                {
                    Some(subagent_id.as_str().to_owned())
                }
                _ => None,
            }))
            .collect();

        let detail = format!(
            "Cancelled because the server {} before the provider work completed.",
            if startup { "restarted" } else { "shut down" }
        );
        let command_id = CommandId::new(format!(
            "command:runtime-reconcile:{}:{}:{}",
            trigger_name(trigger),
            thread_id,
            now
        ))
        .expect("reconcile command id is non-blank");
        let event_id = || IdAllocator.event(Some(thread_id), Some(&command_id), None);

        let mut events: Vec<OrchestrationV2DomainEvent> = Vec::new();
        let mut cancelled_work = CancelledBackgroundWork::default();

        let queued_runs: Vec<&OrchestrationV2Run> = projection
            .runs
            .iter()
            .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
            .collect();
        let queued_messages = if queued_runs.is_empty() {
            Vec::new()
        } else {
            self.projections
                .get_thread_records(
                    thread_id,
                    vec![ProjectionRecordField::Messages],
                    Some(ProjectionRecordFilter {
                        message_ids: Some(
                            queued_runs
                                .iter()
                                .map(|run| run.user_message_id.as_str().to_owned())
                                .collect(),
                        ),
                        ..ProjectionRecordFilter::default()
                    }),
                )
                .await
                .map_err(|cause| {
                    recovery_error(RecoveryOperation::ReadProjections, Some(thread_id))(boxed(
                        cause,
                    ))
                })?
                .messages
        };
        let buffered_message_ids: HashSet<_> = queued_messages
            .iter()
            .filter(|message| {
                crate::pipeline::notification_mailbox::is_buffered_provider_notification(message)
            })
            .map(|message| &message.id)
            .collect();
        // Buffered wakes lost their output with the process. Durable queued
        // messages keep their execution identities and order until the user
        // resumes them.
        for run in &queued_runs {
            if buffered_message_ids.contains(&run.user_message_id) {
                runs.push(run);
                continue;
            }
            if run.queue_held == Some(true) {
                continue;
            }
            let mut payload = (*run).clone();
            payload.queue_held = Some(true);
            events.push(OrchestrationV2DomainEvent::RunUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: None,
                driver: None,
                provider_instance_id: Some(run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }

        for request in &requests {
            let mut payload = (*request).clone();
            payload.status = if startup {
                OrchestrationV2RuntimeRequestStatus::Expired
            } else {
                OrchestrationV2RuntimeRequestStatus::Cancelled
            };
            payload.response_capability = OrchestrationV2ResponseCapability::NotResumable {
                reason: format!(
                    "The server {} before this runtime request was resolved.",
                    if startup { "restarted" } else { "shut down" }
                ),
            };
            payload.resolved_at = Some(now);
            events.push(OrchestrationV2DomainEvent::RuntimeRequestUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: Some(request.node_id.clone()),
                driver: None,
                provider_instance_id: None,
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }

        for run in &runs {
            let instance = Some(run.provider_instance_id.clone());
            let mut payload = (*run).clone();
            payload.status = OrchestrationV2RunStatus::Cancelled;
            payload.queue_position = Some(None);
            payload.completed_at = Some(now);
            events.push(OrchestrationV2DomainEvent::RunUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: None,
                driver: None,
                provider_instance_id: instance.clone(),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
            for attempt in projection.attempts.iter().filter(|attempt| {
                attempt.run_id == run.id
                    && matches!(
                        attempt.status,
                        OrchestrationV2RunAttemptStatus::Pending
                            | OrchestrationV2RunAttemptStatus::Running
                    )
            }) {
                let mut payload = attempt.clone();
                payload.status = OrchestrationV2RunAttemptStatus::Cancelled;
                payload.completed_at = Some(now);
                events.push(OrchestrationV2DomainEvent::RunAttemptUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(attempt.root_node_id.clone()),
                    driver: None,
                    provider_instance_id: instance.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload,
                });
            }
            for node in projection.nodes.iter().filter(|node| {
                node.run_id.as_ref() == Some(&run.id)
                    && !message_request_node_ids.contains(&node.id)
                    && !delegated_task_node_ids.contains(node.id.as_str())
                    && open_node_status(node.status)
            }) {
                let mut payload = node.clone();
                payload.status = OrchestrationV2ExecutionNodeStatus::Cancelled;
                payload.completed_at = Some(now);
                events.push(OrchestrationV2DomainEvent::NodeUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(node.id.clone()),
                    driver: None,
                    provider_instance_id: instance.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload,
                });
            }
            for provider_turn in projection.provider_turns.iter().filter(|turn| {
                turn.run_attempt_id.as_ref().is_some_and(|attempt_id| {
                    projection
                        .attempts
                        .iter()
                        .any(|attempt| &attempt.id == attempt_id && attempt.run_id == run.id)
                }) && matches!(
                    turn.status,
                    OrchestrationV2ProviderTurnStatus::Pending
                        | OrchestrationV2ProviderTurnStatus::Running
                )
            }) {
                let mut payload = provider_turn.clone();
                payload.status = OrchestrationV2ProviderTurnStatus::Cancelled;
                payload.completed_at = Some(now);
                events.push(OrchestrationV2DomainEvent::ProviderTurnUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(provider_turn.node_id.clone()),
                    driver: None,
                    provider_instance_id: instance.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload,
                });
            }
            for message in projection
                .messages
                .iter()
                .filter(|message| message.run_id.as_ref() == Some(&run.id) && message.streaming)
            {
                let mut payload = message.clone();
                payload.streaming = false;
                payload.updated_at = now;
                events.push(OrchestrationV2DomainEvent::MessageUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: message.node_id.clone(),
                    driver: None,
                    provider_instance_id: instance.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload,
                });
            }
            for item in projection.turn_items.iter().filter(|item| {
                let common = turn_item::common(item);
                common.run_id == Some(&run.id)
                    && common
                        .node_id
                        .is_none_or(|node_id| !message_request_node_ids.contains(node_id))
                    && !app_owned_delegation_item(item)
                    && open_item_status(common.status)
            }) {
                // A waiting run's provider turn already settled; its open items
                // are background work. A running run's items die with its turn.
                if run.status == OrchestrationV2RunStatus::Waiting {
                    cancelled_work.record_item(projection, item);
                }
                events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: turn_item::common(item).node_id.cloned(),
                    driver: None,
                    provider_instance_id: instance.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: cancelled_item(item, now),
                });
            }
        }

        // A blocking delegate_task wait dies with the turn that made it, and its
        // continuation never got the tool result. Let the child's result wake
        // the parent like an async task, so it reaches the continued turn.
        let cut_run_ids: HashSet<_> = runs.iter().map(|run| &run.id).collect();
        for task in &projection.subagents {
            if task.origin != OrchestrationV2SubagentOrigin::AppOwned
                || !task
                    .run_id
                    .as_ref()
                    .is_some_and(|id| cut_run_ids.contains(id))
                || task.completion_wake != Some(OrchestrationV2CompletionWake::SettledOnly)
                || !matches!(
                    task.status,
                    OrchestrationV2SubagentStatus::Pending
                        | OrchestrationV2SubagentStatus::Running
                        | OrchestrationV2SubagentStatus::Waiting
                )
            {
                continue;
            }
            let mut payload = task.clone();
            payload.completion_wake = Some(OrchestrationV2CompletionWake::Always);
            payload.updated_at = now;
            events.push(OrchestrationV2DomainEvent::SubagentUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: task.run_id.clone(),
                node_id: Some(task.id.clone()),
                driver: Some(task.driver.clone()),
                provider_instance_id: Some(task.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }

        // Process loss also orphans background-capable turn items on already
        // settled runs (e.g. post-settle Waiting work). Items already cancelled
        // above for recovered nonterminal runs are skipped, to avoid duplicate
        // cancellation events.
        let mut cancelled_stale_node_ids: HashSet<String> = HashSet::new();
        for item in &projection.turn_items {
            let common = turn_item::common(item);
            if common.run_id.is_some_and(|id| cut_run_ids.contains(id))
                || !background_capable(item)
                || !open_item_status(common.status)
                || app_owned_delegation_item(item)
            {
                continue;
            }
            let instance = stale_item_provider_instance_id(item, projection);
            cancelled_work.record_item(projection, item);
            events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: common.run_id.cloned(),
                node_id: common.node_id.cloned(),
                driver: None,
                provider_instance_id: Some(instance.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: cancelled_item(item, now),
            });
            let Some(node_id) = common.node_id else {
                continue;
            };
            if let Some(node) = projection
                .nodes
                .iter()
                .find(|node| &node.id == node_id && open_node_status(node.status))
                && cancelled_stale_node_ids.insert(node.id.as_str().to_owned())
            {
                let mut payload = node.clone();
                payload.status = OrchestrationV2ExecutionNodeStatus::Cancelled;
                payload.completed_at = Some(now);
                events.push(OrchestrationV2DomainEvent::NodeUpdated {
                    id: event_id(),
                    thread_id: thread_id.clone(),
                    run_id: common.run_id.cloned(),
                    node_id: Some(node.id.clone()),
                    driver: None,
                    provider_instance_id: Some(instance),
                    raw_event_id: None,
                    occurred_at: now,
                    payload,
                });
            }
        }

        // No provider process outlives the server, so a native subagent still
        // open here never reports its end, whatever state its run and item are in.
        for subagent in &projection.subagents {
            let ended = end_orphaned_native_subagent(
                EndOrphanedNativeSubagentInput {
                    nodes: &projection.nodes,
                    subagents: &projection.subagents,
                    turn_items: &projection.turn_items,
                    subagent_id: &subagent.id,
                    status: EndedWorkStatus::Cancelled,
                    completed_at: None,
                    result: None,
                    now,
                    emitted: &events,
                },
                || async { Ok::<_, BoxError>(event_id()) },
            )
            .await
            .map_err(reconcile_error)?;
            events.extend(ended);
        }
        // A provider-native subagent thread has no runs; left running, the child
        // would show as working forever.
        let ended = end_runless_root_turns(
            EndRunlessRootTurnsInput {
                thread_id,
                provider_instance_id: &projection.thread.provider_instance_id,
                nodes: &projection.nodes,
                turn_items: &projection.turn_items,
                status: EndedWorkStatus::Cancelled,
                completed_at: None,
                now,
                emitted: &events,
            },
            || async { Ok::<_, BoxError>(event_id()) },
        )
        .await
        .map_err(reconcile_error)?;
        events.extend(ended);

        // All provider processes are gone on startup/shutdown: clear any
        // persisted Waiting roster (including idle threads from settled roots)
        // and idle active threads without resurrecting active status.
        for provider_thread in &projection.provider_threads {
            let needs_idle = provider_thread.status == OrchestrationV2ProviderThreadStatus::Active;
            let needs_roster_clear = !provider_thread.pending_background_tasks.is_empty();
            if !needs_idle && !needs_roster_clear {
                continue;
            }
            if provider_thread.owner_node_id.is_none() {
                for task in &provider_thread.pending_background_tasks {
                    if !cancelled_work
                        .native_ids
                        .insert(task.task_id().as_str().to_owned())
                    {
                        continue;
                    }
                    cancelled_work
                        .record(Some(&provider_thread.id), cancelled_roster_task_work(task));
                }
            }
            let mut payload = provider_thread.clone();
            if needs_idle {
                payload.status = OrchestrationV2ProviderThreadStatus::Idle;
            }
            payload.pending_background_tasks = Vec::new();
            payload.updated_at = now;
            events.push(OrchestrationV2DomainEvent::ProviderThreadUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: Some(provider_thread.driver.clone()),
                provider_instance_id: Some(provider_thread.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }

        let live_sessions: Vec<_> = projection
            .provider_sessions
            .iter()
            .filter(|session| {
                !matches!(
                    session.status,
                    OrchestrationV2ProviderSessionStatus::Stopped
                        | OrchestrationV2ProviderSessionStatus::Error
                )
            })
            .collect();
        for session in &live_sessions {
            let mut payload = (*session).clone();
            payload.status = OrchestrationV2ProviderSessionStatus::Stopped;
            payload.updated_at = now;
            payload.last_error = None;
            events.push(OrchestrationV2DomainEvent::ProviderSessionUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: Some(session.driver.clone()),
                provider_instance_id: Some(session.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }

        for (provider_thread_id, work) in &cancelled_work.by_thread {
            let Some(note_run) = latest_started_run(projection, provider_thread_id) else {
                continue;
            };
            // Its own event: a run snapshot read before this commit could
            // regress a lifecycle change (e.g. a checkpoint completing the run)
            // made since.
            events.push(OrchestrationV2DomainEvent::RunBackgroundWorkCancelled {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: Some(note_run.id.clone()),
                node_id: None,
                driver: None,
                provider_instance_id: Some(note_run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: OrchestrationV2RunBackgroundWorkCancelled {
                    run_id: note_run.id.clone(),
                    restart_cancelled_background_work: merge_restart_cancelled_background_work(
                        note_run
                            .restart_cancelled_background_work
                            .as_deref()
                            .unwrap_or_default(),
                        work,
                    ),
                },
            });
        }

        let continuation_run = (continue_after_restart && startup)
            .then(|| restart_continuation_run(projection, now.as_datetime()))
            .flatten();
        let effects: Vec<PendingEffect> = continuation_run
            .map(|run| PendingEffect {
                id: format!("effect:restart-continuation:{}", run.id),
                command_id: command_id.clone(),
                thread_id: thread_id.clone(),
                request: EffectRequest::ProviderRuntimeContinue {
                    source_run_id: run.id.clone(),
                },
                available_at: None,
            })
            .into_iter()
            .collect();

        let retired_effects = if events.is_empty() {
            let thread = thread_id.clone();
            let reason = detail.clone();
            let retired = self
                .outbox()
                .db()
                .write(move |tx| {
                    EffectOutbox::cancel_unsettled_in(
                        tx,
                        &thread,
                        PROCESS_BOUND_EFFECT_TYPES,
                        &reason,
                    )
                })
                .await
                .map_err(|cause| reconcile_error(boxed(cause)))?;
            self.outbox().signal_cancellations(&retired);
            retired.len()
        } else {
            self.event_sink
                .commit_command(CommandCommit {
                    command_id: command_id.clone(),
                    thread_id: thread_id.clone(),
                    command_type: "provider-runtime.reconcile".to_owned(),
                    accepted_at: now,
                    events,
                    effects,
                    cancel_unsettled_effects: Some(CancelUnsettled {
                        effect_types: PROCESS_BOUND_EFFECT_TYPES.to_vec(),
                        reason: detail,
                    }),
                })
                .await
                .map_err(|cause| {
                    recovery_error(RecoveryOperation::Reconcile, Some(thread_id))(boxed(cause))
                })?
                .cancelled_effect_count
        };
        Ok(ThreadResult {
            terminalized_runs: runs.len(),
            stopped_sessions: live_sessions.len(),
            closed_requests: requests.len(),
            retired_effects,
        })
    }

    pub async fn reconcile(
        &self,
        trigger: ReconcileTrigger,
    ) -> Result<RecoverySummary, ProviderRuntimeRecoveryError> {
        let settings = self.settings.get_settings().await.ok();
        let thread_ids = self
            .projections
            .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
            .await
            .map_err(|cause| {
                recovery_error(RecoveryOperation::ReadProjections, None)(boxed(cause))
            })?;
        let mut summary = RecoverySummary::default();
        for thread_id in thread_ids {
            let projection = self
                .projections
                .get_runtime_recovery_projection(&thread_id)
                .await
                .map_err(|cause| {
                    recovery_error(RecoveryOperation::ReadProjections, Some(&thread_id))(boxed(
                        cause,
                    ))
                })?;
            let enabled = Self::continues_after_restart(settings.as_ref(), &projection);
            let result = self
                .reconcile_projection(&projection, trigger, enabled)
                .await?;
            summary.terminalized_runs += result.terminalized_runs;
            summary.stopped_sessions += result.stopped_sessions;
            summary.closed_requests += result.closed_requests;
            summary.retired_effects += result.retired_effects;
        }
        let outbox = self
            .outbox()
            .reconcile_after_process_loss()
            .await
            .map_err(|cause| recovery_error(RecoveryOperation::DrainOutbox, None)(boxed(cause)))?;
        summary.retired_effects += outbox.cancelled;
        summary.requeued_effects = outbox.requeued;
        Ok(summary)
    }

    /// Snapshots restart intent only while providers are live. A provider may
    /// finish while this commits; reconciliation reads fresh state after
    /// shutdown, and delivery rejects any source run that actually completed.
    pub async fn prepare_for_shutdown(&self) -> Result<(), ProviderRuntimeRecoveryError> {
        let reconcile_error = |cause| recovery_error(RecoveryOperation::Reconcile, None)(cause);
        let Ok(settings) = self.settings.get_settings().await else {
            return Ok(());
        };
        let thread_ids = self
            .projections
            .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
            .await
            .map_err(|cause| reconcile_error(boxed(cause)))?;
        for thread_id in thread_ids {
            // One failing thread must not cost the threads after it their continuation.
            if let Err(cause) = self
                .prepare_thread_for_shutdown(&settings, &thread_id)
                .await
            {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %cause,
                    "Failed to prepare a restart continuation"
                );
            }
        }
        Ok(())
    }

    async fn prepare_thread_for_shutdown(
        &self,
        settings: &turin_contracts::settings::ServerSettings,
        thread_id: &turin_contracts::ThreadId,
    ) -> Result<(), BoxError> {
        let projection = self
            .projections
            .get_runtime_recovery_projection(thread_id)
            .await
            .map_err(boxed)?;
        if !Self::continues_after_restart(Some(settings), &projection) {
            return Ok(());
        }
        let Some(run) = restart_continuation_run(&projection, UtcDateTime::now().as_datetime())
        else {
            return Ok(());
        };
        let command_id = CommandId::new(format!("command:restart-prepare:{}", run.id))
            .expect("prepare command id is non-blank");
        self.event_sink
            .write(WriteInput {
                command_id: Some(command_id.clone()),
                events: Vec::new(),
                effects: vec![PendingEffect {
                    id: format!("effect:restart-continuation:{}", run.id),
                    command_id,
                    thread_id: thread_id.clone(),
                    request: EffectRequest::ProviderRuntimeContinue {
                        source_run_id: run.id.clone(),
                    },
                    available_at: None,
                }],
                guard_pending_user_input_cancellations: false,
            })
            .await
            .map_err(boxed)?;
        Ok(())
    }

    pub async fn recover(&self) -> Result<RecoverySummary, ProviderRuntimeRecoveryError> {
        self.reconcile(ReconcileTrigger::Startup).await
    }
}

#[async_trait]
impl ProviderRuntimeRecovery for ProviderRuntimeRecoveryService {
    async fn reconcile(&self, trigger: ReconcileTrigger) -> Result<RecoverySummary, BoxError> {
        ProviderRuntimeRecoveryService::reconcile(self, trigger)
            .await
            .map_err(boxed)
    }

    async fn prepare_for_shutdown(&self) -> Result<(), BoxError> {
        ProviderRuntimeRecoveryService::prepare_for_shutdown(self)
            .await
            .map_err(boxed)
    }

    async fn recover(&self) -> Result<RecoverySummary, BoxError> {
        ProviderRuntimeRecoveryService::recover(self)
            .await
            .map_err(boxed)
    }
}

#[cfg(test)]
mod tests;
