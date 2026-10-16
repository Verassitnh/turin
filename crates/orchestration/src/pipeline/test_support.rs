//! Test doubles shared by the service tests in this crate.

use super::event_sink::{SinkStream, StreamOptions};
use super::orchestrator_port::{
    BoxError, DispatchResult, OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
    ShellSnapshotOptions, ThreadRecords,
};
use super::seams::{ProjectDirectory, ProjectSummary, ProviderSessionLookup, ServerSettingsSource};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Notify, mpsc};
use turin_contracts::orchestration_v2::{
    OrchestrationV2DomainEvent, OrchestrationV2ProviderCapabilities, OrchestrationV2Run,
    OrchestrationV2ServerCommand, OrchestrationV2StoredEvent, OrchestrationV2ThreadProjection,
    OrchestrationV2ThreadShell, OrchestrationV2ThreadShellSnapshot,
};
use turin_contracts::settings::ServerSettings;
use turin_contracts::{ProjectId, ProviderSessionId, ThreadId};

pub fn thread_json(thread_id: &str, project_id: &str) -> Value {
    let mut thread = super::domain_event::test_support::thread_json(thread_id);
    thread["projectId"] = json!(project_id);
    thread
}

pub fn projection(thread_id: &str, project_id: &str, extra: Value) -> ThreadRecords {
    let mut value = json!({
        "thread": thread_json(thread_id, project_id),
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [],
        "updatedAt": "2026-06-21T00:00:00.000Z",
    });
    for (key, field) in extra.as_object().cloned().unwrap_or_default() {
        value[key] = field;
    }
    serde_json::from_value(value).expect("test projection decodes")
}

pub fn run_json(id: &str, thread_id: &str, ordinal: u64, status: &str) -> Value {
    json!({
        "id": id, "threadId": thread_id, "ordinal": ordinal, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
        "providerThreadId": null, "userMessageId": format!("message:{id}"),
        "rootNodeId": null, "activeAttemptId": null, "status": status,
        "requestedAt": "2026-06-21T00:00:00.000Z", "startedAt": null,
        "completedAt": null, "checkpointId": null, "contextHandoffId": null,
    })
}

pub fn run(id: &str, thread_id: &str, ordinal: u64, status: &str) -> OrchestrationV2Run {
    serde_json::from_value(run_json(id, thread_id, ordinal, status)).unwrap()
}

type PendingFn = Box<dyn FnMut(&OrchestrationV2Run) -> bool + Send>;
type RecordsFn = Box<dyn FnMut(&ThreadId) -> Result<ThreadRecords, OrchestratorError> + Send>;
type DispatchFn = Box<
    dyn FnMut(OrchestrationV2ServerCommand) -> Result<DispatchResult, OrchestratorError> + Send,
>;

/// A scripted `OrchestratorPort`. Unset behaviours return an error or an
/// empty result, never panic, so a test only scripts what it exercises.
pub struct MockOrchestrator {
    pub records: Mutex<RecordsFn>,
    pub projection_fn: Mutex<RecordsFn>,
    pub dispatch_fn: Mutex<DispatchFn>,
    pub dispatched: Mutex<Vec<OrchestrationV2ServerCommand>>,
    pub shell: Mutex<Option<OrchestrationV2ThreadShell>>,
    pub shell_snapshot: Mutex<Option<OrchestrationV2ThreadShellSnapshot>>,
    pub restart_pending: Mutex<PendingFn>,
    pub restart_declined: Notify,
    /// Per event type: the receiver `stream_stored_events_from` hands out.
    pub streams: Mutex<HashMap<String, mpsc::UnboundedReceiver<OrchestrationV2StoredEvent>>>,
    /// Notified whenever a `run.updated` stream is requested.
    pub run_stream_requested: Notify,
    pub read_count: Mutex<usize>,
}

impl Default for MockOrchestrator {
    fn default() -> Self {
        Self {
            records: Mutex::new(Box::new(|id| {
                Err(OrchestratorError::projection(id.clone(), "unscripted"))
            })),
            projection_fn: Mutex::new(Box::new(|id| {
                Err(OrchestratorError::projection(id.clone(), "unscripted"))
            })),
            dispatch_fn: Mutex::new(Box::new(|_| {
                Ok(DispatchResult {
                    sequence: 1,
                    stored_events: vec![],
                })
            })),
            dispatched: Mutex::new(vec![]),
            shell: Mutex::new(None),
            shell_snapshot: Mutex::new(None),
            restart_pending: Mutex::new(Box::new(|_| false)),
            restart_declined: Notify::new(),
            streams: Mutex::new(HashMap::new()),
            run_stream_requested: Notify::new(),
            read_count: Mutex::new(0),
        }
    }
}

