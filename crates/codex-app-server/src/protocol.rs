//! Port of upstream/packages/effect-codex-app-server/src/protocol.ts
use crate::errors::{AppServerError, ProtocolError};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Notify, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};

type Reply = Result<Option<Value>, AppServerError>;
pub type RequestHandler = Arc<
    dyn Fn(String, Option<Value>) -> BoxFuture<'static, Result<Option<Value>, ProtocolError>>
        + Send
        + Sync,
>;
pub type NotificationHandler =
    Arc<dyn Fn(String, Option<Value>) -> BoxFuture<'static, ()> + Send + Sync>;
const MAX_ACTIVE_REQUEST_HANDLERS: usize = 32;
const MAX_BUFFERED_RAW_MESSAGES: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct IncomingMessage {
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

struct SlidingQueue {
    values: Mutex<VecDeque<IncomingMessage>>,
    ready: Notify,
}
impl SlidingQueue {
    fn new() -> Self {
        Self {
            values: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
        }
    }
    fn push(&self, value: IncomingMessage) {
        let mut values = self.values.lock();
        if values.len() == MAX_BUFFERED_RAW_MESSAGES {
            values.pop_front();
        }
        values.push_back(value);
        drop(values);
        self.ready.notify_one();
    }
    async fn next(&self) -> IncomingMessage {
        loop {
            let notified = self.ready.notified();
            if let Some(value) = self.values.lock().pop_front() {
                return value;
            }
            notified.await;
        }
    }
}

struct State {
    pending: Mutex<HashMap<String, (String, oneshot::Sender<Reply>)>>,
    failure: Mutex<Option<AppServerError>>,
    request_handler: Mutex<Option<RequestHandler>>,
    notification_handler: Mutex<Option<NotificationHandler>>,
    handlers: Mutex<JoinSet<()>>,
    active_handlers: AtomicUsize,
    next_id: AtomicU64,
    outgoing: mpsc::UnboundedSender<Value>,
    notifications: Arc<SlidingQueue>,
    requests: Arc<SlidingQueue>,
}

impl State {
    fn fail(&self, error: AppServerError) {
        let mut failure = self.failure.lock();
        if failure.is_some() {
            return;
        }
        *failure = Some(error.clone());
        for (_, (_, sender)) in self.pending.lock().drain() {
            let _ = sender.send(Err(error.clone()));
        }
        self.handlers.lock().abort_all();
    }
    fn send(&self, value: Value) -> Result<(), AppServerError> {
        if let Some(error) = self.failure.lock().clone() {
            return Err(error);
        }
        self.outgoing
            .send(value)
            .map_err(|_| AppServerError::InputStreamEnded)
    }
}

pub struct Protocol {
    state: Arc<State>,
    notifications: Arc<SlidingQueue>,
    requests: Arc<SlidingQueue>,
    tasks: Vec<JoinHandle<()>>,
}

impl Protocol {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, mut output) = mpsc::unbounded_channel::<Value>();
        let notifications = Arc::new(SlidingQueue::new());
        let requests = Arc::new(SlidingQueue::new());
        let state = Arc::new(State {
            pending: Mutex::new(HashMap::new()),
            failure: Mutex::new(None),
            request_handler: Mutex::new(None),
            notification_handler: Mutex::new(None),
            handlers: Mutex::new(JoinSet::new()),
            active_handlers: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
            outgoing,
            notifications: notifications.clone(),
            requests: requests.clone(),
        });
        let read_state = state.clone();
        let read_task = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(value) => {
                                if let Err(error) = route(&read_state, value) {
                                    read_state.fail(error);
                                    break;
                                }
                            }
                            Err(error) => {
                                read_state.fail(AppServerError::ProtocolParse {
                                    operation: "decode-wire-message",
                                    detail: error.to_string(),
                                });
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        read_state.fail(AppServerError::InputStreamEnded);
                        break;
                    }
                    Err(error) => {
                        read_state.fail(AppServerError::Transport {
                            operation: "read-input-stream",
                            detail: error.to_string(),
                        });
                        break;
                    }
                }
            }
        });
        let write_state = state.clone();
        let write_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(value) = output.recv().await {
                let mut bytes = match serde_json::to_vec(&value) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        write_state.fail(AppServerError::ProtocolParse {
                            operation: "encode-wire-message",
                            detail: error.to_string(),
                        });
                        break;
                    }
                };
                bytes.push(b'\n');
                if let Err(error) = writer.write_all(&bytes).await {
                    write_state.fail(AppServerError::Transport {
                        operation: "write-output-stream",
                        detail: error.to_string(),
                    });
                    break;
                }
            }
        });
        Self {
            state,
            notifications,
            requests,
            tasks: vec![read_task, write_task],
        }
    }
    pub fn set_request_handler(&self, handler: RequestHandler) {
        *self.state.request_handler.lock() = Some(handler);
    }
    pub fn set_notification_handler(&self, handler: NotificationHandler) {
        *self.state.notification_handler.lock() = Some(handler);
    }
    pub async fn next_notification(&self) -> Option<IncomingMessage> {
        Some(self.notifications.next().await)
    }
    pub async fn next_request(&self) -> Option<IncomingMessage> {
        Some(self.requests.next().await)
    }
    pub async fn request(&self, method: &str, params: Option<Value>) -> Reply {
        let id = self.state.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.state
            .pending
            .lock()
            .insert(key.clone(), (method.into(), tx));
        let _guard = PendingGuard {
            state: &self.state,
            id: key.clone(),
        };
        let mut message = Map::new();
        message.insert("id".into(), json!(id));
        message.insert("method".into(), json!(method));
        if let Some(params) = params {
            message.insert("params".into(), params);
        }
        self.state.send(Value::Object(message))?;
        rx.await.unwrap_or(Err(AppServerError::Cancelled))
    }
    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), AppServerError> {
        let mut message = Map::new();
        message.insert("method".into(), json!(method));
        if let Some(params) = params {
            message.insert("params".into(), params);
        }
        self.state.send(Value::Object(message))
    }
    pub fn respond(&self, id: Value, result: Option<Value>) -> Result<(), AppServerError> {
        let mut message = Map::new();
        message.insert("id".into(), id);
        if let Some(result) = result {
            message.insert("result".into(), result);
        }
        self.state.send(Value::Object(message))
    }
    pub fn respond_error(&self, id: Value, error: ProtocolError) -> Result<(), AppServerError> {
        self.state.send(json!({"id": id, "error": error}))
    }
    pub fn terminate(&self, error: AppServerError) {
        self.state.fail(error);
    }
    #[cfg(test)]
    pub(crate) fn notifications_len_for_test(&self) -> usize {
        self.notifications.values.lock().len()
    }
    #[cfg(test)]
    pub(crate) fn last_notification_index_for_test(&self) -> Option<u64> {
        self.notifications
            .values
            .lock()
            .back()?
            .params
            .as_ref()?
            .get("index")?
            .as_u64()
    }
}

