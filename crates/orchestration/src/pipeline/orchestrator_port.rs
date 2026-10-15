//! The seam to upstream's `Orchestrator.ts` (`OrchestratorV2`), the decider
//! that turns a command into events and calls the sink. The orchestrator
//! itself is not part of the write pipeline; every pipeline service that
//! upstream builds on top of it (`ThreadManagementService`,
//! `ThreadTitleRegenerationService`, `ThreadSettlementService`,
//! `ThreadLifecycleService`) depends on this trait instead, so they port and
//! test without it. Its implementor (T13c) wires `dispatch` to
//! `ThreadCommandExecutor` + `EventSink::commit_command` and the reads to the
//! projection store (T13a).
//!
//! The error classes are `Orchestrator.ts`'s `OrchestratorV2Error` union; the
//! message dispatchers in `ThreadMessageIntake` switch on them.

use super::event_sink::{SinkStream, StreamOptions};
use async_trait::async_trait;
use turin_contracts::orchestration_v2::{
    OrchestrationV2DomainEvent, OrchestrationV2Run, OrchestrationV2ServerCommand,
    OrchestrationV2StoredEvent, OrchestrationV2ThreadProjection, OrchestrationV2ThreadShell,
    OrchestrationV2ThreadShellSnapshot,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
use turin_contracts::{CommandId, ProjectId, RunId, ThreadId};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Which mode of the thread a `DispatchModeLimit` was exceeded by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Runtime,
    Interaction,
}

impl ModeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Interaction => "interaction",
        }
    }
}

/// The wire literal of a mode (`full-access`, `plan`, ...).
pub fn mode_text<T: serde::Serialize>(mode: &T) -> String {
    serde_json::to_value(mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `OrchestratorV2Error`.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// Its text is what a rejected receipt stores, as upstream's `message` does.
    #[error("Failed to dispatch orchestration command {command_type} ({command_id}).")]
    Dispatch {
        command_id: CommandId,
        command_type: String,
        #[source]
        cause: BoxError,
    },
    #[error("Orchestration command {command_type} ({command_id}) was rejected before commit.")]
    CommandRejected {
        command_id: CommandId,
        command_type: String,
        #[source]
        cause: BoxError,
    },
    #[error("Failed to load orchestration projection for thread {thread_id}.")]
    Projection {
        thread_id: ThreadId,
        #[source]
        cause: BoxError,
    },
    #[error("Failed while streaming orchestration domain events.")]
    DomainEventStream {
        #[source]
        cause: BoxError,
    },
    #[error("Provider adapter failed while dispatching orchestration command {command_id}.")]
    ProviderAdapter {
        command_id: CommandId,
        provider_instance_id: ProviderInstanceId,
        #[source]
        cause: BoxError,
    },
    #[error("Command {command_id} was previously rejected: {detail}")]
    CommandPreviouslyRejected {
        command_id: CommandId,
        command_type: String,
        detail: String,
    },
    #[error(
        "Command {command_id} was already handled for thread {receipt_thread_id} and cannot be replayed for {command_thread_id}."
    )]
    CommandIdConflict {
        command_id: CommandId,
        command_type: String,
        receipt_thread_id: ThreadId,
        command_thread_id: ThreadId,
    },
    #[error(
        "This subagent is run by its provider and cannot take messages. Message the parent thread instead."
    )]
    SubagentThreadReadOnly {
        command_id: CommandId,
        thread_id: ThreadId,
    },
    /// The command's thread runs above the modes its sender may touch (see
    /// `DispatchModeLimit`).
    #[error(
        "Thread {thread_id} now runs in {}/{} mode, above what this caller may change.",
        mode_text(.runtime_mode),
        mode_text(.interaction_mode)
    )]
    ThreadAboveModeLimit {
        command_id: CommandId,
        thread_id: ThreadId,
        mode: ModeKind,
        runtime_mode: RuntimeMode,
        interaction_mode: ProviderInteractionMode,
    },
}

