//! Port of `upstream/packages/provider-opencode/src/server/v2/adapter.ts`.
//!
//! OpenCode 2 has no per-prompt `system` field. Per-turn instructions are
//! `PUT /api/experimental/session/{id}/instructions/entries/t3-code`. A prompt
//! carries `msg_t3_turn_{session}:{attempt}`. Accept-for-session writes a
//! session grant and replies `once`. A decline posts the declined note, then
//! rejects with [`DECLINED`]. A dropped stream reconnects five times, two
//! seconds apart, then backfills messages, permissions, and forms.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    PositiveInt, ProviderSessionId, RuntimeRequestId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ProviderFailureClass, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderSessionStatus,         OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadForkedFrom, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{ProviderApprovalDecision, ProviderInteractionMode, RuntimeMode},
};
use turin_provider_core::id_allocator::{IdAllocator, derive};

use crate::adapter::api::*;
use crate::adapter::logic::*;
use crate::error::EventStreamError;
use crate::events::V2StreamEvent;
use crate::permission_rules::{SessionPermissionRule, session_permission_rules};
use crate::v2_client::{MessageQuery, V2Client};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);

struct V2Turn {
    provider_turn: OrchestrationV2ProviderTurn,
    thread_id: ThreadId,
    run_ordinal: i64,
    interrupted: bool,
    finalized: bool,
    usage: UsageAccum,
    next_ordinal: u64,
    texts: HashMap<String, String>,
    before: Option<String>,
}

struct V2Thread {
    provider_thread: OrchestrationV2ProviderThread,
    directory: String,
    agent: String,
    model_key: String,
    runtime_mode: RuntimeMode,
    interaction: String,
    rules: Vec<SessionPermissionRule>,
    grants: Vec<SessionPermissionRule>,
    instructions: Option<String>,
    active: Option<V2Turn>,
    turns: Vec<OrchestrationV2ProviderTurn>,
    requests: Vec<OrchestrationV2RuntimeRequest>,
    unsettled: bool,
}

enum Ask {
    Permission {
        action: String,
        resources: Vec<String>,
        save: Vec<String>,
    },
    Form(NativeForm),
}

struct Pending {
    request_id: RuntimeRequestId,
    native_id: String,
    session_id: String,
    ask: Ask,
}

struct V2Inner {
    threads: HashMap<String, V2Thread>,
    pending: HashMap<String, Pending>,
    children: HashMap<String, String>,
    windows: HashMap<String, u64>,
    stream_dead: Option<String>,
}

