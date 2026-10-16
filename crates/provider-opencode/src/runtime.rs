//! Port of the process half of
//! `upstream/packages/provider-opencode/src/server/OpenCodeRuntime.ts`.
//!
//! `opencode serve` is started in its own process group. Readiness is the
//! stdout line `server listening on <url>`. The default check after that is
//! v1 `GET /global/health`. Drop kills the group immediately; [`OpenCodeServerProcess::shutdown`]
//! is SIGTERM, a grace period (1s, matching the upstream finalizer), then SIGKILL.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::error::OpenCodeRuntimeError;
use crate::ledger::{OpenCodeServerLedger, ServerLease};
use crate::process_group::{SIGKILL, SIGTERM, signal_process_group};
use crate::semver::{compare_semver_versions, parse_semver};
use crate::v1_client::V1Client;
use crate::version_probe::MINIMUM_OPENCODE_VERSION;

const DEFAULT_HOSTNAME: &str = "127.0.0.1";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_MAX_CHARS: usize = 64 * 1024;
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

pub struct StartOpenCodeServer {
    pub binary_path: String,
    pub directory: String,
    pub server_password: Option<String>,
    pub environment: Option<BTreeMap<String, String>>,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub timeout: Option<Duration>,
    /// Defaults to 1 second, the upstream finalizer's SIGKILL delay.
    pub shutdown_grace: Option<Duration>,
    /// When set, the spawned group is recorded so a later start can reap it.
    pub ledger: Option<OpenCodeServerLedger>,
}

pub struct VerifyInput {
    pub url: String,
    pub server_password: Option<String>,
    pub directory: String,
}

pub type VerifyFn = Arc<
    dyn Fn(
            VerifyInput,
        ) -> Pin<Box<dyn Future<Output = Result<String, OpenCodeRuntimeError>> + Send>>
        + Send
        + Sync,
>;

pub fn default_v1_verify() -> VerifyFn {
    Arc::new(|input| {
        Box::pin(async move {
            let client = V1Client::new(
                &input.url,
                &input.directory,
                input.server_password.as_deref(),
            )?;
            verify_open_code_server_version(&client).await
        })
    })
}

pub fn resolve_open_code_config_content(environment: Option<&BTreeMap<String, String>>) -> String {
    if let Some(environment) = environment
        && let Some(value) = environment.get("OPENCODE_CONFIG_CONTENT")
    {
        return value.clone();
    }
    std::env::var("OPENCODE_CONFIG_CONTENT").unwrap_or_else(|_| "{}".to_string())
}

pub fn resolve_open_code_server_password(
    external: bool,
    server_password: Option<&str>,
    environment: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(password) = server_password {
        return Some(password.to_string());
    }
    if external {
        return None;
    }
    match environment {
        None => std::env::var("OPENCODE_SERVER_PASSWORD").ok(),
        Some(environment) => environment.get("OPENCODE_SERVER_PASSWORD").cloned(),
    }
}

/// Password the v1 session client sends as Basic auth.
///
/// `createOpenCodeSdkClient` adds the header only when `serverPassword` is
/// non-empty. The v1 adapter passes one only for an external server with a
/// configured password (`adapter.ts`). A spawned server still gets the resolved
/// password, including an inherited `OPENCODE_SERVER_PASSWORD`, in its process
/// environment and on the readiness client.
pub fn v1_client_password(external: bool, configured: Option<&str>) -> Option<&str> {
    if !external {
        return None;
    }
    configured.filter(|password| !password.is_empty())
}

pub struct OpenCodeServerProcess {
    pub url: String,
    pub server_password: Option<String>,
    pub version: String,
    pid: u32,
    exit_tx: watch::Sender<Option<i32>>,
    exit_rx: watch::Receiver<Option<i32>>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// Set once the group has been SIGKILLed and its bookkeeping released.
    killed: AtomicBool,
    /// Serialises `shutdown`: a second caller waits for the first to finish.
    stopping: tokio::sync::Mutex<()>,
    shutdown_grace: Duration,
    lease: std::sync::Mutex<Option<ServerLease>>,
}

