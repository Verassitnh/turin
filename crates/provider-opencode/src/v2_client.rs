//! Port of upstream/packages/provider-opencode/src/server/v2/OpenCode2Client.ts
//! and the `@opencode/client` 2.0.28 routes `v2/adapter.ts` calls.
//!
//! Wire envelopes (the effect client unwraps `.data` on some of them):
//! - `{data}` — session create/get/active/prompt/fork/compact/synthetic,
//!   permission list, form list, revert stage
//! - `{data, cursor:{previous,next}}` — messages
//! - `{location, data}` — model, command, skill, agent, provider
//! - raw — `/api/info`, interrupt `{interrupted}`
//!
//! `location` query objects are bracketed (`location[directory]=…`).
//! Pagination sends `order` only on the first page; OpenCode rejects a cursor
//! combined with `order`.

use std::time::Duration;

use futures::Stream;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::OpenCodeRuntimeError;
use crate::events::{self, V2StreamEvent};
use crate::http_client::{self, HttpClient};
use crate::sse::SILENT_STREAM_TIMEOUT;
use crate::urlutil::append_query;

const INFO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct V2Client {
    http: HttpClient,
    silent_timeout: Duration,
}

impl V2Client {
    /// Always sends HTTP Basic, including an empty password (`opencode:`).
    pub fn connect(base_url: &str, password: &str) -> Result<Self, OpenCodeRuntimeError> {
        Ok(Self {
            http: HttpClient::new(base_url, password, true)?,
            silent_timeout: SILENT_STREAM_TIMEOUT,
        })
    }

    /// Records every JSON exchange through `protocol` (see `protocol_log`).
    pub fn with_protocol_log(
        mut self,
        protocol: std::sync::Arc<dyn crate::protocol_log::ProtocolLog>,
    ) -> Self {
        self.http = self.http.with_protocol_log(protocol);
        self
    }

    pub fn with_silent_timeout(mut self, timeout: Duration) -> Self {
        self.silent_timeout = timeout;
        self
    }

    pub async fn server_info(&self) -> Result<ServerInfo, OpenCodeRuntimeError> {
        let bytes = self
            .http
            .send_json(
                "server.info",
                Method::GET,
                "/api/info",
                &[],
                &[],
                None,
                &[200],
                Some(INFO_TIMEOUT),
            )
            .await?;
        http_client::decode_json("server.info", &bytes)
    }

