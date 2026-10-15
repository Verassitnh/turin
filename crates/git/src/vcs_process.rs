//! Port of upstream/apps/server/src/vcs/VcsProcess.ts
//!
//! `VcsError` is the contract union. Boxing it would hide the variant callers match.

#![allow(clippy::result_large_err)]
//!
//! Git checkpoint capture retries a lock or a missing stat twice, 75ms apart.
//! `gh` takes a second permit so GitHub calls stay at four in flight.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use tokio::sync::Semaphore;
use turin_contracts::vcs::{
    VcsError, VcsProcessErrorContext, VcsProcessExitError, VcsProcessExitFailure,
    VcsProcessExitFailureKind, VcsProcessMissingExitCodeError, VcsProcessOutputLimitError,
    VcsProcessOutputReadError, VcsProcessOutputStream, VcsProcessSpawnError,
    VcsProcessSpawnFailure, VcsProcessStdinWriteError, VcsProcessStream, VcsProcessTimeoutError,
    VcsProcessTimeoutFailure,
};

use crate::process::{
    OutputMode, ProcessRunError, ProcessRunInput, ProcessRunner, ProcessStream,
    StdoutChunkCallback, TimeoutBehavior, cause_json,
};

pub const CHECKPOINT_CAPTURE_OPERATION: &str = "GitVcsDriver.checkpoints.captureCheckpoint";
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(30_000);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_000_000;
const OUTPUT_TRUNCATED_MARKER: &str = "\n\n[truncated]";
const VCS_PROCESS_CONCURRENCY: usize = 8;
const GITHUB_PROCESS_CONCURRENCY: usize = 4;

pub struct VcsProcessInput {
    pub operation: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub spawn_cwd: Option<String>,
    pub stdin: Option<String>,
    pub on_stdout_chunk: Option<StdoutChunkCallback>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub clear_env: Vec<String>,
    pub allow_non_zero_exit: bool,
    pub timeout: Option<Duration>,
    pub max_output_bytes: Option<usize>,
    pub output_mode: Option<OutputMode>,
    pub append_truncation_marker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_invalid_utf8: bool,
    pub stderr_invalid_utf8: bool,
}

pub struct VcsProcess {
    runner: ProcessRunner,
    vcs: Arc<Semaphore>,
    github: Arc<Semaphore>,
}

impl VcsProcess {
    pub fn new(runner: ProcessRunner) -> Self {
        Self {
            runner,
            vcs: Arc::new(Semaphore::new(VCS_PROCESS_CONCURRENCY)),
            github: Arc::new(Semaphore::new(GITHUB_PROCESS_CONCURRENCY)),
        }
    }