pub struct ConnectOpenCodeServer {
    pub binary_path: String,
    pub directory: String,
    pub server_url: Option<String>,
    pub server_password: Option<String>,
    pub environment: Option<BTreeMap<String, String>>,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub timeout: Option<Duration>,
    pub shutdown_grace: Option<Duration>,
    pub ledger: Option<OpenCodeServerLedger>,
}

pub struct OpenCodeServerConnection {
    pub url: String,
    pub server_password: Option<String>,
    pub version: String,
    pub external: bool,
    pub process: Option<OpenCodeServerProcess>,
}

impl OpenCodeServerConnection {
    /// Password to give `V1Client` for session calls. Local servers keep the
    /// resolved password on the process; only an external configured password
    /// is copied onto the client.
    pub fn v1_client_password(&self) -> Option<&str> {
        v1_client_password(self.external, self.server_password.as_deref())
    }
}

impl std::fmt::Debug for OpenCodeServerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCodeServerProcess")
            .field("url", &self.url)
            .field("version", &self.version)
            .field("pid", &self.pid)
            .finish()
    }
}

impl OpenCodeServerProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_running(&self) -> bool {
        self.exit_rx.borrow().is_none()
    }

    pub async fn exit_code(&self) -> i32 {
        let mut rx = self.exit_rx.clone();
        if let Some(code) = *rx.borrow() {
            return code;
        }
        loop {
            if rx.changed().await.is_err() {
                return -1;
            }
            if let Some(code) = *rx.borrow() {
                return code;
            }
        }
    }

    pub async fn shutdown(&self) {
        let _stopping = self.stopping.lock().await;
        if self.killed.load(Ordering::SeqCst) {
            return;
        }
        let _ = signal_process_group(self.pid, SIGTERM);
        // Declared after the lock, so it runs first on every exit from here. That
        // includes the caller dropping this future during the grace period.
        let _kill_on_exit = KillOnExit(self);
        tokio::time::sleep(self.shutdown_grace).await;
    }

    fn forget_lease(&self) {
        let lease = self
            .lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(lease) = lease {
            lease.forget();
        }
    }

    fn finish_tasks(&self) {
        let tasks = self
            .tasks
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_else(|poisoned| std::mem::take(&mut *poisoned.into_inner()));
        for task in tasks {
            task.abort();
        }
        let code = (*self.exit_rx.borrow()).or(Some(-1));
        let _ = self.exit_tx.send(code);
    }

    /// SIGKILLs the process group without the grace period. For drop paths.
    /// Safe to call while a `shutdown` is still in its grace period.
    pub fn kill(&self) {
        if self.killed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = signal_process_group(self.pid, SIGKILL);
        self.forget_lease();
        self.finish_tasks();
    }
}

impl Drop for OpenCodeServerProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

struct KillOnExit<'a>(&'a OpenCodeServerProcess);

impl Drop for KillOnExit<'_> {
    fn drop(&mut self) {
        self.0.kill();
    }
}

pub async fn verify_open_code_server_version(
    client: &V1Client,
) -> Result<String, OpenCodeRuntimeError> {
    let health = match client.health().await {
        Ok(health) => health,
        Err(err) if err.detail == "Timed out while waiting for the OpenCode server." => {
            return Err(OpenCodeRuntimeError::new(
                "global.health",
                "Timed out while checking the OpenCode server version.",
            ));
        }
        Err(err) => return Err(err),
    };
    if parse_semver(&health.version).is_none() {
        return Err(OpenCodeRuntimeError::new(
            "global.health",
            format!(
                "OpenCode server returned an invalid version. T3 Code requires OpenCode v{MINIMUM_OPENCODE_VERSION} or newer."
            ),
        ));
    }
    if compare_semver_versions(&health.version, MINIMUM_OPENCODE_VERSION) < 0 {
        return Err(OpenCodeRuntimeError::new(
            "global.health",
            format!(
                "OpenCode v{} is too old. Upgrade to v{MINIMUM_OPENCODE_VERSION} or newer.",
                health.version
            ),
        ));
    }
    Ok(health.version)
}

