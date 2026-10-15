//! Runs a provider CLI for one text generation request.
//!
//! Upstream spawns through Effect's `ChildProcessSpawner`, scoped to the request. Here
//! the child gets its own process group so a timeout takes its whole tree with it, and
//! `kill_on_drop` covers the direct child if the request future is dropped.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use turin_provider_core::process_group::signal_process_group;

/// What a finished CLI run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    /// `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The binary could not be started. The message is the OS error text.
    Spawn(String),
    /// The run took longer than its timeout and was killed.
    TimedOut,
    /// Waiting on the process or reading its pipes failed.
    Io(String),
}

/// Runs `program` with `args` in `cwd`, feeding `stdin` and collecting both output streams.
///
/// `env` is layered over the inherited environment, as upstream's `ChildProcess.make` does.
pub async fn run_command(
    program: &str,
    args: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    stdin: Option<String>,
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| CommandError::Spawn(error.to_string()))?;
    // Signals the whole group when this call ends on any path: a timeout, a dropped request,
    // or a normal exit that left members (grandchildren that closed their pipes) running.
    let _group = ProcessGroupGuard(child.id());

    let child_stdin = child.stdin.take();
    let write_stdin = async move {
        if let (Some(mut pipe), Some(text)) = (child_stdin, stdin) {
            // A child that exits before reading its input is reported by its exit code.
            let _ = pipe.write_all(text.as_bytes()).await;
        }
    };
    let collect = async move {
        let (_, output) = tokio::join!(write_stdin, child.wait_with_output());
        output
    };

    match tokio::time::timeout(timeout, collect).await {
        Ok(Ok(output)) => Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
        }),
        Ok(Err(error)) => Err(CommandError::Io(error.to_string())),
        Err(_) => Err(CommandError::TimedOut),
    }
}

/// Creates a temp directory that is removed when dropped. `tempfile` has no async API, so the
/// `mkdir` runs on tokio's blocking pool rather than a worker thread.
pub async fn make_temp_dir_owned(prefix: String) -> std::io::Result<tempfile::TempDir> {
    tokio::task::spawn_blocking(move || tempfile::Builder::new().prefix(&prefix).tempdir())
        .await
        .map_err(std::io::Error::other)?
}

/// SIGKILLs a process group on drop. The leader is usually reaped by then, so its pid is only
/// a group id while members remain; a pid reused within the same instant is the one residual risk.
struct ProcessGroupGuard(Option<u32>);

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = signal_process_group(i64::from(pid), libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> (String, Vec<String>) {
        (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), script.to_owned()],
        )
    }

    /// A grandchild that closes its pipes and outlives the leader is swept when the call ends.
    #[tokio::test]
    async fn a_grandchild_that_outlives_the_leader_is_killed_with_the_group() {
        let (program, args) = sh("sleep 30 </dev/null >/dev/null 2>&1 & echo $!");
        let output = run_command(
            &program,
            &args,
            "/",
            &BTreeMap::new(),
            None,
            Duration::from_secs(10),
        )
        .await
        .expect("leader exits");
        let grandchild: i32 = output.stdout.trim().parse().expect("grandchild pid");
        let mut gone = false;
        for _ in 0..100 {
            // SAFETY: signal 0 only probes the pid; no signal is delivered.
            if unsafe { libc::kill(grandchild, 0) } == -1 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "grandchild {grandchild} survived the group sweep");
    }

    #[tokio::test]
    async fn a_timeout_reports_timed_out() {
        let (program, args) = sh("sleep 30");
        let error = run_command(
            &program,
            &args,
            "/",
            &BTreeMap::new(),
            None,
            Duration::from_millis(50),
        )
        .await
        .expect_err("timeout");
        assert_eq!(error, CommandError::TimedOut);
    }
}
