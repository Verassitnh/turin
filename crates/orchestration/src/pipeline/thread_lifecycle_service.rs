//! Port of upstream/apps/server/src/orchestration-v2/ThreadLifecycleService.ts
//!
//! Application-level lifecycle operations mapped to V2-native commands, each
//! dispatched through the `ThreadManagementService` chokepoint, then the
//! updated thread is read back.

use super::orchestrator_port::{RecordFilter, ThreadRecords};
use super::thread_management_service::{ThreadManagementError, ThreadManagementService};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{OrchestrationV2Command, OrchestrationV2ServerCommand};
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
use turin_contracts::{CommandId, ThreadId, TrimmedNonEmptyString};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleOperation {
    Archive,
    Unarchive,
    Delete,
    UpdateMetadata,
    SetRuntimeMode,
    SetInteractionMode,
    SetModelSelection,
}

impl LifecycleOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
            Self::Delete => "delete",
            Self::UpdateMetadata => "update-metadata",
            Self::SetRuntimeMode => "set-runtime-mode",
            Self::SetInteractionMode => "set-interaction-mode",
            Self::SetModelSelection => "set-model-selection",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Thread lifecycle operation '{}' failed for {thread_id}.", operation.as_str())]
pub struct ThreadLifecycleError {
    pub operation: LifecycleOperation,
    pub thread_id: ThreadId,
    #[source]
    pub cause: ThreadManagementError,
}

/// `updateMetadata` input; `None` leaves a field unchanged, `Some(None)`
/// clears `branch` / `worktree_path`.
#[derive(Debug, Clone, Default)]
pub struct MetadataUpdate {
    pub title: Option<TrimmedNonEmptyString>,
    pub branch: Option<Option<TrimmedNonEmptyString>>,
    pub worktree_path: Option<Option<TrimmedNonEmptyString>>,
}

pub struct ThreadLifecycleService<'a> {
    threads: &'a ThreadManagementService,
}

impl<'a> ThreadLifecycleService<'a> {
    pub fn new(threads: &'a ThreadManagementService) -> Self {
        Self { threads }
    }

    async fn dispatch(
        &self,
        operation: LifecycleOperation,
        thread_id: &ThreadId,
        command: OrchestrationV2Command,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let wrap = |cause| ThreadLifecycleError {
            operation,
            thread_id: thread_id.clone(),
            cause,
        };
        self.threads
            .dispatch(OrchestrationV2ServerCommand::Client(command))
            .await
            .map_err(wrap)?;
        self.threads
            .orchestrator()
            .get_thread_records(thread_id, &[], &RecordFilter::default())
            .await
            .map_err(|error| wrap(error.into()))
    }

    pub async fn archive(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadArchive {
            command_id,
            thread_id: thread_id.clone(),
        };
        self.dispatch(LifecycleOperation::Archive, &thread_id, command)
            .await
    }

    pub async fn unarchive(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadUnarchive {
            command_id,
            thread_id: thread_id.clone(),
        };
        self.dispatch(LifecycleOperation::Unarchive, &thread_id, command)
            .await
    }

    pub async fn delete(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadDelete {
            command_id,
            thread_id: thread_id.clone(),
        };
        self.dispatch(LifecycleOperation::Delete, &thread_id, command)
            .await
    }

    pub async fn update_metadata(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        update: MetadataUpdate,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadMetadataUpdate {
            command_id,
            thread_id: thread_id.clone(),
            title: update.title,
            regenerate_title: None,
            branch: update.branch,
            worktree_path: update.worktree_path,
            expected_worktree_path: None,
            expected_empty: None,
            limit_recovery: None,
            linked_pull_request: None,
        };
        self.dispatch(LifecycleOperation::UpdateMetadata, &thread_id, command)
            .await
    }

    pub async fn set_runtime_mode(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        runtime_mode: RuntimeMode,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadRuntimeModeSet {
            command_id,
            thread_id: thread_id.clone(),
            runtime_mode,
        };
        self.dispatch(LifecycleOperation::SetRuntimeMode, &thread_id, command)
            .await
    }

    pub async fn set_interaction_mode(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        interaction_mode: ProviderInteractionMode,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadInteractionModeSet {
            command_id,
            thread_id: thread_id.clone(),
            interaction_mode,
        };
        self.dispatch(LifecycleOperation::SetInteractionMode, &thread_id, command)
            .await
    }

