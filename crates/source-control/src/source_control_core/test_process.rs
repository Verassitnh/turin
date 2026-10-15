//! Test-only process runner: runs real programs (git) for tests that need a real repository.
//! The app's real `SourceControlHost` is provided by the git crate; this is the minimal stand-in.

use std::process::Stdio;
use std::time::Duration;

use turin_contracts::vcs::{
    VcsError, VcsProcessErrorContext, VcsProcessExitError, VcsProcessExitFailure,
    VcsProcessExitFailureKind, VcsProcessSpawnError, VcsProcessSpawnFailure,
    VcsProcessTimeoutError, VcsProcessTimeoutFailure,
};

use super::host::{SourceControlProcessInput, SourceControlProcessOutput};

pub(crate) async fn run(
    input: SourceControlProcessInput,
) -> Result<SourceControlProcessOutput, VcsError> {
    let context = VcsProcessErrorContext {
        operation: input.operation.clone(),
        command: input.command.clone(),
        cwd: input.cwd.clone(),
        argument_count: Some(input.args.len() as u64),
    };
    let mut command = tokio::process::Command::new(&input.command);
    command
        .args(&input.args)
        .current_dir(input.spawn_cwd.as_deref().unwrap_or(&input.cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = &input.env {
        command.envs(env);
    }
    let spawn = |error: std::io::Error| {
        VcsError::Spawn(VcsProcessSpawnError::from_process_spawn_error(
            context.clone(),
            VcsProcessSpawnFailure {
                cause: serde_json::Value::String(error.to_string()),
            },
        ))
    };
    let child = command.spawn().map_err(spawn)?;
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(30_000));
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result.map_err(spawn)?,
        Err(_) => {
            return Err(VcsError::Timeout(
                VcsProcessTimeoutError::from_process_timeout_error(
                    context,
                    VcsProcessTimeoutFailure {
                        timeout_ms: timeout.as_millis() as f64,
                    },
                ),
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let code = output.status.code().unwrap_or(-1);
    if !input.allow_non_zero_exit && code != 0 {
        return Err(VcsError::Exit(VcsProcessExitError::from_process_exit(
            context,
            VcsProcessExitFailure {
                exit_code: f64::from(code),
                stderr,
                stderr_truncated: false,
            },
            VcsProcessExitFailureKind::CommandFailed,
            None,
        )));
    }
    Ok(SourceControlProcessOutput {
        exit_code: code,
        stdout,
        stderr,
        ..Default::default()
    })
}