struct PendingGuard<'a> {
    state: &'a State,
    id: String,
}
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.state.pending.lock().remove(&self.id);
    }
}
impl Drop for Protocol {
    fn drop(&mut self) {
        self.state.fail(AppServerError::Cancelled);
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
fn route(state: &Arc<State>, value: Value) -> Result<(), AppServerError> {
    let object = value
        .as_object()
        .ok_or_else(|| AppServerError::ProtocolParse {
            operation: "route-wire-message",
            detail: "expected object".into(),
        })?;
    let id = object.get("id");
    let method = object.get("method").and_then(Value::as_str);
    if let Some(method) = method {
        let message = IncomingMessage {
            method: method.into(),
            params: object.get("params").cloned(),
            id: id.cloned(),
        };
        if let Some(id) = id {
            if id_key(id).is_none() {
                return Err(AppServerError::ProtocolParse {
                    operation: "route-wire-message",
                    detail: "invalid request id".into(),
                });
            }
            state.requests.push(message.clone());
            if let Some(handler) = state.request_handler.lock().clone() {
                if state.active_handlers.fetch_add(1, Ordering::AcqRel)
                    >= MAX_ACTIVE_REQUEST_HANDLERS
                {
                    state.active_handlers.fetch_sub(1, Ordering::AcqRel);
                    state.send(json!({"id": id, "error": ProtocolError::overloaded()}))?;
                } else {
                    let state = state.clone();
                    let handler_state = state.clone();
                    let id = id.clone();
                    let mut handlers = state.handlers.lock();
                    while handlers.try_join_next().is_some() {}
                    handlers.spawn(async move {
                        let reply = handler(message.method, message.params).await;
                        let frame = match reply {
                            Ok(Some(result)) => json!({"id": id, "result": result}),
                            Ok(None) => json!({"id": id}),
                            Err(error) => json!({"id": id, "error": error}),
                        };
                        let _ = handler_state.send(frame);
                        handler_state.active_handlers.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            }
        } else {
            state.notifications.push(message.clone());
            if let Some(handler) = state.notification_handler.lock().clone() {
                let mut handlers = state.handlers.lock();
                while handlers.try_join_next().is_some() {}
                handlers.spawn(async move {
                    handler(message.method, message.params).await;
                });
            }
        }
        return Ok(());
    }
    if let Some(id) = id.and_then(id_key) {
        if let Some((method, tx)) = state.pending.lock().remove(&id) {
            let reply = match object.get("error") {
                Some(value) => match serde_json::from_value::<ProtocolError>(value.clone()) {
                    Ok(error) => Err(AppServerError::Request {
                        method,
                        request_id: id,
                        error: Box::new(error),
                    }),
                    Err(error) => Err(AppServerError::ProtocolParse {
                        operation: "route-wire-message",
                        detail: error.to_string(),
                    }),
                },
                None => Ok(object.get("result").cloned()),
            };
            let _ = tx.send(reply);
        }
        return Ok(());
    }
    Err(AppServerError::ProtocolParse {
        operation: "route-wire-message",
        detail: "unroutable message".into(),
    })
}
