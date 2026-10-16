//! The effect executor: port of `layerExecutor` in
//! upstream `orchestration-v2/EffectWorker.ts`. T13c's handoff left it to the
//! composition root ("T13e left the executor to T13c"); nobody had written it.
//!
//! Each claimed outbox effect goes to the service that owns it. A failure
//! comes back as [`EffectExecutionError`] and the worker retries or fails the
//! effect. Effects whose service is not wired yet fail with a message that
//! names the missing piece, so the row shows why in `last_error` (see
//! `docs/handoff/T22.md`, "Effects not executed").

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::CommandId;
use turin_contracts::UtcDateTime;
use turin_contracts::orchestration_v2::{
    OrchestrationV2BackgroundWorkSettleOutcome, OrchestrationV2BackgroundWorkSettleOutcomeStatus,
    OrchestrationV2BackgroundWorkSettleSubagent, OrchestrationV2InternalCommand,
    OrchestrationV2ServerCommand,
};
use turin_orchestration::core::orchestrator::services::TurnStartInput;
use turin_orchestration::core::provider_session_manager::ProviderSessionManager;
use turin_orchestration::core::provider_turn_control_service::{
    AwaitTerminalInput, InterruptInput, ProviderTurnControlService, SteerInput, SubagentStop,
};
use turin_orchestration::core::provider_turn_start_service::ProviderTurnStartService;
use turin_orchestration::core::restart_continuation_service::RestartContinuationService;
use turin_orchestration::core::run_finalization_service::{FinalizeInput, RunFinalizationService};
use turin_orchestration::core::runtime_request_service::{
    RuntimeRequestResponse, RuntimeRequestService,
};
use turin_orchestration::core::steer_follow_up::after_steer;
use turin_orchestration::pipeline::effect_outbox::{Effect, EffectRequest, SessionTransition};
use turin_orchestration::pipeline::effect_worker::{
    EffectExecutionError, EffectExecutor, error_chain,
    is_non_retryable_provider_turn_control_failure,
};
use turin_orchestration::pipeline::orchestrator_port::BoxError;
use turin_orchestration::pipeline::thread_management_service::{
    StopDelegatedTasksInput, ThreadManagementService,
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2SubagentOutcome, ProviderAdapterV2SubagentOutcomeStatus,
};

pub struct TurinEffectExecutor {
    pub turn_start: Arc<ProviderTurnStartService>,
    pub turn_control: Arc<ProviderTurnControlService>,
    pub runtime_requests: Arc<RuntimeRequestService>,
    pub sessions: Arc<ProviderSessionManager>,
    pub finalization: Arc<RunFinalizationService>,
    pub threads: Arc<ThreadManagementService>,
    pub restart: Arc<RestartContinuationService>,
}

fn settle_outcome(
    outcome: &ProviderAdapterV2SubagentOutcome,
) -> OrchestrationV2BackgroundWorkSettleOutcome {
    OrchestrationV2BackgroundWorkSettleOutcome {
        status: match outcome.status {
            ProviderAdapterV2SubagentOutcomeStatus::Completed => {
                OrchestrationV2BackgroundWorkSettleOutcomeStatus::Completed
            }
            ProviderAdapterV2SubagentOutcomeStatus::Failed => {
                OrchestrationV2BackgroundWorkSettleOutcomeStatus::Failed
            }
            ProviderAdapterV2SubagentOutcomeStatus::Cancelled => {
                OrchestrationV2BackgroundWorkSettleOutcomeStatus::Cancelled
            }
        },
        completed_at: outcome.completed_at,
        result: outcome.result.clone(),
    }
}

