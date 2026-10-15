//! Child process launch for `cursor-agent acp` and other ACP binaries.
//!
//! `kill_on_drop` kills the direct child. The child also runs in its own
//! process group (`process_group(0)`), and dropping the client signals that
//! group. ACP does not exit on stdin EOF after `session/new`.

use std::path::Path;
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
        child: Mutex::new(Some(child)),
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
    Ok(AcpClient::connect_with(
        stdout,
        stdin,
        Some(termination_on_eof),
        Some(guard),
    ))
}

fn missing_pipe(program: &str, stream: &str) -> AcpError {
    AcpError::from(AcpSpawnError {
        command: Some(program.to_owned()),
        cause: format!("ACP child {stream} was not piped"),
    })
}

struct ProcessGuard {
    pid: u32,
    child: Mutex<Option<Child>>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
}

impl ProcessGuard {
    async fn exit_error(&self) -> AcpError {
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return AcpError::from(AcpProcessExitedError {
                code: None,
                pid: Some(i64::from(self.pid)),
                stderr: None,
                cause: None,
            });
        };
        match child.wait().await {
            Ok(status) => AcpError::from(AcpProcessExitedError {
                code: status.code(),
                pid: Some(i64::from(self.pid)),
                stderr: None,
                cause: None,
            }),
            Err(error) => AcpError::from(AcpTransportError {
                operation: Some(TransportOperation::ReadProcessExitStatus),
                method: None,
                detail: None,
                pid: Some(i64::from(self.pid)),
                cause: Some(error.to_string()),
            }),
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.get_mut().take() {
            task.abort();
        }
        let pid = self.pid as i32;
        #[cfg(unix)]
        if pid > 0 {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
        // The process is already reaped. Dropping the tokio child would try to
        // wait again from whatever thread is dropping the client.
        if let Some(child) = self.child.get_mut().take() {
            std::mem::forget(child);
        }
    }
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

    fn client_pid(client: &AcpClient) -> u32 {
        client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<ProcessGuard>())
            .expect("spawn stores a ProcessGuard")
            .pid
    }
}