pub async fn start_open_code_server_process(
    input: StartOpenCodeServer,
    verify: VerifyFn,
) -> Result<OpenCodeServerProcess, OpenCodeRuntimeError> {
    let hostname = input
        .hostname
        .unwrap_or_else(|| DEFAULT_HOSTNAME.to_string());
    let port = match input.port {
        Some(port) => port,
        None => find_available_port()?,
    };
    let timeout = input.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let shutdown_grace = input.shutdown_grace.unwrap_or(DEFAULT_SHUTDOWN_GRACE);
    let server_password = resolve_open_code_server_password(
        false,
        input.server_password.as_deref(),
        input.environment.as_ref(),
    );
    let config = resolve_open_code_config_content(input.environment.as_ref());

    let mut command = Command::new(&input.binary_path);
    command
        .arg("serve")
        .arg(format!("--hostname={hostname}"))
        .arg(format!("--port={port}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    if let Some(environment) = &input.environment {
        command.env_clear();
        for (key, value) in environment {
            command.env(key, value);
        }
    }
    if let Some(password) = &server_password {
        command.env("OPENCODE_SERVER_PASSWORD", password);
    }
    command.env("OPENCODE_CONFIG_CONTENT", &config);

    let mut child = command.spawn().map_err(|err| {
        // Upstream's status classifier looks for ENOENT, so a missing binary says so.
        let detail = if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "Failed to spawn OpenCode server process: spawn {} ENOENT ({err})",
                input.binary_path
            )
        } else {
            format!("Failed to spawn OpenCode server process: {err}")
        };
        OpenCodeRuntimeError::new("startOpenCodeServerProcess", detail)
    })?;
    let pid = child.id().unwrap_or(0);
    let serve_args = vec![
        "serve".to_string(),
        format!("--hostname={hostname}"),
        format!("--port={port}"),
    ];
    let lease = match &input.ledger {
        Some(ledger) => ledger.track(pid, port, &serve_args).await,
        None => None,
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (exit_tx, exit_rx) = watch::channel(None);
    let capture = Arc::new(AtomicBool::new(true));
    let stdout_log = Arc::new(tokio::sync::Mutex::new(String::new()));
    let stderr_log = Arc::new(tokio::sync::Mutex::new(String::new()));
    let (url_tx, mut url_rx) = mpsc::channel(1);

    let mut tasks = Vec::new();
    if let Some(stdout) = stdout {
        let capture = Arc::clone(&capture);
        let stdout_log = Arc::clone(&stdout_log);
        tasks.push(tokio::spawn(async move {
            pump_stdout(stdout, capture, stdout_log, url_tx).await;
        }));
    }
    if let Some(stderr) = stderr {
        let capture = Arc::clone(&capture);
        let stderr_log = Arc::clone(&stderr_log);
        tasks.push(tokio::spawn(async move {
            pump_stderr(stderr, capture, stderr_log).await;
        }));
    }
    let exit_tx_task = exit_tx.clone();
    tasks.push(tokio::spawn(async move {
        let code = wait_child(&mut child).await;
        let _ = exit_tx_task.send(Some(code));
    }));

    // The group is owned from here on. If this future is dropped while it waits
    // for readiness or verify, `Drop` SIGKILLs the group and aborts the readers,
    // the same as upstream closing the Scope that owns the child.
    let mut process = OpenCodeServerProcess {
        url: String::new(),
        server_password: server_password.clone(),
        version: String::new(),
        pid,
        exit_tx,
        exit_rx,
        tasks: std::sync::Mutex::new(tasks),
        killed: AtomicBool::new(false),
        stopping: tokio::sync::Mutex::new(()),
        shutdown_grace,
        lease: std::sync::Mutex::new(lease),
    };
    let mut exit_watch = process.exit_rx.clone();
    let ready = tokio::time::timeout(timeout, async {
        tokio::select! {
            url = url_rx.recv() => url,
            _ = exit_watch.changed() => None,
        }
    })
    .await;

    let url = match ready {
        Ok(Some(url)) => url,
        Ok(None) => {
            if process.exit_rx.borrow().is_none() {
                let _ = tokio::time::timeout(Duration::from_secs(2), exit_watch.changed()).await;
            }
            let code = process.exit_rx.borrow().unwrap_or(-1);
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stdout = stdout_log.lock().await.clone();
            let stderr = stderr_log.lock().await.clone();
            process.shutdown().await;
            return Err(OpenCodeRuntimeError::new(
                "startOpenCodeServerProcess",
                startup_exit_detail(code, &stdout, &stderr),
            ));
        }
        Err(_elapsed) => {
            process.shutdown().await;
            return Err(OpenCodeRuntimeError::new(
                "startOpenCodeServerProcess",
                format!(
                    "Timed out waiting for OpenCode server start after {}ms.",
                    timeout.as_millis()
                ),
            ));
        }
    };
    capture.store(false, Ordering::SeqCst);

    process.url = url.clone();
    match verify(VerifyInput {
        url,
        server_password: server_password.clone(),
        directory: input.directory,
    })
    .await
    {
        Ok(version) => process.version = version,
        Err(err) => {
            process.shutdown().await;
            return Err(err);
        }
    }
    Ok(process)
}

