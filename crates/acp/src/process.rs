//! Child process launch for `cursor-agent acp` and other ACP binaries.
//! Also ports upstream/packages/effect-acp/src/_internal/stdio.ts.
//!
//! `kill_on_drop` kills the direct child. The child also runs in its own
//! process group (`process_group(0)`), and dropping the client signals that
//! group. ACP does not exit on stdin EOF after `session/new`.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::client::AcpClient;
use crate::errors::{
    AcpError, AcpProcessExitedError, AcpSpawnError, AcpTransportError, TransportOperation,
};
use crate::stderr::{append_acp_stderr_tail, sanitize_acp_stderr_excerpt};

const MAX_STDERR_HOOK_CHARS: usize = 32_768;
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

pub type StderrHook = Arc<dyn Fn(String) -> Result<(), AcpError> + Send + Sync>;

pub struct SpawnCommand<'a> {
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub cwd: Option<&'a Path>,
    pub env: Option<&'a HashMap<String, String>>,
    pub extend_env: bool,
    /// `true` starts a new session (`setsid`) so descendant teardown can signal
    /// the child without sharing the caller's session.
    pub detached: bool,
    pub on_stderr: Option<StderrHook>,
}

impl<'a> SpawnCommand<'a> {
    pub fn new(program: &'a str, args: &'a [&'a str]) -> Self {
        Self {
            program,
            args,
            cwd: None,
            env: None,
            extend_env: true,
            detached: false,
            on_stderr: None,
        }
    }
}

pub async fn spawn(command: SpawnCommand<'_>) -> Result<AcpClient, AcpError> {
    let mut cmd = Command::new(command.program);
    cmd.args(command.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = command.cwd {
        cmd.current_dir(cwd);
    }
    if !command.extend_env {
        cmd.env_clear();
    }
    if let Some(env) = command.env {
        cmd.envs(env);
    }
    #[cfg(unix)]
    if command.detached {
        // SAFETY: `setsid` only affects the child after fork, before exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    } else {
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|error| {
        AcpError::from(AcpSpawnError {
            command: Some(command.program.to_owned()),
            cause: error.to_string(),
        })
    })?;
    let pid = child.id().unwrap_or(0);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| missing_pipe(command.program, "stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| missing_pipe(command.program, "stdin"))?;
    let stderr_tail = Arc::new(StdMutex::new(String::new()));
    let stderr_task = child.stderr.take().map(|mut stderr| {
        let tail = Arc::clone(&stderr_tail);
        let on_stderr = command.on_stderr.clone();
        tokio::spawn(async move {
            let mut buf = [0_u8; 4096];
            loop {
                let n = match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                {
                    let mut current = tail.lock().unwrap_or_else(|poison| poison.into_inner());
                    *current = append_acp_stderr_tail(&current, &chunk);
                }
                if let Some(hook) = on_stderr.as_ref()
                    && hook(stderr_hook_chunk(&chunk)).is_err()
                {
                    signal_group(pid);
                    break;
                }
            }
        })
    });
    let guard = Arc::new(ProcessGuard {
        pid,
        state: Mutex::new(ProcessState {
            child: Some(child),
            status: None,
        }),
        stderr_task: Mutex::new(stderr_task),
        stderr_tail,
    });
    let eof_guard = Arc::downgrade(&guard);
    let termination_on_eof: Arc<
        dyn Fn() -> futures::future::BoxFuture<'static, AcpError> + Send + Sync,
    > = Arc::new(move || {
        let eof_guard = Weak::clone(&eof_guard);
        Box::pin(async move {
            match eof_guard.upgrade() {
                Some(guard) => guard.exit_error().await,
                None => AcpError::from(AcpProcessExitedError {
                    code: None,
                    pid: Some(i64::from(pid)),
                    stderr: None,
                    cause: None,
                }),
            }
        })
    });
    let termination_on_exit = Some(exit_watcher(Arc::downgrade(&guard), pid));
    Ok(AcpClient::connect_with(
        stdout,
        stdin,
        Some(termination_on_eof),
        termination_on_exit,
        Some(guard),
    ))
}

/// Resolves when the leader exits, without reaping it. The reader only sees
/// EOF once every holder of stdout closes it, so a helper that keeps the pipe
/// open would hide the exit. Holds only a `Weak` while it waits, so it never
/// keeps the guard alive.
fn exit_watcher(
    guard: Weak<ProcessGuard>,
    pid: u32,
) -> futures::future::BoxFuture<'static, AcpError> {
    Box::pin(async move {
        if wait_for_leader_exit(pid).await.is_err() {
            // The leader was already reaped elsewhere. The reader's EOF path
            // reports that, so this watcher stays quiet.
            return std::future::pending().await;
        }
        match guard.upgrade() {
            Some(guard) => guard.exit_error().await,
            None => AcpError::from(AcpProcessExitedError {
                code: None,
                pid: Some(i64::from(pid)),
                stderr: None,
                cause: None,
            }),
        }
    })
}

/// Blocks until the leader exits, leaving it unreaped so its pid and pgid stay
/// reserved until `exit_error` signals the group.
#[cfg(unix)]
async fn wait_for_leader_exit(pid: u32) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        loop {
            // SAFETY: waitid writes only into the zeroed local `info`.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

#[cfg(not(unix))]
async fn wait_for_leader_exit(_pid: u32) -> std::io::Result<()> {
    std::future::pending().await
}

fn missing_pipe(program: &str, stream: &str) -> AcpError {
    AcpError::from(AcpSpawnError {
        command: Some(program.to_owned()),
        cause: format!("ACP child {stream} was not piped"),
    })
}

pub struct ProcessGuard {
    pid: u32,
    state: Mutex<ProcessState>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    stderr_tail: Arc<StdMutex<String>>,
}

struct ProcessState {
    child: Option<Child>,
    /// Set once the leader is reaped. From then on the pid, and with it the
    /// pgid, may be reused, so no further group signal is sent.
    status: Option<ExitStatus>,
}

impl ProcessGuard {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Reports the child's exit. The first call signals the process group and
    /// then reaps the leader. Later calls return the same status.
    pub async fn exit_error(&self) -> AcpError {
        let mut state = self.state.lock().await;
        if state.status.is_none() {
            let Some(child) = state.child.as_mut() else {
                return AcpError::from(AcpProcessExitedError {
                    code: None,
                    pid: Some(i64::from(self.pid)),
                    stderr: self.stderr_excerpt().await,
                    cause: None,
                });
            };
            // The leader is still unreaped, so its pid and pgid stay reserved.
            // Signal the group before reaping, so surviving helpers die without
            // a risk of hitting a recycled pgid.
            signal_group(self.pid);
            match child.wait().await {
                Ok(status) => state.status = Some(status),
                Err(error) => {
                    return AcpError::from(AcpTransportError {
                        operation: Some(TransportOperation::ReadProcessExitStatus),
                        method: None,
                        detail: None,
                        pid: Some(i64::from(self.pid)),
                        cause: Some(error.to_string()),
                    });
                }
            }
        }
        let code = state.status.and_then(|status| status.code());
        drop(state);
        AcpError::from(AcpProcessExitedError {
            code,
            pid: Some(i64::from(self.pid)),
            stderr: self.stderr_excerpt().await,
            cause: None,
        })
    }

    async fn stderr_excerpt(&self) -> Option<String> {
        if let Some(task) = self.stderr_task.lock().await.take() {
            let _ = timeout(STDERR_DRAIN_TIMEOUT, task).await;
        }
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if tail.is_empty() {
            return None;
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let mut environment = HashMap::new();
        if !home.is_empty() {
            environment.insert("HOME".to_owned(), home.clone());
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            environment.insert("USERPROFILE".to_owned(), profile);
        }
        let excerpt = sanitize_acp_stderr_excerpt(&tail, &home, &environment);
        if excerpt.is_empty() {
            None
        } else {
            Some(excerpt)
        }
    }
}

fn stderr_hook_chunk(chunk: &str) -> String {
    let count = chunk.chars().count();
    if count <= MAX_STDERR_HOOK_CHARS {
        chunk.to_owned()
    } else {
        chunk.chars().skip(count - MAX_STDERR_HOOK_CHARS).collect()
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.get_mut().take() {
            task.abort();
        }
        // An unreaped leader keeps the pgid reserved, so the group signal is
        // safe. Once reaped it is not, and the signal is skipped. Dropping the
        // child lets kill_on_drop SIGKILL the leader if it is still running,
        // and tokio reaps it in the background. Nothing here blocks.
        if self.state.get_mut().status.is_none() {
            signal_group(self.pid);
        }
    }
}

/// SIGKILLs the child's process group. Only valid while the leader is unreaped.
fn signal_group(pid: u32) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        // A failed kill means the group is already gone, which is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn child_runs_in_its_own_process_group_and_dies_on_drop() {
        let client = spawn(SpawnCommand::new("/bin/sleep", &["30"]))
            .await
            .unwrap();
        let pid = client_pid(&client);
        #[cfg(unix)]
        {
            let output = std::process::Command::new("ps")
                .args(["-o", "pgid=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let pgid: u32 = String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap();
            assert_eq!(pgid, pid);
            let parent = unsafe { libc::getpgrp() } as u32;
            assert_ne!(pgid, parent);
        }
        drop(client);
        let mut gone = false;
        for _ in 0..50 {
            let status = std::process::Command::new("ps")
                .args(["-p", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            if !status.success() {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            gone,
            "sleep {pid} was still running after the client was dropped"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_signals_surviving_helpers_before_reaping_and_reports_the_same_status_twice() {
        let dir = std::env::temp_dir().join(format!("turin-acp-helper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("helper.pid");
        let script = format!("sleep 30 & echo $! > '{}'; exit 4", pid_file.display());
        let client = spawn(SpawnCommand::new("/bin/sh", &["-c", &script]))
            .await
            .unwrap();
        let guard = client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<ProcessGuard>())
            .expect("spawn stores a ProcessGuard");
        let helper: i32 = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "helper never started"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        for _ in 0..2 {
            let AcpError::ProcessExited(error) = guard.exit_error().await else {
                panic!("expected a process-exited error");
            };
            assert_eq!(error.code, Some(4));
        }
        let mut gone = false;
        for _ in 0..150 {
            // SAFETY: signal 0 only probes the pid.
            if unsafe { libc::kill(helper, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "helper {helper} survived the leader's exit");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leader_exit_fails_pending_requests_even_when_a_helper_holds_stdout() {
        let dir = std::env::temp_dir().join(format!("turin-acp-exit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("helper.pid");
        // The helper inherits stdout, so the reader never sees EOF. Only the
        // exit watcher can fail the pending request.
        let script = format!(
            "sleep 30 & echo $! > '{}'; read line; exit 3",
            pid_file.display()
        );
        let client = spawn(SpawnCommand::new("/bin/sh", &["-c", &script]))
            .await
            .unwrap();
        let helper: i32 = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "helper never started"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.ext_request("x/test", serde_json::json!({})),
        )
        .await
        .expect("pending request hung after the leader exited");
        let Err(AcpError::ProcessExited(error)) = result else {
            panic!("expected a process-exited error");
        };
        assert_eq!(error.code, Some(3));
        let mut gone = false;
        for _ in 0..150 {
            // SAFETY: signal 0 only probes the pid.
            if unsafe { libc::kill(helper, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "helper {helper} survived the leader's exit");
    }

    #[tokio::test]
    async fn process_exit_attaches_a_sanitized_stderr_excerpt() {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("turin-acp-stderr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("done");
        let script = format!(
            "printf '%s\\n' \"$HOME Authorization: Bearer secret-token-value\" >&2; printf done > '{}'; exit 9",
            marker.display()
        );
        let client = spawn(SpawnCommand::new("/bin/sh", &["-c", &script]))
            .await
            .unwrap();
        wait_for_file(&marker).await;
        let guard = client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<ProcessGuard>())
            .expect("spawn stores a ProcessGuard");
        let AcpError::ProcessExited(error) = guard.exit_error().await else {
            panic!("expected a process-exited error");
        };
        assert_eq!(error.code, Some(9));
        let message = error.to_string();
        let excerpt = error.stderr.unwrap_or_default();
        assert!(excerpt.contains("Bearer [redacted]"), "{excerpt}");
        assert!(!excerpt.contains("secret-token-value"));
        if !home.is_empty() {
            assert!(!excerpt.contains(&home), "{excerpt}");
            assert!(excerpt.contains('~'));
        }
        assert!(message.contains("Bearer [redacted]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stderr_hook_failure_kills_the_child() {
        let client = spawn(SpawnCommand {
            program: "/bin/sh",
            args: &["-c", "echo boom >&2; sleep 30"],
            cwd: None,
            env: None,
            extend_env: true,
            detached: false,
            on_stderr: Some(Arc::new(|_| Err(AcpError::transport("stderr hook failed")))),
        })
        .await
        .unwrap();
        let pid = client_pid(&client);
        let mut gone = false;
        for _ in 0..100 {
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "child {pid} survived a failing stderr hook");
    }

    #[tokio::test]
    async fn extend_env_false_passes_only_the_given_environment() {
        let dir = std::env::temp_dir().join(format!("turin-acp-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("env.txt");
        let script = format!(
            "printf '%s' \"$TURIN_ACP_MARKER:$HOME\" > '{}'",
            marker.display()
        );
        let mut env = HashMap::new();
        env.insert("TURIN_ACP_MARKER".to_owned(), "only-this".to_owned());
        let client = spawn(SpawnCommand {
            program: "/bin/sh",
            args: &["-c", &script],
            cwd: None,
            env: Some(&env),
            extend_env: false,
            detached: false,
            on_stderr: None,
        })
        .await
        .unwrap();
        wait_for_file(&marker).await;
        let text = std::fs::read_to_string(&marker).unwrap();
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(text, "only-this:");
    }

    async fn wait_for_file(path: &std::path::Path) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if path.is_file() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "child never wrote {}",
                path.display()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn client_pid(client: &AcpClient) -> u32 {
        client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<ProcessGuard>())
            .expect("spawn stores a ProcessGuard")
            .pid
    }
}
