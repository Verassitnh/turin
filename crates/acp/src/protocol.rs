//! Port of upstream/packages/effect-acp/src/protocol.ts
//!
//! Newline-delimited JSON-RPC. Client→agent calls and extension calls use
//! separate id counters so they cannot collide. Inbound requests are answered
//! on their own task, so a `session/prompt` can stay pending while the agent
//! sends `session/request_permission` or a `cursor/*` extension request.
//! Requests (frames with `id`) and notifications (frames without `id`) are
//! dispatched to different handler tables.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::errors::{
    AcpError, AcpProtocolParseError, AcpRequestError, AcpTransportError, ProtocolParseOperation,
    RequestId, TransportOperation, acp_request_identity,
};
use crate::meta::RPC_ID_START;
use crate::schema::{decode_elicitation_complete, decode_session_update};

const MAX_BUFFERED_NOTIFICATIONS: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct RequestContext {
    pub request_id: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingNotification {
    SessionUpdate { params: Value },
    ElicitationComplete { method: String, params: Value },
    Ext { method: String, params: Value },
}

pub type RequestCallback = Arc<
    dyn Fn(String, Value, RequestContext) -> BoxFuture<'static, Result<Value, AcpError>>
        + Send
        + Sync,
>;

pub type NotificationCallback =
    Arc<dyn Fn(IncomingNotification) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct Connection {
    inner: Arc<ConnectionInner>,
    // The tasks hold only `ConnectionInner`, never this owner, so dropping the
    // last `Connection` clone aborts them. That closes the child's stdin and
    // releases the task state. Owning the handles inside `ConnectionInner`
    // would form a cycle that never drops.
    _tasks: Arc<TaskSet>,
}

/// Aborts the connection tasks when the last `Connection` clone drops.
struct TaskSet(Vec<JoinHandle<()>>);

impl Drop for TaskSet {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

struct ConnectionInner {
    state: Mutex<State>,
    write_tx: mpsc::UnboundedSender<WriteJob>,
    notifications: Mutex<VecDeque<IncomingNotification>>,
    notify_waiters: Notify,
    on_request: RequestCallback,
    on_notification: NotificationCallback,
    on_termination: Arc<dyn Fn(&AcpError) + Send + Sync>,
}

struct State {
    pending: HashMap<String, Pending>,
    terminated: Option<AcpError>,
    ext_next: i64,
    rpc_next: i64,
}

struct Pending {
    method: String,
    tx: oneshot::Sender<Result<Value, AcpError>>,
}

struct WriteJob {
    bytes: String,
    ack: Option<oneshot::Sender<Result<(), AcpError>>>,
}

enum IdSpace {
    Extension,
    Rpc,
}

struct InboundRequest {
    id_value: Value,
    method: String,
    params: Value,
    context: RequestContext,
}

pub struct ConnectionConfig {
    pub on_request: RequestCallback,
    pub on_notification: NotificationCallback,
    pub on_termination: Arc<dyn Fn(&AcpError) + Send + Sync>,
    pub termination_on_eof: Option<Arc<dyn Fn() -> BoxFuture<'static, AcpError> + Send + Sync>>,
    pub transform_session_update: Arc<dyn Fn(Value) -> Value + Send + Sync>,
}

impl Connection {
    pub fn start<R, W>(read: R, write: W, config: ConnectionConfig) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (write_tx, write_rx) = mpsc::unbounded_channel();
        let (handler_tx, handler_rx) = mpsc::unbounded_channel::<InboundRequest>();
        let inner = Arc::new(ConnectionInner {
            state: Mutex::new(State {
                pending: HashMap::new(),
                terminated: None,
                ext_next: 1,
                rpc_next: RPC_ID_START,
            }),
            write_tx,
            notifications: Mutex::new(VecDeque::new()),
            notify_waiters: Notify::new(),
            on_request: config.on_request,
            on_notification: config.on_notification,
            on_termination: config.on_termination,
        });
        let writer = tokio::spawn(writer_loop(write, write_rx, Arc::clone(&inner)));
        let reader = tokio::spawn(reader_loop(
            read,
            Arc::clone(&inner),
            handler_tx,
            config.termination_on_eof,
            config.transform_session_update,
        ));
        let handlers = tokio::spawn(handler_loop(handler_rx, Arc::clone(&inner)));
        Self {
            inner,
            _tasks: Arc::new(TaskSet(vec![writer, reader, handlers])),
        }
    }

    pub async fn ext_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.request(IdSpace::Extension, method, params).await
    }

    pub async fn rpc_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.request(IdSpace::Rpc, method, params).await
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.inner.ensure_active()?;
        let frame = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.inner.enqueue(WriteJob {
            bytes: format!("{frame}\n"),
            ack: None,
        })
    }

    pub async fn next_notification(&self) -> Option<IncomingNotification> {
        loop {
            // `notify_waiters` stores no permit. Enable the waiter before checking
            // the queue and the terminated flag, so a push or terminate that lands
            // in between still wakes us instead of being lost.
            let notified = self.inner.notify_waiters.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.inner.notifications.lock().pop_front() {
                return Some(item);
            }
            if self.inner.state.lock().terminated.is_some()
                && self.inner.notifications.lock().is_empty()
            {
                return None;
            }
            notified.await;
        }
    }

    pub fn termination_error(&self) -> Option<AcpError> {
        self.inner.state.lock().terminated.clone()
    }

    async fn request(
        &self,
        space: IdSpace,
        method: &str,
        params: Value,
    ) -> Result<Value, AcpError> {
        self.inner.ensure_active()?;
        let id = self.inner.alloc(space)?;
        let key = acp_request_identity(&id);
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.inner.state.lock();
            if let Some(error) = state.terminated.clone() {
                return Err(error);
            }
            state.pending.insert(
                key.clone(),
                Pending {
                    method: method.to_owned(),
                    tx,
                },
            );
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id.to_value(),
            "method": method,
            "params": params,
            "headers": [],
        });
        if let Err(error) = self.inner.enqueue(WriteJob {
            bytes: format!("{frame}\n"),
            ack: None,
        }) {
            self.inner.remove_pending(&key);
            return Err(error);
        }
        let mut guard = PendingGuard {
            inner: Arc::clone(&self.inner),
            key: key.clone(),
            armed: true,
        };
        let result = match rx.await {
            Ok(result) => result,
            Err(_) => Err(self
                .termination_error()
                .unwrap_or_else(|| AcpError::transport("ACP request was dropped"))),
        };
        guard.armed = false;
        result
    }
}

