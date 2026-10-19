//! Port of upstream/packages/effect-codex-app-server/src/client.ts
//! Also: Port of upstream/packages/effect-codex-app-server/src/_internal/shared.ts.
//! Also: Port of upstream/packages/effect-codex-app-server/src/_internal/stdio.ts.
use crate::{
    errors::{AppServerError, ProtocolError, payload_kind},
    generated::meta::{self, MethodMeta},
    protocol::{NotificationHandler, Protocol, ProtocolOptions, RequestHandler},
    schema,
};
use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::Command,
    sync::watch,
};
use tokio_util::task::AbortOnDropHandle;

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
    // Process-group id of the spawned child (0 when there is none). Recorded once at spawn and
    // never cleared: descendants can outlive the leader, so the group must still be signalled
    // after the leader has exited.
    child_pgid: u32,
    child_task: Option<AbortOnDropHandle<()>>,
    stderr_task: Option<AbortOnDropHandle<()>>,
}

impl Client {
    pub fn from_io<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_io_with_exit(reader, writer, None, ProtocolOptions::default())
    }
    pub fn from_io_with_options<R, W>(reader: R, writer: W, options: ProtocolOptions) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_io_with_exit(reader, writer, None, options)
    }
    pub(crate) fn from_io_with_exit<R, W>(
        reader: R,
        writer: W,
        exit: Option<watch::Receiver<Option<AppServerError>>>,
        options: ProtocolOptions,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let raw = Protocol::new_with_process_exit_and_options(reader, writer, exit, options);
        let handlers = Arc::new(Handlers::default());
        let requests = handlers.clone();
        raw.set_request_handler(Arc::new(move |method, params| {
            let requests = requests.clone();
            async move {
                // Mirrors upstream dispatchRequest: only protocol-known methods reach the
                // registered handlers, and a known method without one answers -32601 rather
                // than falling through to the unknown-request handler.
                let Some(entry) = meta::method_meta(meta::SERVER_REQUEST_METHODS, &method) else {
                    let unknown = requests.unknown_request.lock().clone();
                    return match unknown {
                        Some(handler) => handler(method, params).await,
                        None => Err(ProtocolError::method_not_found(&method)),
                    };
                };
                check_payload(&method, "decode-payload", entry.params, params.as_ref())?;
                let handler = requests.requests.lock().get(&method).cloned();
                let Some(handler) = handler else {
                    return Err(ProtocolError::method_not_found(&method));
                };
                let result = handler(params).await.map_err(|error| match error {
                    AppServerError::Request { error, .. } => *error,
                    // Upstream keeps the cause local (internalError with no data), so the
                    // peer only sees the generic message and the detail goes to the log.
                    other => {
                        tracing::warn!(%method, error = %other, "Codex App Server request handler failed");
                        ProtocolError {
                            code: -32603,
                            message: format!(
                                "Codex App Server request handler failed for method '{method}'"
                            ),
                            data: None,
                        }
                    }
                })?;
                check_payload(&method, "encode-payload", entry.response, result.as_ref())?;
                Ok(result)
            }
            .boxed()
        }));
        let notifications = handlers.clone();
        raw.set_notification_handler(Arc::new(move |method, params| {
            let notifications = notifications.clone();
            async move {
                // Mirrors upstream dispatchNotification: a protocol-known notification only
                // reaches its registered handlers (none is a no-op), and everything else goes to
                // the unknown-notification handler.
                let Some(entry) = meta::method_meta(meta::SERVER_NOTIFICATION_METHODS, &method)
                else {
                    let unknown = notifications.unknown_notification.lock().clone();
                    if let Some(handler) = unknown {
                        handler(method, params).await;
                    }
                    return;
                };
                let handlers = notifications
                    .notifications
                    .lock()
                    .get(&method)
                    .cloned()
                    .unwrap_or_default();
                if validate_known(&method, entry.params, params.as_ref()).is_err() {
                    return;
                }
                // Upstream runs these with Effect.forEach, which stops at the first failure and
                // then the outer Effect.ignore discards it.
                for handler in handlers {
                    if handler(params.clone()).await.is_err() {
                        break;
                    }
                }
            }
            .boxed()
        }));
        Self {
            raw,
            handlers,
            child_pgid: 0,
            child_task: None,
            stderr_task: None,
        }
    }
    pub fn spawn(command: &str, args: &[&str], cwd: Option<&Path>) -> Result<Self, AppServerError> {
        Self::spawn_with_options(command, args, cwd, ProtocolOptions::default())
    }
    pub fn spawn_with_options(
        command: &str,
        args: &[&str],
        cwd: Option<&Path>,
        options: ProtocolOptions,
    ) -> Result<Self, AppServerError> {
        Self::spawn_with_env(command, args, cwd, &[], options)
    }
    pub fn spawn_with_env(
        command: &str,
        args: &[&str],
        cwd: Option<&Path>,
        environment: &[(&str, &str)],
        options: ProtocolOptions,
    ) -> Result<Self, AppServerError> {
        Self::spawn_with_env_removed(command, args, cwd, environment, &[], options)
    }
    /// `spawn_with_env`, with `removed` taken out of the inherited environment first.
    /// `environment` is applied after, so a name in both ends up set.
    pub fn spawn_with_env_removed(
        command: &str,
        args: &[&str],
        cwd: Option<&Path>,
        environment: &[(&str, &str)],
        removed: &[&str],
        options: ProtocolOptions,
    ) -> Result<Self, AppServerError> {
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
        for key in removed {
            process.env_remove(key);
        }
        for (key, value) in environment {
            process.env(key, value);
        }
        let mut child = process
            .spawn()
            .map_err(|error| AppServerError::Spawn(error.to_string()))?;
        let pid = child.id().unwrap_or(0);
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let (exit_tx, exit_rx) = watch::channel(None);
        let mut client = Self::from_io_with_exit(stdout, stdin, Some(exit_rx), options);
        if let Some(mut stderr) = child.stderr.take() {
            client.stderr_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            })));
        }
        let terminator = client.raw.termination_handle();
        client.child_pgid = pid;
        client.child_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            let error = match child.wait().await {
                Ok(status) => AppServerError::ProcessExited(status.code()),
                Err(error) => AppServerError::Transport {
                    operation: "read-process-exit-status",
                    detail: error.to_string(),
                },
            };
            let _ = exit_tx.send(Some(error.clone()));
            terminator.terminate(error);
        })));
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
        if let Some(mut task) = self.child_task.take() {
            #[cfg(unix)]
            signal_group(self.child_pgid, libc::SIGTERM);
            #[cfg(not(unix))]
            task.abort();
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                #[cfg(unix)]
                signal_group(self.child_pgid, libc::SIGKILL);
                task.await.map_err(|error| AppServerError::Transport {
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
    method: &str,
    schema_name: Option<&str>,
    payload: Option<&Value>,
) -> Result<(), String> {
    if method == "turn/start" && schema_name == Some("V2TurnStartParams") {
        return crate::extensions::validate_turn_start_params(payload);
    }
    schema::validate_optional(schema_name, payload)
}
/// Mirrors upstream decodeOptionalPayload/encodeOptionalPayload for server requests: a method
/// without a schema must not carry a payload, and a method with one must match it. The wire
/// messages use upstream's wording; the schema detail stays out of the peer-visible error.
pub(crate) fn check_payload(
    method: &str,
    operation: &'static str,
    schema_name: Option<&str>,
    payload: Option<&Value>,
) -> Result<(), ProtocolError> {
    if let (None, Some(payload)) = (schema_name, payload) {
        return Err(ProtocolError {
            code: -32602,
            message: format!("Method '{method}' does not accept a payload during '{operation}'"),
            data: Some(json!({"payloadKind": payload_kind(payload)})),
        });
    }
    validate_known(method, schema_name, payload).map_err(|detail| {
        tracing::debug!(%method, operation, %detail, "Codex App Server payload failed schema validation");
        ProtocolError {
            code: -32602,
            message: format!("Invalid payload for method '{method}' during '{operation}'"),
            data: None,
        }
    })
}
/// The group id is the leader's pid: the kernel does not reuse it while the group has members.
/// Known limit: once the group is empty its id can be reused by an unrelated new group leader,
/// and a shutdown or drop after that would signal that group. The window is the time between
/// the last descendant exiting and this client being dropped.
#[cfg(unix)]
fn signal_group(pgid: u32, signal: libc::c_int) {
    if let pgid @ 1..=u32::MAX = pgid {
        unsafe {
            libc::kill(-(pgid as i32), signal);
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(unix)]
        signal_group(self.child_pgid, libc::SIGKILL);
        drop(self.child_task.take());
        drop(self.stderr_task.take());
    }
}
