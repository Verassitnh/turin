//! Port of upstream/apps/server/src/processRunner.ts
//!
//! Timeouts and output-limit failures kill the child. On Unix the child is its
//! own process group so hooks and `ssh` die with it; a dropped `JoinHandle`
//! would detach that work, so every spawned reader is an `AbortOnDropHandle`.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio_util::task::AbortOnDropHandle;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Error,
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutBehavior {
    Error,
    TimedOutResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStream {
    Stdout,
    Stderr,
    ExitCode,
}

pub type StdoutChunkCallback = std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>;

pub struct ProcessRunInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub spawn_cwd: Option<String>,
    /// `None` keeps the 60s default. `Some(None)` waits until the process exits.
    pub timeout: Option<Option<Duration>>,
    pub env: Option<HashMap<String, String>>,
    /// Removed after `env` is merged over the parent environment.
    pub clear_env: Vec<String>,
    pub stdin: Option<String>,
    pub on_stdout_chunk: Option<StdoutChunkCallback>,
    pub on_stderr_chunk: Option<StdoutChunkCallback>,
    pub max_output_bytes: Option<usize>,
    pub output_mode: Option<OutputMode>,
    pub truncated_marker: Option<String>,
    pub timeout_behavior: Option<TimeoutBehavior>,
}

impl ProcessRunInput {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            cwd: None,
            spawn_cwd: None,
            timeout: None,
            env: None,
            clear_env: Vec::new(),
            stdin: None,
            on_stdout_chunk: None,
            on_stderr_chunk: None,
            max_output_bytes: None,
            output_mode: None,
            truncated_marker: None,
            timeout_behavior: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRunOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_invalid_utf8: bool,
    pub stderr_invalid_utf8: bool,
}

#[derive(Debug, Error)]
pub enum ProcessRunError {
    #[error("failed to spawn process '{command}'")]
    Spawn {
        command: String,
        argument_count: usize,
        cwd: Option<String>,
        spawn_cwd: Option<String>,
        cause: String,
    },
    #[error("failed to write stdin for process '{command}'")]
    Stdin {
        command: String,
        argument_count: usize,
        cwd: Option<String>,
        spawn_cwd: Option<String>,
        stdin_bytes: usize,
        cause: String,
    },
    #[error("process '{command}' {stream:?} exceeded {max_bytes} bytes ({observed_bytes})")]
    OutputLimit {
        command: String,
        argument_count: usize,
        cwd: Option<String>,
        spawn_cwd: Option<String>,
        stream: ProcessStream,
        max_bytes: u64,
        observed_bytes: u64,
    },
    #[error("failed to read {stream:?} for process '{command}'")]
    Read {
        command: String,
        argument_count: usize,
        cwd: Option<String>,
        spawn_cwd: Option<String>,
        stream: ProcessStream,
        cause: String,
    },
    #[error("process '{command}' timed out after {timeout_ms}ms")]
    Timeout {
        command: String,
        argument_count: usize,
        cwd: Option<String>,
        spawn_cwd: Option<String>,
        timeout_ms: f64,
    },
}

#[derive(Debug, Clone)]
struct Invocation {
    command: String,
    argument_count: usize,
    cwd: Option<String>,
    spawn_cwd: Option<String>,
}

impl Invocation {
    fn from_input(input: &ProcessRunInput) -> Self {
        Self {
            command: input.command.clone(),
            argument_count: input.args.len(),
            cwd: input.cwd.clone(),
            spawn_cwd: input.spawn_cwd.clone(),
        }
    }
}

struct Collected {
    text: String,
    truncated: bool,
    invalid_utf8: bool,
}

#[derive(Debug)]
struct CollectError {
    stream: ProcessStream,
    kind: CollectErrorKind,
}

#[derive(Debug)]
enum CollectErrorKind {
    Io(String),
    Limit { max_bytes: u64, observed_bytes: u64 },
}

/// Run one child process. The caller decides what a non-zero exit means.
pub struct ProcessRunner;

