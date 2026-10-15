//! Port of `upstream/packages/provider-opencode/src/server/OpenCodeServerOwner.ts`.
//!
//! One local `opencode serve` per owner. Borrowers share it. The last borrower
//! starts a 30s idle timer; a new borrower cancels that timer. If the process
//! has already exited, the next borrower starts a new one.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::error::OpenCodeRuntimeError;
use crate::runtime::{
    OpenCodeServerProcess, StartOpenCodeServer, VerifyFn, default_v1_verify,
    start_open_code_server_process,
};

const DEFAULT_IDLE: Duration = Duration::from_secs(30);

struct State {
    server: Option<Arc<OpenCodeServerProcess>>,
    borrowers: usize,
    idle: Option<JoinHandle<()>>,
    watch: Option<JoinHandle<()>>,
}

struct Inner {
    binary_path: String,
    directory: String,
    server_password: Option<String>,
    environment: Option<BTreeMap<String, String>>,
    shutdown_grace: Option<Duration>,
    verify: VerifyFn,
    idle: Duration,
    state: tokio::sync::Mutex<State>,
}

#[derive(Clone)]
pub struct OpenCodeServerOwner {
    inner: Arc<Inner>,
}

pub struct OwnerConfig {
    pub binary_path: String,
    pub directory: String,
    pub server_password: Option<String>,
    pub environment: Option<BTreeMap<String, String>>,
    pub idle: Option<Duration>,
    pub shutdown_grace: Option<Duration>,
    pub verify: Option<VerifyFn>,
}

impl OpenCodeServerOwner {
    pub fn new(config: OwnerConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                binary_path: config.binary_path,
                directory: config.directory,
                server_password: config.server_password,
                environment: config.environment,
                shutdown_grace: config.shutdown_grace,
                verify: config.verify.unwrap_or_else(default_v1_verify),
                idle: config.idle.unwrap_or(DEFAULT_IDLE),
                state: tokio::sync::Mutex::new(State {
                    server: None,
                    borrowers: 0,
                    idle: None,
                    watch: None,
                }),
            }),
        }
    }

    pub async fn close(&self) {
        force_close(&self.inner).await;
    }

    pub async fn with_server<T, E, F, Fut>(&self, use_server: F) -> Result<T, E>
    where
        E: From<OpenCodeRuntimeError>,
        F: FnOnce(Arc<OpenCodeServerProcess>) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let server = self.acquire().await.map_err(E::from)?;
        let guard = BorrowGuard {
            owner: self.clone(),
            server: Arc::clone(&server),
            released: AtomicBool::new(false),
        };
        let result = use_server(server).await;
        guard.release().await;
        result
    }

    async fn acquire(&self) -> Result<Arc<OpenCodeServerProcess>, OpenCodeRuntimeError> {
        let mut state = self.inner.state.lock().await;
        cancel_idle(&mut state);
        if let Some(server) = state.server.clone() {
            if server.is_running() {
                state.borrowers += 1;
                return Ok(server);
            }
            state.server = None;
            state.borrowers = 0;
            drop(state);
            server.shutdown().await;
            state = self.inner.state.lock().await;
        }
        let started = start_open_code_server_process(
            self.inner.start_input(),
            Arc::clone(&self.inner.verify),
        )
        .await?;
        let server = Arc::new(started);
        state.server = Some(Arc::clone(&server));
        state.borrowers = 1;
        if let Some(previous) = state.watch.take() {
            previous.abort();
        }
        let watched = Arc::clone(&server);
        let inner = Arc::clone(&self.inner);
        state.watch = Some(tokio::spawn(async move {
            let _ = watched.exit_code().await;
            on_exit(&inner, &watched).await;
        }));
        Ok(server)
    }

    async fn release_server(&self, server: &Arc<OpenCodeServerProcess>) {
        let mut state = self.inner.state.lock().await;
        let Some(current) = &state.server else {
            return;
        };
        if !Arc::ptr_eq(current, server) {
            return;
        }
        state.borrowers = state.borrowers.saturating_sub(1);
        if state.borrowers > 0 {
            return;
        }
        cancel_idle(&mut state);
        let server = Arc::clone(server);
        let inner = Arc::clone(&self.inner);
        let idle = self.inner.idle;
        state.idle = Some(tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            close_if_idle(&inner, server).await;
        }));
    }
}