    pub async fn run(&self, input: VcsProcessInput) -> Result<VcsProcessOutput, VcsError> {
        let checkpoint = input.command == "git"
            && input.operation == CHECKPOINT_CAPTURE_OPERATION
            && input.on_stdout_chunk.is_none();
        if !checkpoint {
            return self.run_bounded(&input).await;
        }

        let mut attempt = 0;
        loop {
            match self.run_bounded(&input).await {
                Ok(output) => return Ok(output),
                Err(error) => {
                    tracing::debug!(
                        operation = %input.operation,
                        error_tag = error_tag(&error),
                        "checkpoint Git command failed"
                    );
                    if attempt < 2 && retryable_exit(&error) {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(75)).await;
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    async fn run_bounded(&self, input: &VcsProcessInput) -> Result<VcsProcessOutput, VcsError> {
        // GitHub's permit is taken first, then a VCS slot, matching upstream.
        let _github = if input.command == "gh" {
            Some(
                self.github
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("github semaphore stays open"),
            )
        } else {
            None
        };
        let _vcs = self
            .vcs
            .clone()
            .acquire_owned()
            .await
            .expect("vcs semaphore stays open");
        self.run_unbounded(input).await
    }

    async fn run_unbounded(&self, input: &VcsProcessInput) -> Result<VcsProcessOutput, VcsError> {
        let context = VcsProcessErrorContext {
            operation: input.operation.clone(),
            command: input.command.clone(),
            cwd: input.cwd.clone(),
            argument_count: Some(input.args.len() as u64),
        };
        let result = self
            .runner
            .run(ProcessRunInput {
                command: input.command.clone(),
                args: input.args.clone(),
                cwd: Some(input.cwd.clone()),
                spawn_cwd: input.spawn_cwd.clone(),
                timeout: Some(Some(input.timeout.unwrap_or(DEFAULT_TIMEOUT))),
                env: input.env.clone(),
                clear_env: input.clear_env.clone(),
                stdin: input.stdin.clone(),
                on_stdout_chunk: input.on_stdout_chunk.clone(),
                max_output_bytes: Some(input.max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)),
                output_mode: Some(input.output_mode.unwrap_or(OutputMode::Truncate)),
                truncated_marker: Some(if input.append_truncation_marker {
                    OUTPUT_TRUNCATED_MARKER.to_owned()
                } else {
                    String::new()
                }),
                timeout_behavior: Some(TimeoutBehavior::Error),
            })
            .await
            .map_err(|error| map_runner_error(&context, input.stdin.as_ref(), error))?;

        let Some(code) = result.code else {
            return Err(VcsError::MissingExitCode(
                VcsProcessMissingExitCodeError::VcsProcessMissingExitCodeError {
                    operation: context.operation,
                    command: context.command,
                    cwd: context.cwd,
                    argument_count: context.argument_count,
                },
            ));
        };
        if !input.allow_non_zero_exit && code != 0 {
            let failure_kind = classify_non_zero_exit(&input.command, &result.stderr);
            let retryable = input.command == "git"
                && failure_kind == VcsProcessExitFailureKind::CommandFailed
                && is_transient_git_exit(&result.stderr);
            return Err(VcsError::Exit(VcsProcessExitError::from_process_exit(
                context,
                VcsProcessExitFailure {
                    exit_code: f64::from(code),
                    stderr: result.stderr,
                    stderr_truncated: result.stderr_truncated,
                },
                failure_kind,
                if retryable { Some(true) } else { None },
            )));
        }
        Ok(VcsProcessOutput {
            exit_code: code,
            stdout: result.stdout,
            stderr: result.stderr,
            stdout_truncated: result.stdout_truncated,
            stderr_truncated: result.stderr_truncated,
            stdout_invalid_utf8: result.stdout_invalid_utf8,
            stderr_invalid_utf8: result.stderr_invalid_utf8,
        })
    }
}

fn map_runner_error(
    context: &VcsProcessErrorContext,
    stdin: Option<&String>,
    error: ProcessRunError,
) -> VcsError {
    match error {
        ProcessRunError::Spawn { cause, .. } => {
            VcsError::Spawn(VcsProcessSpawnError::from_process_spawn_error(
                context.clone(),
                VcsProcessSpawnFailure {
                    cause: cause_json(cause),
                },
            ))
        }
        ProcessRunError::Timeout { timeout_ms, .. } => {
            VcsError::Timeout(VcsProcessTimeoutError::from_process_timeout_error(
                context.clone(),
                VcsProcessTimeoutFailure { timeout_ms },
            ))
        }
        ProcessRunError::Stdin { cause, .. } => {
            VcsError::StdinWrite(VcsProcessStdinWriteError::VcsProcessStdinWriteError {
                operation: context.operation.clone(),
                command: context.command.clone(),
                cwd: context.cwd.clone(),
                argument_count: context.argument_count,
                stdin_bytes: stdin.map(String::len).unwrap_or(0) as u64,
                cause: cause_json(cause),
            })
        }
        ProcessRunError::Read { stream, cause, .. } => {
            VcsError::OutputRead(VcsProcessOutputReadError::VcsProcessOutputReadError {
                operation: context.operation.clone(),
                command: context.command.clone(),
                cwd: context.cwd.clone(),
                argument_count: context.argument_count,
                stream: match stream {
                    ProcessStream::Stdout => VcsProcessStream::Stdout,
                    ProcessStream::Stderr => VcsProcessStream::Stderr,
                    ProcessStream::ExitCode => VcsProcessStream::ExitCode,
                },
                cause: cause_json(cause),
            })
        }
        ProcessRunError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
            ..
        } => VcsError::OutputLimit(VcsProcessOutputLimitError::VcsProcessOutputLimitError {
            operation: context.operation.clone(),
            command: context.command.clone(),
            cwd: context.cwd.clone(),
            argument_count: context.argument_count,
            stream: match stream {
                ProcessStream::Stderr => VcsProcessOutputStream::Stderr,
                ProcessStream::Stdout | ProcessStream::ExitCode => VcsProcessOutputStream::Stdout,
            },
            max_bytes,
            observed_bytes,
        }),
    }
}

fn classify_non_zero_exit(command: &str, stderr: &str) -> VcsProcessExitFailureKind {
    let normalized = stderr.to_lowercase();
    if normalized.contains("authentication failed")
        || normalized.contains("not logged in")
        || normalized.contains("gh auth login")
        || normalized.contains("glab auth login")
        || normalized.contains("az devops login")
        || normalized.contains("please run az login")
        || normalized.contains("no oauth token")
        || normalized.contains("unauthorized")
    {
        return VcsProcessExitFailureKind::Authentication;
    }
    if normalized.contains("api rate limit")
        || normalized.contains("rate limit exceeded")
        || normalized.contains("secondary rate limit")
        || normalized.contains("too many requests")
        || normalized.contains("http 429")
    {
        return VcsProcessExitFailureKind::RateLimited;
    }
    let not_found = (command == "gh"
        && (normalized.contains("could not resolve to a pullrequest")
            || normalized.contains("repository.pullrequest")
            || normalized.contains("no pull requests found for branch")
            || normalized.contains("pull request not found")))
        || (command == "glab"
            && (normalized.contains("merge request not found")
                || normalized.contains("not found")
                || normalized.contains("404")))
        || (command == "az"
            && normalized.contains("pull request")
            && (normalized.contains("not found") || normalized.contains("does not exist")));
    if not_found {
        return VcsProcessExitFailureKind::NotFound;
    }
    VcsProcessExitFailureKind::CommandFailed
}

fn is_transient_git_exit(stderr: &str) -> bool {
    static LOCK: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r#"(?i)unable to create [^\n]*\.lock['"]?: file exists"#).unwrap()
    });
    static MISSING: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)(?:unable to stat|lstat\(|error: open\()[^\n]+: no such file or directory")
            .unwrap()
    });
    LOCK.is_match(stderr) || MISSING.is_match(stderr)
}