    pub async fn create_session(&self, body: &Value) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.create",
            Method::POST,
            "/api/session",
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn get_session(&self, session_id: &str) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.get",
            Method::GET,
            &format!("/api/session/{}", encode(session_id)),
            &[],
            None,
            None,
            &[200],
        )
        .await
    }

    /// `session.remove`: deletes the session. A text generation session is removed
    /// after its one prompt, so OpenCode does not keep it.
    pub async fn remove_session(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.remove",
            Method::DELETE,
            &format!("/api/session/{}", encode(session_id)),
            &[],
            None,
            &[200, 204],
        )
        .await
    }

    pub async fn session_active(&self) -> Result<Map<String, Value>, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.active",
            Method::GET,
            "/api/session/active",
            &[],
            None,
            None,
            &[200],
        )
        .await
    }

    pub async fn update_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.update",
            Method::PATCH,
            &format!("/api/session/{}", encode(session_id)),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn move_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.move",
            Method::POST,
            &format!("/api/session/{}/move", encode(session_id)),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn prompt(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.prompt",
            Method::POST,
            &format!("/api/session/{}/prompt", encode(session_id)),
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn command(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.command",
            Method::POST,
            &format!("/api/session/{}/command", encode(session_id)),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn compact(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.compact",
            Method::POST,
            &format!("/api/session/{}/compact", encode(session_id)),
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn fork_session(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.fork",
            Method::POST,
            &format!("/api/session/{}/fork", encode(session_id)),
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn synthetic(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.synthetic",
            Method::POST,
            &format!("/api/session/{}/synthetic", encode(session_id)),
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn interrupt(
        &self,
        session_id: &str,
        resume: Option<bool>,
    ) -> Result<Interrupt, OpenCodeRuntimeError> {
        let mut query = Vec::new();
        if let Some(resume) = resume {
            query.push(("resume", resume.to_string()));
        }
        let query_ref: Vec<(&str, String)> = query.iter().map(|(k, v)| (*k, v.clone())).collect();
        let bytes = self
            .http
            .send_json(
                "session.interrupt",
                Method::POST,
                &format!("/api/session/{}/interrupt", encode(session_id)),
                &query_ref,
                &[],
                None,
                &[200],
                None,
            )
            .await?;
        http_client::decode_json("session.interrupt", &bytes)
    }

    pub async fn revert_stage(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.revert.stage",
            Method::POST,
            &format!("/api/session/{}/revert/stage", encode(session_id)),
            &[],
            None,
            Some(body),
            &[200],
        )
        .await
    }

    pub async fn revert_clear(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.revert.clear",
            Method::POST,
            &format!("/api/session/{}/revert/clear", encode(session_id)),
            &[],
            None,
            &[200, 204],
        )
        .await
    }

    pub async fn revert_commit(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.revert.commit",
            Method::POST,
            &format!("/api/session/{}/revert/commit", encode(session_id)),
            &[],
            None,
            &[204],
        )
        .await
    }

    pub async fn switch_agent(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.switchAgent",
            Method::POST,
            &format!("/api/session/{}/agent", encode(session_id)),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn switch_model(
        &self,
        session_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.switchModel",
            Method::POST,
            &format!("/api/session/{}/model", encode(session_id)),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn list_messages(&self, query: &MessageQuery) -> Result<Page, OpenCodeRuntimeError> {
        let pairs = message_query(query);
        let refs: Vec<(&str, String)> =
            pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let bytes = self
            .http
            .send_json(
                "message.list",
                Method::GET,
                &format!("/api/session/{}/message", encode(&query.session_id)),
                &refs,
                &[],
                None,
                &[200],
                None,
            )
            .await?;
        http_client::decode_json("message.list", &bytes)
    }

    pub async fn list_models(&self, directory: &str) -> Result<Located, OpenCodeRuntimeError> {
        self.located("model.list", "/api/model", directory).await
    }

    pub async fn list_agents(&self, directory: &str) -> Result<Located, OpenCodeRuntimeError> {
        self.located("agent.list", "/api/agent", directory).await
    }

    pub async fn list_commands(&self, directory: &str) -> Result<Located, OpenCodeRuntimeError> {
        self.located("command.list", "/api/command", directory)
            .await
    }

    pub async fn list_skills(&self, directory: &str) -> Result<Located, OpenCodeRuntimeError> {
        self.located("skill.list", "/api/skill", directory).await
    }

    pub async fn list_providers(&self, directory: &str) -> Result<Located, OpenCodeRuntimeError> {
        self.located("provider.list", "/api/provider", directory)
            .await
    }

    pub async fn list_permissions(
        &self,
        session_id: &str,
    ) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.unwrap_data(
            "permission.list",
            Method::GET,
            &format!("/api/session/{}/permission", encode(session_id)),
            &[],
            None,
            None,
            &[200],
        )
        .await
    }

    pub async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        body: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "permission.reply",
            Method::POST,
            &format!(
                "/api/session/{}/permission/{}/reply",
                encode(session_id),
                encode(request_id)
            ),
            &[],
            Some(body),
            &[200, 204],
        )
        .await
    }

    pub async fn list_forms(&self, session_id: &str) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.unwrap_data(
            "session.form.list",
            Method::GET,
            &format!("/api/session/{}/form", encode(session_id)),
            &[],
            None,
            None,
            &[200],
        )
        .await
    }

    pub async fn reply_form(
        &self,
        session_id: &str,
        form_id: &str,
        answer: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.form.reply",
            Method::POST,
            &format!(
                "/api/session/{}/form/{}/reply",
                encode(session_id),
                encode(form_id)
            ),
            &[],
            Some(&serde_json::json!({ "answer": answer })),
            &[204],
        )
        .await
    }

    pub async fn cancel_form(
        &self,
        session_id: &str,
        form_id: &str,
        message: Option<&str>,
    ) -> Result<(), OpenCodeRuntimeError> {
        let mut query = Vec::new();
        if let Some(message) = message {
            query.push(("message", message.to_string()));
        }
        let refs: Vec<(&str, String)> = query.iter().map(|(k, v)| (*k, v.clone())).collect();
        self.http
            .send_empty(
                "session.form.cancel",
                Method::DELETE,
                &format!(
                    "/api/session/{}/form/{}",
                    encode(session_id),
                    encode(form_id)
                ),
                &refs,
                &[],
                None,
                &[204],
                None,
            )
            .await
    }

    pub async fn put_instruction(
        &self,
        session_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.instructions.entry.put",
            Method::PUT,
            &format!(
                "/api/experimental/session/{}/instructions/entries/{}",
                encode(session_id),
                encode(key)
            ),
            &[],
            Some(&serde_json::json!({ "value": value })),
            &[204],
        )
        .await
    }

    pub async fn mcp_add(
        &self,
        server: &str,
        directory: &str,
        config: &Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        let location = HttpClient::location_query(directory);
        self.http
            .send_empty(
                "mcp.add",
                Method::PUT,
                &format!("/api/experimental/mcp/{}", encode(server)),
                &[],
                &location,
                Some(&serde_json::json!({ "config": config })),
                &[200, 204],
                None,
            )
            .await
    }

    pub async fn mcp_remove(
        &self,
        server: &str,
        directory: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        let location = HttpClient::location_query(directory);
        self.http
            .send_empty(
                "mcp.remove",
                Method::DELETE,
                &format!("/api/experimental/mcp/{}", encode(server)),
                &[],
                &location,
                None,
                &[200, 204],
                None,
            )
            .await
    }

    pub async fn cancel_inbox(
        &self,
        session_id: &str,
        inbox_id: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.empty(
            "session.inbox.cancel",
            Method::DELETE,
            &format!(
                "/api/session/{}/inbox/{}",
                encode(session_id),
                encode(inbox_id)
            ),
            &[],
            None,
            &[204],
        )
        .await
    }

    /// `GET /api/event`. Live once headers are 200. Manual reconnect only.
    /// After [`crate::EventStreamError::Silent`] or a clean end, the adapter
    /// backfills with [`Self::session_active`], [`Self::list_messages`],
    /// [`Self::list_permissions`], and [`Self::list_forms`] before subscribing again.
    pub async fn events(
        &self,
    ) -> Result<
        std::pin::Pin<
            Box<dyn Stream<Item = Result<V2StreamEvent, crate::error::EventStreamError>> + Send>,
        >,
        OpenCodeRuntimeError,
    > {
        let frames = self
            .http
            .open_event_stream(
                "event.subscribe",
                "/api/event",
                &[],
                None,
                self.silent_timeout,
            )
            .await?;
        Ok(Box::pin(filter_frames(frames, events::decode_v2_event)))
    }

    async fn located(
        &self,
        operation: &str,
        path: &str,
        directory: &str,
    ) -> Result<Located, OpenCodeRuntimeError> {
        let location = HttpClient::location_query(directory);
        let bytes = self
            .http
            .send_json(
                operation,
                Method::GET,
                path,
                &[],
                &location,
                None,
                &[200],
                None,
            )
            .await?;
        http_client::decode_json(operation, &bytes)
    }

    #[allow(clippy::too_many_arguments)]
    async fn unwrap_data<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        extra: Option<&[(String, String)]>,
        body: Option<&Value>,
        expected: &[u16],
    ) -> Result<T, OpenCodeRuntimeError> {
        let extra = extra.unwrap_or(&[]);
        let bytes = self
            .http
            .send_json(operation, method, path, query, extra, body, expected, None)
            .await?;
        let envelope: DataEnvelope<T> = http_client::decode_json(operation, &bytes)?;
        Ok(envelope.data)
    }

    async fn empty(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        expected: &[u16],
    ) -> Result<(), OpenCodeRuntimeError> {
        self.http
            .send_empty(operation, method, path, query, &[], body, expected, None)
            .await
    }
}