pub async fn connect_to_open_code_server(
    input: ConnectOpenCodeServer,
    verify: VerifyFn,
) -> Result<OpenCodeServerConnection, OpenCodeRuntimeError> {
    let server_url = input.server_url.as_deref().map(str::trim).unwrap_or("");
    if !server_url.is_empty() {
        let server_password =
            resolve_open_code_server_password(true, input.server_password.as_deref(), None);
        let version = verify(VerifyInput {
            url: server_url.to_string(),
            server_password: server_password.clone(),
            directory: input.directory,
        })
        .await?;
        return Ok(OpenCodeServerConnection {
            url: server_url.to_string(),
            server_password,
            version,
            external: true,
            process: None,
        });
    }
    let process = start_open_code_server_process(
        StartOpenCodeServer {
            binary_path: input.binary_path,
            directory: input.directory,
            server_password: input.server_password,
            environment: input.environment,
            hostname: input.hostname,
            port: input.port,
            timeout: input.timeout,
            shutdown_grace: input.shutdown_grace,
            ledger: input.ledger,
        },
        verify,
    )
    .await?;
    Ok(OpenCodeServerConnection {
        url: process.url.clone(),
        server_password: process.server_password.clone(),
        version: process.version.clone(),
        external: false,
        process: Some(process),
    })
}

fn find_available_port() -> Result<u16, OpenCodeRuntimeError> {
    let listener = std::net::TcpListener::bind((DEFAULT_HOSTNAME, 0)).map_err(|err| {
        OpenCodeRuntimeError::new(
            "startOpenCodeServerProcess",
            format!("Failed to find available port: {err}"),
        )
    })?;
    let port = listener.local_addr().map_err(|err| {
        OpenCodeRuntimeError::new(
            "startOpenCodeServerProcess",
            format!("Failed to find available port: {err}"),
        )
    })?;
    Ok(port.port())
}

fn startup_exit_detail(code: i32, stdout: &str, stderr: &str) -> String {
    let mut parts = vec![format!(
        "OpenCode server exited before startup completed (code: {code})."
    )];
    if !stdout.trim().is_empty() {
        parts.push(format!("stdout:\n{}", stdout.trim()));
    }
    if !stderr.trim().is_empty() {
        parts.push(format!("stderr:\n{}", stderr.trim()));
    }
    parts.join("\n\n")
}