struct PendingGuard {
    inner: Arc<ConnectionInner>,
    key: String,
    armed: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.inner.remove_pending(&self.key);
        }
    }
}

impl ConnectionInner {
    fn ensure_active(&self) -> Result<(), AcpError> {
        match self.state.lock().terminated.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn alloc(&self, space: IdSpace) -> Result<RequestId, AcpError> {
        let mut state = self.state.lock();
        if let Some(error) = state.terminated.clone() {
            return Err(error);
        }
        let id = match space {
            IdSpace::Extension => {
                let id = state.ext_next;
                state.ext_next += 1;
                id
            }
            IdSpace::Rpc => {
                let id = state.rpc_next;
                state.rpc_next += 1;
                id
            }
        };
        Ok(RequestId::Number(id))
    }

    fn remove_pending(&self, key: &str) {
        self.state.lock().pending.remove(key);
    }

    fn enqueue(&self, job: WriteJob) -> Result<(), AcpError> {
        self.ensure_active()?;
        self.write_tx.send(job).map_err(|_| {
            AcpError::from(AcpTransportError {
                operation: None,
                method: None,
                detail: Some(
                    "The ACP output queue was closed before accepting a message".to_owned(),
                ),
                pid: None,
                cause: Some("Output queue closed".to_owned()),
            })
        })
    }

    fn terminate(&self, error: AcpError) {
        let pending = {
            let mut state = self.state.lock();
            if state.terminated.is_some() {
                return;
            }
            state.terminated = Some(error.clone());
            std::mem::take(&mut state.pending)
        };
        for (_, pending) in pending {
            let _ = pending.tx.send(Err(error.clone()));
        }
        self.notify_waiters.notify_waiters();
        (self.on_termination)(&error);
    }

    fn push_notification(&self, notification: IncomingNotification) {
        let mut queue = self.notifications.lock();
        if queue.len() == MAX_BUFFERED_NOTIFICATIONS {
            queue.pop_front();
        }
        queue.push_back(notification);
        self.notify_waiters.notify_waiters();
    }

    fn complete(&self, id: &RequestId, result: Result<Value, AcpError>) {
        let key = acp_request_identity(id);
        if let Some(pending) = self.state.lock().pending.remove(&key) {
            let _ = pending.tx.send(result);
        }
    }
}

async fn writer_loop<W>(
    mut write: W,
    mut rx: mpsc::UnboundedReceiver<WriteJob>,
    inner: Arc<ConnectionInner>,
) where
    W: tokio::io::AsyncWrite + Unpin,
{
    while let Some(job) = rx.recv().await {
        match write
            .write_all(job.bytes.as_bytes())
            .await
            .and(write.flush().await)
        {
            Ok(()) => {
                if let Some(ack) = job.ack {
                    let _ = ack.send(Ok(()));
                }
            }
            Err(error) => {
                let transport = AcpError::from(AcpTransportError {
                    operation: None,
                    method: None,
                    detail: Some("Failed to write an outgoing ACP message".to_owned()),
                    pid: None,
                    cause: Some(error.to_string()),
                });
                if let Some(ack) = job.ack {
                    let _ = ack.send(Err(transport.clone()));
                }
                while let Ok(job) = rx.try_recv() {
                    if let Some(ack) = job.ack {
                        let _ = ack.send(Err(transport.clone()));
                    }
                }
                inner.terminate(transport);
                break;
            }
        }
    }
}

/// Ends the connection if the reader unwinds, e.g. a panicking session-update
/// transform. Upstream terminates on any reader failure (protocol.ts 718-739).
/// Explicit exits already call `terminate`; this covers the unwind path only.
/// A plain drop (the client was dropped and the task aborted) does not
/// terminate, so user hooks do not run during teardown.
struct ReaderUnwindGuard(Arc<ConnectionInner>);

impl Drop for ReaderUnwindGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.terminate(AcpError::from(AcpTransportError {
                operation: Some(TransportOperation::ReadInputStream),
                method: None,
                detail: None,
                pid: None,
                cause: Some("ACP reader stopped after a panic".to_owned()),
            }));
        }
    }
}