impl ProcessRunner {
    pub async fn run(&self, input: ProcessRunInput) -> Result<ProcessRunOutput, ProcessRunError> {
        let invocation = Invocation::from_input(&input);
        let timeout = match input.timeout {
            None => Some(DEFAULT_TIMEOUT),
            Some(duration) => duration,
        };
        let timeout_behavior = input.timeout_behavior.unwrap_or(TimeoutBehavior::Error);
        let max_output_bytes = input.max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
        let output_mode = input.output_mode.unwrap_or(OutputMode::Error);
        let truncated_marker = input.truncated_marker.clone().unwrap_or_default();
        let stdout_marker = truncated_marker.clone();
        let stderr_marker = truncated_marker;

        let mut command = Command::new(&input.command);
        command
            .args(&input.args)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        if let Some(dir) = input.spawn_cwd.as_ref().or(input.cwd.as_ref()) {
            command.current_dir(dir);
        }
        apply_env(&mut command, input.env.as_ref(), &input.clear_env);
        #[cfg(unix)]
        {
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|error| ProcessRunError::Spawn {
            command: invocation.command.clone(),
            argument_count: invocation.argument_count,
            cwd: invocation.cwd.clone(),
            spawn_cwd: invocation.spawn_cwd.clone(),
            cause: error.to_string(),
        })?;
        let pid = child.id();
        // Declared after `child` so a cancelled run kills the group before
        // `kill_on_drop` signals only the leader.
        let _group = KillGroupGuard(pid);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let stdin_text = input.stdin.clone();
        let on_stdout_chunk = input.on_stdout_chunk.clone();
        let on_stderr_chunk = input.on_stderr_chunk.clone();

        let work_invocation = invocation.clone();
        let stdin_bytes = stdin_text.as_ref().map(String::len).unwrap_or(0);
        let work = async move {
            let stdout_handle = AbortOnDropHandle::new(tokio::spawn(collect_stream(
                stdout,
                ProcessStream::Stdout,
                max_output_bytes,
                output_mode,
                stdout_marker,
                on_stdout_chunk,
            )));
            let stderr_handle = AbortOnDropHandle::new(tokio::spawn(collect_stream(
                stderr,
                ProcessStream::Stderr,
                max_output_bytes,
                output_mode,
                stderr_marker,
                on_stderr_chunk,
            )));
            let stdin_handle = AbortOnDropHandle::new(tokio::spawn(write_stdin(stdin, stdin_text)));
            // Readers and the stdin writer run together so a full pipe cannot stall git.
            let (stdout, stderr, stdin_result) =
                tokio::try_join!(stdout_handle, stderr_handle, stdin_handle).map_err(|error| {
                    map_join_error(&work_invocation, ProcessStream::Stdout, error)
                })?;
            if let Err(cause) = stdin_result {
                return Err(ProcessRunError::Stdin {
                    command: work_invocation.command.clone(),
                    argument_count: work_invocation.argument_count,
                    cwd: work_invocation.cwd.clone(),
                    spawn_cwd: work_invocation.spawn_cwd.clone(),
                    stdin_bytes,
                    cause,
                });
            }
            Ok::<_, ProcessRunError>((
                flatten_collect(&work_invocation, stdout)?,
                flatten_collect(&work_invocation, stderr)?,
            ))
        };

        let joined = if let Some(duration) = timeout {
            match tokio::time::timeout(duration, work).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    kill_tree(pid, &mut child).await;
                    return match timeout_behavior {
                        TimeoutBehavior::TimedOutResult => Ok(ProcessRunOutput {
                            stdout: String::new(),
                            stderr: String::new(),
                            code: None,
                            timed_out: true,
                            stdout_truncated: false,
                            stderr_truncated: false,
                            stdout_invalid_utf8: false,
                            stderr_invalid_utf8: false,
                        }),
                        TimeoutBehavior::Error => Err(ProcessRunError::Timeout {
                            command: invocation.command,
                            argument_count: invocation.argument_count,
                            cwd: invocation.cwd,
                            spawn_cwd: invocation.spawn_cwd,
                            timeout_ms: duration.as_secs_f64() * 1000.0,
                        }),
                    };
                }
            }
        } else {
            work.await
        };

        let (stdout, stderr) = match joined {
            Ok(streams) => streams,
            Err(error) => {
                kill_tree(pid, &mut child).await;
                return Err(error);
            }
        };

        let status = child.wait().await.map_err(|error| ProcessRunError::Read {
            command: invocation.command.clone(),
            argument_count: invocation.argument_count,
            cwd: invocation.cwd.clone(),
            spawn_cwd: invocation.spawn_cwd.clone(),
            stream: ProcessStream::ExitCode,
            cause: error.to_string(),
        })?;
        // The leader has exited. Anything it backgrounded is still in the group.
        kill_group(pid);

        Ok(ProcessRunOutput {
            stdout: stdout.text,
            stderr: stderr.text,
            code: status.code(),
            timed_out: false,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            stdout_invalid_utf8: stdout.invalid_utf8,
            stderr_invalid_utf8: stderr.invalid_utf8,
        })
    }
}