fn retryable_exit(error: &VcsError) -> bool {
    matches!(
        error,
        VcsError::Exit(VcsProcessExitError::VcsProcessExitError {
            retryable: Some(true),
            ..
        })
    )
}

fn error_tag(error: &VcsError) -> &'static str {
    match error {
        VcsError::Spawn(_) => "VcsProcessSpawnError",
        VcsError::Exit(_) => "VcsProcessExitError",
        VcsError::Timeout(_) => "VcsProcessTimeoutError",
        VcsError::StdinWrite(_) => "VcsProcessStdinWriteError",
        VcsError::OutputRead(_) => "VcsProcessOutputReadError",
        VcsError::OutputLimit(_) => "VcsProcessOutputLimitError",
        VcsError::MissingExitCode(_) => "VcsProcessMissingExitCodeError",
        VcsError::RepositoryDetection(_) => "VcsRepositoryDetectionError",
        VcsError::UnsupportedOperation(_) => "VcsUnsupportedOperationError",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_auth_rate_limit_and_missing_pull_request() {
        assert_eq!(
            classify_non_zero_exit("gh", "please run gh auth login"),
            VcsProcessExitFailureKind::Authentication
        );
        assert_eq!(
            classify_non_zero_exit("gh", "HTTP 429"),
            VcsProcessExitFailureKind::RateLimited
        );
        assert_eq!(
            classify_non_zero_exit("gh", "pull request not found"),
            VcsProcessExitFailureKind::NotFound
        );
        assert_eq!(
            classify_non_zero_exit("git", "pull request not found"),
            VcsProcessExitFailureKind::CommandFailed
        );
    }

    #[test]
    fn transient_git_exits_are_lock_and_missing_stat() {
        assert!(is_transient_git_exit(
            "fatal: Unable to create '/repo/.git/index.lock': File exists."
        ));
        assert!(is_transient_git_exit(
            "error: open(.git/refs/heads/main): No such file or directory"
        ));
        assert!(!is_transient_git_exit("fatal: not a git repository"));
    }

    #[tokio::test]
    async fn non_zero_git_becomes_an_exit_error_unless_allowed() {
        let process = VcsProcess::new(ProcessRunner);
        let error = process
            .run(VcsProcessInput {
                operation: "status".into(),
                command: "sh".into(),
                args: vec!["-c".into(), "echo nope >&2; exit 2".into()],
                cwd: ".".into(),
                spawn_cwd: None,
                stdin: None,
                on_stdout_chunk: None,
                env: None,
                clear_env: Vec::new(),
                allow_non_zero_exit: false,
                timeout: Some(Duration::from_secs(5)),
                max_output_bytes: None,
                output_mode: None,
                append_truncation_marker: false,
            })
            .await
            .expect_err("exit");
        assert!(matches!(error, VcsError::Exit(_)));
    }
}
