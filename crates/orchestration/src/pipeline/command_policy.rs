//! Port of upstream/apps/server/src/orchestration-v2/CommandPolicy.ts

use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2NativeRefStrength, OrchestrationV2ProviderCapabilities,
    OrchestrationV2ProviderTurnStatus, OrchestrationV2RunStatus, OrchestrationV2ThreadProjection,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::{CommandId, ProviderTurnId, RunId, ThreadId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    QueuedMessages,
    ActiveSteering,
    Interrupt,
    InterruptRestartSteering,
    NativeFork,
    ForkFromTurn,
    Rollback,
    RollbackSnapshot,
    ContextHandoff,
    StrongTerminalStatus,
}

impl Capability {
    pub fn code(self) -> &'static str {
        match self {
            Self::QueuedMessages => "queued_messages",
            Self::ActiveSteering => "active_steering",
            Self::Interrupt => "interrupt",
            Self::InterruptRestartSteering => "interrupt_restart_steering",
            Self::NativeFork => "native_fork",
            Self::ForkFromTurn => "fork_from_turn",
            Self::Rollback => "rollback",
            Self::RollbackSnapshot => "rollback_snapshot",
            Self::ContextHandoff => "context_handoff",
            Self::StrongTerminalStatus => "strong_terminal_status",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CommandPolicyError {
    #[error("Failed to choose message dispatch policy for command {command_id}.")]
    MessageDispatch {
        command_id: CommandId,
        thread_id: ThreadId,
        cause: String,
    },
    #[error(
        "{provider_instance_id} cannot satisfy message dispatch mode {requested_mode} for command {command_id}."
    )]
    Unsupported {
        command_id: CommandId,
        thread_id: ThreadId,
        requested_mode: &'static str,
        provider_instance_id: ProviderInstanceId,
    },
    #[error("{provider_instance_id} cannot satisfy {} for command {command_id}: {detail}", capability.code())]
    CapabilityUnsupported {
        command_id: CommandId,
        thread_id: ThreadId,
        provider_instance_id: ProviderInstanceId,
        capability: Capability,
        detail: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryIntent {
    Auto,
    Steer,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedDispatchMode {
    StartImmediately,
    QueueAfterActive,
    SteerActive { target_run_id: RunId },
    RestartActive { target_run_id: RunId },
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageDispatchDecision {
    StartRun {
        model_selection: ModelSelection,
    },
    SteerActive {
        target_run_id: RunId,
        provider_turn_id: ProviderTurnId,
    },
    RestartActive {
        target_run_id: RunId,
        interrupt_provider_turn_id: ProviderTurnId,
    },
    QueueAfterActive {
        active_run_id: RunId,
    },
}

#[derive(Debug, Clone)]
pub struct CapabilityCheck<'a> {
    pub command_id: &'a CommandId,
    pub thread_id: &'a ThreadId,
    pub provider_instance_id: &'a ProviderInstanceId,
    pub capabilities: &'a OrchestrationV2ProviderCapabilities,
}

fn unsupported(
    input: &CapabilityCheck<'_>,
    capability: Capability,
    detail: &'static str,
) -> CommandPolicyError {
    CommandPolicyError::CapabilityUnsupported {
        command_id: input.command_id.clone(),
        thread_id: input.thread_id.clone(),
        provider_instance_id: input.provider_instance_id.clone(),
        capability,
        detail,
    }
}

pub fn ensure_queued_messages(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if input.capabilities.turns.supports_queued_messages {
        Ok(())
    } else {
        Err(unsupported(
            input,
            Capability::QueuedMessages,
            "providerInstanceId does not support app-owned queued turns",
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteeringExecutionPolicy {
    ActiveSteering,
    InterruptRestart,
}

pub fn decide_steering_execution(
    input: &CapabilityCheck<'_>,
    force_restart: bool,
) -> Result<SteeringExecutionPolicy, CommandPolicyError> {
    let turns = &input.capabilities.turns;
    if !force_restart && turns.supports_active_steering {
        return Ok(SteeringExecutionPolicy::ActiveSteering);
    }
    if turns.supports_interrupt && turns.supports_steering_by_interrupt_restart {
        return Ok(SteeringExecutionPolicy::InterruptRestart);
    }
    Err(unsupported(
        input,
        if force_restart || turns.supports_interrupt {
            Capability::InterruptRestartSteering
        } else {
            Capability::ActiveSteering
        },
        if force_restart {
            "providerInstanceId cannot satisfy a required interrupt-and-restart"
        } else {
            "providerInstanceId cannot steer active turns directly or by interrupt-and-restart"
        },
    ))
}

pub fn ensure_interrupt(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if input.capabilities.turns.supports_interrupt {
        Ok(())
    } else {
        Err(unsupported(
            input,
            Capability::Interrupt,
            "providerInstanceId does not support turn interrupts",
        ))
    }
}

pub fn ensure_native_fork(
    input: &CapabilityCheck<'_>,
    from_specific_turn: bool,
) -> Result<(), CommandPolicyError> {
    if !input.capabilities.threads.can_fork_thread {
        return Err(unsupported(
            input,
            Capability::NativeFork,
            "providerInstanceId does not support native thread forks",
        ));
    }
    if from_specific_turn && !input.capabilities.threads.can_fork_from_turn {
        return Err(unsupported(
            input,
            Capability::ForkFromTurn,
            "providerInstanceId cannot fork from a specific completed turn",
        ));
    }
    if input.capabilities.identity.native_thread_ids != OrchestrationV2NativeRefStrength::Strong {
        return Err(unsupported(
            input,
            Capability::NativeFork,
            "providerInstanceId does not expose strong native thread ids",
        ));
    }
    Ok(())
}

pub fn ensure_rollback(input: &CapabilityCheck<'_>) -> Result<(), CommandPolicyError> {
    if !input.capabilities.threads.can_rollback_thread
        || !input
            .capabilities
            .checkpointing
            .provider_can_rollback_conversation
    {
        return Err(unsupported(
            input,
            Capability::Rollback,
            "providerInstanceId conversation rollback is unavailable",
        ));
    }
    if !input
        .capabilities
        .checkpointing
        .provider_rollback_returns_snapshot
    {
        return Err(unsupported(
            input,
            Capability::RollbackSnapshot,
            "rollback must return a providerInstanceId thread snapshot",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffStrategy {
    ForkDeltaContext,
    DeltaContext,
    FullThreadSummary,
}

pub fn ensure_context_handoff(
    input: &CapabilityCheck<'_>,
    strategy: HandoffStrategy,
) -> Result<(), CommandPolicyError> {
    let context = &input.capabilities.context;
    if !context.can_consume_handoff_summaries {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId cannot consume handoff summaries",
        ));
    }
    if !context.accepts_synthetic_user_context {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId cannot receive synthetic user context",
        ));
    }
    if matches!(
        strategy,
        HandoffStrategy::ForkDeltaContext | HandoffStrategy::DeltaContext
    ) && !context.supports_delta_handoff
    {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId does not support delta handoff",
        ));
    }
    if strategy == HandoffStrategy::FullThreadSummary && !context.supports_full_thread_handoff {
        return Err(unsupported(
            input,
            Capability::ContextHandoff,
            "providerInstanceId does not support full-thread handoff",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkExecutionPolicy {
    NativeFork,
    PortableContext,
}

pub fn decide_fork_execution(
    input: &CapabilityCheck<'_>,
    same_provider: bool,
    has_strong_native_source: bool,
    source_status: OrchestrationV2RunStatus,
    from_specific_turn: bool,
) -> Result<ForkExecutionPolicy, CommandPolicyError> {
    let native = matches!(
        source_status,
        OrchestrationV2RunStatus::Completed | OrchestrationV2RunStatus::Waiting
    ) && same_provider
        && has_strong_native_source
        && input.capabilities.threads.can_fork_thread
        && (!from_specific_turn || input.capabilities.threads.can_fork_from_turn)
        && input.capabilities.identity.native_thread_ids
            == OrchestrationV2NativeRefStrength::Strong;
    if native {
        Ok(ForkExecutionPolicy::NativeFork)
    } else {
        ensure_context_handoff(input, HandoffStrategy::FullThreadSummary)?;
        Ok(ForkExecutionPolicy::PortableContext)
    }
}

fn active_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

/// Resolve the client's delivery intent only after acquiring the thread command lock.
pub fn resolve_message_dispatch_intent(
    projection: &OrchestrationV2ThreadProjection,
    requested: RequestedDispatchMode,
    delivery_intent: Option<DeliveryIntent>,
) -> RequestedDispatchMode {
    let Some(intent) = delivery_intent else {
        return requested;
    };
    let Some(active_run) = projection
        .runs
        .iter()
        .rev()
        .find(|run| active_status(run.status))
    else {
        return RequestedDispatchMode::StartImmediately;
    };
    match intent {
        DeliveryIntent::Steer => RequestedDispatchMode::SteerActive {
            target_run_id: active_run.id.clone(),
        },
        DeliveryIntent::Restart => RequestedDispatchMode::RestartActive {
            target_run_id: active_run.id.clone(),
        },
        DeliveryIntent::Auto => {
            if matches!(
                active_run.status,
                OrchestrationV2RunStatus::Preparing | OrchestrationV2RunStatus::Starting
            ) {
                return RequestedDispatchMode::QueueAfterActive;
            }
            let capabilities = projection
                .provider_threads
                .iter()
                .find(|thread| Some(&thread.id) == active_run.provider_thread_id.as_ref())
                .and_then(|thread| thread.provider_session_id.as_ref())
                .and_then(|session_id| {
                    projection
                        .provider_sessions
                        .iter()
                        .find(|session| &session.id == session_id)
                })
                .map(|session| session.capabilities.turns);
            match capabilities {
                Some(c) if c.supports_active_steering => RequestedDispatchMode::SteerActive {
                    target_run_id: active_run.id.clone(),
                },
                Some(c) if c.supports_queued_messages => RequestedDispatchMode::QueueAfterActive,
                Some(c) if c.supports_steering_by_interrupt_restart => {
                    RequestedDispatchMode::RestartActive {
                        target_run_id: active_run.id.clone(),
                    }
                }
                _ => RequestedDispatchMode::QueueAfterActive,
            }
        }
    }
}

pub fn decide_message_dispatch(
    command_id: &CommandId,
    projection: &OrchestrationV2ThreadProjection,
    requested_model: Option<&ModelSelection>,
    requested_mode: &RequestedDispatchMode,
    capabilities: &OrchestrationV2ProviderCapabilities,
) -> Result<MessageDispatchDecision, CommandPolicyError> {
    let active_run = projection.runs.iter().find(|run| active_status(run.status));
    let model_selection = requested_model
        .unwrap_or(&projection.thread.model_selection)
        .clone();
    let check = CapabilityCheck {
        command_id,
        thread_id: &projection.thread.id,
        provider_instance_id: &model_selection.instance_id,
        capabilities,
    };
    match requested_mode {
        RequestedDispatchMode::SteerActive { target_run_id }
        | RequestedDispatchMode::RestartActive { target_run_id } => {
            let mode = if matches!(requested_mode, RequestedDispatchMode::SteerActive { .. }) {
                "steer_active"
            } else {
                "restart_active"
            };
            let Some(run) = active_run.filter(|run| {
                run.id == *target_run_id
                    && (mode == "steer_active" || run.active_attempt_id.is_some())
            }) else {
                return Err(CommandPolicyError::Unsupported {
                    command_id: command_id.clone(),
                    thread_id: projection.thread.id.clone(),
                    requested_mode: mode,
                    provider_instance_id: model_selection.instance_id,
                });
            };
            let turn = run.active_attempt_id.as_ref().and_then(|attempt_id| {
                projection.provider_turns.iter().find(|turn| {
                    turn.run_attempt_id.as_ref() == Some(attempt_id)
                        && turn.status == OrchestrationV2ProviderTurnStatus::Running
                })
            });
            let Some(turn) = turn else {
                return Err(CommandPolicyError::MessageDispatch {
                    command_id: command_id.clone(),
                    thread_id: projection.thread.id.clone(),
                    cause: format!(
                        "No running providerInstanceId turn found for active run {}.",
                        run.id
                    ),
                });
            };
            if mode == "steer_active" {
                Ok(MessageDispatchDecision::SteerActive {
                    target_run_id: run.id.clone(),
                    provider_turn_id: turn.id.clone(),
                })
            } else {
                Ok(MessageDispatchDecision::RestartActive {
                    target_run_id: run.id.clone(),
                    interrupt_provider_turn_id: turn.id.clone(),
                })
            }
        }
        RequestedDispatchMode::QueueAfterActive | RequestedDispatchMode::StartImmediately => {
            if let Some(run) = active_run {
                ensure_queued_messages(&check)?;
                Ok(MessageDispatchDecision::QueueAfterActive {
                    active_run_id: run.id.clone(),
                })
            } else {
                Ok(MessageDispatchDecision::StartRun { model_selection })
            }
        }
    }
}