async fn wait_child(child: &mut Child) -> i32 {
    match child.wait().await {
        Ok(status) => status.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

async fn pump_stdout(
    stdout: tokio::process::ChildStdout,
    capture: Arc<AtomicBool>,
    log: Arc<tokio::sync::Mutex<String>>,
    url_tx: mpsc::Sender<String>,
) {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    let mut armed = true;
    while let Some(line) = next_line(&mut reader, &mut bytes).await {
        if !armed || !capture.load(Ordering::SeqCst) {
            continue;
        }
        let mut buf = log.lock().await;
        push_capped(&mut buf, &line, STARTUP_MAX_CHARS);
        if let Some(url) = parse_server_url_from_output(&buf) {
            drop(buf);
            let _ = url_tx.send(url).await;
            armed = false;
        }
    }
}

async fn pump_stderr(
    stderr: tokio::process::ChildStderr,
    capture: Arc<AtomicBool>,
    log: Arc<tokio::sync::Mutex<String>>,
) {
    let mut reader = BufReader::new(stderr);
    let mut bytes = Vec::new();
    while let Some(line) = next_line(&mut reader, &mut bytes).await {
        if !capture.load(Ordering::SeqCst) {
            continue;
        }
        let mut buf = log.lock().await;
        push_capped(&mut buf, &line, STARTUP_MAX_CHARS);
    }
}

/// Next line as lossy UTF-8. `None` only at EOF or on a real read error.
/// Invalid bytes decode to U+FFFD and reading goes on, as upstream's
/// `Stream.decodeText` does. A `read_line` error would end the drain, and the
/// dropped pipe then kills the server with SIGPIPE. Bytes past the startup cap
/// are discarded, so a line with no newline cannot grow without bound.
async fn next_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Option<String> {
    line.clear();
    loop {
        let available = match reader.fill_buf().await {
            Ok(available) => available,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if available.is_empty() {
            return (!line.is_empty()).then(|| String::from_utf8_lossy(line).into_owned());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let used = newline.map_or(available.len(), |index| index + 1);
        let room = STARTUP_MAX_CHARS.saturating_sub(line.len());
        line.extend_from_slice(&available[..used.min(room)]);
        reader.consume(used);
        if newline.is_some() {
            return Some(String::from_utf8_lossy(line).into_owned());
        }
    }
}

fn push_capped(buf: &mut String, chunk: &str, max_chars: usize) {
    buf.push_str(chunk);
    let count = buf.chars().count();
    if count > max_chars {
        let skip = count - max_chars;
        *buf = buf.chars().skip(skip).collect();
    }
}

pub fn parse_server_url_from_output(output: &str) -> Option<String> {
    for line in output.split('\n') {
        let lower = line.to_ascii_lowercase();
        let marker = "server listening on";
        let Some(index) = lower.find(marker) else {
            continue;
        };
        let rest = line[index + marker.len()..].trim_start();
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            continue;
        }
        if let Some(url) = rest.split_whitespace().next() {
            return Some(url.to_string());
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    fn grace() -> Duration {
        Duration::from_millis(40)
    }

    fn ok_verify() -> VerifyFn {
        Arc::new(|_| Box::pin(async { Ok("1.19.0".to_string()) }))
    }

    pub(crate) fn write_fake_opencode() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turin-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opencode.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
port=""
for arg in "$@"; do
  case "$arg" in
    --port=*) port="${arg#--port=}" ;;
  esac
done
if [ -n "${TURIN_TEST_LOG:-}" ]; then
  printf '%s\n' "password=${OPENCODE_SERVER_PASSWORD-}" "config=${OPENCODE_CONFIG_CONTENT-}" "opencode_password=${OPENCODE_PASSWORD-}" "home=${HOME-}" "args=$*" >> "$TURIN_TEST_LOG"
fi
if [ -n "${TURIN_TEST_PID:-}" ]; then
  echo $$ > "$TURIN_TEST_PID"
fi
if [ -n "${TURIN_TEST_CHILD:-}" ]; then
  sleep 120 &
  echo $! > "$TURIN_TEST_CHILD"
fi
if [ "${TURIN_TEST_TRAP_TERM:-}" = "1" ]; then
  trap '' TERM
fi
if [ "${TURIN_TEST_EXIT:-}" = "1" ]; then
  echo boom >&2
  exit 7
fi
if [ "${TURIN_TEST_NOISE:-}" = "1" ]; then
  printf '\377\376 bad\n'
fi
if [ "${TURIN_TEST_SILENT:-}" != "1" ]; then
  printf '%s\n' "server listening on http://127.0.0.1:${port}"
fi
while true; do
  if [ "${TURIN_TEST_NOISE:-}" = "1" ]; then
    printf '\377 noise\n'
    sleep 0.02
  else
    sleep 1
  fi
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    pub(crate) fn temp_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "turin-oc-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    pub(crate) fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn password_resolution_matches_the_spawn_rules() {
        let mut env = BTreeMap::new();
        env.insert("OPENCODE_SERVER_PASSWORD".into(), "from-map".into());
        assert_eq!(
            resolve_open_code_server_password(false, Some("explicit"), Some(&env)).as_deref(),
            Some("explicit")
        );
        assert_eq!(
            resolve_open_code_server_password(false, None, Some(&env)).as_deref(),
            Some("from-map")
        );
        assert_eq!(
            resolve_open_code_server_password(true, None, Some(&env)),
            None
        );
        env.insert("OPENCODE_CONFIG_CONTENT".into(), "{\"provider\":{}}".into());
        assert_eq!(
            resolve_open_code_config_content(Some(&env)),
            "{\"provider\":{}}"
        );
    }

    #[test]
    fn password_resolution_prefers_an_explicit_value_and_skips_env_when_external() {
        let mut environment = BTreeMap::new();
        environment.insert("OPENCODE_SERVER_PASSWORD".into(), "from-map".into());
        assert_eq!(
            resolve_open_code_server_password(false, None, Some(&environment)).as_deref(),
            Some("from-map")
        );
        assert_eq!(
            resolve_open_code_server_password(true, None, Some(&environment)).as_deref(),
            None
        );
        assert_eq!(
            resolve_open_code_server_password(false, Some("set"), Some(&environment)).as_deref(),
            Some("set")
        );
        assert_eq!(v1_client_password(false, Some("set")), None);
        assert_eq!(v1_client_password(true, None), None);
        assert_eq!(v1_client_password(true, Some("")), None);
        assert_eq!(v1_client_password(true, Some("set")), Some("set"));
    }

    #[test]
    fn listening_line_is_case_insensitive() {
        assert_eq!(
            parse_server_url_from_output("noise\nServer listening on http://127.0.0.1:9\n"),
            Some("http://127.0.0.1:9".to_string())
        );
    }

    #[tokio::test]
    async fn spawned_server_sets_password_and_config_without_inheriting_env() {
        let binary = write_fake_opencode();
        let log = temp_file("env");
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_LOG".into(), log.display().to_string());
        environment.insert("OPENCODE_CONFIG_CONTENT".into(), "{\"theme\":\"x\"}".into());
        let process = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: Some("s3cret".into()),
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(2)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("password=s3cret"), "{text}");
        assert!(text.contains("config={\"theme\":\"x\"}"), "{text}");
        assert!(
            text.contains("home=\n") || text.lines().any(|line| line == "home="),
            "{text}"
        );
        assert!(process.is_running());
        process.shutdown().await;
        assert!(!process.is_running());
    }

    #[tokio::test]
    async fn shutdown_kills_the_process_group() {
        let binary = write_fake_opencode();
        let child_file = temp_file("child");
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_CHILD".into(), child_file.display().to_string());
        let process = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(2)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .unwrap();
        let child_pid: u32 = std::fs::read_to_string(&child_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(child_pid));
        process.shutdown().await;
        assert!(!alive(child_pid));
        assert!(!alive(process.pid()));
    }

    #[tokio::test]
    async fn a_cancelled_shutdown_still_kills_the_group() {
        let binary = write_fake_opencode();
        let child_file = temp_file("trap-child");
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_TRAP_TERM".into(), "1".into());
        environment.insert("TURIN_TEST_CHILD".into(), child_file.display().to_string());
        let process = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(2)),
                shutdown_grace: Some(Duration::from_secs(3)),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .unwrap();
        let pid = process.pid();
        let child: u32 = std::fs::read_to_string(&child_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SIGTERM is trapped, so the grace period is still running when the caller gives up.
        let _ = tokio::time::timeout(Duration::from_millis(300), process.shutdown()).await;
        // The process is still held here, so only the cancelled shutdown can kill it.
        for _ in 0..100 {
            if !alive(pid) && !alive(child) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "group {pid} survived a cancelled shutdown");
        assert!(!alive(child), "child {child} survived a cancelled shutdown");
        assert!(!process.is_running());
    }

    #[tokio::test]
    async fn exit_before_ready_reports_the_code_and_stderr() {
        let binary = write_fake_opencode();
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_EXIT".into(), "1".into());
        let err = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(2)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .unwrap_err();
        assert!(err.detail.contains("code: 7"), "{}", err.detail);
        assert!(err.detail.contains("boom"), "{}", err.detail);
    }

    #[tokio::test]
    async fn startup_timeout_stops_the_process() {
        let binary = write_fake_opencode();
        let pid_file = temp_file("pid");
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_SILENT".into(), "1".into());
        environment.insert("TURIN_TEST_PID".into(), pid_file.display().to_string());
        let err = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_millis(200)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .unwrap_err();
        assert!(
            err.detail
                .contains("Timed out waiting for OpenCode server start after 200ms."),
            "{}",
            err.detail
        );
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!alive(pid));
    }

    #[tokio::test]
    async fn dropping_a_start_while_it_waits_kills_the_group() {
        let binary = write_fake_opencode();
        let pid_file = temp_file("cancel-pid");
        let child_file = temp_file("cancel-child");
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_SILENT".into(), "1".into());
        environment.insert("TURIN_TEST_PID".into(), pid_file.display().to_string());
        environment.insert("TURIN_TEST_CHILD".into(), child_file.display().to_string());
        let start = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(30)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        );
        // The start never sees a listening line, so the caller's deadline drops it.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), start)
                .await
                .is_err()
        );
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let child: u32 = std::fs::read_to_string(&child_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..100 {
            if !alive(pid) && !alive(child) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "server group {pid} survived the dropped start");
        assert!(
            !alive(child),
            "server child {child} survived the dropped start"
        );
    }

    #[tokio::test]
    async fn invalid_utf8_on_stdout_does_not_stop_the_drain() {
        let binary = write_fake_opencode();
        let mut environment = BTreeMap::new();
        environment.insert("TURIN_TEST_NOISE".into(), "1".into());
        let process = start_open_code_server_process(
            StartOpenCodeServer {
                binary_path: binary.display().to_string(),
                directory: "/work".into(),
                server_password: None,
                environment: Some(environment),
                hostname: None,
                port: Some(9),
                timeout: Some(Duration::from_secs(2)),
                shutdown_grace: Some(grace()),
                ledger: None,
            },
            ok_verify(),
        )
        .await
        .expect("non-UTF-8 bytes before the listening line must not stop startup");
        // Output keeps coming after startup. A stopped reader would close the pipe and kill the server.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            process.is_running(),
            "server died after its pipe was abandoned"
        );
        process.shutdown().await;
    }

    #[tokio::test]
    async fn a_health_body_that_cannot_decode_is_an_invalid_health_response() {
        for body in [
            r#"{"healthy":false,"version":"1.19.0"}"#,
            r#"{"healthy":true}"#,
            r#"{"version":"1.19.0"}"#,
        ] {
            let server = crate::test_support::FakeServer::spawn(move |_| {
                crate::test_support::FakeResponse::json(200, body)
            })
            .await;
            let client = V1Client::new(&server.url, "/work", None).unwrap();
            let err = verify_open_code_server_version(&client).await.unwrap_err();
            assert_eq!(err.operation, "global.health");
            assert_eq!(
                err.detail,
                format!(
                    "OpenCode server returned an invalid health response. T3 Code requires OpenCode v{MINIMUM_OPENCODE_VERSION} or newer."
                ),
                "body {body}"
            );
        }
    }

    #[tokio::test]
    async fn health_rejects_an_old_server() {
        let server = crate::test_support::FakeServer::spawn(|_| {
            crate::test_support::FakeResponse::json(200, r#"{"healthy":true,"version":"1.0.0"}"#)
        })
        .await;
        let client = V1Client::new(&server.url, "/work", None).unwrap();
        let err = verify_open_code_server_version(&client).await.unwrap_err();
        assert!(err.detail.contains("too old"), "{}", err.detail);
    }
}