    pub async fn set_model_selection(
        &self,
        command_id: CommandId,
        thread_id: ThreadId,
        model_selection: ModelSelection,
    ) -> Result<ThreadRecords, ThreadLifecycleError> {
        let command = OrchestrationV2Command::ThreadModelSelectionSet {
            command_id,
            thread_id: thread_id.clone(),
            model_selection,
        };
        self.dispatch(LifecycleOperation::SetModelSelection, &thread_id, command)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::command_receipt_store::CommandReceiptStore;
    use crate::pipeline::hooks_bridge::NoHooks;
    use crate::pipeline::test_support::{
        MockOrchestrator, NoSessions, StaticProjects, StaticSettings, projection,
    };
    use crate::pipeline::thread_command_executor::ThreadCommandExecutor;
    use serde_json::json;
    use std::sync::Arc;
    use turin_persistence::Db;

    #[tokio::test]
    async fn maps_application_lifecycle_operations_to_v2_native_commands() {
        let thread_id = ThreadId::new("thread_lifecycle_service").unwrap();
        let orchestrator = Arc::new(
            MockOrchestrator::default()
                .with_records(|id| Ok(projection(id.as_str(), "project:lifecycle", json!({})))),
        );
        let threads = ThreadManagementService::new(
            ThreadCommandExecutor::new(),
            orchestrator.clone(),
            Arc::new(NoSessions),
            StaticSettings::default_settings(),
            CommandReceiptStore::new(Db::open_in_memory().await.unwrap()),
            Arc::new(StaticProjects(Default::default())),
            Arc::new(NoHooks),
        );
        let service = ThreadLifecycleService::new(&threads);
        let id = |name: &str| CommandId::new(name).unwrap();
        service
            .archive(id("archive"), thread_id.clone())
            .await
            .unwrap();
        service
            .unarchive(id("unarchive"), thread_id.clone())
            .await
            .unwrap();
        service
            .update_metadata(
                id("metadata"),
                thread_id.clone(),
                MetadataUpdate {
                    title: Some(TrimmedNonEmptyString::new("T").unwrap()),
                    ..MetadataUpdate::default()
                },
            )
            .await
            .unwrap();
        service
            .set_runtime_mode(
                id("runtime"),
                thread_id.clone(),
                RuntimeMode::ApprovalRequired,
            )
            .await
            .unwrap();
        service
            .set_interaction_mode(
                id("interaction"),
                thread_id.clone(),
                ProviderInteractionMode::Plan,
            )
            .await
            .unwrap();
        service
            .set_model_selection(
                id("model"),
                thread_id.clone(),
                serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5.2"})).unwrap(),
            )
            .await
            .unwrap();
        service
            .delete(id("delete"), thread_id.clone())
            .await
            .unwrap();
        assert_eq!(
            orchestrator.dispatched_types(),
            [
                "thread.archive",
                "thread.unarchive",
                "thread.metadata.update",
                "thread.runtime-mode.set",
                "thread.interaction-mode.set",
                "thread.model-selection.set",
                "thread.delete",
            ]
        );
    }

    #[tokio::test]
    async fn failures_name_the_operation_and_thread() {
        let thread_id = ThreadId::new("thread:lifecycle-fail").unwrap();
        let orchestrator = Arc::new(MockOrchestrator::default());
        *orchestrator.dispatch_fn.lock() = Box::new(|command| {
            Err(
                crate::pipeline::orchestrator_port::OrchestratorError::CommandRejected {
                    command_id: CommandId::new("x").unwrap(),
                    command_type: format!("{:?}", command).chars().take(10).collect(),
                    cause: "no".into(),
                },
            )
        });
        let threads = ThreadManagementService::new(
            ThreadCommandExecutor::new(),
            orchestrator,
            Arc::new(NoSessions),
            StaticSettings::default_settings(),
            CommandReceiptStore::new(Db::open_in_memory().await.unwrap()),
            Arc::new(StaticProjects(Default::default())),
            Arc::new(NoHooks),
        );
        let error = ThreadLifecycleService::new(&threads)
            .archive(CommandId::new("archive").unwrap(), thread_id.clone())
            .await
            .unwrap_err();
        assert_eq!(error.operation, LifecycleOperation::Archive);
        assert_eq!(
            error.to_string(),
            "Thread lifecycle operation 'archive' failed for thread:lifecycle-fail."
        );
    }
}
