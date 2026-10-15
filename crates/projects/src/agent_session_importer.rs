//! Port of upstream/apps/server/src/project/AgentSessionImporter.ts.
//!
//! Turns the scanner's recent transcripts into v1-imported threads. Each
//! thread is written in one event batch with its provider runtime row, so a
//! later import sees either the whole thread or none of it. Upstream's
//! `Effect.option` and `Effect.catch` become `Option` and per-thread skips.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use futures::StreamExt;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::agent_sessions::{
    AgentSessionImportInput, AgentSessionImportProjectChangedError,
    AgentSessionImportProjectNotFoundError, AgentSessionImportResult, AgentSessionImportSource,
    AgentSessionScanError, AgentSessionScanOperation, AgentSessionSource,
};
use turin_contracts::base_schemas::{ContractError, TrimmedNonEmptyString, UtcDateTime, js_trim};
use turin_contracts::model::{DEFAULT_MODEL, default_model_by_provider};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2AppThreadLineage,
    OrchestrationV2ConversationMessage, OrchestrationV2ConversationMessageRole,
    OrchestrationV2CreationSource, OrchestrationV2DomainEvent, OrchestrationV2NativeRefStrength,
    OrchestrationV2ProviderRef, OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadStatus,
    OrchestrationV2SettleOverride, OrchestrationV2ThreadHistoryOrigin, OrchestrationV2TurnItem,
    OrchestrationV2TurnItemStatus, OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::provider_policy::{DEFAULT_PROVIDER_INTERACTION_MODE, DEFAULT_RUNTIME_MODE};
use turin_contracts::{EventId, MessageId, ProjectId, ThreadId, TurnItemId};

use crate::agent_session_scanner::{
    AgentSessionMessageRole, AgentSessionRecentThread, AgentSessionScanner, AgentSessionThread,
    AgentSessionThreadMessage,
};
use crate::project_path::normalize_project_path_for_comparison;
use crate::project_ports::{
    ImportedProviderRuntime, OrchestrationEventWriter, PortError, ProviderRuntimeRows,
    ProviderThreadIds, ThreadRecordReader,
};
use crate::project_service::ProjectServiceApi;

const IMPORT_EVENT_PREFIX: &str = "agent-session-import:v2";

/// Claude session ids are UUIDs. Anything else cannot be resumed.
static CLAUDE_SESSION_ID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        .expect("static pattern is valid")
});