pub struct OpenCodeV2Session {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    /// Open-time session record. Later status changes are
    /// `provider_session.updated` events.
    session: OrchestrationV2ProviderSession,
    client: V2Client,
    inner: Mutex<V2Inner>,
    tx: mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>>>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl OpenCodeV2Session {
    pub fn open(
        instance_id: ProviderInstanceId,
        provider_session_id: ProviderSessionId,
        cwd: &str,
        client: V2Client,
        model: Option<&str>,
    ) -> Result<Arc<Self>, ProviderAdapterV2Error> {
        let cwd = turin_contracts::TrimmedNonEmptyString::new(cwd)
            .map_err(|_| ProviderAdapterV2Error::protocol("OpenCode session cwd is blank"))?;
        let now = UtcDateTime::now();
        let (tx, rx) = mpsc::unbounded_channel();
        let provider_session = OrchestrationV2ProviderSession {
            id: provider_session_id.clone(),
            driver: opencode_driver(),
            provider_instance_id: instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd,
            model: model.and_then(|model| turin_contracts::TrimmedNonEmptyString::new(model).ok()),
            capabilities: v2_capabilities(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        let session = Arc::new(Self {
            instance_id,
            driver: opencode_driver(),
            provider_session_id,
            session: provider_session,
            client,
            inner: Mutex::new(V2Inner {
                threads: HashMap::new(),
                pending: HashMap::new(),
                children: HashMap::new(),
                windows: HashMap::new(),
                stream_dead: None,
            }),
            tx,
            rx: Mutex::new(Some(rx)),
            reader: Mutex::new(None),
        });
        let this = Arc::clone(&session);
        *session.reader.lock() = Some(tokio::spawn(async move {
            this.follow_events().await;
        }));
        Ok(session)
    }

    pub async fn close(&self) {
        if let Some(handle) = self.reader.lock().take() {
            handle.abort();
        }
    }

    async fn follow_events(self: Arc<Self>) {
        let mut failures = 0u32;
        loop {
            let stream = match self.client.events().await {
                Ok(stream) => stream,
                Err(err) => {
                    failures += 1;
                    if failures >= RECONNECT_ATTEMPTS {
                        self.fail_stream(&err.detail).await;
                        return;
                    }
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    continue;
                }
            };
            if failures > 0 {
                self.reconcile().await;
            }
            failures = 0;
            let mut stream = stream;
            loop {
                match stream.next().await {
                    Some(Ok(event)) => self.on_stream(event).await,
                    Some(Err(EventStreamError::Silent { .. })) | Some(Err(_)) | None => break,
                }
            }
            failures += 1;
            if failures >= RECONNECT_ATTEMPTS {
                self.reconcile().await;
                self.fail_stream("The OpenCode event stream was lost and could not reconnect.")
                    .await;
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    async fn reconcile(&self) {
        let active = self.client.session_active().await.unwrap_or_default();
        let sessions: Vec<String> = self.inner.lock().threads.keys().cloned().collect();
        for session_id in sessions {
            let prompt = {
                let guard = self.inner.lock();
                guard.threads.get(&session_id).and_then(|thread| {
                    thread.active.as_ref().and_then(|turn| {
                        prompt_of(&turn.provider_turn)
                            .map(str::to_owned)
                            .or(turn.before.clone())
                    })
                })
            };
            if let Some(start) = prompt.as_deref() {
                if let Ok(page) = self
                    .client
                    .list_messages(&MessageQuery {
                        session_id: session_id.clone(),
                        limit: Some(50),
                        order: Some("desc".to_owned()),
                        cursor: None,
                        message_type: None,
                    })
                    .await
                {
                    self.project_backfill(&session_id, start, &page.data);
                }
            }
            let still = active.contains_key(&session_id);
            if !still {
                let mut guard = self.inner.lock();
                let outcome = guard.threads.get(&session_id).and_then(|thread| {
                    thread.active.as_ref().map(|turn| turn.interrupted)
                });
                if outcome == Some(true) {
                    self.finish(&mut guard, &session_id, ProviderAdapterV2TurnTerminalStatus::Interrupted, None);
                } else if outcome == Some(false) {
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                        Some("OpenCode did not report an idle item for the lost execution.".to_owned()),
                    );
                }
            }
            if still {
                if let Ok(permissions) = self.client.list_permissions(&session_id).await {
                    for request in permissions {
                        self.show_permission(&request);
                    }
                }
                if let Ok(forms) = self.client.list_forms(&session_id).await {
                    for form in forms {
                        self.show_form(&form);
                    }
                }
            }
        }
    }

    fn project_backfill(&self, session_id: &str, start: &str, messages: &[Value]) {
        let mut recent = Vec::new();
        let mut found = false;
        for message in messages {
            let id = message.get("id").and_then(Value::as_str).unwrap_or("");
            if id == start {
                found = true;
                break;
            }
            recent.push(message);
        }
        if !found && !messages.is_empty() {
            return;
        }
        let mut idle_outcome = None;
        for message in recent.iter().rev() {
            if message.get("type").and_then(Value::as_str) == Some("idle") {
                idle_outcome = message.get("outcome").and_then(Value::as_str).map(str::to_owned);
            }
            if message.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let content = message.get("content").and_then(Value::as_array);
            let Some(content) = content else { continue };
            for part in content {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                if part_type == "text" || part_type == "reasoning" {
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    let id = part.get("id").and_then(Value::as_str).unwrap_or("part");
                    self.emit_text(session_id, id, text, part_type == "reasoning", false);
                } else if part_type == "tool" {
                    let id = part.get("id").and_then(Value::as_str).unwrap_or("tool");
                    let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let state = part.get("state").cloned().unwrap_or(Value::Null);
                    self.emit_tool(session_id, id, name, &state);
                }
            }
        }
        if let Some(outcome) = idle_outcome {
            let mut guard = self.inner.lock();
            let status = match outcome.as_str() {
                "succeeded" => ProviderAdapterV2TurnTerminalStatus::Completed,
                "failed" => ProviderAdapterV2TurnTerminalStatus::Failed,
                _ => ProviderAdapterV2TurnTerminalStatus::Interrupted,
            };
            self.finish(&mut guard, session_id, status, None);
        }
    }

    async fn fail_stream(&self, message: &str) {
        let mut guard = self.inner.lock();
        guard.stream_dead = Some(message.to_owned());
        let mut session = self.session.clone();
        session.status = OrchestrationV2ProviderSessionStatus::Error;
        session.last_error = Some(message.to_owned());
        session.updated_at = UtcDateTime::now();
        let ids: Vec<String> = guard.threads.keys().cloned().collect();
        let mut terminals = Vec::new();
        for id in ids {
            if guard.threads.get(&id).and_then(|thread| thread.active.as_ref()).is_some() {
                terminals.push(id);
            }
        }
        drop(guard);
        self.emit(ProviderAdapterV2Event::ProviderSessionUpdated {
            driver: self.driver.clone(),
            provider_session: session,
        });
        for id in terminals {
            let mut guard = self.inner.lock();
            self.finish(
                &mut guard,
                &id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(message.to_owned()),
            );
        }
    }

    fn emit(&self, event: ProviderAdapterV2Event) {
        let _ = self.tx.send(Ok(event));
    }

    async fn on_stream(&self, event: V2StreamEvent) {
        match event {
            V2StreamEvent::UnreadableExecutionEnded { session_id, execution_type } => {
                let mut guard = self.inner.lock();
                let status = if execution_type.contains("interrupt") {
                    ProviderAdapterV2TurnTerminalStatus::Interrupted
                } else if execution_type.contains("fail") {
                    ProviderAdapterV2TurnTerminalStatus::Failed
                } else {
                    ProviderAdapterV2TurnTerminalStatus::Completed
                };
                self.finish(&mut guard, &session_id, status, None);
            }
            V2StreamEvent::UnreadableExecutionStarted { .. } => {}
            V2StreamEvent::Event(event) => self.on_event(&event.event_type, &event.data),
        }
    }

    fn on_event(&self, event_type: &str, data: &Value) {
        let session_id = str_field(data, "sessionID");
        match event_type {
            "session.created" => {
                let parent = str_field(data, "parentID");
                let id = str_field(data, "id");
                if let (Some(id), Some(parent)) = (id, parent) {
                    self.link_child(&id, &parent);
                }
            }
            "session.execution.succeeded" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    let interrupted = guard
                        .threads
                        .get(&session_id)
                        .and_then(|thread| thread.active.as_ref())
                        .is_some_and(|turn| turn.interrupted);
                    let status = if interrupted {
                        ProviderAdapterV2TurnTerminalStatus::Interrupted
                    } else {
                        ProviderAdapterV2TurnTerminalStatus::Completed
                    };
                    self.finish(&mut guard, &session_id, status, None);
                }
            }
            "session.execution.failed" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Failed,
                        data.get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    );
                }
            }
            "session.execution.interrupted" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                        None,
                    );
                }
            }
            "session.step.ended" => {
                if let Some(session_id) = session_id {
                    if let Some(tokens) = parse_step_tokens(data) {
                        let mut guard = self.inner.lock();
                        if let Some(turn) = guard
                            .threads
                            .get_mut(&session_id)
                            .and_then(|thread| thread.active.as_mut())
                        {
                            let id = str_field(data, "id").unwrap_or_else(|| "step".to_owned());
                            turn.usage.add_step(&id, tokens);
                        }
                    }
                }
            }
            "session.text.delta" | "session.text.ended" | "session.reasoning.delta" | "session.reasoning.ended" => {
                if let Some(session_id) = session_id {
                    let reasoning = event_type.contains("reasoning");
                    let id = str_field(data, "id")
                        .or_else(|| str_field(data, "partID"))
                        .unwrap_or_else(|| "text".to_owned());
                    let streaming = event_type.ends_with("delta");
                    let text = if streaming {
                        data.get("delta").and_then(Value::as_str).unwrap_or("")
                    } else {
                        data.get("text").and_then(Value::as_str).unwrap_or("")
                    };
                    if streaming {
                        let mut guard = self.inner.lock();
                        if let Some(turn) = guard
                            .threads
                            .get_mut(&session_id)
                            .and_then(|thread| thread.active.as_mut())
                        {
                            let slot = turn.texts.entry(id.clone()).or_default();
                            slot.push_str(text);
                            let owned = slot.clone();
                            drop(guard);
                            self.emit_text(&session_id, &id, &owned, reasoning, true);
                            return;
                        }
                    }
                    self.emit_text(&session_id, &id, text, reasoning, streaming);
                }
            }
            "session.tool.input.started"
            | "session.tool.called"
            | "session.tool.progress"
            | "session.tool.success"
            | "session.tool.failed" => {
                if event_type.contains("question") {
                    return;
                }
                if let Some(session_id) = session_id {
                    let name = data
                        .get("name")
                        .or_else(|| data.get("tool"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    if name == "subagent" || name == "task" {
                        if let Some(child) = data
                            .pointer("/state/metadata/sessionID")
                            .or_else(|| data.get("sessionID"))
                            .and_then(Value::as_str)
                        {
                            if child != session_id {
                                self.link_child(child, &session_id);
                            }
                        }
                    }
                    let id = str_field(data, "id")
                        .or_else(|| str_field(data, "callID"))
                        .unwrap_or_else(|| name.to_owned());
                    self.emit_tool(&session_id, &id, name, data);
                }
            }
            "permission.asked" => self.show_permission(data),
            "form.created" => {
                let form = data.get("form").cloned().unwrap_or_else(|| data.clone());
                self.show_form(&form);
            }
            _ => {}
        }
    }

    fn link_child(&self, child: &str, parent: &str) {
        let mut guard = self.inner.lock();
        if !guard.threads.contains_key(parent) {
            return;
        }
        guard.children.insert(child.to_owned(), parent.to_owned());
        let subagent = guard.threads.get(parent).and_then(|thread| {
            let turn = thread.active.as_ref()?;
            Some(subagent_for(
                &self.instance_id,
                &thread.provider_thread,
                &turn.thread_id,
                &turn.provider_turn,
                child,
            ))
        });
        drop(guard);
        if let Some(subagent) = subagent {
            self.emit(ProviderAdapterV2Event::SubagentUpdated {
                driver: self.driver.clone(),
                subagent,
            });
        }
    }

    fn emit_text(&self, session_id: &str, part_id: &str, text: &str, reasoning: bool, streaming: bool) {
        let mut guard = self.inner.lock();
        let Some(turn) = guard.threads.get_mut(session_id).and_then(|thread| thread.active.as_mut()) else {
            return;
        };
        if !streaming {
            turn.texts.insert(part_id.to_owned(), text.to_owned());
        }
        let item = text_item(turn, &self.driver, part_id, text, reasoning, streaming);
        drop(guard);
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    fn emit_tool(&self, session_id: &str, call_id: &str, name: &str, state: &Value) {
        let mut guard = self.inner.lock();
        let Some(turn) = guard.threads.get_mut(session_id).and_then(|thread| thread.active.as_mut()) else {
            return;
        };
        let status_name = state
            .get("status")
            .or_else(|| state.pointer("/state/status"))
            .and_then(Value::as_str)
            .unwrap_or("running");
        let status = match status_name {
            "completed" | "success" => OrchestrationV2TurnItemStatus::Completed,
            "error" | "failed" => OrchestrationV2TurnItemStatus::Failed,
            _ => OrchestrationV2TurnItemStatus::Running,
        };
        if name == "task" || name == "subagent" {
            turn.usage.has_subagents = true;
        }
        let input = state.get("input").cloned().unwrap_or_else(|| json!({}));
        let output = state
            .get("output")
            .or_else(|| state.pointer("/state/output"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let shown = output.as_deref().map(|text| {
            if name == "task" || name == "subagent" {
                subagent_output(text)
            } else {
                text.to_owned()
            }
        });
        let base = tool_base(turn, &self.driver, call_id, status);
        let item = open_code_tool_turn_item(base, name, &input, shown.as_deref(), state.get("metadata"));
        drop(guard);
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    fn show_permission(&self, data: &Value) {
        let native_id = str_field(data, "id").unwrap_or_default();
        let session_id = str_field(data, "sessionID").unwrap_or_default();
        let action = str_field(data, "action")
            .or_else(|| str_field(data, "permission"))
            .unwrap_or_else(|| "bash".to_owned());
        let resources = data
            .get("resources")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let save = data
            .get("save")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        self.show_ask(
            &session_id,
            &native_id,
            Ask::Permission { action, resources, save },
            open_code_permission_request_kind(
                data.get("action").and_then(Value::as_str).unwrap_or("bash"),
                None,
            ),
        );
    }

    fn show_form(&self, data: &Value) {
        let Ok(form) = serde_json::from_value::<NativeForm>(data.clone()) else {
            return;
        };
        let session_id = str_field(data, "sessionID").unwrap_or_default();
        let native_id = form.id.clone();
        if form_questions(&form).is_err() {
            let session_id = session_id.clone();
            let form_id = form.id.clone();
            let client = self.client.clone();
            tokio::spawn(async move {
                let _ = client.cancel_form(&session_id, &form_id, None).await;
            });
            return;
        }
        self.show_ask(&session_id, &native_id, Ask::Form(form), "user_input");
    }

    fn show_ask(&self, session_id: &str, native_id: &str, ask: Ask, kind: &str) {
        let mut guard = self.inner.lock();
        if guard.pending.contains_key(native_id) {
            return;
        }
        let owner = guard
            .children
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| session_id.to_owned());
        let Some((provider_turn_id, thread_id)) = guard.threads.get(&owner).and_then(|thread| {
            thread.active.as_ref().map(|turn| (turn.provider_turn.id.clone(), turn.thread_id.clone()))
        }) else {
            return;
        };
        let request_id = IdAllocator.runtime_request(&self.driver, Some(&provider_turn_id), Some(native_id));
        let now = UtcDateTime::now();
        let request = OrchestrationV2RuntimeRequest {
            id: request_id.clone(),
            node_id: derive::node_from_provider_item(&self.driver, native_id),
            provider_turn_id: Some(provider_turn_id),
            native_request_ref: Some(native_ref(native_id, false)),
            kind: match kind {
                "file-read" => OrchestrationV2RuntimeRequestKind::FileRead,
                "file-change" => OrchestrationV2RuntimeRequestKind::FileChange,
                "user_input" => OrchestrationV2RuntimeRequestKind::UserInput,
                _ => OrchestrationV2RuntimeRequestKind::Command,
            },
            status: OrchestrationV2RuntimeRequestStatus::Pending,
            response_capability: OrchestrationV2ResponseCapability::Live {
                provider_session_id: self.provider_session_id.clone(),
            },
            created_at: now,
            resolved_at: None,
            decision: None,
            answers: None,
        };
        if let Some(thread) = guard.threads.get_mut(&owner) {
            thread.requests.push(request.clone());
        }
        guard.pending.insert(
            native_id.to_owned(),
            Pending {
                request_id,
                native_id: native_id.to_owned(),
                session_id: session_id.to_owned(),
                ask,
            },
        );
        drop(guard);
        self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: self.driver.clone(),
            thread_id: Some(thread_id),
            runtime_request: request,
        });
    }

    fn finish(
        &self,
        guard: &mut V2Inner,
        session_id: &str,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<String>,
    ) {
        let Some(thread) = guard.threads.get_mut(session_id) else {
            return;
        };
        let Some(mut turn) = thread.active.take() else {
            return;
        };
        if turn.finalized {
            thread.active = Some(turn);
            return;
        }
        turn.finalized = true;
        thread.unsettled = false;
        turn.provider_turn.status = match status {
            ProviderAdapterV2TurnTerminalStatus::Completed => OrchestrationV2ProviderTurnStatus::Completed,
            ProviderAdapterV2TurnTerminalStatus::Interrupted => OrchestrationV2ProviderTurnStatus::Interrupted,
            ProviderAdapterV2TurnTerminalStatus::Cancelled => OrchestrationV2ProviderTurnStatus::Cancelled,
            ProviderAdapterV2TurnTerminalStatus::Failed => OrchestrationV2ProviderTurnStatus::Failed,
        };
        turn.provider_turn.completed_at = Some(UtcDateTime::now());
        turn.provider_turn.turn_token_usage = Some(
            turn.usage.to_turn_usage(status == ProviderAdapterV2TurnTerminalStatus::Completed),
        );
        let provider_turn = turn.provider_turn.clone();
        let thread_id = turn.thread_id.clone();
        let run_ordinal = turn.run_ordinal.max(1) as u64;
        if let Some(stored) = thread.turns.iter_mut().find(|item| item.id == provider_turn.id) {
            *stored = provider_turn.clone();
        }
        thread.provider_thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.provider_thread.updated_at = UtcDateTime::now();
        let provider_thread = thread.provider_thread.clone();
        let driver = self.driver.clone();
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: driver.clone(),
            thread_id: Some(thread_id),
            provider_turn: provider_turn.clone(),
        });
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver.clone(),
            provider_thread,
            retained_native_turn_ids: None,
        });
        self.emit(ProviderAdapterV2Event::TurnTerminal {
            driver,
            provider_thread_id: provider_turn.provider_thread_id.clone(),
            provider_turn_id: provider_turn.id,
            run_ordinal: PositiveInt::new(run_ordinal).expect("run ordinal"),
            status,
            failure: failure.map(|message| {
                provider_failure(OrchestrationV2ProviderFailureClass::ProviderError, &message)
            }),
            failure_item_ordinal: None,
            retry: None,
            retry_started_at: None,
            thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
        });
    }

    fn require_session(guard: &V2Inner, thread: &OrchestrationV2ProviderThread) -> Result<String, ProviderAdapterV2Error> {
        thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .filter(|id| guard.threads.contains_key(id))
            .ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "Provider thread {} has no OpenCode session id",
                    thread.id
                ))
            })
    }

    async fn remember_windows(&self, directory: &str) {
        let Ok(located) = self.client.list_models(directory).await else {
            return;
        };
        let mut windows = HashMap::new();
        for model in located.data {
            let slug = model
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| model.get("slug").and_then(Value::as_str));
            if let (Some(slug), Some(window)) = (slug, context_window_of(&model)) {
                windows.insert(slug.to_owned(), window);
            }
        }
        self.inner.lock().windows.extend(windows);
    }
}

