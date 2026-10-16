//! Child process launch for `cursor-agent acp` and other ACP binaries.
//! Also ports upstream/packages/effect-acp/src/_internal/stdio.ts.
//!
//! `kill_on_drop` kills the direct child. The child also runs in its own
//! process group (`process_group(0)`), and dropping the client signals that
//! group. ACP does not exit on stdin EOF after `session/new`.

use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Weak};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::client::AcpClient;
use crate::errors::{
    AcpError, AcpProcessExitedError, AcpSpawnError, AcpTransportError, TransportOperation,
};

pub struct SpawnCommand<'a> {
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub cwd: Option<&'a Path>,
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
    #[cfg(unix)]
    {
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
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut buf = [0_u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
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

struct ProcessGuard {
    pid: u32,
    state: Mutex<ProcessState>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
}

struct ProcessState {
    child: Option<Child>,
    /// Set once the leader is reaped. From then on the pid, and with it the
    /// pgid, may be reused, so no further group signal is sent.
    status: Option<ExitStatus>,
}

impl ProcessGuard {
    /// Reports the child's exit. The first call signals the process group and
    /// then reaps the leader. Later calls return the same status.
    async fn exit_error(&self) -> AcpError {
        let mut state = self.state.lock().await;
        if state.status.is_none() {
            let Some(child) = state.child.as_mut() else {
                return AcpError::from(AcpProcessExitedError {
                    code: None,
                    pid: Some(i64::from(self.pid)),
                    stderr: None,
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
        AcpError::from(AcpProcessExitedError {
            code: state.status.and_then(|status| status.code()),
            pid: Some(i64::from(self.pid)),
            stderr: None,
            cause: None,
        })
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

    #[tokio::test]
    async fn child_runs_in_its_own_process_group_and_dies_on_drop() {
        let client = spawn(SpawnCommand {
            program: "/bin/sleep",
            args: &["30"],
            cwd: None,
        })
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
        let client = spawn(SpawnCommand {
            program: "/bin/sh",
            args: &["-c", &script],
            cwd: None,
        })
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
        let client = spawn(SpawnCommand {
            program: "/bin/sh",
            args: &["-c", &script],
            cwd: None,
        })
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

    fn client_pid(client: &AcpClient) -> u32 {
        client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<ProcessGuard>())
            .expect("spawn stores a ProcessGuard")
            .pid
    }
}