impl MockOrchestrator {
    pub fn with_records(
        self,
        records: impl FnMut(&ThreadId) -> Result<ThreadRecords, OrchestratorError> + Send + 'static,
    ) -> Self {
        *self.records.lock() = Box::new(records);
        self
    }

    pub fn dispatched_types(&self) -> Vec<String> {
        self.dispatched
            .lock()
            .iter()
            .map(|command| {
                serde_json::to_value(command).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    pub fn open_stream(
        &self,
        event_type: &str,
    ) -> mpsc::UnboundedSender<OrchestrationV2StoredEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.streams.lock().insert(event_type.to_owned(), rx);
        tx
    }
}

#[async_trait]
impl OrchestratorPort for MockOrchestrator {
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, OrchestratorError> {
        self.dispatched.lock().push(command.clone());
        (self.dispatch_fn.lock())(command)
    }

    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        _fields: &[RecordField],
        _filter: &RecordFilter,
    ) -> Result<ThreadRecords, OrchestratorError> {
        *self.read_count.lock() += 1;
        (self.records.lock())(thread_id)
    }

    async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
        (self.projection_fn.lock())(thread_id)
    }

    async fn get_thread_shell(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2ThreadShell>, OrchestratorError> {
        Ok(self.shell.lock().clone())
    }

    async fn get_shell_snapshot(
        &self,
        _options: &ShellSnapshotOptions,
    ) -> Result<OrchestrationV2ThreadShellSnapshot, OrchestratorError> {
        self.shell_snapshot
            .lock()
            .clone()
            .ok_or_else(|| OrchestratorError::DomainEventStream {
                cause: "no snapshot scripted".into(),
            })
    }

    async fn get_thread_event_sequence(&self, _: &ThreadId) -> Result<i64, OrchestratorError> {
        Ok(0)
    }

    async fn get_thread_history_page(
        &self,
        thread_id: &ThreadId,
        _cursor: &str,
        _through_entry_id: Option<&str>,
        _conversation_only: bool,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ThreadHistoryPage,
        OrchestratorError,
    > {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_timeline_page(
        &self,
        thread_id: &ThreadId,
        _options: crate::projection::store::history::TimelinePageOptions,
    ) -> Result<crate::projection::store::history::ProjectionTimelinePage, OrchestratorError> {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_message_count(&self, thread_id: &ThreadId) -> Result<u64, OrchestratorError> {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_turn_item(
        &self,
        thread_id: &ThreadId,
        _item_id: &turin_contracts::TurnItemId,
    ) -> Result<Option<turin_contracts::orchestration_v2::OrchestrationV2TurnItem>, OrchestratorError>
    {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_checkpoint_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<crate::projection::store::contexts::CheckpointContext, OrchestratorError> {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_thread_snapshot(
        &self,
        thread_id: &ThreadId,
    ) -> Result<crate::projection::store::ThreadSnapshot, OrchestratorError> {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn get_thread_snapshot_window(
        &self,
        thread_id: &ThreadId,
        _options: crate::projection::store::ThreadSnapshotWindowOptions,
    ) -> Result<crate::projection::store::ThreadSnapshot, OrchestratorError> {
        Err(OrchestratorError::projection(
            thread_id.clone(),
            "not scripted",
        ))
    }

    async fn search_thread_stream(
        &self,
        input: &turin_contracts::orchestration_v2::OrchestrationV2SearchThreadInput,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            turin_contracts::orchestration_v2::OrchestrationV2SearchThreadResult,
        >,
        OrchestratorError,
    > {
        Err(OrchestratorError::projection(
            input.thread_id.clone(),
            "search is not scripted",
        ))
    }

    async fn restart_continuation_pending(&self, run: &OrchestrationV2Run) -> bool {
        (self.restart_pending.lock())(run)
    }

    async fn await_restart_continuation(&self, _run: &OrchestrationV2Run) {
        self.restart_declined.notified().await;
    }

    async fn recover_delegated_task(&self, _: &ThreadId, _: &turin_contracts::RunId) {}

    async fn stream_stored_events_from(
        &self,
        options: StreamOptions,
    ) -> Result<SinkStream, OrchestratorError> {
        let kind = options.event_type.unwrap_or_default();
        if kind == "run.updated" {
            self.run_stream_requested.notify_one();
        }
        let receiver = self.streams.lock().remove(&kind);
        Ok(match receiver {
            Some(mut rx) => stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Ok).boxed(),
            None => stream::pending().boxed(),
        })
    }

    async fn stream_domain_events(
        &self,
    ) -> Result<BoxStream<'static, OrchestrationV2DomainEvent>, OrchestratorError> {
        Ok(stream::pending().boxed())
    }
}

pub struct StaticSettings(pub ServerSettings);

impl StaticSettings {
    pub fn with(value: Value) -> Arc<Self> {
        Arc::new(Self(
            serde_json::from_value(value).expect("test settings decode"),
        ))
    }

    pub fn default_settings() -> Arc<Self> {
        Self::with(json!({}))
    }
}

#[async_trait]
impl ServerSettingsSource for StaticSettings {
    async fn get_settings(&self) -> Result<ServerSettings, BoxError> {
        Ok(self.0.clone())
    }
}

pub struct NoSessions;

#[async_trait]
impl ProviderSessionLookup for NoSessions {
    async fn get(
        &self,
        _: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn super::seams::SessionTaskStopper>>, BoxError> {
        Ok(None)
    }
}

pub struct StaticProjects(pub HashMap<ProjectId, String>);

#[async_trait]
impl ProjectDirectory for StaticProjects {
    async fn get(&self, project_id: &ProjectId) -> Result<Option<ProjectSummary>, BoxError> {
        Ok(self.0.get(project_id).map(|root| ProjectSummary {
            project_id: project_id.clone(),
            workspace_root: root.clone(),
        }))
    }
}

/// A complete provider capability set (Codex-like). `runtimePolicy` is
/// absent, so it decodes to the client-boundary default.
pub fn capabilities_json() -> Value {
    json!({
        "sessions": {
            "supportsMultipleProviderThreadsPerSession": false,
            "supportsModelSwitchInSession": false,
            "supportsProviderSwitchingViaHandoff": true,
            "supportsRuntimeModeSwitchInSession": false,
            "pendingRequestsSurviveRestart": false,
        },
        "threads": {
            "canCreateEmptyThread": true,
            "canReadThreadSnapshot": false,
            "canRollbackThread": true,
            "canForkThread": false,
            "canForkFromTurn": false,
            "canForkFromSubagentThread": false,
            "exposesNativeThreadId": true,
        },
        "turns": {
            "exposesNativeTurnId": false,
            "emitsTurnStarted": true,
            "emitsTurnCompleted": true,
            "supportsInterrupt": true,
            "supportsActiveSteering": false,
            "supportsSteeringByInterruptRestart": true,
            "supportsQueuedMessages": true,
            "terminalStatusQuality": "strong",
        },
        "streaming": {
            "streamsAssistantText": true,
            "streamsReasoning": true,
            "streamsToolOutput": true,
            "streamsPlanText": false,
            "emitsMessageCompleted": true,
        },
        "tools": {
            "exposesToolItemIds": true,
            "emitsToolStarted": true,
            "emitsToolCompleted": true,
            "emitsToolOutput": true,
            "supportsMcpTools": false,
            "supportsDynamicToolCallbacks": false,
        },
        "approvals": {
            "supportsCommandApproval": true,
            "supportsFileReadApproval": true,
            "supportsFileChangeApproval": true,
            "supportsApplyPatchApproval": false,
            "approvalsHaveNativeRequestIds": false,
            "approvalCallbacksAreLiveOnly": true,
            "approvalsCanOriginateFromSubagents": false,
        },
        "planning": {
            "emitsPlanUpdated": true,
            "emitsTodoList": true,
            "emitsProposedPlan": false,
            "supportsStructuredQuestions": true,
            "planDeltasHaveItemIds": false,
        },
        "subagents": {
            "supportsSubagents": false,
            "exposesSubagentThreadIds": false,
            "emitsSubagentLifecycle": false,
            "canWaitForSubagents": false,
            "canCloseSubagents": false,
            "canForkSubagentThread": false,
        },
        "context": {
            "acceptsSystemContext": false,
            "acceptsDeveloperContext": false,
            "acceptsSyntheticUserContext": true,
            "canGenerateSummaries": true,
            "canConsumeHandoffSummaries": true,
            "supportsDeltaHandoff": true,
            "supportsFullThreadHandoff": true,
            "maxRecommendedHandoffChars": null,
        },
        "checkpointing": {
            "appCanCheckpointFilesystem": true,
            "supportsNestedCheckpointScopes": true,
            "providerCanRollbackConversation": true,
            "providerRollbackReturnsSnapshot": true,
            "providerCanReadConversationSnapshot": false,
        },
        "identity": {
            "nativeThreadIds": "strong",
            "nativeTurnIds": "weak",
            "nativeItemIds": "weak",
            "nativeRequestIds": "weak",
        },
    })
}

pub fn default_capabilities() -> OrchestrationV2ProviderCapabilities {
    serde_json::from_value(capabilities_json()).expect("test capabilities decode")
}

pub fn stored(sequence: u64, event: Value) -> OrchestrationV2StoredEvent {
    OrchestrationV2StoredEvent {
        sequence,
        command_id: None,
        event: serde_json::from_value(event).expect("test event decodes"),
    }
}
