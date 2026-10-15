//! Port of the HTTP surface `OpenCodeRuntime.ts` and `server/adapter.ts` call
//! on `@opencode-ai/sdk@1.15.13` subpath `/v2` (`dist/v2/gen/sdk.gen.js`).
//!
//! The package root (`dist/gen/sdk.gen.js`) is an older client. Permission
//! replies use `POST /permission/{requestID}/reply` (`Permission.reply`).
//! The deprecated `POST /session/{sessionID}/permissions/{permissionID}`
//! (`Permission.respond`) is not called.
//!
//! The SDK's `{ data }` field is its client return wrapper. The wire body is
//! the resource itself (a session object, an array, a boolean). Nested session
//! and message documents stay `serde_json::Value` so unknown fields survive
//! until the adapter types the ones it reads.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::OpenCodeRuntimeError;
use crate::events::{self, V1Event};
use crate::http_client::{self, HttpClient};
use crate::sse::{EventStream, SILENT_STREAM_TIMEOUT};

pub const OPENCODE_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct V1Client {
    http: HttpClient,
    silent_timeout: Duration,
    /// One `GET /event` at a time, shared by clones of this client.
    event_open: Arc<AtomicBool>,
}

impl V1Client {
    /// `directory` is the session cwd. An empty value is rejected: the 1.x
    /// server otherwise runs the request in its own cwd and turns never finish.
    /// Non-GET requests send it as `x-opencode-directory` (percent-encoded).
    /// GET and HEAD send `?directory=` instead. The `GET /event` subscription
    /// sends the header, as the SDK's SSE path does.
    pub fn new(
        base_url: &str,
        directory: &str,
        server_password: Option<&str>,
    ) -> Result<Self, OpenCodeRuntimeError> {
        if directory.is_empty() {
            return Err(OpenCodeRuntimeError::new(
                "v1.client",
                "OpenCode v1 requests need a directory.",
            ));
        }
        let http = HttpClient::new(base_url, server_password.unwrap_or(""), false)?
            .with_directory(directory);
        Ok(Self {
            http,
            silent_timeout: SILENT_STREAM_TIMEOUT,
            event_open: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Test hook. Production callers keep the 45s watchdog.
    pub fn with_silent_timeout(mut self, timeout: Duration) -> Self {
        self.silent_timeout = timeout;
        self
    }

    pub async fn health(&self) -> Result<Health, OpenCodeRuntimeError> {
        let bytes = self
            .http
            .send_json(
                "global.health",
                Method::GET,
                "/global/health",
                &[],
                &[],
                None,
                &[200],
                Some(OPENCODE_HEALTH_TIMEOUT),
            )
            .await?;
        // Any decode failure, including `healthy: false`, is the upstream health
        // detail. A raw serde message would not tell the user what to upgrade.
        serde_json::from_slice(&bytes).map_err(|err| {
            OpenCodeRuntimeError::new(
                "global.health",
                format!(
                    "OpenCode server returned an invalid health response. T3 Code requires OpenCode v{} or newer.",
                    crate::version_probe::MINIMUM_OPENCODE_VERSION
                ),
            )
            .with_cause(err)
        })
    }

    pub async fn list_commands(&self) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.get_json("command.list", "/command").await
    }

    pub async fn list_providers(&self) -> Result<Value, OpenCodeRuntimeError> {
        self.get_json("provider.list", "/provider").await
    }

    pub async fn list_agents(&self) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.get_json("app.agents", "/agent").await
    }

    pub async fn list_skills(&self) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.get_json("app.skills", "/skill").await
    }

    pub async fn create_session(
        &self,
        body: &impl Serialize,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json("session.create", Method::POST, "/session", body, 200)
            .await
    }

    pub async fn get_session(&self, session_id: &str) -> Result<Value, OpenCodeRuntimeError> {
        self.get_json("session.get", &session_path(session_id))
            .await
    }

    pub async fn update_session(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json(
            "session.update",
            Method::PATCH,
            &session_path(session_id),
            body,
            200,
        )
        .await
    }

    pub async fn session_status(&self) -> Result<Value, OpenCodeRuntimeError> {
        self.get_json("session.status", "/session/status").await
    }

