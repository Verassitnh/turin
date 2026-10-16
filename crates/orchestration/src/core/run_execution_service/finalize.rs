//! Port of upstream/apps/server/src/orchestration-v2/RunExecutionService.ts (lines 583-820).
//!
//! `writeFinalRunEvents`: the terminal write of a root run. It settles the attempt, the run, its root
//! node and the provider thread in one transaction together with the
//! checkpoint capture effect.

use turin_contracts::{
    CommandId, RunAttemptId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2CheckpointScope, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ProviderThread, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
    },
};
use turin_provider_core::{
    failure::{ProviderFailureTurnItemInput, make_provider_failure_turn_item},
    id_allocator::IdAllocator,
    provider_adapter::ProviderAdapterV2TurnTerminalStatus,
};

use super::{
    AsyncBool, RefreshOnce, RunExecutionInner, RunOwnedSubagentTerminalStatus, TerminalSummary,
    cascade_terminalize_run_owned_subagents, final_provider_thread_status,
    make_interrupt_result_turn_item,
};
use crate::{
    core::run_execution_service::OpenRunOwnedSubagentProjection,
    pipeline::{
        effect_outbox::{EffectRequest, PendingEffect},
        event_sink::{RunCurrentWrite, WriteInput},
        orchestrator_port::BoxError,
    },
};

/// `isRunOwnedSubagentTerminalStatus`.
pub(super) fn run_owned_terminal_status(
    status: ProviderAdapterV2TurnTerminalStatus,
) -> Option<RunOwnedSubagentTerminalStatus> {
    match status {
        ProviderAdapterV2TurnTerminalStatus::Interrupted => {
            Some(RunOwnedSubagentTerminalStatus::Interrupted)
        }
        ProviderAdapterV2TurnTerminalStatus::Failed => Some(RunOwnedSubagentTerminalStatus::Failed),
        ProviderAdapterV2TurnTerminalStatus::Cancelled => {
            Some(RunOwnedSubagentTerminalStatus::Cancelled)
        }
        ProviderAdapterV2TurnTerminalStatus::Completed => None,
    }
}

fn attempt_status(status: ProviderAdapterV2TurnTerminalStatus) -> OrchestrationV2RunAttemptStatus {
    match status {
        ProviderAdapterV2TurnTerminalStatus::Completed => {
            OrchestrationV2RunAttemptStatus::Completed
        }
        ProviderAdapterV2TurnTerminalStatus::Interrupted => {
            OrchestrationV2RunAttemptStatus::Interrupted
        }
        ProviderAdapterV2TurnTerminalStatus::Cancelled => {
            OrchestrationV2RunAttemptStatus::Cancelled
        }
        ProviderAdapterV2TurnTerminalStatus::Failed => OrchestrationV2RunAttemptStatus::Failed,
    }
}

/// The run status a terminal persists. A completed turn leaves the run
/// `waiting`, because background work or a later message may continue it.
fn persisted_run_status(status: ProviderAdapterV2TurnTerminalStatus) -> OrchestrationV2RunStatus {
    match status {
        ProviderAdapterV2TurnTerminalStatus::Completed => OrchestrationV2RunStatus::Waiting,
        ProviderAdapterV2TurnTerminalStatus::Interrupted => OrchestrationV2RunStatus::Interrupted,
        ProviderAdapterV2TurnTerminalStatus::Cancelled => OrchestrationV2RunStatus::Cancelled,
        ProviderAdapterV2TurnTerminalStatus::Failed => OrchestrationV2RunStatus::Failed,
    }
}

fn node_status(
    status: OrchestrationV2RunStatus,
) -> turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus {
    use turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus as NodeStatus;
    match status {
        OrchestrationV2RunStatus::Waiting => NodeStatus::Waiting,
        OrchestrationV2RunStatus::Interrupted => NodeStatus::Interrupted,
        OrchestrationV2RunStatus::Cancelled => NodeStatus::Cancelled,
        _ => NodeStatus::Failed,
    }
}

/// The ownership gate of a terminal write (`writeIfRunCurrent`).
#[derive(Debug, Clone)]
pub(super) struct RunCurrentGate {
    pub active_attempt_id: RunAttemptId,
    pub expected_status: OrchestrationV2RunStatus,
}