#[async_trait::async_trait]
impl ProviderSessionRuntime for OpenCodeV2Session {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.provider_session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }
    fn events(&self) -> ProviderEventStream {
        match self.rx.lock().take() {
            Some(rx) => Box::pin(UnboundedReceiverStream::new(rx)),
            None => Box::pin(futures::stream::empty()),
        }
    }
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let pending = self.inner.lock().threads.values().any(|thread| thread.active.is_some());
        Some(Box::pin(async move { Ok(pending) }))
    }
    fn get_model_context_window(
        &self,
        model_selection: &turin_contracts::model_selection::ModelSelection,
        _cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        Some(self.inner.lock().windows.get(model_selection.model.as_str()).copied())
    }

    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        if input.existing_provider_thread.as_ref().and_then(|thread| thread.native_thread_ref.as_ref()).is_some()
        {
            return self
                .resume_thread(ProviderAdapterV2ResumeThreadInput {
                    provider_thread: input.existing_provider_thread.expect("checked"),
                    thread_id: Some(input.thread_id),
                    model_selection: Some(input.model_selection),
                    runtime_policy: Some(input.runtime_policy),
                })
                .await;
        }
        let directory = input.runtime_policy.cwd.clone().unwrap_or_else(|| {
            self.session.cwd.as_str().to_owned()
        });
        self.remember_windows(&directory).await;
        let parsed = parse_open_code_model_slug(input.model_selection.model.as_str()).ok_or_else(|| {
            ProviderAdapterV2Error::protocol(malformed_model(input.model_selection.model.as_str()))
        })?;
        let rules = rules_for(&input.runtime_policy, &[]);
        let mut body = json!({
            "location": { "directory": directory },
            "model": { "providerID": parsed.provider_id, "modelID": parsed.model_id },
            "permissions": rules,
        });
        if let Some(title) = input.title.as_deref().map(str::trim).filter(|title| !title.is_empty()) {
            body["title"] = json!(title);
        }
        let created = self.client.create_session(&body).await.map_err(http_err)?;
        let native_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode session.create returned no id"))?
            .to_owned();
        let now = UtcDateTime::now();
        let provider_thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &input.thread_id,
            &native_id,
            input.existing_provider_thread,
            now,
        );
        self.inner.lock().threads.insert(
            native_id,
            V2Thread {
                provider_thread: provider_thread.clone(),
                directory,
                agent: "build".to_owned(),
                model_key: format!("{}/{}", parsed.provider_id, parsed.model_id),
                runtime_mode: input.runtime_policy.runtime_mode,
                interaction: interaction_name(&input.runtime_policy).to_owned(),
                rules,
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            },
        );
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: self.driver.clone(),
            provider_thread: provider_thread.clone(),
            retained_native_turn_ids: None,
        });
        Ok(provider_thread)
    }

    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "Provider thread {} has no OpenCode session id",
                    input.provider_thread.id
                ))
            })?;
        let native = self.client.get_session(&native_id).await.map_err(http_err)?;
        let directory = input
            .runtime_policy
            .as_ref()
            .and_then(|policy| policy.cwd.clone())
            .or_else(|| native.pointer("/location/directory").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| self.session.cwd.as_str().to_owned());
        let mut thread = input.provider_thread;
        thread.provider_session_id = Some(self.provider_session_id.clone());
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.updated_at = UtcDateTime::now();
        let policy = input.runtime_policy.clone().unwrap_or(ProviderAdapterV2RuntimePolicy {
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            cwd: Some(directory.clone()),
            approval_policy: None,
            sandbox_policy: None,
            reasoning_effort: None,
        });
        let rules = rules_for(&policy, &[]);
        self.client
            .update_session(&native_id, &json!({ "permissions": rules }))
            .await
            .map_err(http_err)?;
        if native.pointer("/location/directory").and_then(Value::as_str) != Some(directory.as_str()) {
            let _ = self
                .client
                .move_session(&native_id, &json!({ "directory": directory }))
                .await;
        }
        self.inner.lock().threads.insert(
            native_id,
            V2Thread {
                provider_thread: thread.clone(),
                directory,
                agent: native.get("agent").and_then(Value::as_str).unwrap_or("build").to_owned(),
                model_key: String::new(),
                runtime_mode: policy.runtime_mode,
                interaction: interaction_name(&policy).to_owned(),
                rules,
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            },
        );
        Ok(thread)
    }

    async fn start_turn(&self, input: ProviderAdapterV2TurnInput) -> Result<(), ProviderAdapterV2Error> {
        if self.inner.lock().stream_dead.is_some() {
            return Err(ProviderAdapterV2Error::protocol(
                "OpenCode event stream has ended; reconnect the provider session before starting another turn.",
            ));
        }
        let session_id = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            if guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()).is_some() {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode session {session_id} already has an active turn"
                )));
            }
            session_id
        };
        let parsed = parse_open_code_model_slug(input.model_selection.model.as_str()).ok_or_else(|| {
            ProviderAdapterV2Error::protocol(malformed_model(input.model_selection.model.as_str()))
        })?;
        let still_stopping = {
            let unsettled = self
                .inner
                .lock()
                .threads
                .get(&session_id)
                .is_some_and(|thread| thread.unsettled);
            if unsettled {
                let active = self.client.session_active().await.map_err(http_err)?;
                if active.contains_key(&session_id) {
                    let _ = self.client.interrupt(&session_id, None).await;
                    true
                } else {
                    self.inner.lock().threads.get_mut(&session_id).map(|thread| thread.unsettled = false);
                    false
                }
            } else {
                false
            }
        };
        let agent = if input.runtime_policy.interaction_mode == ProviderInteractionMode::Plan {
            "plan"
        } else {
            "build"
        };
        let model_key = format!("{}/{}", parsed.provider_id, parsed.model_id);
        let variant = model_option(&input.model_selection, "variant");
        let switch_agent = {
            let mut guard = self.inner.lock();
            let thread = guard.threads.get_mut(&session_id).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!("OpenCode session {session_id} is not registered"))
            })?;
            if thread.agent != agent {
                thread.agent = agent.to_owned();
                true
            } else {
                false
            }
        };
        if switch_agent {
            self.client
                .switch_agent(&session_id, &json!({ "agent": agent }))
                .await
                .map_err(http_err)?;
        }
        let rules = {
            let mut guard = self.inner.lock();
            if let Some(thread) = guard.threads.get_mut(&session_id) {
                thread.runtime_mode = input.runtime_policy.runtime_mode;
                thread.interaction = interaction_name(&input.runtime_policy).to_owned();
                let rules = rules_for(&input.runtime_policy, &thread.grants);
                thread.rules = rules.clone();
                rules
            } else {
                rules_for(&input.runtime_policy, &[])
            }
        };
        self.client
            .update_session(&session_id, &json!({ "permissions": rules }))
            .await
            .map_err(http_err)?;
        let switch_model = {
            let mut guard = self.inner.lock();
            guard.threads.get_mut(&session_id).and_then(|thread| {
                let next_key = match &variant {
                    Some(variant) => format!("{model_key}#{variant}"),
                    None => model_key.clone(),
                };
                if thread.model_key != next_key {
                    thread.model_key = next_key;
                    Some(())
                } else {
                    None
                }
            })
        };
        if switch_model.is_some() {
            let mut body = json!({
                "providerID": parsed.provider_id,
                "modelID": parsed.model_id,
            });
            if let Some(variant) = &variant {
                body["variant"] = json!(variant);
            }
            self.client.switch_model(&session_id, &body).await.map_err(http_err)?;
        }
        let instructions = per_turn_instructions(input.model_selection.model.as_str());
        let changed = self
            .inner
            .lock()
            .threads
            .get(&session_id)
            .is_none_or(|thread| thread.instructions.as_deref() != Some(instructions.as_str()));
        if changed {
            self.client
                .put_instruction(&session_id, INSTRUCTIONS_KEY, &instructions)
                .await
                .map_err(http_err)?;
            if let Some(thread) = self.inner.lock().threads.get_mut(&session_id) {
                thread.instructions = Some(instructions);
            }
        }
        let prompt_id = turn_prompt_id(&session_id, input.attempt_id.as_str());
        let now = UtcDateTime::now();
        let provider_turn = OrchestrationV2ProviderTurn {
            id: derive::provider_turn(&self.driver, &prompt_id),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(native_ref(&prompt_id, true)),
            ordinal: PositiveInt::new(input.provider_turn_ordinal.max(1) as u64)
                .unwrap_or(PositiveInt::new(1).expect("one")),
            status: OrchestrationV2ProviderTurnStatus::Running,
            started_at: Some(now),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        };
        {
            let mut guard = self.inner.lock();
            let thread = guard.threads.get_mut(&session_id).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!("OpenCode session {session_id} is not registered"))
            })?;
            thread.turns.push(provider_turn.clone());
            thread.provider_thread.status = OrchestrationV2ProviderThreadStatus::Active;
            thread.active = Some(V2Turn {
                provider_turn: provider_turn.clone(),
                thread_id: input.thread_id.clone(),
                run_ordinal: input.run_ordinal,
                interrupted: false,
                finalized: false,
                usage: UsageAccum::new(),
                next_ordinal: input.provider_turn_ordinal.max(0) as u64 * 100 + 1,
                texts: HashMap::new(),
                before: None,
            });
        }
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn,
        });
        if still_stopping {
            let mut guard = self.inner.lock();
            self.finish(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(OPENCODE_2_STILL_STOPPING.to_owned()),
            );
            return Err(ProviderAdapterV2Error::protocol(OPENCODE_2_STILL_STOPPING));
        }
        let text = turin_provider_core::attachment_prompt::provider_message_text_with_attachment_paths(
            &input.message.text,
            &input.message.attachments,
            |_| None,
        );
        let result = self.submit(&session_id, &input, &text, &prompt_id).await;
        if let Err(err) = result {
            let mut guard = self.inner.lock();
            if let Some(thread) = guard.threads.get_mut(&session_id) {
                thread.unsettled = true;
            }
            self.finish(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(err.to_string()),
            );
            return Err(err);
        }
        Ok(())
    }

    fn compact_thread(
        &self,
        mut input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        input.message.text = "/compact".to_owned();
        Some(Box::pin(async move { self.start_turn(input).await }))
    }

    async fn steer_turn(&self, input: ProviderAdapterV2SteerInput) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            Self::require_session(&guard, &input.provider_thread)?
        };
        let text = input.message.text.trim().to_owned();
        if text.is_empty() {
            return Err(ProviderAdapterV2Error::protocol(
                "OpenCode steering requires text or an attachment",
            ));
        }
        let id = steer_prompt_id(&session_id, input.message.message_id.as_str());
        self.client
            .prompt(
                &session_id,
                &json!({ "id": id, "text": text, "delivery": "steer" }),
            )
            .await
            .map(|_| ())
            .map_err(http_err)
    }

    async fn interrupt_turn(&self, input: ProviderAdapterV2InterruptInput) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            if let Some(turn) = guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()) {
                if turn.provider_turn.id != input.provider_turn_id {
                    return Ok(());
                }
            }
            session_id
        };
        if let Some(turn) = self.inner.lock().threads.get_mut(&session_id).and_then(|thread| thread.active.as_mut())
        {
            turn.interrupted = true;
        }
        let reply = self.client.interrupt(&session_id, None).await.map_err(http_err)?;
        if !reply.interrupted {
            let mut guard = self.inner.lock();
            if let Some(turn) = guard.threads.get_mut(&session_id).and_then(|thread| thread.active.as_mut()) {
                if !turn.finalized {
                    turn.interrupted = false;
                    return Err(ProviderAdapterV2Error::protocol(format!(
                        "OpenCode session {session_id} had nothing running to stop"
                    )));
                }
            }
        }
        Ok(())
    }

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let pending = {
            let mut guard = self.inner.lock();
            let key = guard
                .pending
                .iter()
                .find(|(_, pending)| pending.request_id == input.request_id)
                .map(|(key, _)| key.clone());
            let Some(key) = key else {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "No pending OpenCode request {}",
                    input.request_id
                )));
            };
            guard.pending.remove(&key).expect("just found")
        };
        match pending.ask {
            Ask::Permission { action, resources, save } => {
                let decision = input.decision.ok_or_else(|| {
                    ProviderAdapterV2Error::protocol(format!(
                        "OpenCode approval request {} requires a decision",
                        input.request_id
                    ))
                })?;
                if matches!(
                    decision,
                    ProviderApprovalDecision::AcceptForSession | ProviderApprovalDecision::AcceptAlways
                ) {
                    let patterns = if save.is_empty() { vec!["*".to_owned()] } else { save.clone() };
                    let rules = {
                        let mut guard = self.inner.lock();
                        guard.threads.get_mut(&pending.session_id).map(|thread| {
                            for resource in &patterns {
                                let grant = SessionPermissionRule {
                                    action: action.clone(),
                                    resource: resource.clone(),
                                    effect: "allow".to_owned(),
                                };
                                if !thread.grants.iter().any(|existing| {
                                    existing.action == grant.action && existing.resource == grant.resource
                                }) {
                                    thread.grants.push(grant);
                                }
                            }
                            let rules = session_permission_rules(
                                mode_name(thread.runtime_mode),
                                &thread.interaction,
                                &thread.grants,
                                &[],
                            );
                            thread.rules = rules.clone();
                            rules
                        })
                    };
                    if let Some(rules) = rules {
                        let _ = self
                            .client
                            .update_session(&pending.session_id, &json!({ "permissions": rules }))
                            .await;
                    }
                }
                if decision == ProviderApprovalDecision::Decline {
                    let note = declined_note(&action, &resources);
                    let _ = self
                        .client
                        .synthetic(
                            &pending.session_id,
                            &json!({ "text": note, "delivery": "steer", "resume": false }),
                        )
                        .await;
                }
                let mut body = json!({ "decision": v2_permission_decision(decision) });
                if decision == ProviderApprovalDecision::Decline {
                    body["message"] = json!(DECLINED);
                }
                self.client
                    .reply_permission(&pending.session_id, &pending.native_id, &body)
                    .await
                    .map_err(http_err)?;
            }
            Ask::Form(form) => {
                let decline = matches!(
                    input.decision,
                    Some(ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel)
                );
                if input.answers.is_none() || decline {
                    self.client
                        .cancel_form(&pending.session_id, &form.id, None)
                        .await
                        .map_err(http_err)?;
                } else {
                    let answers = input.answers.unwrap_or_default();
                    let map = answers.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
                    let answer = form_answer(&form, &map);
                    self.client
                        .reply_form(&pending.session_id, &form.id, &answer)
                        .await
                        .map_err(http_err)?;
                }
            }
        }
        Ok(())
    }

    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let session_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode snapshot has no session"))?;
        let page = self
            .client
            .list_messages(&MessageQuery::new(&session_id))
            .await
            .map_err(http_err)?;
        let guard = self.inner.lock();
        let stored = guard.threads.get(&session_id);
        Ok(ProviderAdapterV2ThreadSnapshot {
            provider_thread: input.provider_thread,
            provider_turns: stored.map(|thread| thread.turns.clone()).unwrap_or_default(),
            messages: messages_of(&page.data),
            runtime_requests: stored.map(|thread| thread.requests.clone()).unwrap_or_default(),
            provider_payload: Some(Value::Array(page.data)),
        })
    }

    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let session_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode rollback has no session"))?;
        {
            let guard = self.inner.lock();
            if guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()).is_some() {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "Cannot roll back OpenCode thread {} while a turn is active",
                    input.provider_thread.id
                )));
            }
        }
        let kept = match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => None,
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => Some(provider_turn.ordinal.get()),
        };
        let prompts = user_prompt_ids(&input.provider_thread_turns);
        let boundary = boundary_after(&input.provider_thread_turns, &input.provider_thread.id, kept, &prompts)
            .map_err(ProviderAdapterV2Error::protocol)?;
        if let Some(boundary) = boundary {
            if let Err(err) = self
                .client
                .revert_stage(&session_id, &json!({ "messageID": boundary, "files": false }))
                .await
            {
                let _ = self.client.revert_clear(&session_id).await;
                return Err(http_err(err));
            }
            if let Err(err) = self.client.revert_commit(&session_id).await {
                let _ = self.client.revert_clear(&session_id).await;
                return Err(http_err(err));
            }
        }
        self.read_thread_snapshot(ProviderAdapterV2ReadThreadSnapshotInput {
            provider_thread: input.provider_thread,
        })
        .await
    }

    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let session_id = input
            .source_provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode fork has no session"))?;
        let turns = input.source_provider_turns.clone().unwrap_or_default();
        let before = if let Some(turn_id) = &input.provider_turn_id {
            let selected = turns.iter().find(|turn| &turn.id == turn_id).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "OpenCode fork boundary turn {turn_id} was not found"
                ))
            })?;
            let prompts = user_prompt_ids(&turns);
            boundary_after(&turns, &input.source_provider_thread.id, Some(selected.ordinal.get()), &prompts)
                .map_err(ProviderAdapterV2Error::protocol)?
        } else {
            None
        };
        let body = match &before {
            Some(before) => json!({ "before": before }),
            None => json!({}),
        };
        let forked = self.client.fork_session(&session_id, &body).await.map_err(http_err)?;
        let fork_id = forked
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode fork returned no id"))?;
        let now = UtcDateTime::now();
        let mut thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &input.target_thread_id,
            fork_id,
            None,
            now,
        );
        thread.forked_from = Some(OrchestrationV2ProviderThreadForkedFrom {
            provider_thread_id: input.source_provider_thread.id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            checkpoint_id: None,
        });
        let directory = input
            .runtime_policy
            .as_ref()
            .and_then(|policy| policy.cwd.clone())
            .unwrap_or_else(|| self.session.cwd.as_str().to_owned());
        self.inner.lock().threads.insert(
            fork_id.to_owned(),
            V2Thread {
                provider_thread: thread.clone(),
                directory,
                agent: "build".to_owned(),
                model_key: String::new(),
                runtime_mode: input
                    .runtime_policy
                    .as_ref()
                    .map(|policy| policy.runtime_mode)
                    .unwrap_or(RuntimeMode::FullAccess),
                interaction: input
                    .runtime_policy
                    .as_ref()
                    .map(interaction_name)
                    .unwrap_or("default")
                    .to_owned(),
                rules: Vec::new(),
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            },
        );
        Ok(thread)
    }
}