fn apply_env(command: &mut Command, env: Option<&HashMap<String, String>>, clear: &[String]) {
    if env.is_none() && clear.is_empty() {
        return;
    }
    let mut merged: HashMap<String, String> = std::env::vars().collect();
    if let Some(env) = env {
        merged.extend(env.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    for key in clear {
        merged.remove(key);
    }
    command.env_clear();
    for (key, value) in merged {
        command.env(key, value);
    }
}

fn map_join_error(
    invocation: &Invocation,
    stream: ProcessStream,
    error: tokio::task::JoinError,
) -> ProcessRunError {
    ProcessRunError::Read {
        command: invocation.command.clone(),
        argument_count: invocation.argument_count,
        cwd: invocation.cwd.clone(),
        spawn_cwd: invocation.spawn_cwd.clone(),
        stream,
        cause: error.to_string(),
    }
}

fn flatten_collect(
    invocation: &Invocation,
    joined: Result<Collected, CollectError>,
) -> Result<Collected, ProcessRunError> {
    match joined {
        Ok(collected) => Ok(collected),
        Err(error) => Err(match error.kind {
            CollectErrorKind::Io(cause) => ProcessRunError::Read {
                command: invocation.command.clone(),
                argument_count: invocation.argument_count,
                cwd: invocation.cwd.clone(),
                spawn_cwd: invocation.spawn_cwd.clone(),
                stream: error.stream,
                cause,
            },
            CollectErrorKind::Limit {
                max_bytes,
                observed_bytes,
            } => ProcessRunError::OutputLimit {
                command: invocation.command.clone(),
                argument_count: invocation.argument_count,
                cwd: invocation.cwd.clone(),
                spawn_cwd: invocation.spawn_cwd.clone(),
                stream: error.stream,
                max_bytes,
                observed_bytes,
            },
        }),
    }
}

async fn collect_stream<R>(
    pipe: Option<R>,
    stream: ProcessStream,
    max_output_bytes: usize,
    output_mode: OutputMode,
    truncated_marker: String,
    on_chunk: Option<StdoutChunkCallback>,
) -> Result<Collected, CollectError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(Collected {
            text: String::new(),
            truncated: false,
            invalid_utf8: false,
        });
    };
    let mut stored = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buf).await.map_err(|error| CollectError {
            stream,
            kind: CollectErrorKind::Io(error.to_string()),
        })?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        if let Some(callback) = &on_chunk {
            callback(chunk);
        }
        if truncated {
            continue;
        }
        let remaining = max_output_bytes.saturating_sub(stored.len());
        if output_mode == OutputMode::Error && chunk.len() > remaining {
            return Err(CollectError {
                stream,
                kind: CollectErrorKind::Limit {
                    max_bytes: max_output_bytes as u64,
                    observed_bytes: (stored.len() + chunk.len()) as u64,
                },
            });
        }
        if chunk.len() > remaining {
            stored.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            continue;
        }
        stored.extend_from_slice(chunk);
    }
    // An exact-size stream is not truncated. Truncation is only a chunk that
    // did not fit, which the loop above already recorded.
    let invalid_utf8 = std::str::from_utf8(&stored).is_err();
    let mut text = String::from_utf8_lossy(&stored).into_owned();
    if truncated && !truncated_marker.is_empty() {
        text.push_str(&truncated_marker);
    }
    Ok(Collected {
        text,
        truncated,
        invalid_utf8,
    })
}