/// Import failure that stops the whole batch. Per-thread failures are skipped.
#[derive(Debug, thiserror::Error)]
pub enum AgentSessionImportError {
    #[error(transparent)]
    ProjectNotFound(#[from] AgentSessionImportProjectNotFoundError),
    #[error(transparent)]
    ProjectChanged(#[from] AgentSessionImportProjectChangedError),
    #[error(transparent)]
    Scan(#[from] AgentSessionScanError),
}

/// Why one transcript was not imported. Logged and counted as skipped.
#[derive(Debug, thiserror::Error)]
enum ImportThreadError {
    #[error("Session '{0}' cannot be resumed.")]
    Unresumable(String),
    #[error("Imported thread '{thread_id}' belongs to project '{actual}', not '{expected}'.")]
    ProjectConflict {
        thread_id: ThreadId,
        expected: ProjectId,
        actual: ProjectId,
    },
    #[error("Imported thread '{0}' already contains non-imported activity.")]
    ThreadModified(ThreadId),
    #[error(transparent)]
    Invalid(#[from] ContractError),
    #[error("{0:#}")]
    Port(PortError),
}

impl From<PortError> for ImportThreadError {
    fn from(cause: PortError) -> Self {
        Self::Port(cause)
    }
}

pub struct AgentSessionImporter {
    scanner: Arc<dyn AgentSessionScanner>,
    projects: Arc<dyn ProjectServiceApi>,
    threads: Arc<dyn ThreadRecordReader>,
    events: Arc<dyn OrchestrationEventWriter>,
    provider_thread_ids: Arc<dyn ProviderThreadIds>,
    runtimes: Arc<dyn ProviderRuntimeRows>,
}

impl AgentSessionImporter {
    pub fn new(
        scanner: Arc<dyn AgentSessionScanner>,
        projects: Arc<dyn ProjectServiceApi>,
        threads: Arc<dyn ThreadRecordReader>,
        events: Arc<dyn OrchestrationEventWriter>,
        provider_thread_ids: Arc<dyn ProviderThreadIds>,
        runtimes: Arc<dyn ProviderRuntimeRows>,
    ) -> Self {
        Self {
            scanner,
            projects,
            threads,
            events,
            provider_thread_ids,
            runtimes,
        }
    }

    /// Imports every recent transcript for the project's workspace root that
    /// is not already a thread. Repeats are idempotent: an imported thread
    /// counts as imported, and nothing is written twice.
    pub async fn import_recent_agent_threads(
        &self,
        input: &AgentSessionImportInput,
    ) -> Result<AgentSessionImportResult, AgentSessionImportError> {
        let project = self
            .projects
            .get_by_id(&input.project_id, false)
            .await
            .map_err(|cause| read_projects_error(&cause))?
            .ok_or_else(|| AgentSessionImportProjectNotFoundError::NotFound {
                project_id: input.project_id.clone(),
            })?;
        let root = project.workspace_root.as_str();
        if let Some(expected) = &input.expected_workspace_root
            && normalize_project_path_for_comparison(root)
                != normalize_project_path_for_comparison(expected.as_str())
        {
            return Err(AgentSessionImportProjectChangedError::Changed {
                project_id: input.project_id.clone(),
            }
            .into());
        }

        let runtime_payloads = self
            .runtimes
            .list_runtime_payloads()
            .await
            .map_err(|cause| read_projects_error(&cause))?;
        let completed_sources: Vec<AgentSessionImportSource> = runtime_payloads
            .into_iter()
            .flatten()
            .flat_map(|payload| completed_sources_for(payload, root))
            .collect();

        let mut outcomes = self.scanner.recent_threads(root, &completed_sources);
        let mut imported_thread_ids = HashSet::<ThreadId>::new();
        let mut imported_count = 0;
        let mut skipped_count = 0;
        while let Some(outcome) = outcomes.next().await {
            match outcome? {
                AgentSessionRecentThread::Skipped => skipped_count += 1,
                AgentSessionRecentThread::AlreadyImported { source } => {
                    imported_thread_ids.insert(import_thread_id(&source));
                    imported_count += 1;
                }
                AgentSessionRecentThread::Duplicate { source } => {
                    let thread_id = import_thread_id(&source);
                    if imported_thread_ids.contains(&thread_id) {
                        // Upstream `Effect.ignore`: a failed record does not fail the batch.
                        let _ = self
                            .runtimes
                            .record_imported_transcript(&thread_id, &source)
                            .await;
                    }
                }
                AgentSessionRecentThread::Importable { thread, source } => {
                    let thread_id = import_thread_id(&source);
                    match self
                        .import_thread(&input.project_id, root, &thread_id, &source, &thread)
                        .await
                    {
                        Ok(()) => {
                            imported_thread_ids.insert(thread_id);
                            imported_count += 1;
                        }
                        Err(cause) => {
                            tracing::warn!(
                                provider = ?thread.source,
                                session_id = %thread.provider_session_id,
                                error = %cause,
                                "Could not import an agent session"
                            );
                            skipped_count += 1;
                        }
                    }
                }
            }
        }

        Ok(AgentSessionImportResult {
            imported_count,
            skipped_count,
        })
    }

    async fn import_thread(
        &self,
        project_id: &ProjectId,
        workspace_root: &str,
        thread_id: &ThreadId,
        source: &AgentSessionImportSource,
        thread: &AgentSessionThread,
    ) -> Result<(), ImportThreadError> {
        if thread.source == AgentSessionSource::ClaudeAgent
            && !CLAUDE_SESSION_ID_PATTERN.is_match(&thread.provider_session_id)
        {
            return Err(ImportThreadError::Unresumable(
                thread.provider_session_id.clone(),
            ));
        }

        // Upstream `Effect.option`: a failed read counts as "no thread yet".
        if let Some(existing) = self
            .threads
            .get_thread_records(thread_id)
            .await
            .ok()
            .flatten()
        {
            if existing.project_id != *project_id {
                return Err(ImportThreadError::ProjectConflict {
                    thread_id: thread_id.clone(),
                    expected: project_id.clone(),
                    actual: existing.project_id,
                });
            }
            if existing.history_origin != Some(OrchestrationV2ThreadHistoryOrigin::V1Import) {
                return Err(ImportThreadError::ThreadModified(thread_id.clone()));
            }
            self.runtimes
                .record_imported_transcript(thread_id, source)
                .await?;
            return Ok(());
        }

        let driver_slug = driver_slug(thread.source);
        let driver = ProviderDriverKind::new(driver_slug)?;
        let model = TrimmedNonEmptyString::new(
            thread
                .model
                .clone()
                .or_else(|| default_model_by_provider(driver_slug).map(str::to_owned))
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        )?;
        let provider_thread_id = self
            .provider_thread_ids
            .derive_provider_thread(&driver, &thread.provider_session_id);
        let created_at = parse_utc(&thread.created_at)?;
        let updated_at = parse_utc(&thread.updated_at)?;
        let title = if js_trim(&thread.title).is_empty() {
            "Untitled thread"
        } else {
            thread.title.as_str()
        };
        let native_id = TrimmedNonEmptyString::new(&thread.provider_session_id)?;

        let app_thread = OrchestrationV2AppThread {
            created_by: OrchestrationV2Actor::System,
            creation_source: OrchestrationV2CreationSource::Server,
            id: thread_id.clone(),
            project_id: project_id.clone(),
            title: TrimmedNonEmptyString::new(title)?,
            provider_instance_id: thread.provider_instance_id.clone(),
            model_selection: ModelSelection {
                instance_id: thread.provider_instance_id.clone(),
                model,
                options: None,
            },
            runtime_mode: DEFAULT_RUNTIME_MODE,
            interaction_mode: DEFAULT_PROVIDER_INTERACTION_MODE,
            branch: None,
            worktree_path: None,
            linked_pull_request: Some(None),
            pull_requests: None,
            branch_pull_request: Some(None),
            active_provider_thread_id: Some(provider_thread_id.clone()),
            history_origin: Some(OrchestrationV2ThreadHistoryOrigin::V1Import),
            lineage: OrchestrationV2AppThreadLineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: thread_id.clone(),
            },
            forked_from: None,
            created_at,
            updated_at,
            archived_at: None,
            settled_override: Some(OrchestrationV2SettleOverride::Settled),
            settled_at: Some(updated_at),
            unsettled_at: Some(None),
            snoozed_until: Some(None),
            snoozed_at: Some(None),
            last_snooze_wake_at: None,
            limit_recovery: None,
            pinned_at: Some(None),
            auto_settle_disabled_at: None,
            pin_order_key: Some(None),
            active_order_key: Some(None),
            last_visited_at: None,
            title_regeneration: None,
            rollback_request_id: None,
            rollback_failure: None,
            deleted_at: None,
        };
        let provider_thread = OrchestrationV2ProviderThread {
            id: provider_thread_id.clone(),
            driver: driver.clone(),
            provider_instance_id: thread.provider_instance_id.clone(),
            provider_session_id: None,
            app_thread_id: Some(thread_id.clone()),
            owner_node_id: None,
            native_thread_ref: Some(OrchestrationV2ProviderRef {
                driver: driver.clone(),
                native_id: Some(native_id),
                strength: OrchestrationV2NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::Idle,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at,
            updated_at,
        };

        // Codex resumes by its native thread id. Claude's CLI resumes by session id.
        let resume_cursor = if thread.source == AgentSessionSource::Codex {
            json!({ "threadId": thread.provider_session_id })
        } else {
            json!({ "threadId": thread_id.as_str(), "resume": thread.provider_session_id })
        };
        self.runtimes
            .upsert_imported(&ImportedProviderRuntime {
                thread_id: thread_id.clone(),
                provider_name: driver_slug.to_owned(),
                provider_instance_id: thread.provider_instance_id.clone(),
                adapter_key: driver_slug.to_owned(),
                runtime_mode: DEFAULT_RUNTIME_MODE,
                last_seen_at: thread.updated_at.clone(),
                resume_cursor,
                runtime_payload: json!({ "cwd": workspace_root }),
            })
            .await?;

        let mut events = vec![OrchestrationV2DomainEvent::ThreadCreated {
            id: event_id(format!("{IMPORT_EVENT_PREFIX}:thread:{thread_id}:created"))?,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(thread.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: created_at,
            payload: app_thread,
        }];
        for (index, message) in thread.messages.iter().enumerate() {
            events.extend(message_events(thread_id, index, message)?);
        }
        events.push(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id: event_id(format!(
                "{IMPORT_EVENT_PREFIX}:provider-thread:{provider_thread_id}"
            ))?,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: Some(driver),
            provider_instance_id: Some(thread.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: updated_at,
            payload: provider_thread,
        });
        self.events.write(events).await?;

        self.runtimes
            .record_imported_transcript(thread_id, source)
            .await?;
        Ok(())
    }
}

/// Upstream `importRecentAgentThreads` builds its `completedSources` from the
/// runtime payload only when the payload's `cwd` is this workspace. A payload
/// that does not decode contributes nothing.
fn completed_sources_for(payload: Value, workspace_root: &str) -> Vec<AgentSessionImportSource> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ImportedTranscriptPayload {
        cwd: Option<String>,
        imported_transcripts: Option<Vec<AgentSessionImportSource>>,
    }

    let Ok(payload) = serde_json::from_value::<ImportedTranscriptPayload>(payload) else {
        return Vec::new();
    };
    match payload.cwd {
        Some(cwd)
            if normalize_project_path_for_comparison(&cwd)
                == normalize_project_path_for_comparison(workspace_root) =>
        {
            payload.imported_transcripts.unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// The thread id an import of this session uses. Prefix and both parts are
/// non-blank, so the id is never blank.
fn import_thread_id(source: &AgentSessionImportSource) -> ThreadId {
    ThreadId::new(format!(
        "import:{}:{}",
        source.provider_instance_id.as_str(),
        source.provider_session_id.as_str()
    ))
    .expect("import thread ids are never blank")
}

fn driver_slug(source: AgentSessionSource) -> &'static str {
    match source {
        AgentSessionSource::ClaudeAgent => "claudeAgent",
        AgentSessionSource::Codex => "codex",
    }
}

fn parse_utc(value: &str) -> Result<UtcDateTime, ContractError> {
    value.parse()
}

fn event_id(value: String) -> Result<EventId, ContractError> {
    EventId::new(value)
}

fn read_projects_error(cause: &impl std::fmt::Display) -> AgentSessionScanError {
    AgentSessionScanError::ScanError {
        operation: AgentSessionScanOperation::ReadProjects,
        cause: json!(cause.to_string()),
    }
}

/// The user and assistant events for one transcript message. The message row
/// and its turn item share an ordinal, and both are completed on arrival.
fn message_events(
    thread_id: &ThreadId,
    index: usize,
    message: &AgentSessionThreadMessage,
) -> Result<[OrchestrationV2DomainEvent; 2], ImportThreadError> {
    let ordinal = index as u64 + 1;
    let suffix = format!("{index:06}");
    let message_id = MessageId::new(format!("{thread_id}:{suffix}"))?;
    let turn_item_id = TurnItemId::new(format!(
        "{IMPORT_EVENT_PREFIX}:turn-item:{thread_id}:{suffix}"
    ))?;
    let at = parse_utc(&message.created_at)?;
    let is_user = message.role == AgentSessionMessageRole::User;
    let role = if is_user {
        OrchestrationV2ConversationMessageRole::User
    } else {
        OrchestrationV2ConversationMessageRole::Assistant
    };
    let conversation_message = OrchestrationV2ConversationMessage {
        notification: None,
        created_by: if is_user {
            OrchestrationV2Actor::User
        } else {
            OrchestrationV2Actor::Agent
        },
        creation_source: OrchestrationV2CreationSource::Server,
        scheduled_task_id: None,
        sender_thread_id: None,
        id: message_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        role,
        text: message.text.clone(),
        context: None,
        attachments: Vec::new(),
        streaming: false,
        created_at: at,
        updated_at: at,
        delegated_completion: None,
    };
    let turn_item = if is_user {
        OrchestrationV2TurnItem::UserMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: turn_item_id.clone(),
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: None,
            started_at: Some(at),
            completed_at: Some(at),
            updated_at: at,
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Server,
            message_id: message_id.clone(),
            scheduled_task_id: None,
            sender_thread_id: None,
            input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
            text: message.text.clone(),
            context: None,
            attachments: Vec::new(),
        }
    } else {
        OrchestrationV2TurnItem::AssistantMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: turn_item_id.clone(),
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: None,
            started_at: Some(at),
            completed_at: Some(at),
            updated_at: at,
            message_id: message_id.clone(),
            text: message.text.clone(),
            attachments: None,
            streaming: false,
        }
    };
    Ok([
        OrchestrationV2DomainEvent::MessageUpdated {
            id: event_id(format!(
                "{IMPORT_EVENT_PREFIX}:message:{thread_id}:{suffix}"
            ))?,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: at,
            payload: conversation_message,
        },
        OrchestrationV2DomainEvent::TurnItemUpdated {
            id: event_id(format!(
                "{IMPORT_EVENT_PREFIX}:turn-item:{thread_id}:{suffix}"
            ))?,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: at,
            payload: turn_item,
        },
    ])
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::CommandId;
    use turin_contracts::ProviderThreadId;
    use turin_contracts::provider_instance::ProviderInstanceId;

    use super::*;
    use crate::project_ports::ThreadRecordSummary;
    use crate::project_service::{ProjectCreateInput, create_payload};
    use crate::testing::FakeProjects;

    const PROJECT_ROOT: &str = "/workspace/project";

    struct FakeScanner {
        recent: Vec<AgentSessionRecentThread>,
    }

    #[async_trait::async_trait]
    impl AgentSessionScanner for FakeScanner {
        async fn scan(
            &self,
        ) -> Result<turin_contracts::agent_sessions::AgentSessionScanResult, AgentSessionScanError>
        {
            Ok(turin_contracts::agent_sessions::AgentSessionScanResult {
                candidates: Vec::new(),
                scanned_at: "2026-01-01T00:00:00.000Z".to_owned(),
                truncated: None,
            })
        }

        fn recent_threads<'a>(
            &'a self,
            _workspace_root: &'a str,
            _completed_sources: &'a [AgentSessionImportSource],
        ) -> futures::stream::BoxStream<'a, Result<AgentSessionRecentThread, AgentSessionScanError>>
        {
            futures::stream::iter(self.recent.clone().into_iter().map(Ok)).boxed()
        }
    }

    /// Stands in for the orchestrator: a write of `thread.created` makes the
    /// thread readable as a v1 import.
    #[derive(Default)]
    struct FakeOrchestrator {
        threads: Mutex<Vec<(ThreadId, ThreadRecordSummary)>>,
        writes: Mutex<Vec<Vec<OrchestrationV2DomainEvent>>>,
    }

    #[async_trait::async_trait]
    impl ThreadRecordReader for FakeOrchestrator {
        async fn get_thread_records(
            &self,
            thread_id: &ThreadId,
        ) -> Result<Option<ThreadRecordSummary>, PortError> {
            Ok(self
                .threads
                .lock()
                .iter()
                .find(|(id, _)| id == thread_id)
                .map(|(_, record)| record.clone()))
        }
    }

    #[async_trait::async_trait]
    impl OrchestrationEventWriter for FakeOrchestrator {
        async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), PortError> {
            for event in &events {
                if let OrchestrationV2DomainEvent::ThreadCreated {
                    thread_id, payload, ..
                } = event
                {
                    self.threads.lock().push((
                        thread_id.clone(),
                        ThreadRecordSummary {
                            project_id: payload.project_id.clone(),
                            history_origin: payload.history_origin,
                        },
                    ));
                }
            }
            self.writes.lock().push(events);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRuntimes {
        upserts: Mutex<Vec<ImportedProviderRuntime>>,
        recorded: Mutex<Vec<(ThreadId, AgentSessionImportSource)>>,
    }

    #[async_trait::async_trait]
    impl ProviderRuntimeRows for FakeRuntimes {
        async fn list_runtime_payloads(&self) -> Result<Vec<Option<Value>>, PortError> {
            Ok(Vec::new())
        }

        async fn upsert_imported(&self, row: &ImportedProviderRuntime) -> Result<(), PortError> {
            self.upserts.lock().push(row.clone());
            Ok(())
        }

        async fn record_imported_transcript(
            &self,
            thread_id: &ThreadId,
            source: &AgentSessionImportSource,
        ) -> Result<(), PortError> {
            self.recorded
                .lock()
                .push((thread_id.clone(), source.clone()));
            Ok(())
        }
    }

    struct FakeProviderThreadIds;

    impl ProviderThreadIds for FakeProviderThreadIds {
        fn derive_provider_thread(
            &self,
            driver: &ProviderDriverKind,
            native_thread_id: &str,
        ) -> ProviderThreadId {
            ProviderThreadId::new(format!(
                "provider-thread:{}:native-thread:{native_thread_id}",
                driver.as_str()
            ))
            .expect("test id is non-blank")
        }
    }

    fn codex_source(provider_instance_id: &ProviderInstanceId) -> AgentSessionImportSource {
        AgentSessionImportSource {
            provider: AgentSessionSource::Codex,
            provider_instance_id: provider_instance_id.clone(),
            provider_session_id: TrimmedNonEmptyString::new("native-codex-thread").unwrap(),
            file_path: TrimmedNonEmptyString::new("/tmp/native-codex-thread.jsonl").unwrap(),
            size: 100,
            mtime_ms: Some(2.0),
            device: 3.0,
            inode: Some(4.0),
            birthtime_ms: Some(1.0),
        }
    }

    fn codex_thread(provider_instance_id: &ProviderInstanceId) -> AgentSessionThread {
        AgentSessionThread {
            source: AgentSessionSource::Codex,
            provider_instance_id: provider_instance_id.clone(),
            provider_session_id: "native-codex-thread".to_owned(),
            title: "Imported thread".to_owned(),
            model: Some("gpt-5.4".to_owned()),
            created_at: "2026-09-01T10:00:00.000Z".to_owned(),
            updated_at: "2026-09-01T10:01:00.000Z".to_owned(),
            messages: vec![
                AgentSessionThreadMessage {
                    role: AgentSessionMessageRole::User,
                    text: "Fix it".to_owned(),
                    created_at: "2026-09-01T10:00:00.000Z".to_owned(),
                },
                AgentSessionThreadMessage {
                    role: AgentSessionMessageRole::Assistant,
                    text: "Fixed".to_owned(),
                    created_at: "2026-09-01T10:01:00.000Z".to_owned(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn imports_messages_once_and_preserves_the_provider_native_resume_binding() {
        let provider_instance_id = ProviderInstanceId::new("codex").unwrap();
        let project_id = ProjectId::new("agent-session-import-project").unwrap();
        let thread_id = ThreadId::new(format!(
            "import:{}:native-codex-thread",
            provider_instance_id.as_str()
        ))
        .unwrap();
        let scanner = Arc::new(FakeScanner {
            recent: vec![AgentSessionRecentThread::Importable {
                thread: codex_thread(&provider_instance_id),
                source: codex_source(&provider_instance_id),
            }],
        });
        let projects = Arc::new(FakeProjects::new());
        projects
            .create(ProjectCreateInput {
                command_id: CommandId::new("create-project").unwrap(),
                project_id: project_id.clone(),
                payload: create_payload(
                    TrimmedNonEmptyString::new("Project").unwrap(),
                    PROJECT_ROOT.to_owned(),
                ),
            })
            .await
            .unwrap();
        let orchestrator = Arc::new(FakeOrchestrator::default());
        let runtimes = Arc::new(FakeRuntimes::default());
        let importer = AgentSessionImporter::new(
            scanner,
            projects,
            orchestrator.clone(),
            orchestrator.clone(),
            Arc::new(FakeProviderThreadIds),
            runtimes.clone(),
        );
        let input = AgentSessionImportInput {
            project_id: project_id.clone(),
            expected_workspace_root: None,
        };

        let expected = AgentSessionImportResult {
            imported_count: 1,
            skipped_count: 0,
        };
        assert_eq!(
            importer.import_recent_agent_threads(&input).await.unwrap(),
            expected
        );
        assert_eq!(
            importer.import_recent_agent_threads(&input).await.unwrap(),
            expected
        );

        let writes = orchestrator.writes.lock().clone();
        assert_eq!(writes.len(), 1);
        let types: Vec<String> = writes[0]
            .iter()
            .map(|event| {
                serde_json::to_value(event).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            types,
            [
                "thread.created",
                "message.updated",
                "turn-item.updated",
                "message.updated",
                "turn-item.updated",
                "provider-thread.updated",
            ]
        );

        let created = writes[0]
            .iter()
            .find_map(|event| match event {
                OrchestrationV2DomainEvent::ThreadCreated { payload, .. } => Some(payload),
                _ => None,
            })
            .unwrap();
        let provider_thread = writes[0]
            .iter()
            .find_map(|event| match event {
                OrchestrationV2DomainEvent::ProviderThreadUpdated { payload, .. } => Some(payload),
                _ => None,
            })
            .unwrap();
        assert_eq!(created.id, thread_id);
        assert_eq!(
            created.active_provider_thread_id.as_ref(),
            Some(&provider_thread.id)
        );
        assert_eq!(
            created.history_origin,
            Some(OrchestrationV2ThreadHistoryOrigin::V1Import)
        );
        assert_eq!(provider_thread.app_thread_id.as_ref(), Some(&thread_id));
        assert_eq!(
            provider_thread.native_thread_ref,
            Some(OrchestrationV2ProviderRef {
                driver: ProviderDriverKind::new("codex").unwrap(),
                native_id: Some(TrimmedNonEmptyString::new("native-codex-thread").unwrap()),
                strength: OrchestrationV2NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            })
        );
        let texts: Vec<&str> = writes[0]
            .iter()
            .filter_map(|event| match event {
                OrchestrationV2DomainEvent::MessageUpdated { payload, .. } => {
                    Some(payload.text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["Fix it", "Fixed"]);

        let upserts = runtimes.upserts.lock().clone();
        assert_eq!(upserts.len(), 1);
        assert_eq!(upserts[0].thread_id, thread_id);
        assert_eq!(upserts[0].provider_instance_id, provider_instance_id);
        assert_eq!(
            upserts[0].resume_cursor,
            json!({ "threadId": "native-codex-thread" })
        );
        assert_eq!(runtimes.recorded.lock().len(), 2);
    }
}