impl OpenCodeV2Session {
    async fn submit(
        &self,
        session_id: &str,
        input: &ProviderAdapterV2TurnInput,
        text: &str,
        prompt_id: &str,
    ) -> Result<(), ProviderAdapterV2Error> {
        let bare = input.message.attachments.is_empty();
        if bare && text.trim() == "/compact" {
            return self
                .client
                .compact(session_id, &json!({ "id": prompt_id }))
                .await
                .map(|_| ())
                .map_err(http_err);
        }
        if bare {
            if let Some(command) = command_of(text.trim()) {
                let directory = self
                    .inner
                    .lock()
                    .threads
                    .get(session_id)
                    .map(|thread| thread.directory.clone())
                    .unwrap_or_default();
                let commands = self.client.list_commands(&directory).await.map(|located| located.data).unwrap_or_default();
                if commands.iter().any(|entry| entry.get("name").and_then(Value::as_str) == Some(command.name.as_str()))
                {
                    let newest = self
                        .client
                        .list_messages(&MessageQuery {
                            session_id: session_id.to_owned(),
                            limit: Some(1),
                            order: Some("desc".to_owned()),
                            cursor: None,
                            message_type: None,
                        })
                        .await
                        .ok()
                        .and_then(|page| page.data.first().and_then(|message| message.get("id").and_then(Value::as_str).map(str::to_owned)));
                    if let Some(turn) = self.inner.lock().threads.get_mut(session_id).and_then(|thread| thread.active.as_mut())
                    {
                        turn.before = Some(newest.unwrap_or_default());
                    }
                    return self
                        .client
                        .command(session_id, &json!({ "command": command.name, "arguments": command.text }))
                        .await
                        .map_err(http_err);
                }
            }
        }
        let mut skills = Vec::new();
        if text.contains('$') {
            let directory = self
                .inner
                .lock()
                .threads
                .get(session_id)
                .map(|thread| thread.directory.clone())
                .unwrap_or_default();
            let known = self
                .client
                .list_skills(&directory)
                .await
                .map(|located| {
                    located
                        .data
                        .iter()
                        .filter_map(|skill| skill.get("id").or_else(|| skill.get("name")).and_then(Value::as_str).map(str::to_owned))
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default();
            skills = skills_named(text, &known);
        }
        let mut body = json!({ "id": prompt_id, "text": text.trim() });
        if !skills.is_empty() {
            body["skills"] = json!(skills.into_iter().map(|id| json!({ "id": id })).collect::<Vec<_>>());
        }
        self.client.prompt(session_id, &body).await.map(|_| ()).map_err(http_err)
    }
}

impl Drop for OpenCodeV2Session {
    fn drop(&mut self) {
        if let Some(handle) = self.reader.lock().take() {
            handle.abort();
        }
    }
}

fn http_err(err: crate::error::OpenCodeRuntimeError) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::protocol(err.detail)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn interaction_name(policy: &ProviderAdapterV2RuntimePolicy) -> &'static str {
    if policy.interaction_mode == ProviderInteractionMode::Plan {
        "plan"
    } else {
        "default"
    }
}

fn mode_name(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}

fn rules_for(policy: &ProviderAdapterV2RuntimePolicy, grants: &[SessionPermissionRule]) -> Vec<SessionPermissionRule> {
    let interaction = if policy.interaction_mode == ProviderInteractionMode::Plan {
        "plan"
    } else {
        "default"
    };
    session_permission_rules(mode_name(policy.runtime_mode), interaction, grants, &[])
}

fn user_prompt_ids(turns: &[OrchestrationV2ProviderTurn]) -> HashSet<String> {
    turns.iter().filter_map(|turn| prompt_of(turn).map(str::to_owned)).collect()
}

fn make_thread(
    instance_id: &ProviderInstanceId,
    session_id: &ProviderSessionId,
    thread_id: &ThreadId,
    native_id: &str,
    existing: Option<OrchestrationV2ProviderThread>,
    now: UtcDateTime,
) -> OrchestrationV2ProviderThread {
    let mut thread = existing.unwrap_or_else(|| OrchestrationV2ProviderThread {
        id: derive::provider_thread(&opencode_driver(), Some(instance_id), native_id),
        driver: opencode_driver(),
        provider_instance_id: instance_id.clone(),
        provider_session_id: Some(session_id.clone()),
        app_thread_id: Some(thread_id.clone()),
        owner_node_id: None,
        native_thread_ref: None,
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
        created_at: now,
        updated_at: now,
    });
    thread.provider_session_id = Some(session_id.clone());
    thread.native_thread_ref = Some(native_ref(native_id, false));
    thread.status = OrchestrationV2ProviderThreadStatus::Idle;
    thread.updated_at = now;
    thread
}

fn tool_base(
    turn: &mut V2Turn,
    driver: &ProviderDriverKind,
    native_id: &str,
    status: OrchestrationV2TurnItemStatus,
) -> ToolItemBase {
    let ordinal = turn.next_ordinal;
    turn.next_ordinal += 1;
    ToolItemBase {
        id: derive::turn_item_from_provider_item(driver, native_id),
        thread_id: turn.thread_id.clone(),
        run_id: None,
        node_id: Some(derive::node_from_provider_item(driver, native_id)),
        provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
        provider_turn_id: Some(turn.provider_turn.id.clone()),
        native_item_ref: Some(native_ref(native_id, false)),
        ordinal,
        status,
        title: None,
        started_at: Some(UtcDateTime::now()),
        completed_at: matches!(
            status,
            OrchestrationV2TurnItemStatus::Completed | OrchestrationV2TurnItemStatus::Failed
        )
        .then(UtcDateTime::now),
        updated_at: UtcDateTime::now(),
    }
}

fn text_item(
    turn: &mut V2Turn,
    driver: &ProviderDriverKind,
    part_id: &str,
    text: &str,
    reasoning: bool,
    streaming: bool,
) -> turin_contracts::orchestration_v2::OrchestrationV2TurnItem {
    let ordinal = turn.next_ordinal;
    turn.next_ordinal += 1;
    let now = UtcDateTime::now();
    let id = derive::turn_item_from_provider_item(driver, part_id);
    if reasoning {
        turin_contracts::orchestration_v2::OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: turn.thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn.id.clone()),
            native_item_ref: Some(native_ref(part_id, false)),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Running,
            title: None,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            text: text.to_owned(),
            streaming,
        }
    } else {
        turin_contracts::orchestration_v2::OrchestrationV2TurnItem::AssistantMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: turn.thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn.id.clone()),
            native_item_ref: Some(native_ref(part_id, false)),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Running,
            title: None,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            message_id: derive::message_from_provider_item(driver, part_id),
            text: text.to_owned(),
            attachments: None,
            streaming,
        }
    }
}

