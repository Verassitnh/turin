//! Port of `runOpenCodeCommand` in `upstream/packages/provider-opencode/src/server/OpenCodeRuntime.ts`.
//!
//! The command runs in its own process group. Stdout and stderr are drained to
//! EOF and only the first `max_output_bytes` of each are kept, as upstream's
//! `collectUint8StreamText` keeps draining after truncation so the child can
//! exit. However the future ends (finish, error, or a caller's timeout dropping
//! it), the group is SIGKILLed, the same as upstream's finalizer. Windows is not
//! ported: there is no Windows checkout in this worktree.

use std::collections::BTreeMap;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::error::OpenCodeRuntimeError;
use crate::process_group::{SIGKILL, signal_process_group};
use crate::version_probe::{OpenCodeBinary, OpenCodeCommandResult};

/// Output cap for `opencode --version`. Upstream does not cap this probe, but
/// a hostile or broken binary must not grow memory without bound.
const VERSION_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// `opencode --version`, run through [`run_open_code_command`].
pub struct SystemOpenCodeBinary;

impl OpenCodeBinary for SystemOpenCodeBinary {
    fn run_version<'a>(
        &'a self,
        binary_path: &'a str,
        environment: Option<&'a BTreeMap<String, String>>,
    ) -> impl std::future::Future<Output = Result<OpenCodeCommandResult, OpenCodeRuntimeError>> + Send + 'a
    {
        run_open_code_command(
            binary_path,
            &["--version"],
            environment,
            None,
            Some(VERSION_MAX_OUTPUT_BYTES),
        )
    }
}

/// Runs `binary_path args…` and collects its output. `environment` replaces the
/// child's environment when set; otherwise the child inherits this process's.
pub async fn run_open_code_command(
    binary_path: &str,
    args: &[&str],
    environment: Option<&BTreeMap<String, String>>,
    cwd: Option<&str>,
    max_output_bytes: Option<usize>,
) -> Result<OpenCodeCommandResult, OpenCodeRuntimeError> {
    run(binary_path, args, environment, cwd, max_output_bytes)
        .await
        .map_err(|err| {
            OpenCodeRuntimeError::new(
                "runOpenCodeCommand",
                format!(
                    "Failed to execute '{binary_path} {}': {err}",
                    args.join(" ")
                ),
            )
        })
}

async fn run(
    binary_path: &str,
    args: &[&str],
    environment: Option<&BTreeMap<String, String>>,
    cwd: Option<&str>,
    max_output_bytes: Option<usize>,
) -> std::io::Result<OpenCodeCommandResult> {
    let mut command = Command::new(binary_path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(environment) = environment {
        command.env_clear();
        command.envs(environment);
    }
    let mut child = command.spawn()?;
    // Armed before anything else can fail or be cancelled.
    let _group = GroupKill(child.id());
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let cap = max_output_bytes.unwrap_or(usize::MAX);
    let (stdout, stderr, status) =
        tokio::join!(collect(stdout, cap), collect(stderr, cap), child.wait());
    let status = status?;
    Ok(OpenCodeCommandResult {
        stdout: String::from_utf8_lossy(&stdout?).into_owned(),
        stderr: String::from_utf8_lossy(&stderr?).into_owned(),
        code: status.code().unwrap_or(-1),
    })
}

/// Keeps the first `cap` bytes and drains the rest.
async fn collect<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(kept);
        }
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// SIGKILLs the command's process group when dropped. The group is the
/// command's pid because it was started with `process_group(0)`.
struct GroupKill(Option<u32>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // The group may already be gone, which is fine.
            let _ = signal_process_group(pid, SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::{alive, temp_file, write_fake_opencode};

    #[tokio::test]
    async fn a_timed_out_version_probe_leaves_no_process() {
        let binary = write_fake_opencode();
        let pid_file = temp_file("version-pid");
        let child_file = temp_file("version-child");
        let mut environment = BTreeMap::new();
        // The fake binary runs `sleep` and needs PATH, so the environment cannot be empty.
        environment.insert("PATH".into(), std::env::var("PATH").unwrap_or_default());
        environment.insert("TURIN_TEST_PID".into(), pid_file.display().to_string());
        environment.insert("TURIN_TEST_CHILD".into(), child_file.display().to_string());
        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            SystemOpenCodeBinary.run_version(&binary.display().to_string(), Some(&environment)),
        )
        .await;
        assert!(timed_out.is_err(), "the fake --version should not finish");
        let read = |path: &std::path::Path| -> u32 {
            std::fs::read_to_string(path)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let (pid, child) = (read(&pid_file), read(&child_file));
        for _ in 0..100 {
            if !alive(pid) && !alive(child) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!alive(pid), "--version group {pid} survived its timeout");
        assert!(
            !alive(child),
            "--version child {child} survived its timeout"
        );
    }

    #[tokio::test]
    async fn output_is_capped_and_the_command_still_finishes() {
        let result = run_open_code_command(
            "/bin/sh",
            &["-c", "printf 0123456789abcdefghijklmnop"],
            None,
            None,
            Some(10),
        )
        .await
        .unwrap();
        assert_eq!(result.stdout, "0123456789");
        assert_eq!(result.code, 0);
    }

    #[tokio::test]
    async fn a_missing_binary_is_wrapped_like_upstream() {
        let err = run_open_code_command(
            "/nonexistent/opencode-turin",
            &["--version"],
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.operation, "runOpenCodeCommand");
        assert!(
            err.detail
                .starts_with("Failed to execute '/nonexistent/opencode-turin --version': "),
            "{}",
            err.detail
        );
    }
}