async fn write_stdin(stdin: Option<ChildStdin>, text: Option<String>) -> Result<(), String> {
    let Some(text) = text else {
        return Ok(());
    };
    let Some(mut stdin) = stdin else {
        return Err("stdin pipe missing".to_owned());
    };
    stdin
        .write_all(text.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stdin.shutdown().await.map_err(|error| error.to_string())
}

async fn kill_tree(pid: Option<u32>, child: &mut Child) {
    kill_group(pid);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

struct KillGroupGuard(Option<u32>);

impl Drop for KillGroupGuard {
    fn drop(&mut self) {
        kill_group(self.0.take());
    }
}

fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    #[cfg(unix)]
    // SAFETY: `pid` is the child's id and `process_group(0)` made it the leader.
    // A negative pid asks the kernel to signal that group, including hooks it spawned.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

pub fn cause_json(message: impl AsRef<str>) -> Value {
    Value::String(message.as_ref().to_owned())
}

pub fn io_not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> ProcessRunner {
        ProcessRunner
    }

    #[tokio::test]
    async fn echo_returns_stdout_and_zero() {
        let output = runner()
            .run(ProcessRunInput::new("echo", vec!["hello".into()]))
            .await
            .expect("echo");
        assert_eq!(output.stdout, "hello\n");
        assert_eq!(output.code, Some(0));
        assert!(!output.stdout_truncated);
    }

    #[tokio::test]
    async fn captures_stdout_stderr_and_stdin() {
        let output = runner()
            .run(ProcessRunInput {
                stdin: Some("ping".into()),
                ..ProcessRunInput::new(
                    "sh",
                    vec!["-c".into(), "cat; echo out; echo err >&2".into()],
                )
            })
            .await
            .expect("sh");
        assert_eq!(output.code, Some(0));
        assert_eq!(output.stdout, "pingout\n");
        assert_eq!(output.stderr, "err\n");
        assert!(!output.timed_out);
    }

    #[tokio::test]
    async fn non_zero_exit_is_not_a_runner_error() {
        let output = runner()
            .run(ProcessRunInput::new(
                "sh",
                vec!["-c".into(), "exit 3".into()],
            ))
            .await
            .expect("sh");
        assert_eq!(output.code, Some(3));
    }

    #[tokio::test]
    async fn error_mode_rejects_output_past_the_cap() {
        let error = runner()
            .run(ProcessRunInput {
                max_output_bytes: Some(16),
                output_mode: Some(OutputMode::Error),
                timeout: Some(Some(Duration::from_secs(5))),
                ..ProcessRunInput::new(
                    "sh",
                    vec!["-c".into(), "dd if=/dev/zero bs=64 count=1".into()],
                )
            })
            .await
            .expect_err("limit");
        match error {
            ProcessRunError::OutputLimit {
                stream: ProcessStream::Stdout,
                max_bytes,
                observed_bytes,
                ..
            } => {
                assert_eq!(max_bytes, 16);
                assert!(observed_bytes > 16);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncate_mode_keeps_a_prefix_and_drains() {
        let output = runner()
            .run(ProcessRunInput {
                max_output_bytes: Some(8),
                output_mode: Some(OutputMode::Truncate),
                truncated_marker: Some("\n\n[truncated]".into()),
                timeout: Some(Some(Duration::from_secs(5))),
                ..ProcessRunInput::new("sh", vec!["-c".into(), "printf 'abcdefghijklmnop'".into()])
            })
            .await
            .expect("truncate");
        assert_eq!(output.code, Some(0));
        assert!(output.stdout_truncated);
        assert!(output.stdout.starts_with("abcdefgh"));
        assert!(output.stdout.ends_with("\n\n[truncated]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("child.pid");
        let script = format!("sleep 60 &\necho $! > {}\nwait\n", pid_path.display());
        let started = std::time::Instant::now();
        let error = runner()
            .run(ProcessRunInput {
                timeout: Some(Some(Duration::from_millis(500))),
                ..ProcessRunInput::new("sh", vec!["-c".into(), script])
            })
            .await
            .expect_err("timeout");
        assert!(matches!(error, ProcessRunError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: i32 = std::fs::read_to_string(&pid_path)
            .expect("pid file")
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(!alive, "grandchild {pid} survived the timeout");
    }

    #[tokio::test]
    async fn invalid_utf8_is_flagged() {
        let output = runner()
            .run(ProcessRunInput::new(
                "sh",
                vec!["-c".into(), "printf '\\377'".into()],
            ))
            .await
            .expect("printf");
        assert!(output.stdout_invalid_utf8);
        assert_eq!(output.stdout, "\u{FFFD}");
    }

    #[tokio::test]
    async fn missing_command_is_a_spawn_error() {
        let error = runner()
            .run(ProcessRunInput::new(
                "turin-git-missing-command",
                Vec::new(),
            ))
            .await
            .expect_err("spawn");
        assert!(matches!(error, ProcessRunError::Spawn { .. }));
    }
}