impl OrchestratorError {
    pub fn projection(thread_id: ThreadId, cause: impl Into<BoxError>) -> Self {
        Self::Projection {
            thread_id,
            cause: cause.into(),
        }
    }

    /// Failures raised in receipt validation or planning, before
    /// `commitCommand`. Generic dispatch errors can follow a commit and remain
    /// uncertain, so they are not in this list.
    pub fn was_not_accepted(&self) -> bool {
        matches!(
            self,
            Self::CommandRejected { .. }
                | Self::Projection { .. }
                | Self::ProviderAdapter { .. }
                | Self::CommandPreviouslyRejected { .. }
                | Self::CommandIdConflict { .. }
                | Self::SubagentThreadReadOnly { .. }
                | Self::ThreadAboveModeLimit { .. }
        )
    }
}

/// `OrchestratorV2DispatchResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchResult {
    pub sequence: u64,
    pub stored_events: Vec<OrchestrationV2StoredEvent>,
}

/// Rows `getThreadRecords` is asked for. Upstream returns
/// `Pick<Projection, "thread" | K>`; Turin returns the whole projection type
/// with unrequested collections empty, so the read side may ignore the field
/// list and load everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordField {
    Runs,
    Attempts,
    Nodes,
    Subagents,
    ProviderSessions,
    ProviderThreads,
    ProviderTurns,
    RuntimeRequests,
    Messages,
    TurnItems,
}

/// `ProjectionRecordFilter`.
#[derive(Debug, Clone, Default)]
pub struct RecordFilter {
    pub message_roles:
        Option<Vec<turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole>>,
    pub message_ids: Option<Vec<turin_contracts::MessageId>>,
    pub run_ids: Option<Vec<RunId>>,
    pub turn_item_types: Option<Vec<String>>,
    /// Skips finished tool call, assistant message and reasoning nodes of
    /// finished runs. Command decisions only read root, subagent and
    /// active-run nodes.
    pub omit_settled_leaf_nodes: bool,
}

pub type ThreadRecords = OrchestrationV2ThreadProjection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellLocation {
    #[default]
    Active,
    Archive,
}

/// `ShellSnapshotOptions`.
#[derive(Debug, Clone, Default)]
pub struct ShellSnapshotOptions {
    pub location: ShellLocation,
    /// Reads only this project's threads.
    pub project_id: Option<ProjectId>,
    /// For background sweeps, not clients: skips settled threads.
    pub unsettled_only: bool,
}

#[async_trait]
pub trait OrchestratorPort: Send + Sync {
    /// Decides and commits one command (idempotent by command id).
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, OrchestratorError>;

    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, OrchestratorError>;

    async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, OrchestratorError>;

    async fn get_thread_shell(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2ThreadShell>, OrchestratorError>;

    async fn get_shell_snapshot(
        &self,
        options: &ShellSnapshotOptions,
    ) -> Result<OrchestrationV2ThreadShellSnapshot, OrchestratorError>;

    async fn get_thread_event_sequence(
        &self,
        thread_id: &ThreadId,
    ) -> Result<i64, OrchestratorError>;

    /// Whether a restart continuation of this cut run has not started yet, so
    /// the run's status is not its outcome. A failed read counts as not pending.
    async fn restart_continuation_pending(&self, run: &OrchestrationV2Run) -> bool;

    /// Resolves once that continuation has run or was declined. A declined one
    /// records no thread event, so a waiter needs this to wake.
    async fn await_restart_continuation(&self, run: &OrchestrationV2Run);

    /// Settles a delegated child whose restart continuation of
    /// `source_run_id` declined or failed.
    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId);

    /// Stored events of the log, replayed from `after_sequence` then live
    /// (`EventSink::stream`).
    async fn stream_stored_events_from(
        &self,
        options: StreamOptions,
    ) -> Result<SinkStream, OrchestratorError>;

    /// Every committed thread event from now on (`streamDomainEvents`).
    async fn stream_domain_events(
        &self,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<OrchestrationV2DomainEvent, OrchestratorError>>,
        OrchestratorError,
    >;
}
