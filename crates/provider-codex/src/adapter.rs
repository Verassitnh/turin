//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts.
//! Codex native thread IDs survive app-server restarts; the session owns one
//! long-lived client and forwards notifications in their received order.
use crate::{
    approvals::{PendingRequest, RequestBroker},
    params::{RuntimePolicy, thread_runtime_params, turn_start_params},
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::sync::mpsc;
use turin_codex_app_server::{
    client::Client,
    errors::AppServerError,
    extensions::{
        ThreadHistoryMode, decode_thread_history_metadata, is_archived_session_error,
        response_thread, revert_codex_thread, user_agent_version,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexEventKind {
    TurnStarted,
    TurnCompleted,
    ItemStarted,
    ItemUpdated,
    ItemCompleted,
    PlanUpdated,
    TokenUsage,
    DiffUpdated,
    Error,
    Other,
}

#[derive(Debug, Clone)]
pub struct CodexEvent {
    pub kind: CodexEventKind,
    pub method: String,
    pub params: Value,
}

impl CodexEvent {
    pub fn from_notification(method: String, params: Value) -> Self {
        let kind = match method.as_str() {
            "turn/started" => CodexEventKind::TurnStarted,
            "turn/completed" => CodexEventKind::TurnCompleted,
            "item/started" => CodexEventKind::ItemStarted,
            "item/completed" => CodexEventKind::ItemCompleted,
            "turn/plan/updated" => CodexEventKind::PlanUpdated,
            "thread/tokenUsage/updated" => CodexEventKind::TokenUsage,
            "turn/diff/updated" => CodexEventKind::DiffUpdated,
            "error" => CodexEventKind::Error,
            m if m.starts_with("item/") && m.ends_with("/delta") => CodexEventKind::ItemUpdated,
            _ => CodexEventKind::Other,
        };
        Self {
            kind,
            method,
            params,
        }
    }
}

pub struct CodexAdapter;

pub struct CodexSession {
    client: Arc<Client>,
    events: Mutex<Option<mpsc::UnboundedReceiver<CodexEvent>>>,
    requests: Mutex<Option<mpsc::UnboundedReceiver<PendingRequest>>>,
    pub broker: RequestBroker,
    pub version: Option<String>,
    pub codex_home: Option<String>,
    /// The `additionalContext` of each thread's latest turn that carried one.
    additional_context: Mutex<std::collections::HashMap<String, serde_json::Map<String, Value>>>,
}

fn payload(response: Option<Value>, method: &str) -> Result<Value, AppServerError> {
    response.ok_or_else(|| AppServerError::InvalidPayload {
        method: method.into(),
        detail: "missing response".into(),
    })
}

fn id(response: &Value, field: &str, method: &str) -> Result<String, AppServerError> {
    response
        .pointer(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppServerError::InvalidPayload {
            method: method.into(),
            detail: format!("missing {field}"),
        })
}

impl CodexAdapter {
    pub fn spawn(
        binary: &str,
        args: &[&str],
        cwd: &Path,
        environment: &[(&str, &str)],
    ) -> Result<Client, AppServerError> {
        Self::spawn_removing(binary, args, cwd, environment, &[])
    }

    /// `spawn`, with `removed` taken out of the inherited environment first.
    pub fn spawn_removing(
        binary: &str,
        args: &[&str],
        cwd: &Path,
        environment: &[(&str, &str)],
        removed: &[&str],
    ) -> Result<Client, AppServerError> {
        Client::spawn_with_env_removed(
            binary,
            args,
            Some(cwd),
            environment,
            removed,
            Default::default(),
        )
    }

    pub async fn initialize(client: Client, version: &str) -> Result<CodexSession, AppServerError> {
        let (broker, requests) = RequestBroker::new();
        broker.install(&client);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        // A protocol-known notification reaches only its registered handlers, so every
        // known method gets a forwarding handler, and unknown ones take the fallback.
        for entry in turin_codex_app_server::generated::meta::SERVER_NOTIFICATION_METHODS {
            let tx = event_tx.clone();
            let method = entry.method.to_owned();
            client.handle_server_notification(entry.method, move |params| {
                let _ = tx.send(CodexEvent::from_notification(
                    method.clone(),
                    params.unwrap_or(Value::Null),
                ));
                async { Ok(()) }
            });
        }
        client.handle_unknown_server_notification(Arc::new(move |method, params| {
            let tx = event_tx.clone();
            Box::pin(async move {
                let _ = tx.send(CodexEvent::from_notification(
                    method,
                    params.unwrap_or(Value::Null),
                ));
            })
        }));
        let init = client.request("initialize", Some(json!({
            "clientInfo":{"name":"T3 Code","title":"T3 Code","version":version},
            "capabilities":{"experimentalApi":true,"optOutNotificationMethods":["turn/diff/updated"]}
        }))).await;
        let init = match init {
            Ok(value) => payload(value, "initialize")?,
            Err(AppServerError::Request { error, .. })
                if error.code == -32600 && error.message.contains("Already initialized") =>
            {
                Value::Null
            }
            Err(error) => return Err(error),
        };
        client.notify("initialized", None)?;
        let version = init
            .get("userAgent")
            .and_then(Value::as_str)
            .and_then(user_agent_version)
            .map(str::to_owned);
        let codex_home = init
            .get("codexHome")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(CodexSession {
            client: Arc::new(client),
            events: Mutex::new(Some(event_rx)),
            requests: Mutex::new(Some(requests)),
            broker,
            version,
            codex_home,
            additional_context: Mutex::new(Default::default()),
        })
    }
}

impl CodexSession {
    pub fn client(&self) -> &Client {
        &self.client
    }
    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<CodexEvent>> {
        self.events.lock().take()
    }
    pub fn take_requests(&self) -> Option<mpsc::UnboundedReceiver<PendingRequest>> {
        self.requests.lock().take()
    }

    pub async fn ensure_thread(&self, cwd: &str, model: &str) -> Result<String, AppServerError> {
        let response = self.start_thread(cwd, model).await?;
        id(&response, "/thread/id", "thread/start")
    }

    pub async fn start_thread(&self, cwd: &str, model: &str) -> Result<Value, AppServerError> {
        let response = payload(
            self.client
                .request(
                    "thread/start",
                    Some(thread_runtime_params(Some(cwd), Some(model))),
                )
                .await?,
            "thread/start",
        )?;
        Ok(response)
    }

    pub async fn resume_thread(
        &self,
        native_thread_id: &str,
        cwd: Option<&str>,
        model: Option<&str>,
    ) -> Result<Value, AppServerError> {
        let mut params = thread_runtime_params(cwd, model);
        params["threadId"] = json!(native_thread_id);
        params["excludeTurns"] = json!(true);
        let response = self
            .client
            .request("thread/resume", Some(params.clone()))
            .await;
        let response = match response {
            Err(AppServerError::Request { ref error, .. })
                if is_archived_session_error(&error.message) =>
            {
                self.client
                    .request(
                        "thread/unarchive",
                        Some(json!({"threadId":native_thread_id})),
                    )
                    .await?;
                self.client.request("thread/resume", Some(params)).await?
            }
            other => other?,
        };
        payload(response, "thread/resume")
    }

    pub async fn start_turn(
        &self,
        thread_id: &str,
        input: Vec<Value>,
        model: &str,
        reasoning_effort: Option<&str>,
        service_tier: Option<&str>,
        policy: &RuntimePolicy,
    ) -> Result<String, AppServerError> {
        let params = turn_start_params(
            thread_id,
            input,
            model,
            reasoning_effort,
            service_tier,
            policy,
        );
        // Codex drops client developer messages during compaction but only resends
        // additionalContext when it changes; remember what to restore.
        {
            let mut contexts = self.additional_context.lock();
            match params.get("additionalContext").and_then(Value::as_object) {
                Some(context) => {
                    contexts.insert(thread_id.to_owned(), context.clone());
                }
                None => {
                    contexts.remove(thread_id);
                }
            }
        }
        let response = payload(
            self.client.request("turn/start", Some(params)).await?,
            "turn/start",
        )?;
        let turn_id = id(&response, "/turn/id", "turn/start")?;
        let mode = serde_json::to_value(policy.runtime_mode).expect("runtime mode");
        if let Some(mode) = mode.as_str() {
            self.broker.set_runtime_mode(thread_id, mode);
        }
        Ok(turn_id)
    }

    pub async fn steer_turn(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<Value>,
    ) -> Result<(), AppServerError> {
        self.client
            .request(
                "turn/steer",
                Some(json!({"threadId":thread_id,"expectedTurnId":expected_turn_id,"input":input})),
            )
            .await?;
        Ok(())
    }

    pub async fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), AppServerError> {
        self.client
            .request(
                "turn/interrupt",
                Some(json!({"threadId":thread_id,"turnId":turn_id})),
            )
            .await?;
        Ok(())
    }

    pub async fn unload_thread(&self, thread_id: &str) -> Result<(), AppServerError> {
        self.broker.unload_thread(thread_id);
        self.client
            .request("thread/unsubscribe", Some(json!({"threadId":thread_id})))
            .await?;
        Ok(())
    }

    pub async fn compact_thread(&self, thread_id: &str) -> Result<Value, AppServerError> {
        payload(
            self.client
                .request("thread/compact/start", Some(json!({"threadId":thread_id})))
                .await?,
            "thread/compact/start",
        )
    }

    pub async fn revert_thread(
        &self,
        thread_id: &str,
        num_turns: u64,
    ) -> Result<Value, AppServerError> {
        let read = payload(
            self.client
                .request(
                    "thread/read",
                    Some(json!({"threadId":thread_id,"includeTurns":false})),
                )
                .await?,
            "thread/read",
        )?;
        let metadata = decode_thread_history_metadata(&read)?;
        if metadata.history_mode != Some(ThreadHistoryMode::Paginated) {
            return Err(AppServerError::InvalidPayload {
                method: "thread/revert".into(),
                detail: "Codex thread history is not paginated".into(),
            });
        }
        if !metadata.loaded {
            self.resume_thread(thread_id, None, None).await?;
        }
        let result = revert_codex_thread(&self.client, thread_id, num_turns).await?;
        response_thread(&result).cloned()
    }

    /// `restoreAdditionalContext`: after a compaction, puts the thread's current
    /// additional context back as injected items, as Codex sent it: application
    /// context as developer input, untrusted context as quoted user-side input.
    /// Bounded to 10 s; a failure is only logged.
    pub async fn restore_additional_context(&self, thread_id: &str) {
        let Some(context) = self.additional_context.lock().get(thread_id).cloned() else {
            return;
        };
        let items: Vec<Value> = context
            .iter()
            .map(|(key, entry)| {
                let value = entry.get("value").and_then(Value::as_str).unwrap_or_default();
                if entry.get("kind").and_then(Value::as_str) == Some("untrusted") {
                    json!({"type": "message", "role": "user", "content": [
                        {"type": "input_text", "text": format!("<external_{key}>{value}</external_{key}>")}
                    ]})
                } else {
                    json!({"type": "message", "role": "developer", "content": [
                        {"type": "input_text", "text": format!("<{key}>{value}</{key}>")}
                    ]})
                }
            })
            .collect();
        let request = self.client.request(
            "thread/inject_items",
            Some(json!({"threadId": thread_id, "items": items})),
        );
        match tokio::time::timeout(std::time::Duration::from_secs(10), request).await {
            Ok(Ok(_)) => {}
            Ok(Err(cause)) => {
                tracing::warn!(%cause, "Failed to restore Codex additional context after compaction.")
            }
            Err(_) => {
                tracing::warn!("Failed to restore Codex additional context after compaction.")
            }
        }
    }

    pub async fn shutdown(self) -> Result<(), AppServerError> {
        let client = Arc::try_unwrap(self.client).map_err(|_| AppServerError::Cancelled)?;
        client.shutdown().await
    }
}