fn subagent_for(
    instance_id: &ProviderInstanceId,
    thread: &OrchestrationV2ProviderThread,
    thread_id: &ThreadId,
    turn: &OrchestrationV2ProviderTurn,
    child_session: &str,
) -> OrchestrationV2Subagent {
    let now = UtcDateTime::now();
    OrchestrationV2Subagent {
        id: derive::node_from_provider_item(&opencode_driver(), child_session),
        thread_id: thread_id.clone(),
        run_id: None,
        parent_node_id: turn.node_id.clone(),
        origin: OrchestrationV2SubagentOrigin::ProviderNative,
        created_by: OrchestrationV2Actor::Agent,
        driver: opencode_driver(),
        provider_instance_id: instance_id.clone(),
        provider_thread_id: Some(thread.id.clone()),
        child_thread_id: None,
        native_task_ref: Some(native_ref(child_session, false)),
        prompt: String::new(),
        title: None,
        model: None,
        model_selection: None,
        completion_wake: None,
        completion_delivery: None,
        status: OrchestrationV2SubagentStatus::Running,
        progress: None,
        workflow: None,
        result: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
    }
}

fn messages_of(messages: &[Value]) -> Vec<OrchestrationV2ConversationMessage> {
    let now = UtcDateTime::now();
    messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role").or_else(|| message.pointer("/info/role")).and_then(Value::as_str)?;
            let id = message.get("id").or_else(|| message.pointer("/info/id")).and_then(Value::as_str)?;
            let text = message
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    message.get("content").or_else(|| message.get("parts")).and_then(Value::as_array).map(|parts| {
                        parts
                            .iter()
                            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })?;
            if text.is_empty() {
                return None;
            }
            Some(OrchestrationV2ConversationMessage {
                notification: None,
                created_by: if role == "user" { OrchestrationV2Actor::User } else { OrchestrationV2Actor::Agent },
                creation_source: OrchestrationV2CreationSource::Provider,
                scheduled_task_id: None,
                sender_thread_id: None,
                id: derive::message_from_provider_item(&opencode_driver(), id),
                thread_id: ThreadId::new("thread").expect("thread"),
                run_id: None,
                node_id: None,
                role: if role == "user" {
                    OrchestrationV2ConversationMessageRole::User
                } else {
                    OrchestrationV2ConversationMessageRole::Assistant
                },
                text,
                context: None,
                attachments: Vec::new(),
                streaming: false,
                created_at: now,
                updated_at: now,
                delegated_completion: None,
            })
        })
        .collect()
}