impl Inner {
    fn start_input(&self) -> StartOpenCodeServer {
        StartOpenCodeServer {
            binary_path: self.binary_path.clone(),
            directory: self.directory.clone(),
            server_password: self.server_password.clone(),
            environment: self.environment.clone(),
            hostname: None,
            port: None,
            timeout: None,
            shutdown_grace: self.shutdown_grace,
            ledger: None,
        }
    }
}

async fn on_exit(inner: &Inner, server: &Arc<OpenCodeServerProcess>) {
    let mut state = inner.state.lock().await;
    let Some(current) = &state.server else {
        return;
    };
    if !Arc::ptr_eq(current, server) {
        return;
    }
    cancel_idle(&mut state);
    state.server = None;
    state.borrowers = 0;
    drop(state);
    server.shutdown().await;
}

async fn close_if_idle(inner: &Inner, server: Arc<OpenCodeServerProcess>) {
    let mut state = inner.state.lock().await;
    let Some(current) = &state.server else {
        return;
    };
    if !Arc::ptr_eq(current, &server) || state.borrowers > 0 {
        return;
    }
    state.idle = None;
    state.server = None;
    state.borrowers = 0;
    drop(state);
    server.shutdown().await;
}

async fn force_close(inner: &Inner) {
    let mut state = inner.state.lock().await;
    cancel_idle(&mut state);
    if let Some(watch) = state.watch.take() {
        watch.abort();
    }
    let server = state.server.take();
    state.borrowers = 0;
    drop(state);
    if let Some(server) = server {
        server.shutdown().await;
    }
}

fn cancel_idle(state: &mut State) {
    if let Some(task) = state.idle.take() {
        task.abort();
    }
}

struct BorrowGuard {
    owner: OpenCodeServerOwner,
    server: Arc<OpenCodeServerProcess>,
    released: AtomicBool,
}

impl BorrowGuard {
    async fn release(self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        self.owner.release_server(&self.server).await;
    }
}

impl Drop for BorrowGuard {
    fn drop(&mut self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let owner = self.owner.clone();
        let server = Arc::clone(&self.server);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                owner.release_server(&server).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ok_verify() -> VerifyFn {
        Arc::new(|_| Box::pin(async { Ok("1.19.0".to_string()) }))
    }

    fn write_fake_opencode() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turin-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opencode.sh");
        if path.exists() {
            return path;
        }
        std::fs::write(
            &path,
            r#"#!/bin/sh
port=""
for arg in "$@"; do
  case "$arg" in
    --port=*) port="${arg#--port=}" ;;
  esac
done
printf '%s\n' "server listening on http://127.0.0.1:${port}"
while true; do
  sleep 1
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

    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn borrowers_share_one_process_and_idle_closes_it() {
        let binary = write_fake_opencode();
        let owner = OpenCodeServerOwner::new(OwnerConfig {
            binary_path: binary.display().to_string(),
            directory: "/work".into(),
            server_password: None,
            environment: Some(BTreeMap::new()),
            idle: Some(Duration::from_millis(150)),
            shutdown_grace: Some(Duration::from_millis(40)),
            verify: Some(ok_verify()),
        });
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let owner_held = owner.clone();
        let held = tokio::spawn(async move {
            owner_held
                .with_server(|server| async move {
                    let pid = server.pid();
                    let _ = gate_rx.await;
                    Ok::<_, OpenCodeRuntimeError>(pid)
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        let second = owner
            .with_server(|server| async move { Ok::<_, OpenCodeRuntimeError>(server.pid()) })
            .await
            .unwrap();
        let _ = gate_tx.send(());
        let first = held.await.unwrap().unwrap();
        assert_eq!(first, second);
        assert!(alive(first));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(!alive(first));
        owner.close().await;
    }
}
