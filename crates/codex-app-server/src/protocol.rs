//! Port of upstream/packages/effect-codex-app-server/src/protocol.ts
use crate::errors::{AppServerError, ProtocolError, payload_kind};
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
    sync::{Notify, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::task::AbortOnDropHandle;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Raw,
    Decoded,
    DecodeFailed,
}
#[derive(Debug, Clone)]
pub struct ProtocolLogEvent {
    pub direction: Direction,
    pub stage: Stage,
    pub payload: Value,
}
pub type ProtocolLogger = Arc<dyn Fn(ProtocolLogEvent) + Send + Sync>;
#[derive(Clone, Default)]
pub struct ProtocolOptions {
    pub log_incoming: bool,
    pub log_outgoing: bool,
    pub logger: Option<ProtocolLogger>,
    pub on_termination: Option<Arc<dyn Fn(AppServerError) + Send + Sync>>,
}

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
    options: ProtocolOptions,
    stop: watch::Sender<bool>,
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
        let _ = self.stop.send(true);
        drop(failure);
        if let Some(callback) = &self.options.on_termination {
            callback(error);
        }
    }
    fn log(&self, event: ProtocolLogEvent) {
        if (event.direction == Direction::Incoming && !self.options.log_incoming)
            || (event.direction == Direction::Outgoing && !self.options.log_outgoing)
        {
            return;
        }
        if let Some(logger) = &self.options.logger {
            logger(event);
        } else {
            tracing::debug!(?event, "Codex App Server protocol event");
        }
    }
    fn send(&self, value: Value) -> Result<(), AppServerError> {
        if let Some(error) = self.failure.lock().clone() {
            return Err(error);
        }
        self.log(ProtocolLogEvent {
            direction: Direction::Outgoing,
            stage: Stage::Decoded,
            payload: value.clone(),
        });
        self.outgoing
            .send(value)
            .map_err(|_| AppServerError::InputStreamEnded)
    }
}

pub struct Protocol {
    state: Arc<State>,
    notifications: Arc<SlidingQueue>,
    requests: Arc<SlidingQueue>,
    _tasks: Vec<AbortOnDropHandle<()>>,
}

#[derive(Clone)]
pub struct TerminationHandle(Arc<State>);
impl TerminationHandle {
    pub fn terminate(&self, error: AppServerError) {
        self.0.fail(error);
    }
}

impl Protocol {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::new_with_process_exit_and_options(reader, writer, None, ProtocolOptions::default())
    }
    pub fn new_with_options<R, W>(reader: R, writer: W, options: ProtocolOptions) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::new_with_process_exit_and_options(reader, writer, None, options)
    }
    pub fn new_with_process_exit<R, W>(
        reader: R,
        writer: W,
        exit: Option<watch::Receiver<Option<AppServerError>>>,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::new_with_process_exit_and_options(reader, writer, exit, ProtocolOptions::default())
    }
    pub fn new_with_process_exit_and_options<R, W>(
        reader: R,
        writer: W,
        exit: Option<watch::Receiver<Option<AppServerError>>>,
        options: ProtocolOptions,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, mut output) = mpsc::unbounded_channel::<Value>();
        let (stop, mut stop_rx) = watch::channel(false);
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
            options,
            stop,
        });
        let read_state = state.clone();
        let read_task = tokio::spawn(async move {
            let mut exit = exit;
            let mut lines = BufReader::new(reader).lines();
            loop {
                if *stop_rx.borrow() {
                    break;
                }
                let line = tokio::select! {
                    biased;
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            break;
                        }
                        continue;
                    }
                    line = lines.next_line() => line,
                };
                match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        read_state.log(ProtocolLogEvent {
                            direction: Direction::Incoming,
                            stage: Stage::Raw,
                            payload: Value::String(line.clone()),
                        });
                        match serde_json::from_str::<Value>(&line) {
                            Ok(value) => {
                                read_state.log(ProtocolLogEvent {
                                    direction: Direction::Incoming,
                                    stage: Stage::Decoded,
                                    payload: value.clone(),
                                });
                                if let Err(error) = route(&read_state, value).await {
                                    read_state.log(ProtocolLogEvent {
                                        direction: Direction::Incoming,
                                        stage: Stage::DecodeFailed,
                                        payload: json!({ "operation": error_operation(&error) }),
                                    });
                                    read_state.fail(error);
                                    break;
                                }
                            }
                            Err(error) => {
                                read_state.log(ProtocolLogEvent {
                                    direction: Direction::Incoming,
                                    stage: Stage::DecodeFailed,
                                    payload: json!({"operation":"decode-wire-message"}),
                                });
                                read_state.fail(AppServerError::protocol_parse(
                                    "decode-wire-message",
                                    error.to_string(),
                                ));
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        let error = if let Some(exit) = exit.as_mut() {
                            loop {
                                if *stop_rx.borrow() {
                                    break read_state
                                        .failure
                                        .lock()
                                        .clone()
                                        .unwrap_or(AppServerError::InputStreamEnded);
                                }
                                if let Some(error) = exit.borrow().clone() {
                                    break error;
                                }
                                tokio::select! {
                                    biased;
                                    _ = stop_rx.changed() => {}
                                    changed = exit.changed() => {
                                        if changed.is_err() {
                                            break AppServerError::InputStreamEnded;
                                        }
                                    }
                                }
                            }
                        } else {
                            AppServerError::InputStreamEnded
                        };
                        read_state.fail(error);
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
                        write_state.fail(AppServerError::protocol_parse(
                            "encode-wire-message",
                            error.to_string(),
                        ));
                        break;
                    }
                };
                bytes.push(b'\n');
                write_state.log(ProtocolLogEvent {
                    direction: Direction::Outgoing,
                    stage: Stage::Raw,
                    payload: Value::String(String::from_utf8_lossy(&bytes).into_owned()),
                });
                if let Err(error) = writer.write_all(&bytes).await {
                    write_state.fail(AppServerError::Transport {
                        operation: "write-output-stream",
                        detail: error.to_string(),
                    });
                    break;
                }
                let _ = writer.flush().await;
            }
        });
        Self {
            state,
            notifications,
            requests,
            _tasks: vec![
                AbortOnDropHandle::new(read_task),
                AbortOnDropHandle::new(write_task),
            ],
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
    pub fn termination_handle(&self) -> TerminationHandle {
        TerminationHandle(self.state.clone())
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
    }
}

fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
async fn route(state: &Arc<State>, value: Value) -> Result<(), AppServerError> {
    if state.failure.lock().is_some() {
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        return Err(unroutable(&value));
    };
    let id = object.get("id");
    let method = object.get("method").and_then(Value::as_str);
    if let Some(method) = method {
        let message = IncomingMessage {
            method: method.into(),
            params: object.get("params").cloned(),
            id: id.cloned(),
        };
        if object.contains_key("id") {
            let Some(id) = id else {
                return Err(unroutable(&value));
            };
            if id_key(id).is_none() {
                return Err(unroutable(&value));
            }
            state.requests.push(message.clone());
            let handler = state.request_handler.lock().clone();
            if let Some(handler) = handler {
                if let Some(guard) = ActiveGuard::try_new(state) {
                    let handler_state = state.clone();
                    let id = id.clone();
                    let mut handlers = state.handlers.lock();
                    while handlers.try_join_next().is_some() {}
                    handlers.spawn(async move {
                        let _guard = guard;
                        let reply = handler(message.method, message.params).await;
                        let frame = match reply {
                            Ok(Some(result)) => json!({"id": id, "result": result}),
                            Ok(None) => json!({"id": id}),
                            Err(error) => json!({"id": id, "error": error}),
                        };
                        if let Err(error) = handler_state.send(frame) {
                            handler_state.fail(error);
                        }
                    });
                } else {
                    state.send(json!({"id": id, "error": ProtocolError::overloaded()}))?;
                }
            }
        } else {
            state.notifications.push(message.clone());
            let handler = state.notification_handler.lock().clone();
            if let Some(handler) = handler {
                handler(message.method, message.params).await;
            }
        }
        return Ok(());
    }
    if let Some(id) = id.and_then(id_key) {
        if object.contains_key("method") {
            return Err(unroutable(&value));
        }
        let parsed_error = match object.get("error") {
            Some(error_value) => Some(
                serde_json::from_value::<ProtocolError>(error_value.clone())
                    .map_err(|_| unroutable(&value))?,
            ),
            None => None,
        };
        if let Some((method, tx)) = state.pending.lock().remove(&id) {
            let reply = match parsed_error {
                Some(error) => Err(AppServerError::Request {
                    method,
                    request_id: id,
                    error: Box::new(error),
                }),
                None => Ok(object.get("result").cloned()),
            };
            let _ = tx.send(reply);
        }
        return Ok(());
    }
    Err(unroutable(&value))
}

struct ActiveGuard(Arc<State>);

impl ActiveGuard {
    fn try_new(state: &Arc<State>) -> Option<Self> {
        let previous = state.active_handlers.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_ACTIVE_REQUEST_HANDLERS {
            state.active_handlers.fetch_sub(1, Ordering::AcqRel);
            None
        } else {
            Some(Self(state.clone()))
        }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active_handlers.fetch_sub(1, Ordering::AcqRel);
    }
}

fn unroutable(value: &Value) -> AppServerError {
    let kind = payload_kind(value);
    let Some(object) = value.as_object() else {
        return AppServerError::ProtocolParse(Box::new(crate::errors::ProtocolParseInfo {
            operation: "route-wire-message",
            detail: String::new(),
            method: None,
            request_id: None,
            payload_kind: Some(kind),
            present_fields: None,
        }));
    };
    const FIELDS: [&str; 5] = ["id", "method", "params", "result", "error"];
    let present_fields = FIELDS
        .into_iter()
        .filter(|field| object.contains_key(*field))
        .collect();
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let request_id = object.get("id").and_then(id_key);
    AppServerError::ProtocolParse(Box::new(crate::errors::ProtocolParseInfo {
        operation: "route-wire-message",
        detail: String::new(),
        method,
        request_id,
        payload_kind: Some(kind),
        present_fields: Some(present_fields),
    }))
}

fn error_operation(error: &AppServerError) -> &'static str {
    match error {
        AppServerError::ProtocolParse(info) => info.operation,
        _ => "route-wire-message",
    }
}