/// Messages the adapter asks for while backfilling a dropped stream.
#[derive(Debug, Clone)]
pub struct MessageQuery {
    pub session_id: String,
    pub limit: Option<u64>,
    pub order: Option<String>,
    pub cursor: Option<Value>,
    pub message_type: Option<String>,
}

impl MessageQuery {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            limit: None,
            order: None,
            cursor: None,
            message_type: None,
        }
    }
}

fn message_query(query: &MessageQuery) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    if let Some(limit) = query.limit {
        pairs.push(("limit".to_string(), limit.to_string()));
    }
    if let Some(order) = &query.order {
        pairs.push(("order".to_string(), order.clone()));
    }
    if let Some(cursor) = &query.cursor {
        append_query(&mut pairs, "cursor", cursor);
    }
    if let Some(message_type) = &query.message_type {
        pairs.push(("type".to_string(), message_type.clone()));
    }
    pairs
}

/// Walk every message, dropping `order` after the first page.
pub fn paginate_messages(
    client: V2Client,
    query: MessageQuery,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<Value, OpenCodeRuntimeError>> + Send>> {
    Box::pin(futures::stream::unfold(
        PageState {
            client,
            query,
            first: true,
            buffered: Vec::new(),
            done: false,
        },
        |mut state| async move {
            if state.done && state.buffered.is_empty() {
                return None;
            }
            if state.buffered.is_empty() {
                if state.done {
                    return None;
                }
                if !state.first {
                    state.query.order = None;
                }
                state.first = false;
                match state.client.list_messages(&state.query).await {
                    Ok(page) => {
                        let next = page.cursor.next.clone();
                        state.done = page.data.is_empty() || next.is_none();
                        state.query.cursor = next;
                        state.buffered = page.data;
                        if state.buffered.is_empty() {
                            return None;
                        }
                    }
                    Err(err) => {
                        state.done = true;
                        return Some((Err(err), state));
                    }
                }
            }
            let item = state.buffered.remove(0);
            Some((Ok(item), state))
        },
    ))
}

struct PageState {
    client: V2Client,
    query: MessageQuery,
    first: bool,
    buffered: Vec<Value>,
    done: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    pub pid: i64,
    pub urls: Vec<String>,
    pub paths: ServerPaths,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerPaths {
    pub tmp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Interrupt {
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DataEnvelope<T> {
    pub data: T,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Page {
    pub data: Vec<Value>,
    pub cursor: Cursor,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Cursor {
    #[serde(default)]
    pub previous: Option<Value>,
    #[serde(default)]
    pub next: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Located {
    pub location: Value,
    pub data: Vec<Value>,
}

fn encode(value: &str) -> String {
    crate::auth::encode_uri_component(value)
}

fn filter_frames<T, F>(
    frames: crate::sse::EventStream<crate::sse::SseFrame>,
    decode: F,
) -> impl Stream<Item = Result<T, crate::error::EventStreamError>> + Send
where
    T: Send,
    F: Fn(&crate::sse::SseFrame) -> Option<T> + Send + 'static,
{
    futures::stream::unfold((frames, decode), |(mut frames, decode)| async move {
        use futures::StreamExt;
        loop {
            match frames.next().await? {
                Err(err) => return Some((Err(err), (frames, decode))),
                Ok(frame) => {
                    if let Some(event) = decode(&frame) {
                        return Some((Ok(event), (frames, decode)));
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::basic_authorization;
    use crate::events::V2StreamEvent;
    use crate::test_support::{FakeResponse, FakeServer};
    use futures::StreamExt;
    use serde_json::json;

    #[tokio::test]
    async fn info_is_raw_and_session_create_unwraps_data() {
        let server = FakeServer::spawn(|request| {
            if request.target.starts_with("/api/info") {
                assert_eq!(
                    request.authorization.as_deref(),
                    Some(basic_authorization("pass€word").as_str())
                );
                return FakeResponse::json(
                    200,
                    r#"{"version":"2.0.18","pid":7,"urls":["http://127.0.0.1:1"],"paths":{"tmp":"/tmp/opencode"}}"#,
                );
            }
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/api/session");
            assert!(request.body.contains("\"id\":\"prompt-1\""));
            FakeResponse::json(200, r#"{"data":{"id":"ses","parentID":"p"}}"#)
        })
        .await;
        let client = V2Client::connect(&server.url, "pass€word").unwrap();
        let info = client.server_info().await.unwrap();
        assert_eq!(info.version, "2.0.18");
        assert_eq!(info.pid, 7);
        let session = client
            .create_session(&json!({"id": "prompt-1"}))
            .await
            .unwrap();
        assert_eq!(session["id"], "ses");
        assert_eq!(session["parentID"], "p");
    }

    #[tokio::test]
    async fn model_list_keeps_the_location_envelope_and_bracket_query() {
        let server = FakeServer::spawn(|request| {
            assert!(
                request.target.contains("location%5Bdirectory%5D=%2Fwork")
                    || request.target.contains("location[directory]=/work")
                    || request.target.contains("location%5Bdirectory%5D=/work"),
                "{}",
                request.target
            );
            FakeResponse::json(
                200,
                r#"{"location":{"directory":"/work"},"data":[{"id":"m","name":"M"}]}"#,
            )
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let listed = client.list_models("/work").await.unwrap();
        assert_eq!(listed.location["directory"], "/work");
        assert_eq!(listed.data[0]["id"], "m");
    }

    #[tokio::test]
    async fn pagination_drops_order_after_the_first_page() {
        let server = FakeServer::spawn(|request| {
            if request.target.contains("cursor=") {
                assert!(!request.target.contains("order="), "{}", request.target);
                return FakeResponse::json(200, r#"{"data":[{"id":"m2"}],"cursor":{}}"#);
            }
            assert!(request.target.contains("order=desc"), "{}", request.target);
            FakeResponse::json(200, r#"{"data":[{"id":"m1"}],"cursor":{"next":"c2"}}"#)
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let mut query = MessageQuery::new("ses");
        query.order = Some("desc".into());
        query.limit = Some(50);
        let mut stream = paginate_messages(client, query);
        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert_eq!(first["id"], "m1");
        assert_eq!(second["id"], "m2");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn remove_session_is_delete_on_the_session_path() {
        let server = FakeServer::spawn(|request| {
            assert_eq!(request.method, "DELETE");
            assert_eq!(request.target, "/api/session/ses_1");
            FakeResponse::empty(204)
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        client.remove_session("ses_1").await.unwrap();
    }

    #[tokio::test]
    async fn remove_session_reports_a_missing_session() {
        let server = FakeServer::spawn(|_| FakeResponse::json(404, r#"{"error":"missing"}"#)).await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        assert!(client.remove_session("ses_gone").await.is_err());
    }

    #[tokio::test]
    async fn form_cancel_is_delete_with_message_and_reply_is_204() {
        let server = FakeServer::spawn(|request| {
            if request.method == "DELETE" {
                assert!(request.target.contains("/form/form-1"));
                assert!(request.target.contains("message=no"), "{}", request.target);
                return FakeResponse::empty(204);
            }
            assert_eq!(request.method, "POST");
            assert_eq!(request.body, r#"{"answer":{"choice":"a"}}"#);
            FakeResponse::empty(204)
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        client
            .reply_form("ses", "form-1", &json!({"choice": "a"}))
            .await
            .unwrap();
        client
            .cancel_form("ses", "form-1", Some("no"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn event_stream_is_live_on_200_and_classifies_a_quiet_socket() {
        let server = FakeServer::spawn(|_| FakeResponse::sse(Vec::new(), true)).await;
        let client = V2Client::connect(&server.url, "pw")
            .unwrap()
            .with_silent_timeout(Duration::from_millis(80));
        let mut events = client.events().await.unwrap();
        let err = events.next().await.unwrap().unwrap_err();
        assert!(matches!(err, crate::error::EventStreamError::Silent));
    }

    #[tokio::test]
    async fn event_stream_decodes_data_frames_and_stops_at_eof() {
        let server = FakeServer::spawn(|_| {
            FakeResponse::sse(
                vec![
                    b": heartbeat\n\ndata: {\"id\":\"e\",\"type\":\"server.connected\",\"data\":{}}\n\n".to_vec(),
                ],
                false,
            )
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let mut events = client.events().await.unwrap();
        match events.next().await.unwrap().unwrap() {
            V2StreamEvent::Event(event) => assert_eq!(event.event_type, "server.connected"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(events.next().await.is_none());
    }
}
