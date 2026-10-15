//! Port of upstream/packages/effect-codex-app-server/src/client.ts
use crate::{
    errors::{AppServerError, ProtocolError},
    generated::meta::{self, MethodMeta},
    protocol::{NotificationHandler, Protocol, RequestHandler},
    schema,
};
use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::{Child, Command},
    task::JoinHandle,
};

type ServerRequestHandler = Arc<
    dyn Fn(Option<Value>) -> BoxFuture<'static, Result<Option<Value>, AppServerError>>
        + Send
        + Sync,
>;
type ServerNotificationHandler =
    Arc<dyn Fn(Option<Value>) -> BoxFuture<'static, Result<(), AppServerError>> + Send + Sync>;

#[derive(Default)]
struct Handlers {
    requests: Mutex<HashMap<String, ServerRequestHandler>>,
    notifications: Mutex<HashMap<String, Vec<ServerNotificationHandler>>>,
    unknown_request: Mutex<Option<RequestHandler>>,
    unknown_notification: Mutex<Option<NotificationHandler>>,
}

pub struct Client {
    pub raw: Protocol,
    handlers: Arc<Handlers>,
    child: Option<Child>,
    stderr_task: Option<JoinHandle<()>>,
}

impl Client {
    pub fn from_io<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let raw = Protocol::new(reader, writer);
        let handlers = Arc::new(Handlers::default());
        let requests = handlers.clone();
        raw.set_request_handler(Arc::new(move |method, params| {
            let requests = requests.clone();
            async move {
                let handler = requests.requests.lock().get(&method).cloned();
                let unknown = requests.unknown_request.lock().clone();
                if let Some(handler) = handler {
                    let entry = meta::method_meta(meta::SERVER_REQUEST_METHODS, &method);
                    validate_known(
                        &method,
                        entry.and_then(|entry| entry.params),
                        params.as_ref(),
                    )
                    .map_err(invalid_params)?;
                    let result = handler(params).await.map_err(|error| match error {
                        AppServerError::Request { error, .. } => *error,
                        other => ProtocolError {
                            code: -32603,
                            message: format!(
                                "Codex App Server request handler failed for method '{method}'"
                            ),
                            data: Some(Value::String(other.to_string())),
                        },
                    })?;
                    validate_known(
                        &method,
                        entry.and_then(|entry| entry.response),
                        result.as_ref(),
                    )
                    .map_err(invalid_params)?;
                    Ok(result)
                } else if let Some(handler) = unknown {
                    handler(method, params).await
                } else {
                    Err(ProtocolError::method_not_found(&method))
                }
            }
            .boxed()
        }));
        let notifications = handlers.clone();
        raw.set_notification_handler(Arc::new(move |method, params| {
            let notifications = notifications.clone();
            async move {
                let handlers = notifications.notifications.lock().get(&method).cloned();
                let unknown = notifications.unknown_notification.lock().clone();
                if let Some(handlers) = handlers {
                    let entry = meta::method_meta(meta::SERVER_NOTIFICATION_METHODS, &method);
                    if validate_known(
                        &method,
                        entry.and_then(|entry| entry.params),
                        params.as_ref(),
                    )
                    .is_err()
                    {
                        return;
                    }
                    for handler in handlers {
                        let _ = handler(params.clone()).await;
                    }
                } else if let Some(handler) = unknown {
                    handler(method, params).await;
                }
            }
            .boxed()
        }));
        Self {
            raw,
            handlers,
            child: None,
            stderr_task: None,
        }
    }
    pub fn spawn(command: &str, args: &[&str], cwd: Option<&Path>) -> Result<Self, AppServerError> {
        let mut process = Command::new(command);
        process
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.as_std_mut().process_group(0);
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        let mut child = process
            .spawn()
            .map_err(|error| AppServerError::Spawn(error.to_string()))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let mut client = Self::from_io(stdout, stdin);
        if let Some(mut stderr) = child.stderr.take() {
            client.stderr_task = Some(tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            }));
        }
        client.child = Some(child);
        Ok(client)
    }
    pub fn spawn_codex(cwd: Option<&Path>) -> Result<Self, AppServerError> {
        Self::spawn("codex", &["app-server"], cwd)
    }
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Option<Value>, AppServerError> {
        let entry = known_method(meta::CLIENT_REQUEST_METHODS, method)?;
        validate_known(method, entry.params, params.as_ref()).map_err(|detail| {
            AppServerError::InvalidPayload {
                method: method.into(),
                detail,
            }
        })?;
        let result = self.raw.request(method, params).await?;
        validate_known(method, entry.response, result.as_ref()).map_err(|detail| {
            AppServerError::InvalidPayload {
                method: method.into(),
                detail,
            }
        })?;
        Ok(result)
    }
    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), AppServerError> {
        let entry = known_method(meta::CLIENT_NOTIFICATION_METHODS, method)?;
        validate_known(method, entry.params, params.as_ref()).map_err(|detail| {
            AppServerError::InvalidPayload {
                method: method.into(),
                detail,
            }
        })?;
        self.raw.notify(method, params)
    }
    pub fn handle_server_request<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Option<Value>, AppServerError>> + Send + 'static,
    {
        self.handlers.requests.lock().insert(
            method.into(),
            Arc::new(move |params| handler(params).boxed()),
        );
    }
    pub fn handle_server_notification<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Option<Value>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), AppServerError>> + Send + 'static,
    {
        self.handlers
            .notifications
            .lock()
            .entry(method.into())
            .or_default()
            .push(Arc::new(move |params| handler(params).boxed()));
    }
    pub fn handle_unknown_server_request(&self, handler: RequestHandler) {
        *self.handlers.unknown_request.lock() = Some(handler);
    }
    pub fn handle_unknown_server_notification(&self, handler: NotificationHandler) {
        *self.handlers.unknown_notification.lock() = Some(handler);
    }
    pub async fn shutdown(mut self) -> Result<(), AppServerError> {
        self.raw.terminate(AppServerError::Cancelled);
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
            }
            #[cfg(not(unix))]
            child
                .start_kill()
                .map_err(|error| AppServerError::Transport {
                    operation: "read-process-exit-status",
                    detail: error.to_string(),
                })?;
            if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                child
                    .start_kill()
                    .map_err(|error| AppServerError::Transport {
                        operation: "read-process-exit-status",
                        detail: error.to_string(),
                    })?;
                child
                    .wait()
                    .await
                    .map_err(|error| AppServerError::Transport {
                        operation: "read-process-exit-status",
                        detail: error.to_string(),
                    })?;
            }
        }
        Ok(())
    }
}
fn known_method(
    methods: &'static [MethodMeta],
    method: &str,
) -> Result<&'static MethodMeta, AppServerError> {
    meta::method_meta(methods, method).ok_or_else(|| AppServerError::InvalidPayload {
        method: method.into(),
        detail: "unknown method".into(),
    })
}
fn validate_known(
    _method: &str,
    schema_name: Option<&str>,
    payload: Option<&Value>,
) -> Result<(), String> {
    schema::validate_optional(schema_name, payload)
}
fn invalid_params(detail: String) -> ProtocolError {
    ProtocolError {
        code: -32602,
        message: format!("Invalid params: {detail}"),
        data: None,
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.as_ref().and_then(Child::id) {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}
