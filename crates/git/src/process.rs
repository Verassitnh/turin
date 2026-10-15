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
/// Upstream `processGroupGraceMillis`. Time a signalled group gets to exit
/// after SIGTERM before it is SIGKILLed. SIGTERM lets git remove its own
/// `index.lock` and ref locks; SIGKILL leaves them behind.
const KILL_GRACE: Duration = Duration::from_secs(1);
/// Upstream `processGroupPollIntervalMillis`.
const KILL_POLL: Duration = Duration::from_millis(10);

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
        // No `kill_on_drop`: tokio would SIGKILL the leader before `KillGroupGuard`
        // can send SIGTERM. The guard signals the whole group on every early exit.
        command
            .args(&input.args)
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
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let stdin_text = input.stdin.clone();
        let on_stdout_chunk = input.on_stdout_chunk.clone();
        let on_stderr_chunk = input.on_stderr_chunk.clone();

        let work_invocation = invocation.clone();
        let stdin_bytes = stdin_text.as_ref().map(String::len).unwrap_or(0);
        // Pinned so a timeout can signal the group while the readers still run.
        // Dropping the readers first closes the pipes, and a shell writing to
        // them dies of SIGPIPE before its SIGTERM trap can clean up.
        let mut work = std::pin::pin!(async move {
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
        });
        // Declared after `work`: on cancellation the guard drops first and
        // signals the group before the readers are aborted.
        let mut group = KillGroupGuard(pid);

        let joined = if let Some(duration) = timeout {
            match tokio::time::timeout(duration, work.as_mut()).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    kill_tree(pid, &mut child).await;
                    group.disarm();
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
                group.disarm();
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
        terminate_group(pid).await;
        group.disarm();

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

#[derive(Clone, Copy)]
enum GroupSignal {
    Term,
    Kill,
}

/// Stops a timed-out or failed child. The leader gets the grace window to
/// clean up, then whatever is still in the group is SIGKILLed.
async fn kill_tree(pid: Option<u32>, child: &mut Child) {
    let deadline = tokio::time::Instant::now() + KILL_GRACE;
    signal_group(pid, GroupSignal::Term);
    let leader_exited = tokio::time::timeout_at(deadline, child.wait())
        .await
        .is_ok();
    wait_for_group_exit(pid, deadline).await;
    kill_group_if_alive(pid);
    if !leader_exited {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Clean exit: the leader is already reaped, so only backgrounded work can
/// remain. Give it the same SIGTERM, grace, SIGKILL sequence.
async fn terminate_group(pid: Option<u32>) {
    signal_group(pid, GroupSignal::Term);
    wait_for_group_exit(pid, tokio::time::Instant::now() + KILL_GRACE).await;
    kill_group_if_alive(pid);
}

async fn wait_for_group_exit(pid: Option<u32>, deadline: tokio::time::Instant) {
    while group_alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(KILL_POLL).await;
    }
}

/// Dropping the guard means the caller cancelled or unwound mid-run. `Drop`
/// cannot await, so SIGTERM goes out now and the SIGKILL fallback runs on a
/// detached thread. Early exits that already killed the group call `disarm`.
struct KillGroupGuard(Option<u32>);

impl KillGroupGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillGroupGuard {
    fn drop(&mut self) {
        let Some(pid) = self.0.take() else {
            return;
        };
        signal_group(Some(pid), GroupSignal::Term);
        std::thread::spawn(move || {
            std::thread::sleep(KILL_GRACE);
            kill_group_if_alive(Some(pid));
        });
    }
}

fn kill_group_if_alive(pid: Option<u32>) {
    if group_alive(pid) {
        signal_group(pid, GroupSignal::Kill);
    }
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: GroupSignal) {
    let Some(pid) = pid else {
        return;
    };
    let signal = match signal {
        GroupSignal::Term => libc::SIGTERM,
        GroupSignal::Kill => libc::SIGKILL,
    };
    // SAFETY: `pid` is the child's id and `process_group(0)` made it the leader.
    // A negative pid asks the kernel to signal that group, including hooks it spawned.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

/// Whether any process still has the group id. A zombie leader counts until it
/// is reaped, so callers reap the leader before asking.
#[cfg(unix)]
fn group_alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence and sends nothing.
    unsafe { libc::kill(-(pid as i32), 0) == 0 }
}

#[cfg(windows)]
fn signal_group(pid: Option<u32>, _signal: GroupSignal) {
    let Some(pid) = pid else {
        return;
    };
    // `taskkill` has no graceful step here, so Windows children are stopped at
    // the TERM stage and `group_alive` reports false afterwards.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .status();
}

#[cfg(windows)]
fn group_alive(_pid: Option<u32>) -> bool {
    false
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

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_sends_sigterm_so_the_leader_can_clean_up() {
        // git removes `.git/index.lock` from its SIGTERM handler. SIGKILL skips it.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("cleaned");
        let script = format!(
            "trap 'echo cleaned > {}; exit 0' TERM\nwhile :; do sleep 0.05; done\n",
            marker.display()
        );
        let error = runner()
            .run(ProcessRunInput {
                timeout: Some(Some(Duration::from_millis(300))),
                ..ProcessRunInput::new("sh", vec!["-c".into(), script])
            })
            .await
            .expect_err("timeout");
        assert!(matches!(error, ProcessRunError::Timeout { .. }));
        assert!(marker.exists(), "leader did not run its SIGTERM cleanup");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_escalates_to_sigkill_when_sigterm_is_ignored() {
        let script = "trap '' TERM\nsleep 30\n".to_owned();
        let started = std::time::Instant::now();
        let error = runner()
            .run(ProcessRunInput {
                timeout: Some(Some(Duration::from_millis(200))),
                ..ProcessRunInput::new("sh", vec!["-c".into(), script])
            })
            .await
            .expect_err("timeout");
        assert!(matches!(error, ProcessRunError::Timeout { .. }));
        // The grace window ran first, then SIGKILL ended `sleep 30` well before it would exit.
        let elapsed = started.elapsed();
        assert!(
            elapsed >= KILL_GRACE,
            "SIGKILL came before the grace window"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "SIGKILL never came: {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_run_sends_sigterm_to_the_group() {
        // Caller abort drops the future. The guard must SIGTERM the leader; it must not
        // SIGKILL it outright, or the cleanup trap never runs. The script detaches its
        // stdio: dropping the readers closes the pipes at the same moment, and a shell
        // that still writes to them dies of SIGPIPE before its trap runs.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("cleaned");
        let script = format!(
            "exec >/dev/null 2>&1\ntrap 'echo cleaned > {}; exit 0' TERM\nwhile :; do sleep 0.05; done\n",
            marker.display()
        );
        let run = ProcessRunner.run(ProcessRunInput {
            timeout: Some(None),
            ..ProcessRunInput::new("sh", vec!["-c".into(), script])
        });
        let _ = tokio::time::timeout(Duration::from_millis(300), run).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(marker.exists(), "dropped run did not SIGTERM its leader");
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