    pub async fn session_children(
        &self,
        session_id: &str,
    ) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.get_json(
            "session.children",
            &format!("{}/children", session_path(session_id)),
        )
        .await
    }

    pub async fn session_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<Value>, OpenCodeRuntimeError> {
        self.get_json(
            "session.messages",
            &format!("{}/message", session_path(session_id)),
        )
        .await
    }

    pub async fn prompt(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json(
            "session.prompt",
            Method::POST,
            &format!("{}/message", session_path(session_id)),
            body,
            200,
        )
        .await
    }

    pub async fn prompt_async(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<(), OpenCodeRuntimeError> {
        self.http
            .send_empty(
                "session.promptAsync",
                Method::POST,
                &format!("{}/prompt_async", session_path(session_id)),
                &[],
                &[],
                Some(&serde_json::to_value(body).map_err(|err| {
                    OpenCodeRuntimeError::new("session.promptAsync", err.to_string())
                })?),
                &[204],
                None,
            )
            .await
    }

    pub async fn command(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json(
            "session.command",
            Method::POST,
            &format!("{}/command", session_path(session_id)),
            body,
            200,
        )
        .await
    }

    pub async fn abort_session(&self, session_id: &str) -> Result<bool, OpenCodeRuntimeError> {
        self.send_json(
            "session.abort",
            Method::POST,
            &format!("{}/abort", session_path(session_id)),
            &(),
            200,
        )
        .await
    }

    pub async fn fork_session(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json(
            "session.fork",
            Method::POST,
            &format!("{}/fork", session_path(session_id)),
            body,
            200,
        )
        .await
    }

    pub async fn summarize_session(
        &self,
        session_id: &str,
        body: &impl Serialize,
    ) -> Result<bool, OpenCodeRuntimeError> {
        self.send_json(
            "session.summarize",
            Method::POST,
            &format!("{}/summarize", session_path(session_id)),
            body,
            200,
        )
        .await
    }

    pub async fn reply_permission(
        &self,
        request_id: &str,
        body: &PermissionReply,
    ) -> Result<bool, OpenCodeRuntimeError> {
        self.send_json(
            "permission.reply",
            Method::POST,
            &permission_reply_path(request_id),
            body,
            200,
        )
        .await
    }

    pub async fn reply_question(
        &self,
        request_id: &str,
        body: &QuestionReply,
    ) -> Result<bool, OpenCodeRuntimeError> {
        self.send_json(
            "question.reply",
            Method::POST,
            &format!("/question/{}/reply", encode(request_id)),
            body,
            200,
        )
        .await
    }

    pub async fn mcp_status(&self) -> Result<Value, OpenCodeRuntimeError> {
        self.get_json("mcp.status", "/mcp").await
    }

    pub async fn mcp_add(&self, body: &impl Serialize) -> Result<Value, OpenCodeRuntimeError> {
        self.send_json("mcp.add", Method::POST, "/mcp", body, 200)
            .await
    }

    /// `GET /event`. Resolves once the headers are 200. The first frame is
    /// `server.connected`. The stream does not reconnect; a 45s gap with no
    /// bytes (including heartbeat comments) yields [`crate::EventStreamError::Silent`].
    ///
    /// One subscription per client (and its clones), matching the SDK client
    /// the v1 adapter keeps for a directory. Dropping the stream allows
    /// another subscribe.
    pub async fn events(
        &self,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<V1Event, crate::error::EventStreamError>> + Send>>,
        OpenCodeRuntimeError,
    > {
        use futures::StreamExt;
        let frames = self.events_from(None).await?;
        Ok(Box::pin(frames.filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.event.map(Ok),
                Err(err) => Some(Err(err)),
            }
        })))
    }

    /// `GET /event` with the SDK's `Last-Event-ID` resume header. Every frame
    /// comes through, with or without an event, so the caller can track `id:`
    /// and `retry:` the way the SDK's SSE client does.
    pub async fn events_from(
        &self,
        last_event_id: Option<&str>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<V1Frame, crate::error::EventStreamError>> + Send>>,
        OpenCodeRuntimeError,
    > {
        let claim = EventClaim::take(&self.event_open).ok_or_else(|| {
            OpenCodeRuntimeError::new(
                "event.subscribe",
                "An OpenCode event subscription is already open for this client.",
            )
        })?;
        // `claim` is released on an error, and on cancellation while the headers are awaited.
        let frames = self
            .http
            .open_event_stream(
                "event.subscribe",
                "/event",
                &[],
                last_event_id,
                self.silent_timeout,
            )
            .await?;
        Ok(Box::pin(Subscribed {
            inner: Box::pin(filter_frames(frames, |frame| {
                Some(V1Frame {
                    id: frame.id.clone(),
                    retry: frame.retry,
                    event: events::decode_v1_event(frame),
                })
            })),
            _claim: claim,
        }))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        path: &str,
    ) -> Result<T, OpenCodeRuntimeError> {
        let bytes = self
            .http
            .send_json(operation, Method::GET, path, &[], &[], None, &[200], None)
            .await?;
        http_client::decode_json(operation, &bytes)
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        body: &impl Serialize,
        expected: u16,
    ) -> Result<T, OpenCodeRuntimeError> {
        let value = serde_json::to_value(body)
            .map_err(|err| OpenCodeRuntimeError::new(operation, err.to_string()))?;
        let body = if value.is_null() { None } else { Some(value) };
        let bytes = self
            .http
            .send_json(
                operation,
                method,
                path,
                &[],
                &[],
                body.as_ref(),
                &[expected],
                None,
            )
            .await?;
        http_client::decode_json(operation, &bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Serialize)]
pub struct Health {
    #[serde(deserialize_with = "deserialize_true")]
    pub healthy: bool,
    pub version: String,
}

fn deserialize_true<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        Ok(true)
    } else {
        Err(serde::de::Error::custom("healthy must be true"))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionReply {
    pub reply: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionReply {
    pub answers: Vec<Vec<String>>,
}

fn session_path(session_id: &str) -> String {
    format!("/session/{}", encode(session_id))
}

/// `Permission.reply` in `@opencode-ai/sdk@1.15.13` `dist/v2/gen/sdk.gen.js`.
pub fn permission_reply_path(request_id: &str) -> String {
    format!("/permission/{}/reply", encode(request_id))
}

fn encode(value: &str) -> String {
    crate::auth::encode_uri_component(value)
}

/// One 1.x SSE frame: its cursor fields and the event it carried, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct V1Frame {
    pub id: Option<String>,
    pub retry: Option<u64>,
    pub event: Option<V1Event>,
}

struct Subscribed<S> {
    inner: S,
    /// Freed with the stream; never read.
    _claim: EventClaim,
}

/// The client's one subscription slot. Dropping it frees the slot, so a
/// cancelled or failed open does not leave the client refusing every later
/// subscribe.
struct EventClaim(Arc<AtomicBool>);

impl EventClaim {
    fn take(flag: &Arc<AtomicBool>) -> Option<Self> {
        (!flag.swap(true, Ordering::AcqRel)).then(|| Self(Arc::clone(flag)))
    }
}

impl Drop for EventClaim {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<S> Stream for Subscribed<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}

fn filter_frames<T, F>(
    frames: EventStream<crate::sse::SseFrame>,
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

    #[test]
    fn an_empty_directory_is_rejected() {
        match V1Client::new("http://127.0.0.1:1", "", None) {
            Err(err) => assert!(err.detail.contains("directory"), "{}", err.detail),
            Ok(_) => panic!("empty directory was accepted"),
        }
    }

    #[test]
    fn permission_replies_use_the_sdk_v2_route() {
        let path = permission_reply_path("req/1");
        assert_eq!(path, "/permission/req%2F1/reply");
        assert!(!path.contains("/permissions/"));
    }
    use crate::auth::basic_authorization;
    use crate::test_support::{FakeResponse, FakeServer};
    use futures::StreamExt;
    use serde_json::json;

    #[tokio::test]
    async fn health_sends_utf8_basic_and_directory_query() {
        let server = FakeServer::spawn(|request| {
            assert_eq!(request.method, "GET");
            assert!(request.target.starts_with("/global/health?"));
            assert!(request.target.contains("directory=%2Fwork"));
            assert_eq!(
                request.authorization.as_deref(),
                Some(basic_authorization("pässwörd").as_str())
            );
            assert!(request.directory_header.is_none());
            FakeResponse::json(200, r#"{"healthy":true,"version":"1.19.0"}"#)
        })
        .await;
        let client = V1Client::new(&server.url, "/work", Some("pässwörd")).unwrap();
        let health = client.health().await.unwrap();
        assert_eq!(health.version, "1.19.0");
        assert!(health.healthy);
    }

    #[tokio::test]
    async fn post_keeps_the_directory_header_and_session_body() {
        let server = FakeServer::spawn(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/session");
            assert_eq!(
                request.directory_header.as_deref(),
                Some(crate::auth::encode_uri_component("/work dir").as_str())
            );
            assert_eq!(
                request.body,
                r#"{"permission":[{"permission":"*","pattern":"*","action":"allow"}]}"#
            );
            FakeResponse::json(200, r#"{"id":"ses_1","title":"t"}"#)
        })
        .await;
        let client = V1Client::new(&server.url, "/work dir", Some("pw")).unwrap();
        let session = client
            .create_session(&json!({
                "permission": [{"permission": "*", "pattern": "*", "action": "allow"}]
            }))
            .await
            .unwrap();
        assert_eq!(session["id"], "ses_1");
    }

    #[tokio::test]
    async fn missing_or_empty_password_sends_no_authorization() {
        let server = FakeServer::spawn(|request| {
            assert!(request.authorization.is_none());
            FakeResponse::json(200, r#"{"healthy":true,"version":"1.19.0"}"#)
        })
        .await;
        for password in [None, Some("")] {
            V1Client::new(&server.url, "/work", password)
                .unwrap()
                .health()
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn html_is_not_a_v1_api_response() {
        let server = FakeServer::spawn(|_| FakeResponse {
            status: 200,
            content_type: Some("text/html".to_string()),
            body: b"<!doctype html>".to_vec(),
            stream: None,
            hold: false,
            follow: None,
            truncated: false,
        })
        .await;
        let client = V1Client::new(&server.url, "/work", None).unwrap();
        let err = client.health().await.unwrap_err();
        assert!(err.detail.contains("text/html"), "{}", err.detail);
    }

    #[tokio::test]
    async fn html_event_stream_is_an_unsupported_server() {
        let server = FakeServer::spawn(|_| FakeResponse {
            status: 200,
            content_type: Some("text/html".to_string()),
            body: b"<!doctype html>".to_vec(),
            stream: None,
            hold: false,
            follow: None,
            truncated: false,
        })
        .await;
        let client = V1Client::new(&server.url, "/work", None).unwrap();
        match client.events().await {
            Err(err) => assert!(
                err.detail.contains(
                    "Request is not supported by this version of OpenCode Server (Server responded with text/html)"
                ),
                "{}",
                err.detail
            ),
            Ok(_) => panic!("html was accepted as an event stream"),
        }
    }

    #[tokio::test]
    async fn event_stream_yields_properties_and_does_not_reconnect() {
        let mut server = FakeServer::spawn(|_| {
            FakeResponse::sse(
                vec![b": heartbeat\n\ndata: {\"id\":\"1\",\"type\":\"server.connected\",\"properties\":{}}\n\n".to_vec()],
                false,
            )
        })
        .await;
        let client = V1Client::new(&server.url, "/work", Some("pw"))
            .unwrap()
            .with_silent_timeout(Duration::from_millis(200));
        let mut events = client.events().await.unwrap();
        let opened = server.requests.recv().await.unwrap();
        assert_eq!(opened.target, "/event");
        assert_eq!(opened.directory_header.as_deref(), Some("%2Fwork"));
        let event = events.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, "server.connected");
        assert!(events.next().await.is_none());
        let follow_up =
            tokio::time::timeout(Duration::from_millis(50), server.requests.recv()).await;
        assert!(
            follow_up.is_err(),
            "the stream must not reconnect on its own"
        );
    }

    #[tokio::test]
    async fn prompt_async_is_204_and_permission_reply_posts_once_always_reject() {
        let server = FakeServer::spawn(|request| {
            if request.target.ends_with("/prompt_async") {
                assert_eq!(request.method, "POST");
                assert!(request.body.contains("\"messageID\":\"m1\""));
                return FakeResponse::empty(204);
            }
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/permission/req/reply");
            assert_eq!(request.body, r#"{"reply":"once"}"#);
            FakeResponse::json(200, "true")
        })
        .await;
        let client = V1Client::new(&server.url, "/work", Some("pw")).unwrap();
        client
            .prompt_async(
                "ses",
                &json!({"messageID": "m1", "parts": [{"type": "text", "text": "hi"}]}),
            )
            .await
            .unwrap();
        let replied = client
            .reply_permission(
                "req",
                &PermissionReply {
                    reply: "once".into(),
                    message: None,
                },
            )
            .await
            .unwrap();
        assert!(replied);
    }

    #[tokio::test]
    async fn a_cancelled_open_frees_the_subscription_slot() {
        // Accepts connections and never answers, so the open is still waiting for headers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let client = V1Client::new(&url, "/work", None).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.events())
                .await
                .is_err()
        );
        // The second open must wait on the server like the first did, not fail as "already open".
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.events())
                .await
                .is_err(),
            "a cancelled open left the client refusing subscriptions"
        );
    }

    #[tokio::test]
    async fn one_event_subscription_per_client() {
        let server = FakeServer::spawn(|_| {
            FakeResponse::sse(
                vec![b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\n".to_vec()],
                true,
            )
        })
        .await;
        let client = V1Client::new(&server.url, "/work", None).unwrap();
        let first = client.events().await.unwrap();
        match client.clone().events().await {
            Err(err) => assert!(err.detail.contains("already open"), "{}", err.detail),
            Ok(_) => panic!("a second subscription opened"),
        }
        drop(first);
        let _again = client
            .events()
            .await
            .expect("subscribe again after the first stream is dropped");
    }
}