pub(super) struct FinalRunInput<'a> {
    pub run: &'a OrchestrationV2Run,
    pub root_node: &'a OrchestrationV2ExecutionNode,
    pub checkpoint_scope: &'a OrchestrationV2CheckpointScope,
    pub provider_thread: &'a OrchestrationV2ProviderThread,
    pub attempt: &'a OrchestrationV2RunAttempt,
    pub should_finalize_run: Option<&'a AsyncBool>,
    pub has_unpaired_run_interrupt_request: Option<&'a AsyncBool>,
    pub open_run_owned_subagents: Option<&'a OpenRunOwnedSubagentProjection>,
    pub terminal: &'a TerminalSummary,
    pub failure_item_persisted: bool,
    pub refresh: &'a RefreshOnce,
    pub write_if_run_current: Option<RunCurrentGate>,
}

impl RunExecutionInner {
    /// `writeFinalRunEvents`. Returns once the terminal events are committed,
    /// or immediately when a superseded attempt writes nothing or the
    /// ownership gate declined the write.
    pub(super) async fn write_final_run_events(
        &self,
        input: FinalRunInput<'_>,
    ) -> Result<Finalized, BoxError> {
        let run = input.run;
        let root_node = input.root_node;
        let completed_at = UtcDateTime::now();
        let terminal = input.terminal;
        let allocate_event_id = || IdAllocator.event(Some(&run.thread_id), None, None);
        let mut finalized_attempt = input.attempt.clone();
        finalized_attempt.status = attempt_status(terminal.status);
        finalized_attempt.completed_at = Some(completed_at);

        let should_finalize_run = match input.should_finalize_run {
            None => true,
            Some(probe) => probe().await?,
        };
        if !should_finalize_run {
            // Superseded attempt (steer / selection restart). Emit
            // run_interrupt_result only when hard Stop left an unpaired request
            // for this run; plain steers and already-paired stops emit nothing.
            if terminal.status == ProviderAdapterV2TurnTerminalStatus::Interrupted {
                let has_unpaired_request = match input.has_unpaired_run_interrupt_request {
                    None => false,
                    Some(probe) => probe().await.unwrap_or(false),
                };
                if has_unpaired_request {
                    let payload = make_interrupt_result_turn_item(
                        run,
                        root_node,
                        input.provider_thread,
                        &completed_at,
                    );
                    self.events
                        .write(WriteInput {
                            events: vec![OrchestrationV2DomainEvent::TurnItemUpdated {
                                id: allocate_event_id(),
                                thread_id: run.thread_id.clone(),
                                run_id: Some(run.id.clone()),
                                node_id: Some(root_node.id.clone()),
                                driver: None,
                                provider_instance_id: Some(run.provider_instance_id.clone()),
                                raw_event_id: None,
                                occurred_at: completed_at,
                                payload,
                            }],
                            ..WriteInput::default()
                        })
                        .await?;
                    input.refresh.run().await;
                }
            }
            return Ok(Finalized::Superseded);
        }

        let empty = OpenRunOwnedSubagentProjection::default();
        let open = input.open_run_owned_subagents.unwrap_or(&empty);
        let has_open_subagent_projection = !open.subagents.is_empty()
            || !open.turn_items.is_empty()
            || !open.child_turn_items.is_empty()
            || !open.nodes.is_empty();
        let cascaded_subagent_events = match run_owned_terminal_status(terminal.status) {
            Some(status) if has_open_subagent_projection => {
                let mut cascade_ids = allocate_event_id;
                cascade_terminalize_run_owned_subagents(
                    run,
                    open,
                    status,
                    &completed_at,
                    &mut cascade_ids,
                )?
            }
            _ => Vec::new(),
        };

        let completed = terminal.status == ProviderAdapterV2TurnTerminalStatus::Completed;
        let persisted_status = persisted_run_status(terminal.status);
        // Completion cohorts are advanced by Orchestrator while a provider
        // turn is in flight. Do not replay the run snapshot captured at start
        // over a newer acknowledgement, successor, or Stop barrier.
        let mut finalized_run = run.clone();
        finalized_run.delegated_completion = None;
        finalized_run.status = persisted_status;
        finalized_run.completed_at = if completed { None } else { Some(completed_at) };
        let mut finalized_root_node = root_node.clone();
        finalized_root_node.status = node_status(persisted_status);
        finalized_root_node.completed_at = if completed { None } else { Some(completed_at) };
        finalized_root_node.checkpoint_scope_id = Some(input.checkpoint_scope.id.clone());
        let mut finalized_provider_thread = input.provider_thread.clone();
        finalized_provider_thread.status =
            final_provider_thread_status(terminal.thread_disposition);
        finalized_provider_thread.updated_at = completed_at;

        let run_event_id = allocate_event_id();
        let node_event_id = allocate_event_id();
        let provider_thread_event_id = allocate_event_id();
        let provider_instance_id = Some(run.provider_instance_id.clone());
        // Stopped runs capture too: their checkpoint is the rollback point for
        // the next message. The capture is enqueued with these terminal events,
        // ahead of any later run's start on this thread's effect lane.
        let effects = if matches!(
            terminal.status,
            ProviderAdapterV2TurnTerminalStatus::Completed
                | ProviderAdapterV2TurnTerminalStatus::Interrupted
                | ProviderAdapterV2TurnTerminalStatus::Cancelled
        ) {
            vec![PendingEffect {
                id: format!("effect:checkpoint.capture:{}", run.id),
                command_id: CommandId::new(format!(
                    "command:effect:checkpoint.capture:{}",
                    run.id
                ))?,
                thread_id: run.thread_id.clone(),
                request: EffectRequest::CheckpointCapture {
                    run_id: run.id.clone(),
                    scope_id: input.checkpoint_scope.id.clone(),
                },
                available_at: None,
            }]
        } else {
            Vec::new()
        };

        let mut events = Vec::new();
        // Terminalize open run-owned subagent rows before the root run
        // settles so projections never keep a forever-running subagent card.
        events.extend(cascaded_subagent_events);
        events.push(OrchestrationV2DomainEvent::RunAttemptUpdated {
            id: allocate_event_id(),
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: completed_at,
            payload: finalized_attempt,
        });
        if terminal.status == ProviderAdapterV2TurnTerminalStatus::Interrupted {
            events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                id: allocate_event_id(),
                thread_id: run.thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node.id.clone()),
                driver: None,
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: completed_at,
                payload: make_interrupt_result_turn_item(
                    run,
                    root_node,
                    input.provider_thread,
                    &completed_at,
                ),
            });
        }
        if terminal.status == ProviderAdapterV2TurnTerminalStatus::Failed
            && !input.failure_item_persisted
            && let Some(failure) = &terminal.failure
        {
            events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                id: allocate_event_id(),
                thread_id: run.thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node.id.clone()),
                driver: None,
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: completed_at,
                payload: make_provider_failure_turn_item(ProviderFailureTurnItemInput {
                    driver: &terminal.driver,
                    thread_id: run.thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(root_node.id.clone()),
                    provider_thread_id: terminal.provider_thread_id.clone(),
                    provider_turn_id: terminal.provider_turn_id.clone(),
                    item_ordinal: terminal
                        .failure_item_ordinal
                        .map_or(0, |ordinal| ordinal.get()),
                    failure: failure.clone(),
                    retry: None,
                    retry_started_at: None,
                    occurred_at: completed_at,
                }),
            });
        }
        events.push(OrchestrationV2DomainEvent::RunUpdated {
            id: run_event_id,
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: completed_at,
            payload: finalized_run,
        });
        events.push(OrchestrationV2DomainEvent::NodeUpdated {
            id: node_event_id,
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: completed_at,
            payload: finalized_root_node,
        });
        events.push(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id: provider_thread_event_id,
            thread_id: run.thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: completed_at,
            payload: finalized_provider_thread,
        });

        let write = WriteInput {
            events,
            effects,
            ..WriteInput::default()
        };
        match input.write_if_run_current {
            Some(gate) => {
                let committed = self
                    .events
                    .write_if_run_current(RunCurrentWrite {
                        write,
                        thread_id: run.thread_id.clone(),
                        run_id: run.id.clone(),
                        active_attempt_id: gate.active_attempt_id,
                        expected_status: gate.expected_status,
                    })
                    .await?;
                if !committed {
                    return Ok(Finalized::Declined);
                }
            }
            None => self.events.write(write).await?,
        }
        input.refresh.run().await;
        Ok(Finalized::Written)
    }
}

/// What `write_final_run_events` did. Upstream returns `void`; the caller
/// only needs the distinction in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Finalized {
    Written,
    /// Superseded attempt: no terminal events.
    Superseded,
    /// The ownership gate rejected the write.
    Declined,
}