async fn reader_loop<R>(
    read: R,
    inner: Arc<ConnectionInner>,
    handler_tx: mpsc::UnboundedSender<InboundRequest>,
    termination_on_eof: Option<Arc<dyn Fn() -> BoxFuture<'static, AcpError> + Send + Sync>>,
    transform_session_update: Arc<dyn Fn(Value) -> Value + Send + Sync>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let _unwind = ReaderUnwindGuard(Arc::clone(&inner));
    let mut lines = BufReader::new(read).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim_end_matches('\r');
                if line.is_empty() {
                    continue;
                }
                let value: Value = match serde_json::from_str(line) {
                    Ok(value) => value,
                    Err(_) => {
                        inner.terminate(AcpError::from(AcpProtocolParseError {
                            operation: ProtocolParseOperation::DecodeWireMessage,
                            method: None,
                            request_id: None,
                            issue_count: None,
                            issue_kinds: None,
                            maximum_path_depth: None,
                            cause: Some("invalid json".to_owned()),
                        }));
                        return;
                    }
                };
                if let Err(error) =
                    route_message(&inner, &handler_tx, &transform_session_update, value).await
                {
                    inner.terminate(error);
                    return;
                }
            }
            Ok(None) => {
                let error = match &termination_on_eof {
                    Some(classify) => classify().await,
                    None => AcpError::InputStreamEnded,
                };
                inner.terminate(error);
                return;
            }
            Err(error) => {
                inner.terminate(AcpError::from(AcpTransportError {
                    operation: Some(TransportOperation::ReadInputStream),
                    method: None,
                    detail: None,
                    pid: None,
                    cause: Some(error.to_string()),
                }));
                return;
            }
        }
    }
}