impl TurinEffectExecutor {
    async fn run(&self, effect: &Effect, will_retry: bool) -> Result<(), BoxError> {
        let thread_id = effect.thread_id.clone();
        match &effect.request {
            EffectRequest::ProviderSessionDetach {
                provider_session_id,
                detail,
                ..
            } => {
                // The MCP credential revoke has no Turin counterpart (no MCP server yet).
                self.sessions
                    .detach(provider_session_id, &thread_id, detail.clone())
                    .await?;
                Ok(())
            }
            EffectRequest::ProviderTurnStart { run_id } => {
                self.turn_start
                    .start(TurnStartInput {
                        thread_id,
                        run_id: run_id.clone(),
                        will_retry,
                    })
                    .await
            }
            EffectRequest::ProviderTurnInterrupt {
                provider_session_id,
                provider_thread_id,
                provider_turn_id,
                subagent,
            } => {
                let requested_at: Option<UtcDateTime> = effect.created_at.parse().ok();
                let stop = subagent.as_ref().map(|subagent| SubagentStop {
                    id: subagent.id.clone(),
                    native_task_id: subagent.native_task_id.clone(),
                    requested_at,
                });
                let outcome = match self
                    .turn_control
                    .interrupt(InterruptInput {
                        thread_id: thread_id.clone(),
                        provider_session_id: provider_session_id.clone(),
                        provider_thread_id: provider_thread_id.clone(),
                        provider_turn_id: provider_turn_id.clone(),
                        subagent: stop,
                    })
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let text = error_chain(&error);
                        if subagent.is_some() {
                            tracing::warn!(effect_id = %effect.id, %text, "subagent stop failed");
                        }
                        if subagent.is_none()
                            && is_non_retryable_provider_turn_control_failure(
                                "provider-turn.interrupt",
                                &text,
                            )
                        {
                            None
                        } else {
                            return Err(Box::new(error));
                        }
                    }
                };
                // A targeted Stop repairs only an outcome the provider observed;
                // a full-turn Stop keeps its process-teardown repair.
                if subagent.is_some() && outcome.is_none() {
                    return Ok(());
                }
                let settle_subagent = match (subagent, &outcome, requested_at) {
                    (Some(subagent), Some(outcome), Some(stopped_at)) => {
                        Some(OrchestrationV2BackgroundWorkSettleSubagent {
                            id: subagent.id.clone(),
                            stopped_at,
                            outcome: settle_outcome(outcome),
                        })
                    }
                    _ => None,
                };
                self.threads
                    .dispatch(OrchestrationV2ServerCommand::Internal(
                        OrchestrationV2InternalCommand::ThreadBackgroundWorkSettle {
                            command_id: CommandId::new(format!(
                                "{}:background-work-settled",
                                effect.id
                            ))?,
                            thread_id,
                            provider_thread_id: provider_thread_id.clone(),
                            provider_turn_id: provider_turn_id.clone(),
                            subagent: settle_subagent,
                        },
                    ))
                    .await?;
                Ok(())
            }
            EffectRequest::ProviderTurnSteer {
                provider_session_id,
                provider_thread_id,
                provider_turn_id,
                message_id,
            } => {
                let steered = self
                    .turn_control
                    .steer(SteerInput {
                        thread_id,
                        provider_session_id: provider_session_id.clone(),
                        provider_thread_id: provider_thread_id.clone(),
                        provider_turn_id: provider_turn_id.clone(),
                        message_id: message_id.clone(),
                    })
                    .await;
                // A steer that met a completed turn re-dispatches the message as
                // a follow-up; one a provider took for a delegated completion is
                // accepted.
                after_steer(self.threads.as_ref(), effect, steered).await
            }
            EffectRequest::ProviderTurnRestart {
                provider_session_id,
                provider_thread_id,
                provider_turn_id,
                interrupted_attempt_id,
                run_id,
                session_transition,
            } => {
                let replacement = match session_transition {
                    Some(SessionTransition::Replace {
                        replacement_provider_session_id,
                    }) => Some(replacement_provider_session_id.clone()),
                    _ => None,
                };
                self.turn_control
                    .interrupt_and_await_terminal(AwaitTerminalInput {
                        thread_id: thread_id.clone(),
                        provider_session_id: provider_session_id.clone(),
                        replacement_provider_session_id: replacement,
                        provider_thread_id: provider_thread_id.clone(),
                        provider_turn_id: provider_turn_id.clone(),
                        interrupted_attempt_id: interrupted_attempt_id.clone(),
                    })
                    .await?;
                let detach_detail = match session_transition {
                    Some(SessionTransition::Replace { .. }) => {
                        Some("Selection change requires a provider session restart.")
                    }
                    Some(SessionTransition::Detach) => {
                        Some("Provider thread handoff replaced this session binding.")
                    }
                    None => None,
                };
                if let Some(detail) = detach_detail {
                    self.sessions
                        .detach(provider_session_id, &thread_id, Some(detail.to_owned()))
                        .await?;
                }
                self.turn_start
                    .start(TurnStartInput {
                        thread_id,
                        run_id: run_id.clone(),
                        will_retry,
                    })
                    .await
            }
            EffectRequest::RuntimeRequestRespond {
                provider_session_id,
                request_id,
                decision,
                answers,
            } => {
                self.runtime_requests
                    .respond(RuntimeRequestResponse {
                        thread_id,
                        provider_session_id: provider_session_id.clone(),
                        request_id: request_id.clone(),
                        decision: *decision,
                        answers: answers.clone(),
                    })
                    .await?;
                Ok(())
            }
            EffectRequest::CheckpointCapture { run_id, scope_id } => {
                self.finalization
                    .finalize(FinalizeInput {
                        thread_id,
                        run_id: run_id.clone(),
                        scope_id: scope_id.clone(),
                    })
                    .await?;
                Ok(())
            }
            EffectRequest::DelegatedTasksStop { reason } => {
                self.threads
                    .stop_delegated_tasks(StopDelegatedTasksInput {
                        thread_id,
                        command_id: effect.command_id.clone(),
                        reason: reason.clone(),
                    })
                    .await?;
                Ok(())
            }
            // Turin has no terminal drawer or preview feature (PORTING.md cut list).
            EffectRequest::TerminalCleanup {} | EffectRequest::PreviewCleanup {} => Ok(()),
            // Attachment files are kept until the cleanup service is wired.
            EffectRequest::AttachmentCleanup { .. } => Ok(()),
            // Title generation needs N3's text generation wired into the app.
            EffectRequest::ThreadTitleGenerate { .. } => {
                tracing::debug!(effect_id = %effect.id, "thread title generation is not wired; keeping the title");
                Ok(())
            }
            EffectRequest::ProviderRuntimeContinue { source_run_id } => {
                self.restart
                    .execute_continue_effect(&thread_id, source_run_id, will_retry)
                    .await
            }
            EffectRequest::ProviderThreadRollback { .. } => {
                Err("provider-thread.rollback is not wired: CheckpointRollbackService is not ported (T13c item 2)".into())
            }
        }
    }
}

#[async_trait]
impl EffectExecutor for TurinEffectExecutor {
    async fn execute(&self, effect: &Effect, will_retry: bool) -> Result<(), EffectExecutionError> {
        self.run(effect, will_retry)
            .await
            .map_err(|cause| EffectExecutionError::new(effect, cause))
    }
}