async fn route_message(
    inner: &Arc<ConnectionInner>,
    handler_tx: &mpsc::UnboundedSender<InboundRequest>,
    transform_session_update: &Arc<dyn Fn(Value) -> Value + Send + Sync>,
    value: Value,
) -> Result<(), AcpError> {
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let id = value.get("id").and_then(RequestId::from_value);
    let has_result = value.get("result").is_some() || value.get("error").is_some();

    if has_result {
        if let Some(id) = id {
            if let Some(error) = value.get("error") {
                let (code, message, data) = parse_rpc_error(error);
                let key = acp_request_identity(&id);
                let method = inner
                    .state
                    .lock()
                    .pending
                    .get(&key)
                    .map(|pending| pending.method.clone());
                if let Some(method) = method {
                    inner.complete(
                        &id,
                        Err(AcpError::from(AcpRequestError::from_protocol_error(
                            code,
                            message,
                            data,
                            &method,
                            id.clone(),
                        ))),
                    );
                }
            } else if let Some(result) = value.get("result").cloned() {
                inner.complete(&id, Ok(result));
            }
        }
        return Ok(());
    }

    let Some(method) = method else {
        return Err(AcpError::from(AcpProtocolParseError {
            operation: ProtocolParseOperation::DecodeWireMessage,
            method: None,
            request_id: None,
            issue_count: None,
            issue_kinds: None,
            maximum_path_depth: None,
            cause: Some("missing method".to_owned()),
        }));
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    if id.is_none() {
        return dispatch_notification(inner, transform_session_update, method, params).await;
    }
    let id = id.unwrap();
    let context = RequestContext {
        request_id: acp_request_identity(&id),
        method: method.clone(),
    };
    handler_tx
        .send(InboundRequest {
            id_value: id.to_value(),
            method,
            params,
            context,
        })
        .map_err(|_| AcpError::transport("ACP request handler queue closed"))
}

async fn dispatch_notification(
    inner: &Arc<ConnectionInner>,
    transform_session_update: &Arc<dyn Fn(Value) -> Value + Send + Sync>,
    method: String,
    params: Value,
) -> Result<(), AcpError> {
    use crate::meta::client::{ELICITATION_COMPLETE, SESSION_UPDATE};
    use crate::meta::v1_client::SESSION_ELICITATION_COMPLETE;

    if method == SESSION_UPDATE {
        decode_session_update(&params).map_err(|issue| {
            AcpError::from(AcpProtocolParseError::from_schema(
                ProtocolParseOperation::DecodeNotificationPayload,
                SESSION_UPDATE,
                &issue,
            ))
        })?;
        let params = transform_session_update(params);
        let notification = IncomingNotification::SessionUpdate {
            params: params.clone(),
        };
        inner.push_notification(notification.clone());
        run_notification_callback(inner, notification).await;
        return Ok(());
    }
    if method == ELICITATION_COMPLETE || method == SESSION_ELICITATION_COMPLETE {
        decode_elicitation_complete(&params).map_err(|issue| {
            AcpError::from(AcpProtocolParseError::from_schema(
                ProtocolParseOperation::DecodeNotificationPayload,
                &method,
                &issue,
            ))
        })?;
        let notification = IncomingNotification::ElicitationComplete {
            method: method.clone(),
            params,
        };
        inner.push_notification(notification.clone());
        run_notification_callback(inner, notification).await;
        return Ok(());
    }
    let notification = IncomingNotification::Ext {
        method: method.clone(),
        params,
    };
    inner.push_notification(notification.clone());
    run_notification_callback(inner, notification).await;
    Ok(())
}

/// Runs the notification callback in isolation, like upstream
/// `isolateNotificationHandler` (_internal/shared.ts): a panic is logged and the
/// reader keeps going. Only the callback is guarded. A transform defect is not,
/// and it terminates the connection through `ReaderUnwindGuard`.
async fn run_notification_callback(inner: &ConnectionInner, notification: IncomingNotification) {
    let method = notification_method(&notification).to_owned();
    let callback = Arc::clone(&inner.on_notification);
    let future = match std::panic::catch_unwind(AssertUnwindSafe(|| callback(notification))) {
        Ok(future) => future,
        Err(_) => {
            tracing::error!(%method, "ACP notification handler panicked");
            return;
        }
    };
    if AssertUnwindSafe(future).catch_unwind().await.is_err() {
        tracing::error!(%method, "ACP notification handler panicked");
    }
}

fn notification_method(notification: &IncomingNotification) -> &str {
    use crate::meta::client::SESSION_UPDATE;
    match notification {
        IncomingNotification::SessionUpdate { .. } => SESSION_UPDATE,
        IncomingNotification::ElicitationComplete { method, .. }
        | IncomingNotification::Ext { method, .. } => method,
    }
}

async fn handler_loop(
    mut rx: mpsc::UnboundedReceiver<InboundRequest>,
    inner: Arc<ConnectionInner>,
) {
    while let Some(request) = rx.recv().await {
        let callback = Arc::clone(&inner.on_request);
        let method = request.method.clone();
        let params = request.params.clone();
        let context = request.context.clone();
        // Run in place with catch_unwind rather than a detached spawn: a
        // handler can no longer outlive the connection that owns it.
        let joined = AssertUnwindSafe(async move { callback(method, params, context).await })
            .catch_unwind()
            .await;
        let extension = !crate::meta::is_client_core_method(&request.method)
            && !crate::meta::is_agent_core_method(&request.method);
        let result = match joined {
            Ok(result) => result,
            Err(_) => Err(AcpError::from(AcpRequestError::from_handler(
                AcpError::transport("handler panicked"),
                &request.method,
                extension,
            ))),
        };
        let error_for_body = match &result {
            Err(AcpError::Request(error)) => error.to_protocol_error(),
            Err(other) => AcpRequestError::from_handler(other.clone(), &request.method, extension)
                .to_protocol_error(),
            Ok(_) => Value::Null,
        };
        let body = match &result {
            Ok(value) => json!({
                "jsonrpc": "2.0",
                "id": request.id_value,
                "result": value,
            }),
            Err(_) => json!({
                "jsonrpc": "2.0",
                "id": request.id_value,
                "error": error_for_body,
            }),
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        if inner
            .enqueue(WriteJob {
                bytes: format!("{body}\n"),
                ack: Some(ack_tx),
            })
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    }
}

fn parse_rpc_error(error: &Value) -> (i64, String, Option<Value>) {
    if let Some(items) = error.get("data").and_then(Value::as_array) {
        for item in items {
            if item.get("_tag").and_then(Value::as_str) == Some("Fail")
                && let Some(inner) = item.get("error")
                && let Some(parsed) = plain_error(inner)
            {
                return parsed;
            }
        }
    }
    plain_error(error).unwrap_or((-32603, "Internal error".to_owned(), None))
}

fn plain_error(error: &Value) -> Option<(i64, String, Option<Value>)> {
    let code = error.get("code")?.as_i64()?;
    let message = error.get("message")?.as_str()?.to_owned();
    let data = error.get("data").filter(|data| !data.is_array()).cloned();
    Some((code, message, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::duplex;

    fn noop_request() -> RequestCallback {
        Arc::new(|method, _params, _ctx| {
            Box::pin(async move { Err(AcpError::from(AcpRequestError::method_not_found(&method))) })
        })
    }

    fn noop_notification() -> NotificationCallback {
        Arc::new(|_note| Box::pin(async {}))
    }

    fn start_pair(
        on_request: RequestCallback,
        on_notification: NotificationCallback,
    ) -> (Connection, tokio::io::DuplexStream, tokio::io::DuplexStream) {
        let (client_read, mut_agent_write) = duplex(1 << 20);
        let (agent_read, client_write) = duplex(1 << 20);
        let connection = Connection::start(
            client_read,
            client_write,
            ConnectionConfig {
                on_request,
                on_notification,
                on_termination: Arc::new(|_| {}),
                termination_on_eof: None,
                transform_session_update: Arc::new(|value| value),
            },
        );
        let _ = &mut_agent_write;
        (connection, agent_read, mut_agent_write)
    }

    async fn read_value(read: &mut tokio::io::DuplexStream) -> Value {
        use tokio::io::AsyncReadExt;
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            let n = read.read(&mut byte).await.unwrap();
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        serde_json::from_slice(line.trim_ascii()).unwrap()
    }

    async fn write_value(write: &mut tokio::io::DuplexStream, value: Value) {
        use tokio::io::AsyncWriteExt;
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        write.write_all(&bytes).await.unwrap();
    }

    #[tokio::test]
    async fn notification_has_no_id_and_extension_ids_start_at_one() {
        let (connection, mut read, _write) = start_pair(noop_request(), noop_notification());
        connection
            .notify("session/cancel", json!({"sessionId": "session-1"}))
            .await
            .unwrap();
        let outbound = read_value(&mut read).await;
        assert_eq!(
            outbound,
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "session-1"}})
        );
        let pending = tokio::spawn({
            let connection = connection.clone();
            async move {
                connection
                    .ext_request("x/test", json!({"hello": "world"}))
                    .await
            }
        });
        let outbound = read_value(&mut read).await;
        assert_eq!(outbound["id"], 1);
        assert_eq!(outbound["headers"], json!([]));
        assert_eq!(outbound["method"], "x/test");
        pending.abort();
    }

    #[tokio::test]
    async fn string_response_id_does_not_complete_a_numeric_request() {
        let (connection, mut read, mut write) = start_pair(noop_request(), noop_notification());
        let response = tokio::spawn({
            let connection = connection.clone();
            async move {
                connection
                    .ext_request("x/test", json!({"hello": "world"}))
                    .await
            }
        });
        let outbound = read_value(&mut read).await;
        assert_eq!(outbound["id"], 1);
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": "1", "result": {"ok": false}}),
        )
        .await;
        tokio::task::yield_now().await;
        assert!(!response.is_finished());
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}),
        )
        .await;
        assert_eq!(response.await.unwrap().unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn rpc_ids_do_not_collide_with_extension_ids() {
        let (connection, mut read, mut write) = start_pair(noop_request(), noop_notification());
        let rpc = tokio::spawn({
            let connection = connection.clone();
            async move { connection.rpc_request("initialize", json!({})).await }
        });
        let ext = tokio::spawn({
            let connection = connection.clone();
            async move {
                connection
                    .ext_request("x/test", json!({"hello": "world"}))
                    .await
            }
        });
        let first = read_value(&mut read).await;
        let second = read_value(&mut read).await;
        let ids = [
            first["id"].as_i64().unwrap(),
            second["id"].as_i64().unwrap(),
        ];
        assert!(ids.contains(&1));
        assert!(ids.contains(&RPC_ID_START));
        for frame in [first, second] {
            let id = frame["id"].clone();
            write_value(
                &mut write,
                json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}),
            )
            .await;
        }
        assert!(rpc.await.unwrap().is_ok());
        assert!(ext.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn inbound_request_is_answered_while_an_outbound_request_is_pending() {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_cb = Arc::clone(&hits);
        let on_request: RequestCallback = Arc::new(move |method, _params, ctx| {
            let hits_cb = Arc::clone(&hits_cb);
            Box::pin(async move {
                hits_cb.fetch_add(1, Ordering::SeqCst);
                assert_eq!(method, "session/request_permission");
                assert_eq!(ctx.request_id, "$t3:jsonrpc:number:0");
                Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow"}}))
            })
        });
        let (connection, mut read, mut write) = start_pair(on_request, noop_notification());
        let pending = tokio::spawn({
            let connection = connection.clone();
            async move {
                connection
                    .rpc_request("session/prompt", json!({"sessionId": "s"}))
                    .await
            }
        });
        let outbound = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "session/request_permission",
                "params": {"sessionId": "s"}
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["id"], 0);
        assert_eq!(reply["result"]["outcome"]["optionId"], "allow");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": outbound["id"], "result": {"stopReason": "end_turn"}}),
        )
        .await;
        assert_eq!(pending.await.unwrap().unwrap()["stopReason"], "end_turn");
    }

    #[tokio::test]
    async fn request_and_notification_of_the_same_method_stay_on_separate_paths() {
        let requests = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let requests_cb = Arc::clone(&requests);
        let notifications_cb = Arc::clone(&notifications);
        let on_request: RequestCallback = Arc::new(move |method, _params, _ctx| {
            let requests_cb = Arc::clone(&requests_cb);
            Box::pin(async move {
                assert_eq!(method, "cursor/update_todos");
                requests_cb.fetch_add(1, Ordering::SeqCst);
                Ok(json!({}))
            })
        });
        let on_notification: NotificationCallback = Arc::new(move |note| {
            let notifications_cb = Arc::clone(&notifications_cb);
            Box::pin(async move {
                if let IncomingNotification::Ext { method, .. } = note
                    && method == "cursor/update_todos"
                {
                    notifications_cb.fetch_add(1, Ordering::SeqCst);
                }
            })
        });
        let (connection, mut read, mut write) = start_pair(on_request, on_notification);
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "cursor/update_todos",
                "params": {"toolCallId": "t", "todos": [], "merge": false}
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["id"], 4);
        assert_eq!(reply["result"], json!({}));
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "cursor/update_todos",
                "params": {"toolCallId": "t"}
            }),
        )
        .await;
        let note = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            connection.next_notification(),
        )
        .await
        .unwrap();
        assert!(matches!(note, Some(IncomingNotification::Ext { .. })));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn standard_json_rpc_errors_become_request_failures() {
        let (connection, mut read, mut write) = start_pair(noop_request(), noop_notification());
        let pending = tokio::spawn({
            let connection = connection.clone();
            async move { connection.ext_request("x/auth", json!({})).await }
        });
        let outbound = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": outbound["id"],
                "error": {"code": -32000, "message": "Authentication required"}
            }),
        )
        .await;
        let error = pending.await.unwrap().unwrap_err();
        let AcpError::Request(error) = error else {
            panic!("expected request error");
        };
        assert_eq!(error.code, -32000);
        assert_eq!(error.error_message, "Authentication required");
    }

    #[tokio::test]
    async fn invalid_session_update_ends_the_connection_without_copying_the_payload() {
        let secret = "acp-core-notification-secret-sentinel";
        let (connection, _read, mut write) = start_pair(noop_request(), noop_notification());
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": {"secret": secret}, "update": {"sessionUpdate": "plan", "entries": []}}
            }),
        )
        .await;
        let note = connection.next_notification().await;
        assert!(note.is_none());
        let error = connection.termination_error().unwrap();
        let text = error.to_string();
        assert!(!text.contains(secret));
        assert!(text.contains("decode-notification-payload"));
    }

    // Needs a multi-thread runtime: on current_thread the consumer cannot run
    // between the queue check and the await, so the race never opens.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn next_notification_does_not_miss_a_push_racing_the_wait() {
        let (connection, _read, _write) = start_pair(noop_request(), noop_notification());
        // The window between the queue check and the await is a few instructions
        // wide. The QC probe needed about 200k rounds to hit it reliably.
        for index in 0..200_000 {
            let consumer = tokio::spawn({
                let connection = connection.clone();
                async move { connection.next_notification().await }
            });
            connection
                .inner
                .push_notification(IncomingNotification::Ext {
                    method: "x/race".to_owned(),
                    params: json!({"index": index}),
                });
            let note = tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
                .await
                .expect("consumer missed a push")
                .unwrap();
            assert!(matches!(note, Some(IncomingNotification::Ext { .. })));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn next_notification_wakes_on_termination_racing_the_wait() {
        for _ in 0..1_000 {
            let (connection, _read, _write) = start_pair(noop_request(), noop_notification());
            let consumer = tokio::spawn({
                let connection = connection.clone();
                async move { connection.next_notification().await }
            });
            connection
                .inner
                .terminate(AcpError::transport("connection closed for test"));
            let note = tokio::time::timeout(std::time::Duration::from_secs(2), consumer)
                .await
                .expect("consumer missed the termination wakeup")
                .unwrap();
            assert!(note.is_none());
        }
    }

    #[tokio::test]
    async fn dropping_the_connection_closes_the_outbound_stream() {
        use tokio::io::AsyncReadExt;
        let (connection, mut read, _write) = start_pair(noop_request(), noop_notification());
        connection
            .notify("session/cancel", json!({"sessionId": "s"}))
            .await
            .unwrap();
        let _ = read_value(&mut read).await;
        drop(connection);
        // Before the owner split, the writer kept the stream open forever.
        let mut buf = [0_u8; 16];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), read.read(&mut buf))
            .await
            .expect("writer task still holds the outbound stream after drop")
            .unwrap();
        assert_eq!(n, 0);
    }

    fn session_update_frame() -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": "s1", "update": {"sessionUpdate": "plan", "entries": []}}
        })
    }

    #[tokio::test]
    async fn panicking_notification_callback_keeps_the_reader_alive() {
        let on_notification: NotificationCallback = Arc::new(|note| {
            Box::pin(async move {
                if let IncomingNotification::SessionUpdate { .. } = note {
                    panic!("notification callback defect");
                }
            })
        });
        let (connection, mut read, mut write) = start_pair(noop_request(), on_notification);
        let pending = tokio::spawn({
            let connection = connection.clone();
            async move { connection.ext_request("x/test", json!({})).await }
        });
        let outbound = read_value(&mut read).await;
        write_value(&mut write, session_update_frame()).await;
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": outbound["id"], "result": {"ok": true}}),
        )
        .await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
            .await
            .expect("pending request hung after a callback panic")
            .unwrap();
        assert_eq!(result.unwrap()["ok"], true);
        assert!(connection.termination_error().is_none());
    }

    #[tokio::test]
    async fn transform_defect_terminates_the_connection_and_fails_pending_requests() {
        let terminations = Arc::new(AtomicUsize::new(0));
        let terminations_cb = Arc::clone(&terminations);
        let (client_read, mut agent_write) = duplex(1 << 20);
        let (mut agent_read, client_write) = duplex(1 << 20);
        let connection = Connection::start(
            client_read,
            client_write,
            ConnectionConfig {
                on_request: noop_request(),
                on_notification: noop_notification(),
                on_termination: Arc::new(move |_| {
                    terminations_cb.fetch_add(1, Ordering::SeqCst);
                }),
                termination_on_eof: None,
                transform_session_update: Arc::new(|_| panic!("normalizer bug")),
            },
        );
        let pending = tokio::spawn({
            let connection = connection.clone();
            async move { connection.ext_request("x/test", json!({})).await }
        });
        // Wait for the request to reach the wire, then feed a session/update to the reader.
        let _outbound = read_value(&mut agent_read).await;
        write_value(&mut agent_write, session_update_frame()).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), pending)
            .await
            .expect("pending request hung after a transform defect")
            .unwrap();
        let Err(AcpError::Transport(error)) = result else {
            panic!("expected a transport termination error");
        };
        assert_eq!(error.operation, Some(TransportOperation::ReadInputStream));
        assert!(connection.termination_error().is_some());
        assert_eq!(terminations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sliding_buffer_keeps_the_newest_thirty_two_notifications() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen_cb = Arc::clone(&seen);
        let on_notification: NotificationCallback = Arc::new(move |_note| {
            let seen_cb = Arc::clone(&seen_cb);
            Box::pin(async move {
                seen_cb.fetch_add(1, Ordering::SeqCst);
            })
        });
        let (connection, _read, mut write) = start_pair(noop_request(), on_notification);
        for index in 0..64 {
            write_value(
                &mut write,
                json!({"jsonrpc": "2.0", "method": "x/performance", "params": {"index": index}}),
            )
            .await;
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while seen.load(Ordering::SeqCst) < 64 {
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "reader observed {} of 64 notifications",
                    seen.load(Ordering::SeqCst)
                );
            }
            tokio::task::yield_now().await;
        }
        let mut retained = Vec::new();
        for _ in 0..32 {
            retained.push(connection.next_notification().await.unwrap());
        }
        match &retained[0] {
            IncomingNotification::Ext { params, .. } => assert_eq!(params["index"], 32),
            other => panic!("unexpected {other:?}"),
        }
        match retained.last().unwrap() {
            IncomingNotification::Ext { params, .. } => assert_eq!(params["index"], 63),
            other => panic!("unexpected {other:?}"),
        }
    }
}
