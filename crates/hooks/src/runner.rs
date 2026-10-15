//! Hook runner: matches events to configured hooks and runs them.
//!
//! New functionality (PORTING.md "Hooks (new)"). Process conventions ported
//! from upstream `apps/server/src/processRunner.ts` (bounded output capture,
//! timeout result shape) and `apps/server/src/project/ProjectSetupScriptRunner.ts`
//! (captured-output env hygiene: `NO_COLOR`/`FORCE_COLOR`/empty `COLORTERM`).
//!
//! Every hook runs as `/bin/sh -c <command>` in its own process group; the
//! group is killed on timeout, on shutdown, and if the owning task is
//! aborted, so grandchildren (background jobs, dev servers) cannot survive.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;
use turin_contracts::{ProjectId, ThreadId};

use notify::Watcher;

use crate::config::{
    HookConfigSource, HookCwd, HookPaths, HookSpec, HooksConfig, PendingProjectHooks,
    ProjectHooksStatus,
};
use crate::event::{HookEvent, HookEventKind};
use crate::references::normalize_lexically;
use crate::trust::{TrustError, TrustStore, hash_config_tree};

/// Per-stream output capture limit, mirroring upstream's bounded capture.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Appended when a stream's output was cut at the limit.
pub const TRUNCATED_MARKER: &str = "\n...[turin: output truncated]";
/// Coalescing window for config-file change events before a hot reload.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);
/// How long to keep draining stdout/stderr after the shell has exited. A
/// background job that inherited the pipes holds them open past that point;
/// once the grace expires the run returns and the leftover group is killed.
const DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Capacity of the results broadcast channel. Results are informational; a
/// lagging consumer drops old results rather than blocking hook execution.
const RESULTS_CHANNEL_CAPACITY: usize = 1024;

/// Everything a dispatched event knows about the project and thread it
/// belongs to. Drives cwd resolution, env vars, and the stdin envelope.
#[derive(Debug, Clone)]
pub struct HookDispatchContext {
    pub project_id: ProjectId,
    pub project_root: PathBuf,
    pub thread_id: Option<ThreadId>,
    /// The thread's workspace path (a worktree), when the event has one.
    pub thread_cwd: Option<PathBuf>,
}

/// What a blocking `thread.created` hook reports back through the return
/// protocol: its last non-blank stdout line, a JSON object
/// `{"worktreePath": "...", "branch": "..."}` (both optional). The
/// orchestrator binds the new thread to this workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookReturn {
    /// Existing directory the thread should run in. A relative path in the
    /// hook's output is resolved against the directory the hook ran in.
    #[serde(default)]
    pub worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// Parses the return protocol from a hook's stdout. Malformed or absent
/// output is `None` (no change), never an error. A `worktreePath` that does
/// not name an existing directory is dropped.
pub fn parse_hook_return(stdout: &str, hook_cwd: &Path) -> Option<HookReturn> {
    let line = stdout.lines().rev().find(|line| !line.trim().is_empty())?;
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let raw: HookReturn = serde_json::from_str(line).ok()?;
    let branch = raw
        .branch
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty());
    let worktree_path = raw
        .worktree_path
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| normalize_lexically(&hook_cwd.join(path)))
        .filter(|path| {
            let exists = path.is_dir();
            if !exists {
                tracing::warn!(path = %path.display(), "hook returned a worktreePath that is not an existing directory; ignoring it");
            }
            exists
        });
    if worktree_path.is_none() && branch.is_none() {
        return None;
    }
    Some(HookReturn {
        worktree_path,
        branch,
    })
}

/// Outcome of a single hook run, in upstream `ProcessRunOutput` shape plus
/// hook attribution. On timeout the output fields are empty (upstream
/// `timeoutBehavior: "timedOutResult"` also discards partials).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookRunResult {
    pub event: HookEventKind,
    pub command: String,
    pub source: HookConfigSource,
    pub hook_index: usize,
    /// Directory the command ran in.
    pub cwd: PathBuf,
    /// Whether the orchestrator actually waited for this run (after
    /// `thread.created`-only coercion).
    pub blocking: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_invalid_utf8: bool,
    pub stderr_invalid_utf8: bool,
    /// Spawn or wait failure, when the command never produced an exit code.
    pub error: Option<String>,
    /// Workspace the hook reported through the return protocol. Only parsed
    /// for blocking `thread.created` hooks that exited 0.
    pub returned: Option<HookReturn>,
}

/// Everything the results channel carries.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HookUpdate {
    /// A hook run finished (blocking or not).
    Run(Box<HookRunResult>),
    /// Project hooks exist but are not trusted (never approved, or changed
    /// since approval). Nothing in them runs; the UI offers "Approve" via
    /// [`HookRunner::trust_project_hooks`].
    ProjectHooksPending(Box<PendingProjectHooks>),
}

impl HookUpdate {
    pub fn as_run(&self) -> Option<&HookRunResult> {
        match self {
            Self::Run(run) => Some(run),
            Self::ProjectHooksPending(_) => None,
        }
    }
}

/// What [`HookRunner::dispatch`] did: awaited results for blocking hooks and
/// the number of non-blocking runs it spawned into the background.
#[derive(Debug, Clone, Default)]
pub struct DispatchSummary {
    pub blocking: Vec<HookRunResult>,
    pub non_blocking_spawned: usize,
    /// Set when untrusted/changed project hooks subscribe to this event and
    /// were therefore skipped.
    pub project_hooks_pending: Option<PendingProjectHooks>,
    /// Workspace reported by blocking `thread.created` hooks (return
    /// protocol). Later hooks override earlier ones field by field. `None`
    /// when no hook reported anything: the thread keeps its workspace.
    pub thread_workspace: Option<HookReturn>,
}

/// Resolves a hook's working directory. `cwd = "thread"` with no thread cwd
/// in the context falls back to the project root.
pub fn resolve_hook_cwd(cwd: HookCwd, context: &HookDispatchContext) -> PathBuf {
    match cwd {
        HookCwd::Project => context.project_root.clone(),
        HookCwd::Thread => context
            .thread_cwd
            .clone()
            .unwrap_or_else(|| context.project_root.clone()),
    }
}

/// Blocking is only honored for `thread.created`; every other event coerces
/// it to non-blocking. Returns `(should_block, coerced)`.
pub fn resolve_blocking(event: HookEventKind, spec_blocking: bool) -> (bool, bool) {
    if spec_blocking && event != HookEventKind::ThreadCreated {
        (false, true)
    } else {
        (spec_blocking, false)
    }
}

/// The `TURIN_*` variables added to the hook's environment, mirroring
/// upstream `projectScriptRuntimeEnv` (`T3CODE_PROJECT_ROOT`,
/// `T3CODE_WORKTREE_PATH`).
pub fn hook_env_vars(
    event: HookEventKind,
    context: &HookDispatchContext,
    cwd: &Path,
) -> Vec<(&'static str, String)> {
    let mut vars: Vec<(&'static str, String)> = vec![
        ("TURIN_EVENT", event.as_str().to_owned()),
        ("TURIN_PROJECT_ID", context.project_id.as_str().to_owned()),
        (
            "TURIN_PROJECT_ROOT",
            context.project_root.display().to_string(),
        ),
        ("TURIN_CWD", cwd.display().to_string()),
    ];
    if let Some(thread_id) = &context.thread_id {
        vars.push(("TURIN_THREAD_ID", thread_id.as_str().to_owned()));
    }
    if let Some(thread_cwd) = &context.thread_cwd {
        vars.push(("TURIN_WORKTREE_PATH", thread_cwd.display().to_string()));
    }
    vars
}

/// The JSON object written to the hook's stdin.
pub fn hook_stdin_json(
    event: &HookEvent,
    context: &HookDispatchContext,
    cwd: &Path,
) -> serde_json::Value {
    let mut root = serde_json::Map::new();
    root.insert(
        "event".to_owned(),
        serde_json::Value::String(event.kind.as_str().to_owned()),
    );
    root.insert(
        "projectId".to_owned(),
        serde_json::Value::String(context.project_id.as_str().to_owned()),
    );
    if let Some(thread_id) = &context.thread_id {
        root.insert(
            "threadId".to_owned(),
            serde_json::Value::String(thread_id.as_str().to_owned()),
        );
    }
    root.insert(
        "projectRoot".to_owned(),
        serde_json::Value::String(context.project_root.display().to_string()),
    );
    root.insert(
        "cwd".to_owned(),
        serde_json::Value::String(cwd.display().to_string()),
    );
    root.insert("payload".to_owned(), event.payload.clone());
    serde_json::Value::Object(root)
}

/// Runs matching hooks for `event`. Blocking hooks (thread.created only) run
/// sequentially — global first, then project — and are awaited; everything
/// else is spawned into the runner's background `JoinSet`. Every result is
/// published on the results channel, blocking or not.
pub async fn dispatch(
    runner: &HookRunner,
    event: HookEvent,
    context: &HookDispatchContext,
) -> DispatchSummary {
    // Re-check the trust hash against the files on disk right now, not only
    // on watcher events: the watcher can lag or miss an edit.
    let config = runner.config_checked().await;
    let mut summary = DispatchSummary::default();
    // Hooks later in this dispatch see the workspace an earlier blocking hook
    // returned, so `cwd = "thread"` and TURIN_WORKTREE_PATH follow it.
    let mut context = context.clone();
    let context = &mut context;

    if let Some(pending) = config.pending_project_hooks()
        && pending.hooks.iter().any(|hook| hook.event == event.kind)
    {
        tracing::warn!(
            event = event.kind.as_str(),
            file = %pending.file.display(),
            reason = ?pending.reason,
            "project hooks are not trusted; skipping them until approved"
        );
        let _ = runner
            .inner
            .results
            .send(HookUpdate::ProjectHooksPending(Box::new(pending.clone())));
        summary.project_hooks_pending = Some(pending.clone());
    }

    for spec in config.hooks_for(event.kind) {
        let (should_block, coerced) = resolve_blocking(event.kind, spec.blocking);
        if coerced {
            tracing::warn!(
                event = event.kind.as_str(),
                command = %spec.command,
                source = %spec.source,
                "blocking = true is only valid for thread.created; running this hook non-blocking"
            );
        }
        if spec.cwd == HookCwd::Thread && context.thread_cwd.is_none() {
            tracing::warn!(
                event = event.kind.as_str(),
                command = %spec.command,
                "cwd = \"thread\" but the event carries no thread cwd; using the project root"
            );
        }
        let cwd = resolve_hook_cwd(spec.cwd, context);
        let plan = HookRunPlan {
            spec: spec.clone(),
            blocking: should_block,
            cwd: cwd.clone(),
            env: hook_env_vars(event.kind, context, &cwd)
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
            stdin: serde_json::to_string(&hook_stdin_json(&event, context, &cwd))
                .expect("stdin envelope is always valid JSON"),
            drain_grace: runner.drain_grace(),
        };
        if should_block {
            let result = run_hook(&plan).await;
            let _ = runner
                .inner
                .results
                .send(HookUpdate::Run(Box::new(result.clone())));
            if let Some(returned) = &result.returned {
                let merged = summary
                    .thread_workspace
                    .get_or_insert_with(Default::default);
                if let Some(path) = &returned.worktree_path {
                    merged.worktree_path = Some(path.clone());
                    context.thread_cwd = Some(path.clone());
                }
                if returned.branch.is_some() {
                    merged.branch = returned.branch.clone();
                }
            }
            summary.blocking.push(result);
        } else {
            let results = runner.inner.results.clone();
            let spawned = runner.try_spawn(async move {
                let result = run_hook(&plan).await;
                let _ = results.send(HookUpdate::Run(Box::new(result)));
            });
            if spawned {
                summary.non_blocking_spawned += 1;
            }
        }
    }
    summary
}

/// Everything needed to run one hook, detached from the dispatch context.
struct HookRunPlan {
    spec: HookSpec,
    blocking: bool,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    stdin: String,
    drain_grace: Duration,
}

/// Environment keys the app process carries under the `TURIN_` prefix.
fn inherited_turin_keys() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.as_encoded_bytes().starts_with(b"TURIN_"))
        .collect()
}

async fn run_hook(plan: &HookRunPlan) -> HookRunResult {
    let started = Instant::now();
    let mut result = HookRunResult {
        event: plan.spec.event,
        command: plan.spec.command.clone(),
        source: plan.spec.source,
        hook_index: plan.spec.index,
        cwd: plan.cwd.clone(),
        blocking: plan.blocking,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        stdout_invalid_utf8: false,
        stderr_invalid_utf8: false,
        error: None,
        returned: None,
    };

    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(&plan.spec.command);
    command.current_dir(&plan.cwd);
    // The hook inherits the app's environment. PATH hydration from the login
    // shell happens once at app startup (upstream `fixPath`), not per hook.
    // Inherited TURIN_* keys are scrubbed first so stale values (a thread id
    // from a hook that launched Turin) and future app secrets never leak;
    // fresh values for this event are set right after.
    for key in inherited_turin_keys() {
        command.env_remove(key);
    }
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    // Upstream setup-script hygiene: captured output is never a terminal, so
    // suppress truecolor probes and ANSI output.
    command.env("COLORTERM", "");
    command.env("NO_COLOR", "1");
    command.env("FORCE_COLOR", "0");
    command.kill_on_drop(true);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    // Own process group: the child leads a fresh group so a timeout or
    // shutdown can kill every descendant at once (PORTING.md process rules).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            result.error = Some(format!(
                "failed to spawn /bin/sh -c {:?}: {err}",
                plan.spec.command
            ));
            result.duration_ms = started.elapsed().as_millis() as u64;
            return result;
        }
    };

    let mut guard = ChildGroupGuard::new(child);
    let timeout = Duration::from_secs(plan.spec.timeout_secs);
    match tokio::time::timeout(timeout, guard.run(plan.stdin.as_bytes(), plan.drain_grace)).await {
        Ok(capture) => {
            guard.finished();
            match capture {
                Ok(run) => {
                    result.stdout = run.stdout.text;
                    result.stdout_truncated = run.stdout.truncated;
                    result.stdout_invalid_utf8 = run.stdout.invalid_utf8;
                    result.stderr = run.stderr.text;
                    result.stderr_truncated = run.stderr.truncated;
                    result.stderr_invalid_utf8 = run.stderr.invalid_utf8;
                    result.exit_code = run.exit_code;
                    result.error = run.error;
                }
                Err(message) => result.error = Some(message),
            }
        }
        Err(_) => {
            // Timed out: kill the whole process group, reap the child, and
            // report a synthetic result. Partial output is not preserved,
            // matching upstream's `timeoutBehavior: "timedOutResult"`.
            guard.kill_group_and_reap().await;
            guard.finished();
            result.timed_out = true;
        }
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    if plan.blocking
        && plan.spec.event == HookEventKind::ThreadCreated
        && result.exit_code == Some(0)
        && !result.timed_out
        && !result.stdout_truncated
    {
        result.returned = parse_hook_return(&result.stdout, &plan.cwd);
    }
    result
}

/// One captured output stream.
#[derive(Debug, Default)]
struct StreamCapture {
    text: String,
    truncated: bool,
    invalid_utf8: bool,
}

/// One completed run: both streams plus the exit code, or a fatal message.
struct RunCapture {
    stdout: StreamCapture,
    stderr: StreamCapture,
    exit_code: Option<i32>,
    error: Option<String>,
}

/// Bytes captured so far from one stream. Lives outside the reader future so
/// a reader that is cut off (shell exited, pipe still held open) still leaves
/// everything it read behind.
#[derive(Default)]
struct StreamBuf {
    bytes: Vec<u8>,
    truncated: bool,
}

impl StreamBuf {
    fn into_capture(mut self) -> StreamCapture {
        if self.truncated {
            self.bytes.extend_from_slice(TRUNCATED_MARKER.as_bytes());
        }
        match String::from_utf8(self.bytes) {
            Ok(text) => StreamCapture {
                text,
                truncated: self.truncated,
                invalid_utf8: false,
            },
            Err(err) => StreamCapture {
                text: String::from_utf8_lossy(err.as_bytes()).into_owned(),
                truncated: self.truncated,
                invalid_utf8: true,
            },
        }
    }
}

/// Reads until EOF into `buf`, keeping at most `max_bytes`; the rest is
/// drained and discarded so the child can never block on a full pipe. Mirrors
/// upstream's truncate mode. Cancel-safe: dropping the future loses nothing
/// already appended to `buf`.
async fn fill_bounded<R: AsyncRead + Unpin>(
    reader: Option<R>,
    buf: &mut StreamBuf,
    max_bytes: usize,
) {
    let Some(mut reader) = reader else {
        return;
    };
    // Heap buffer: keeps the future (and every JoinSet task cell) small.
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if !buf.truncated {
                    if buf.bytes.len() + n > max_bytes {
                        let keep = max_bytes.saturating_sub(buf.bytes.len());
                        buf.bytes.extend_from_slice(&chunk[..keep]);
                        buf.truncated = true;
                    } else {
                        buf.bytes.extend_from_slice(&chunk[..n]);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Owns the hook child for its whole run. Dropping it before the run
/// finishes (timeout path handled explicitly; task abort path via Drop)
/// kills the child's whole process group, so grandchildren cannot survive.
///
/// The pgid is recorded at spawn: `Child::id()` turns `None` once the shell
/// has been reaped, but the group lives on while any member does, and
/// `kill(-pgid)` still reaches it.
struct ChildGroupGuard {
    child: Option<Child>,
    /// The shell's pid, which is its process group id (`process_group(0)`).
    pgid: Option<u32>,
    finished: bool,
}

impl ChildGroupGuard {
    fn new(child: Child) -> Self {
        Self {
            pgid: child.id(),
            child: Some(child),
            finished: false,
        }
    }

    /// Writes stdin, drains both output streams, and waits for exit.
    ///
    /// Returns once the shell has exited plus at most `drain_grace` of extra
    /// draining, not when the pipes hit EOF: a background job that inherited
    /// stdout would otherwise hold the run until the timeout. If the pipes are
    /// still held after the grace, that leftover group is killed.
    async fn run(
        &mut self,
        stdin_bytes: &[u8],
        drain_grace: Duration,
    ) -> Result<RunCapture, String> {
        let child = self
            .child
            .as_mut()
            .expect("the guard owns the child for the whole run");
        let stdin_handle = child.stdin.take();
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();
        let mut stdout = StreamBuf::default();
        let mut stderr = StreamBuf::default();
        // Scoped so the reader futures' borrows of the buffers end here.
        let (status, leftover_group) = {
            let io = async {
                let write = async {
                    if let Some(mut handle) = stdin_handle {
                        // A hook that never reads stdin gets EPIPE here; that is
                        // not an error (upstream only fails on spawn/read/exit).
                        let _ = handle.write_all(stdin_bytes).await;
                        let _ = handle.shutdown().await;
                    }
                };
                tokio::join!(
                    write,
                    fill_bounded(stdout_handle, &mut stdout, MAX_OUTPUT_BYTES),
                    fill_bounded(stderr_handle, &mut stderr, MAX_OUTPUT_BYTES),
                );
            };
            tokio::pin!(io);
            let mut leftover_group = false;
            let status = tokio::select! {
                status = child.wait() => {
                    // Shell is gone; give already-written output a moment to
                    // arrive. A timeout here means something else still holds
                    // the pipes.
                    leftover_group = tokio::time::timeout(drain_grace, &mut io).await.is_err();
                    status
                }
                () = &mut io => child.wait().await,
            };
            (status, leftover_group)
        };
        if leftover_group {
            self.kill_group();
        }
        let stdout = stdout.into_capture();
        let stderr = stderr.into_capture();
        match status {
            Ok(status) => Ok(RunCapture {
                stdout,
                stderr,
                exit_code: status.code(),
                error: None,
            }),
            // Wait failures (e.g. ECHILD after a signal race) still yield the
            // captured output.
            Err(err) => Ok(RunCapture {
                stdout,
                stderr,
                exit_code: None,
                error: Some(format!("failed to wait for hook process: {err}")),
            }),
        }
    }

    async fn kill_group_and_reap(&mut self) {
        self.kill_group();
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait().await;
        }
    }

    fn kill_group(&mut self) {
        if let Some(pgid) = self.pgid {
            let _ = kill_process_group(pgid);
        }
        // Belt and braces: also kill the child directly, which is the only
        // option on platforms without process groups.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }

    /// Marks the run as cleanly finished so `Drop` will not kill anything.
    fn finished(&mut self) {
        self.finished = true;
    }
}

impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.kill_group();
        }
    }
}

/// SIGKILL to `-pgid`: every member of the group the child leads. The child
/// was spawned with `process_group(0)`, so its pid is the group id, and the
/// kernel never reuses a pgid while a member is alive.
#[cfg(unix)]
fn kill_process_group(pgid: u32) -> bool {
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
fn kill_process_group(_pgid: u32) -> bool {
    false
}

/// Owns hook runs and the results channel. Clone it freely; shutdown drains
/// every clone through the shared inner state.
#[derive(Clone)]
pub struct HookRunner {
    inner: Arc<HookRunnerInner>,
}

/// The filesystem watcher and the directories it currently watches.
struct WatchState {
    watcher: notify::RecommendedWatcher,
    dirs: Vec<PathBuf>,
}

struct HookRunnerInner {
    paths: HookPaths,
    config: RwLock<Arc<HooksConfig>>,
    /// Background hook runs and the hot-reload task. `None` after shutdown.
    tasks: Mutex<Option<JoinSet<()>>>,
    results: broadcast::Sender<HookUpdate>,
    /// Dropped on shutdown to stop file watching.
    watcher: Mutex<Option<WatchState>>,
    /// Post-exit output drain, in milliseconds (see [`DRAIN_GRACE`]).
    drain_grace_ms: AtomicU64,
}

impl HookRunner {
    /// Default paths: `~/.turin/hooks.toml` plus `<project>/.turin/hooks.toml`.
    pub fn for_project(project_root: &Path) -> Self {
        Self::with_paths(HookPaths::for_project(project_root))
    }

    /// Explicit paths (each optional). Missing files simply mean no hooks.
    /// Hot reload watches the parent directories of the existing paths; a
    /// runtime must be active (call inside async code, or rely on `reload()`).
    pub fn with_paths(paths: HookPaths) -> Self {
        let config = Arc::new(HooksConfig::load(&paths));
        log_config(&config);
        let (results, _rx) = broadcast::channel(RESULTS_CHANNEL_CAPACITY);
        let inner = Arc::new(HookRunnerInner {
            paths,
            config: RwLock::new(config),
            tasks: Mutex::new(Some(JoinSet::new())),
            results,
            watcher: Mutex::new(None),
            drain_grace_ms: AtomicU64::new(DRAIN_GRACE.as_millis() as u64),
        });
        let runner = Self { inner };
        runner.start_watcher();
        runner
    }

    /// Subscribes to every completed hook run (blocking and non-blocking) and
    /// to pending-trust notices. A lagging subscriber loses old updates; it
    /// never slows hooks down.
    pub fn subscribe_results(&self) -> broadcast::Receiver<HookUpdate> {
        self.inner.results.subscribe()
    }

    #[cfg(test)]
    fn set_drain_grace(&self, grace: Duration) {
        self.inner
            .drain_grace_ms
            .store(grace.as_millis() as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn task_count(&self) -> usize {
        self.inner.tasks.lock().as_ref().map_or(0, JoinSet::len)
    }

    fn drain_grace(&self) -> Duration {
        Duration::from_millis(self.inner.drain_grace_ms.load(Ordering::Relaxed))
    }

    /// The current config snapshot.
    pub fn config(&self) -> Arc<HooksConfig> {
        self.inner.config.read().clone()
    }

    /// Re-reads both config files and swaps the snapshot in. Also triggered
    /// automatically when a config file changes on disk.
    pub fn reload(&self) -> Arc<HooksConfig> {
        self.inner.reload_config()
    }

    /// Approves the project's current `.turin/` tree (`hooks.toml` and every
    /// file under it) and reloads so its hooks run from the next event on.
    /// `reviewed_hash` is the [`PendingProjectHooks::hash`] the user was
    /// shown; if anything changed since, nothing is approved
    /// ([`TrustError::HashMismatch`]) and the UI should present the new
    /// contents. A tree that cannot be approved (symlink leaving `.turin/`)
    /// yields [`TrustError::Untrustable`]. Approval is keyed on the canonical
    /// project root. Hashing runs on the blocking pool.
    pub async fn trust_project_hooks(
        &self,
        reviewed_hash: &str,
    ) -> Result<Arc<HooksConfig>, TrustError> {
        let inner = self.inner.clone();
        let reviewed_hash = reviewed_hash.to_owned();
        tokio::task::spawn_blocking(move || inner.trust_project_hooks(&reviewed_hash))
            .await
            .expect("trust task does not panic")
    }

    /// Withdraws the approval for this project's hooks and reloads; they go
    /// back to pending.
    pub async fn revoke_project_hooks_trust(&self) -> Result<Arc<HooksConfig>, TrustError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.revoke_project_hooks_trust())
            .await
            .expect("trust task does not panic")
    }

    /// The config snapshot, reloaded first if the project config tree no
    /// longer matches what the snapshot trusted (or a project config
    /// appeared that the snapshot does not know about).
    async fn config_checked(&self) -> Arc<HooksConfig> {
        let config = self.config();
        let stale = match config.project_status() {
            ProjectHooksStatus::Trusted { hash } => {
                let inner = self.inner.clone();
                let hash = hash.clone();
                tokio::task::spawn_blocking(move || {
                    inner.paths.current_project_hash().as_deref() != Some(hash.as_str())
                })
                .await
                .unwrap_or(true)
            }
            ProjectHooksStatus::Absent => self
                .inner
                .paths
                .project
                .as_deref()
                .is_some_and(|file| file.is_file()),
            ProjectHooksStatus::Pending(_) => false,
        };
        if !stale {
            return config;
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.reload_config())
            .await
            .unwrap_or(config)
    }

    /// Runs matching hooks. See [`dispatch`].
    pub async fn dispatch(
        &self,
        event: HookEvent,
        context: &HookDispatchContext,
    ) -> DispatchSummary {
        dispatch(self, event, context).await
    }

    /// Stops watching config files, aborts every background hook run
    /// (killing its process group), and drains the task set. Dispatch after
    /// shutdown runs blocking hooks only.
    pub async fn shutdown(&self) {
        *self.inner.watcher.lock() = None;
        // Take the set out before awaiting so the guard is never held across
        // an await point (PORTING.md lock rules).
        let tasks = self.inner.tasks.lock().take();
        if let Some(mut tasks) = tasks {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }

    /// Spawns onto the runner's `JoinSet` (never a bare `JoinHandle`, per
    /// PORTING.md task rules). Returns false when shut down or when no
    /// runtime is active.
    fn try_spawn<F>(&self, future: F) -> bool
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("hooks background task not spawned: no tokio runtime is active");
            return false;
        }
        let mut tasks = self.inner.tasks.lock();
        match tasks.as_mut() {
            Some(set) => {
                // Reap finished runs so a long session does not accumulate
                // one task cell per hook run, and so panics are seen.
                while let Some(joined) = set.try_join_next() {
                    if let Err(err) = joined
                        && err.is_panic()
                    {
                        tracing::error!(%err, "hooks background task panicked");
                    }
                }
                set.spawn(future);
                true
            }
            None => {
                tracing::warn!("hooks background task not spawned: runner is shut down");
                false
            }
        }
    }

    fn start_watcher(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!(
                "hooks hot reload disabled: construct the runner inside a tokio runtime (reload() still works)"
            );
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let watcher = match notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!(%err, "hooks hot reload disabled: no filesystem watcher available");
                return;
            }
        };
        *self.inner.watcher.lock() = Some(WatchState {
            watcher,
            dirs: Vec::new(),
        });
        self.inner.rearm_watches();

        // The reload task holds only a Weak: the JoinSet lives in the inner
        // state, so a strong reference would keep the runner alive forever.
        let weak: Weak<HookRunnerInner> = Arc::downgrade(&self.inner);
        let paths = self.inner.paths.clone();
        let spawned = self.try_spawn(async move {
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    None => return,
                    Some(Ok(event)) => {
                        // Reads of our own config files must not trigger reloads.
                        if matches!(event.kind, notify::EventKind::Access(_))
                            || !event
                                .paths
                                .iter()
                                .any(|path| paths.is_relevant_event_path(path))
                        {
                            continue;
                        }
                        // Coalesce bursts (atomic saves emit several events),
                        // then reload once.
                        tokio::time::sleep(RELOAD_DEBOUNCE).await;
                        while rx.try_recv().is_ok() {}
                        let Some(inner) = weak.upgrade() else {
                            return;
                        };
                        // Re-arm first (a `.turin/` may have appeared or been
                        // replaced), then read: anything written before the
                        // new watch existed is picked up by this reload.
                        inner.rearm_watches();
                        // Hashing the config tree is blocking filesystem work.
                        let _ = tokio::task::spawn_blocking(move || inner.reload_config()).await;
                    }
                    Some(Err(err)) => {
                        tracing::warn!(%err, "hooks hot reload: filesystem watcher error")
                    }
                }
            }
        });
        if !spawned {
            *self.inner.watcher.lock() = None;
        }
    }
}

impl HookRunnerInner {
    fn trust_project_hooks(&self, reviewed_hash: &str) -> Result<Arc<HooksConfig>, TrustError> {
        let paths = &self.paths;
        let (Some(file), Some(root), Some(store)) = (
            paths.project.as_deref(),
            paths.canonical_project_root(),
            paths.trust_store.as_deref(),
        ) else {
            return Err(TrustError::Unavailable);
        };
        if !file.is_file() {
            return Err(TrustError::NoProjectHooks(file.to_path_buf()));
        }
        let config_dir = file.parent().ok_or(TrustError::Unavailable)?;
        let snapshot =
            hash_config_tree(config_dir, file, Some(&root)).map_err(TrustError::Untrustable)?;
        if snapshot.hash != reviewed_hash {
            return Err(TrustError::HashMismatch {
                expected: reviewed_hash.to_owned(),
                actual: snapshot.hash,
            });
        }
        TrustStore::new(store).approve(&root, &snapshot.hash)?;
        Ok(self.reload_config())
    }

    fn revoke_project_hooks_trust(&self) -> Result<Arc<HooksConfig>, TrustError> {
        let (Some(root), Some(store)) = (
            self.paths.canonical_project_root(),
            self.paths.trust_store.as_deref(),
        ) else {
            return Err(TrustError::Unavailable);
        };
        TrustStore::new(store).revoke(&root)?;
        Ok(self.reload_config())
    }

    fn reload_config(&self) -> Arc<HooksConfig> {
        let fresh = Arc::new(HooksConfig::load(&self.paths));
        log_config(&fresh);
        let previous = std::mem::replace(&mut *self.config.write(), fresh.clone());
        // Tell the UI as soon as project hooks become (or stay) untrusted,
        // not only when the next event happens to subscribe to them.
        if let ProjectHooksStatus::Pending(pending) = fresh.project_status()
            && previous.project_status() != fresh.project_status()
        {
            tracing::warn!(
                file = %pending.file.display(),
                reason = ?pending.reason,
                "project hooks are not trusted; they stay inert until approved"
            );
            let _ = self
                .results
                .send(HookUpdate::ProjectHooksPending(pending.clone()));
        }
        fresh
    }

    /// Points the watcher at the nearest existing directory above each
    /// config file: the file's own directory when it exists, otherwise the
    /// closest ancestor (project root, `$HOME`), so a `.turin/` created later
    /// is noticed. Watches directories, not files: editors save atomically
    /// (temp file + rename), which replaces the inode a file watch points at.
    /// Always re-watches from scratch, because a deleted and recreated
    /// directory has lost its old watch.
    fn rearm_watches(&self) {
        let mut guard = self.watcher.lock();
        let Some(state) = guard.as_mut() else {
            return;
        };
        for dir in state.dirs.drain(..) {
            let _ = state.watcher.unwatch(&dir);
        }
        for path in [&self.paths.global, &self.paths.project]
            .into_iter()
            .flatten()
        {
            let Some(dir) = path
                .parent()
                .and_then(|parent| parent.ancestors().find(|dir| dir.is_dir()))
            else {
                continue;
            };
            if state.dirs.iter().any(|watched| watched == dir) {
                continue;
            }
            match state
                .watcher
                .watch(dir, notify::RecursiveMode::NonRecursive)
            {
                Ok(()) => state.dirs.push(dir.to_path_buf()),
                Err(err) => {
                    tracing::warn!(%err, dir = %dir.display(), "hooks hot reload: cannot watch directory")
                }
            }
        }
    }
}

fn log_config(config: &HooksConfig) {
    for error in config.errors() {
        tracing::warn!(%error, "skipping invalid hooks config entry");
    }
    tracing::info!(
        hooks = config.all_hooks().len(),
        errors = config.errors().len(),
        "hooks config loaded"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn context(project_root: &Path, thread_cwd: Option<&Path>) -> HookDispatchContext {
        HookDispatchContext {
            project_id: ProjectId::random(),
            project_root: project_root.to_path_buf(),
            thread_id: thread_cwd.map(|_| ThreadId::random()),
            thread_cwd: thread_cwd.map(Path::to_path_buf),
        }
    }

    fn write_config(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn project_with_config(body: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize: `pwd` in the hook resolves symlinks (/var ->
        // /private/var on macOS), and assertions compare against this path.
        let project = std::fs::canonicalize(dir.path()).unwrap();
        let config_path = project.join(".turin").join("hooks.toml");
        write_config(&config_path, body);
        let out = project.join("out");
        std::fs::create_dir_all(&out).unwrap();
        (dir, project, config_path)
    }

    /// Trust store used by the test runners, outside both the project's
    /// `.turin/` and anything a hook writes.
    fn trust_store_for(project_config: &Path) -> PathBuf {
        project_root_of(project_config)
            .join(".home")
            .join("trusted-hooks.json")
    }

    fn project_root_of(project_config: &Path) -> PathBuf {
        project_config
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf()
    }

    fn paths_for(project_config: &Path) -> HookPaths {
        HookPaths {
            global: None,
            project: Some(project_config.to_path_buf()),
            project_root: Some(project_root_of(project_config)),
            trust_store: Some(trust_store_for(project_config)),
        }
    }

    /// Records the file's current bytes as approved, as the UI would.
    fn approve_file(project_config: &Path) {
        let root = std::fs::canonicalize(project_root_of(project_config)).unwrap();
        let paths = paths_for(project_config);
        TrustStore::new(&trust_store_for(project_config))
            .approve(&root, &paths.current_project_hash().unwrap())
            .unwrap();
    }

    /// A runner whose project hooks are already approved.
    fn runner(project_config: &Path) -> HookRunner {
        approve_file(project_config);
        HookRunner::with_paths(paths_for(project_config))
    }

    /// A runner for a project the user has never approved.
    fn untrusted_runner(project_config: &Path) -> HookRunner {
        HookRunner::with_paths(paths_for(project_config))
    }

    /// Next completed hook run, skipping pending-trust notices.
    async fn recv_run(
        rx: &mut broadcast::Receiver<HookUpdate>,
    ) -> Result<HookRunResult, broadcast::error::RecvError> {
        loop {
            if let HookUpdate::Run(run) = rx.recv().await? {
                return Ok(*run);
            }
        }
    }

    #[test]
    fn resolve_cwd_project_vs_thread() {
        let project = Path::new("/proj");
        let thread = Path::new("/proj/wt");
        let with_thread = context(project, Some(thread));
        assert_eq!(resolve_hook_cwd(HookCwd::Project, &with_thread), project);
        assert_eq!(resolve_hook_cwd(HookCwd::Thread, &with_thread), thread);
        let without_thread = context(project, None);
        assert_eq!(resolve_hook_cwd(HookCwd::Project, &without_thread), project);
        // Thread cwd missing: falls back to the project root.
        assert_eq!(resolve_hook_cwd(HookCwd::Thread, &without_thread), project);
    }

    #[test]
    fn resolve_blocking_only_honored_for_thread_created() {
        assert_eq!(
            resolve_blocking(HookEventKind::ThreadCreated, true),
            (true, false)
        );
        assert_eq!(
            resolve_blocking(HookEventKind::ThreadCreated, false),
            (false, false)
        );
        assert_eq!(
            resolve_blocking(HookEventKind::TurnCompleted, true),
            (false, true)
        );
        assert_eq!(
            resolve_blocking(HookEventKind::GitCommitted, true),
            (false, true)
        );
        assert_eq!(
            resolve_blocking(HookEventKind::GitCommitted, false),
            (false, false)
        );
    }

    #[test]
    fn env_vars_and_stdin_shape() {
        let project = Path::new("/proj");
        let thread = Path::new("/proj/wt");
        let ctx = context(project, Some(thread));
        let vars = hook_env_vars(HookEventKind::ThreadCreated, &ctx, thread);
        let pairs: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        assert!(pairs.contains(&("TURIN_EVENT".into(), "thread.created".into())));
        assert!(pairs.contains(&("TURIN_PROJECT_ID".into(), ctx.project_id.as_str().into())));
        assert!(pairs.contains(&(
            "TURIN_THREAD_ID".into(),
            ctx.thread_id.as_ref().unwrap().as_str().into()
        )));
        assert!(pairs.contains(&("TURIN_PROJECT_ROOT".into(), "/proj".into())));
        assert!(pairs.contains(&("TURIN_CWD".into(), "/proj/wt".into())));
        assert!(pairs.contains(&("TURIN_WORKTREE_PATH".into(), "/proj/wt".into())));
        let no_thread = context(project, None);
        let vars = hook_env_vars(HookEventKind::TurnFailed, &no_thread, project);
        let names: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert!(!names.contains(&"TURIN_THREAD_ID"));
        assert!(!names.contains(&"TURIN_WORKTREE_PATH"));

        let event = HookEvent::thread_created(serde_json::json!({"branch": "feature/x"}));
        let stdin = hook_stdin_json(&event, &ctx, thread);
        assert_eq!(stdin["event"], "thread.created");
        assert_eq!(stdin["projectId"], ctx.project_id.as_str());
        assert_eq!(stdin["threadId"], ctx.thread_id.as_ref().unwrap().as_str());
        assert_eq!(stdin["projectRoot"], "/proj");
        assert_eq!(stdin["cwd"], "/proj/wt");
        assert_eq!(stdin["payload"]["branch"], "feature/x");
        // No thread: the envelope omits threadId entirely.
        let stdin = hook_stdin_json(&event, &no_thread, project);
        assert!(stdin.get("threadId").is_none());
    }

    #[tokio::test]
    async fn delivers_env_and_stdin_to_hook() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "pwd > out/pwd.txt; env > out/env.txt; cat > out/stdin.json"
cwd = "project"
blocking = true
"#,
        );
        let thread = project.join("wt");
        std::fs::create_dir_all(&thread).unwrap();
        let runner = runner(&config_path);
        let ctx = context(&project, Some(&thread));
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({"branch": "feature/x"})),
                &ctx,
            )
            .await;
        assert_eq!(summary.blocking.len(), 1);
        let result = &summary.blocking[0];
        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert_eq!(result.cwd, project);

        let env_text = std::fs::read_to_string(project.join("out/env.txt")).unwrap();
        assert!(
            env_text.contains("TURIN_EVENT=thread.created"),
            "{env_text}"
        );
        assert!(env_text.contains(&format!("TURIN_PROJECT_ID={}", ctx.project_id.as_str())));
        assert!(env_text.contains(&format!(
            "TURIN_THREAD_ID={}",
            ctx.thread_id.as_ref().unwrap().as_str()
        )));
        assert!(env_text.contains(&format!("TURIN_CWD={}", project.display())));
        assert!(env_text.contains(&format!("TURIN_PROJECT_ROOT={}", project.display())));
        assert!(env_text.contains(&format!("TURIN_WORKTREE_PATH={}", thread.display())));
        assert!(env_text.contains("NO_COLOR=1"), "{env_text}");
        assert!(env_text.contains("FORCE_COLOR=0"), "{env_text}");

        let stdin: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(project.join("out/stdin.json")).unwrap())
                .unwrap();
        assert_eq!(stdin["event"], "thread.created");
        assert_eq!(stdin["projectId"], ctx.project_id.as_str());
        assert_eq!(stdin["threadId"], ctx.thread_id.as_ref().unwrap().as_str());
        assert_eq!(stdin["payload"]["branch"], "feature/x");

        let pwd = std::fs::read_to_string(project.join("out/pwd.txt")).unwrap();
        assert_eq!(pwd.trim(), project.display().to_string());
    }

    #[tokio::test]
    async fn thread_cwd_runs_in_thread_workspace() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "pwd > pwd.txt"
cwd = "thread"
blocking = true
"#,
        );
        let thread = project.join("wt");
        std::fs::create_dir_all(&thread).unwrap();
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, Some(&thread)),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        let pwd = std::fs::read_to_string(thread.join("pwd.txt")).unwrap();
        assert_eq!(pwd.trim(), thread.display().to_string());
    }

    #[tokio::test]
    async fn timeout_kills_child_and_grandchild() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "echo $$ > out/pids.txt; sleep 30 & echo $! >> out/pids.txt; sleep 30 & echo $! >> out/pids.txt; wait"
blocking = true
timeout_secs = 1
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let result = &summary.blocking[0];
        assert!(result.timed_out);
        assert_eq!(result.exit_code, None);
        assert!(result.duration_ms >= 1000, "{}", result.duration_ms);

        let pids_text = std::fs::read_to_string(project.join("out/pids.txt")).unwrap();
        let pids: Vec<libc::pid_t> = pids_text
            .lines()
            .map(|line| line.trim().parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 3, "shell + two sleeps, got {pids_text}");
        for pid in pids {
            assert!(wait_for_pid_gone(pid), "pid {pid} survived the group kill");
        }
    }

    fn wait_for_pid_gone(pid: libc::pid_t) -> bool {
        for _ in 0..100 {
            let rc = unsafe { libc::kill(pid, 0) };
            if rc == -1 {
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno == Some(libc::ESRCH) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[tokio::test]
    async fn blocking_hook_is_awaited() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "sleep 1; touch out/marker.txt"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.non_blocking_spawned, 0);
        assert_eq!(summary.blocking.len(), 1);
        assert!(summary.blocking[0].blocking);
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        assert!(project.join("out/marker.txt").exists());
        let published = tokio::time::timeout(Duration::from_secs(5), recv_run(&mut results))
            .await
            .expect("blocking result is published")
            .unwrap();
        assert_eq!(published.command, summary.blocking[0].command);
    }

    #[tokio::test]
    async fn non_blocking_hook_runs_in_background() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "sleep 1; touch out/marker.txt"
"#,
        );
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.blocking.is_empty());
        assert_eq!(summary.non_blocking_spawned, 1);
        // Dispatch returned before the hook finished.
        assert!(!project.join("out/marker.txt").exists());
        let published = tokio::time::timeout(Duration::from_secs(30), recv_run(&mut results))
            .await
            .expect("non-blocking result is published")
            .unwrap();
        assert!(!published.blocking);
        assert_eq!(published.exit_code, Some(0), "stderr: {}", published.stderr);
        assert!(project.join("out/marker.txt").exists());
    }

    #[tokio::test]
    async fn blocking_is_coerced_on_other_events() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "turn.completed"
command = "sleep 1; touch out/marker.txt"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        let summary = runner
            .dispatch(
                HookEvent::turn_completed(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        // Coerced to non-blocking: nothing awaited.
        assert!(summary.blocking.is_empty());
        assert_eq!(summary.non_blocking_spawned, 1);
        assert!(!project.join("out/marker.txt").exists());
        let published = tokio::time::timeout(Duration::from_secs(30), recv_run(&mut results))
            .await
            .expect("coerced result is published")
            .unwrap();
        assert!(!published.blocking);
    }

    #[tokio::test]
    async fn global_hooks_run_before_project_hooks() {
        let global_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let global_path = global_dir.path().join("hooks.toml");
        let project = std::fs::canonicalize(project_dir.path()).unwrap();
        let project_config = project.join(".turin").join("hooks.toml");
        std::fs::create_dir_all(project.join("out")).unwrap();
        write_config(
            &global_path,
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"echo global >> out/order.txt\"\nblocking = true\n",
        );
        write_config(
            &project_config,
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"echo project >> out/order.txt\"\nblocking = true\n",
        );
        approve_file(&project_config);
        let runner = HookRunner::with_paths(HookPaths {
            global: Some(global_path),
            ..paths_for(&project_config)
        });
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking.len(), 2);
        assert_eq!(summary.blocking[0].source, HookConfigSource::Global);
        assert_eq!(summary.blocking[1].source, HookConfigSource::Project);
        let order = std::fs::read_to_string(project.join("out/order.txt")).unwrap();
        assert_eq!(order, "global\nproject\n");
    }

    #[tokio::test]
    async fn output_is_bounded_and_flagged() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "dd if=/dev/zero bs=1024 count=1200 2>/dev/null | tr '\\0' x"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let result = &summary.blocking[0];
        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert!(result.stdout_truncated);
        assert!(!result.stderr_truncated);
        assert!(result.stdout.len() <= MAX_OUTPUT_BYTES + TRUNCATED_MARKER.len());
        assert!(result.stdout.ends_with(TRUNCATED_MARKER));
        assert!(result.stdout.starts_with('x'));
    }

    #[tokio::test]
    async fn invalid_utf8_is_flagged_lossily() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "printf '\\377'"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let result = &summary.blocking[0];
        assert!(result.stdout_invalid_utf8);
        assert_eq!(result.stdout, "\u{fffd}");
    }

    #[tokio::test]
    async fn spawn_failure_is_reported_not_panicked() {
        let (_dir, _project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "true"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        // A cwd that cannot exist forces a spawn error; it must come back as
        // a result, never a panic.
        let bad = HookDispatchContext {
            project_id: ProjectId::random(),
            project_root: PathBuf::from("/definitely/not/a/real/dir"),
            thread_id: None,
            thread_cwd: None,
        };
        let summary = runner
            .dispatch(HookEvent::thread_created(serde_json::json!({})), &bad)
            .await;
        let result = &summary.blocking[0];
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("failed to spawn")
        );
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn reload_picks_up_new_config() {
        let (_dir, project, config_path) = project_with_config(
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"touch out/v1.txt\"\nblocking = true\n",
        );
        let runner = runner(&config_path);
        runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(project.join("out/v1.txt").exists());

        write_config(
            &config_path,
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"touch out/v2.txt\"\nblocking = true\n",
        );
        approve_file(&config_path);
        let fresh = runner.reload();
        assert_eq!(fresh.hooks_for(HookEventKind::ThreadCreated).len(), 1);
        assert_eq!(fresh.all_hooks()[0].command, "touch out/v2.txt");
        runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(project.join("out/v2.txt").exists());
    }

    /// Polls `condition` until true; filesystem event latency varies.
    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn global_only_runner(global: &Path) -> HookRunner {
        HookRunner::with_paths(HookPaths {
            global: Some(global.to_path_buf()),
            project: None,
            project_root: None,
            trust_store: None,
        })
    }

    fn turn_completed_hooks(runner: &HookRunner) -> usize {
        runner
            .config()
            .hooks_for(HookEventKind::TurnCompleted)
            .len()
    }

    const TURN_COMPLETED_ONE: &str =
        "[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/one.txt\"\n";
    const TURN_COMPLETED_TWO: &str = "[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/one.txt\"\n\n[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/two.txt\"\n";

    #[tokio::test]
    async fn watcher_hot_reloads_on_file_change() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("hooks.toml");
        write_config(&global, TURN_COMPLETED_ONE);
        let runner = global_only_runner(&global);
        assert_eq!(turn_completed_hooks(&runner), 1);
        write_config(&global, TURN_COMPLETED_TWO);
        wait_until("global edit reloaded", || {
            turn_completed_hooks(&runner) == 2
        })
        .await;
    }

    #[tokio::test]
    async fn watcher_arms_when_the_config_directory_appears_later() {
        // Neither `.turin/` exists when the runner is built (the normal
        // starting state); the nearest existing ancestor is watched instead.
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let global = home.join("home").join(".turin").join("hooks.toml");
        std::fs::create_dir_all(home.join("home")).unwrap();
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project_config = project.join(".turin").join("hooks.toml");
        let runner = HookRunner::with_paths(HookPaths {
            global: Some(global.clone()),
            project: Some(project_config.clone()),
            project_root: Some(project.clone()),
            trust_store: Some(home.join("trusted-hooks.json")),
        });
        assert_eq!(turn_completed_hooks(&runner), 0);

        // Global (trusted): applies as soon as the file appears.
        write_config(&global, TURN_COMPLETED_ONE);
        wait_until("global config created later", || {
            turn_completed_hooks(&runner) == 1
        })
        .await;

        // Project: appears, but only as a pending approval, through the
        // trust gate.
        write_config(&project_config, TURN_COMPLETED_TWO);
        wait_until("project config created later", || {
            runner.config().pending_project_hooks().is_some()
        })
        .await;
        assert_eq!(turn_completed_hooks(&runner), 1, "project hooks stay inert");
        let hash = runner
            .config()
            .pending_project_hooks()
            .unwrap()
            .hash
            .clone();
        runner.trust_project_hooks(&hash).await.unwrap();
        assert_eq!(turn_completed_hooks(&runner), 3);
    }

    #[tokio::test]
    async fn watcher_survives_the_config_directory_being_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let global = std::fs::canonicalize(dir.path())
            .unwrap()
            .join(".turin")
            .join("hooks.toml");
        write_config(&global, TURN_COMPLETED_ONE);
        let runner = global_only_runner(&global);
        std::fs::remove_dir_all(global.parent().unwrap()).unwrap();
        wait_until("deleted directory drops hooks", || {
            turn_completed_hooks(&runner) == 0
        })
        .await;
        write_config(&global, TURN_COMPLETED_TWO);
        wait_until("recreated directory is watched again", || {
            turn_completed_hooks(&runner) == 2
        })
        .await;
    }

    #[tokio::test]
    async fn deleting_a_config_under_a_symlinked_path_reloads() {
        // FSEvents reports canonical paths; the configured path is not.
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let global = link.join("hooks.toml");
        write_config(&global, TURN_COMPLETED_ONE);
        let runner = global_only_runner(&global);
        assert_eq!(turn_completed_hooks(&runner), 1);
        std::fs::remove_file(&global).unwrap();
        wait_until("delete reloaded", || turn_completed_hooks(&runner) == 0).await;
    }

    #[tokio::test]
    async fn untrusted_project_hooks_do_not_run_and_report_pending() {
        let (_dir, project, config_path) = project_with_config(
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"touch out/pwned.txt\"\nblocking = true\n",
        );
        let runner = untrusted_runner(&config_path);
        let mut updates = runner.subscribe_results();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.blocking.is_empty());
        assert_eq!(summary.non_blocking_spawned, 0);
        assert!(!project.join("out/pwned.txt").exists());
        let pending = summary.project_hooks_pending.expect("pending in summary");
        assert_eq!(pending.reason, crate::PendingReason::Untrusted);
        assert_eq!(pending.hooks[0].command, "touch out/pwned.txt");
        match updates.try_recv().expect("pending notice on the channel") {
            HookUpdate::ProjectHooksPending(notice) => assert_eq!(*notice, pending),
            other => panic!("unexpected update {other:?}"),
        }
        // Events the pending hooks do not subscribe to stay quiet.
        let quiet = runner
            .dispatch(
                HookEvent::turn_failed(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(quiet.project_hooks_pending.is_none());
    }

    #[tokio::test]
    async fn trusting_project_hooks_activates_them() {
        let (_dir, project, config_path) = project_with_config(
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"touch out/ran.txt\"\nblocking = true\n",
        );
        let runner = untrusted_runner(&config_path);
        let hash = runner
            .config()
            .pending_project_hooks()
            .expect("pending")
            .hash
            .clone();
        // A stale hash (the file changed after review) approves nothing.
        assert!(matches!(
            runner.trust_project_hooks("0000").await,
            Err(TrustError::HashMismatch { .. })
        ));
        assert!(runner.config().pending_project_hooks().is_some());

        let config = runner.trust_project_hooks(&hash).await.unwrap();
        assert!(config.pending_project_hooks().is_none());
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        assert!(summary.project_hooks_pending.is_none());
        assert!(project.join("out/ran.txt").exists());

        // The approval is on disk: a fresh runner picks it up.
        let again = HookRunner::with_paths(paths_for(&config_path));
        assert_eq!(again.config().all_hooks().len(), 1);

        // And can be withdrawn.
        let config = runner.revoke_project_hooks_trust().await.unwrap();
        assert!(config.all_hooks().is_empty());
        assert!(config.pending_project_hooks().is_some());
    }

    #[tokio::test]
    async fn hot_reload_after_edit_drops_project_hooks_until_reapproved() {
        let (_dir, project, config_path) = project_with_config(TURN_COMPLETED_ONE);
        let runner = runner(&config_path);
        let mut updates = runner.subscribe_results();
        assert_eq!(turn_completed_hooks(&runner), 1);

        // An agent, `git pull` or branch switch rewrites the file.
        write_config(
            &config_path,
            "[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/pwned.txt\"\n",
        );
        wait_until("edit flips project hooks to pending", || {
            runner.config().pending_project_hooks().is_some()
        })
        .await;
        assert_eq!(turn_completed_hooks(&runner), 0, "old and new both inert");
        assert_eq!(
            runner.config().pending_project_hooks().unwrap().reason,
            crate::PendingReason::Changed
        );
        match tokio::time::timeout(Duration::from_secs(5), updates.recv())
            .await
            .expect("notice published on reload")
            .unwrap()
        {
            HookUpdate::ProjectHooksPending(notice) => {
                assert_eq!(notice.reason, crate::PendingReason::Changed)
            }
            other => panic!("unexpected update {other:?}"),
        }
        let summary = runner
            .dispatch(
                HookEvent::turn_completed(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.non_blocking_spawned, 0);
        assert!(!project.join("out/pwned.txt").exists());
    }

    #[tokio::test]
    async fn shutdown_aborts_background_runs_and_kills_groups() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "echo $$ > out/hook.pid; sleep 30; touch out/never.txt"
"#,
        );
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.non_blocking_spawned, 1);

        // Wait until the hook has actually started (it writes its pid first),
        // so shutdown is exercising abort-mid-run, not abort-before-spawn.
        let pid_path = project.join("out/hook.pid");
        let mut started = false;
        for _ in 0..100 {
            if pid_path.exists() {
                started = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(started, "hook never started");

        runner.shutdown().await;
        // The aborted run never completes or publishes a result.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), recv_run(&mut results))
                .await
                .is_err(),
            "aborted hook must not publish a result"
        );
        assert!(!project.join("out/never.txt").exists());
        // Its process group was killed.
        let pid_text = std::fs::read_to_string(&pid_path).unwrap();
        let pid: libc::pid_t = pid_text.trim().parse().unwrap();
        assert!(wait_for_pid_gone(pid), "pid {pid} survived shutdown");
        // Dispatch after shutdown spawns nothing (and does not panic).
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.non_blocking_spawned, 0);
    }

    fn pid_alive(pid: libc::pid_t) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn read_pid(path: &Path) -> libc::pid_t {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    async fn wait_for_file(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{} never appeared", path.display());
    }

    const SH_EXITS_BEFORE_BACKGROUND_JOB: &str = r#"
[[hook]]
event = "thread.created"
command = "sleep 600 & echo $! > out/gc.pid; exit 0"
blocking = BLOCKING
timeout_secs = 1
"#;

    #[tokio::test]
    async fn timeout_kills_group_after_the_shell_was_reaped() {
        // sh exits at once but its background job keeps stdout open. With a
        // drain grace far beyond the timeout the run is still pending when
        // the timeout fires, so the leader is already reaped: the kill must
        // use the pgid recorded at spawn.
        let (_dir, project, config_path) =
            project_with_config(&SH_EXITS_BEFORE_BACKGROUND_JOB.replace("BLOCKING", "true"));
        let runner = runner(&config_path);
        runner.set_drain_grace(Duration::from_secs(60));
        let started = Instant::now();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.blocking[0].timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
        let gc = read_pid(&project.join("out/gc.pid"));
        assert!(
            wait_for_pid_gone(gc),
            "background job {gc} survived timeout"
        );
    }

    #[tokio::test]
    async fn shutdown_kills_group_after_the_shell_was_reaped() {
        let (_dir, project, config_path) =
            project_with_config(&SH_EXITS_BEFORE_BACKGROUND_JOB.replace("BLOCKING", "false"));
        let runner = runner(&config_path);
        runner.set_drain_grace(Duration::from_secs(60));
        runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let gc_path = project.join("out/gc.pid");
        wait_for_file(&gc_path).await;
        // Let the shell exit and be reaped while the run is still draining.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let gc = read_pid(&gc_path);
        assert!(pid_alive(gc));
        runner.shutdown().await;
        assert!(
            wait_for_pid_gone(gc),
            "background job {gc} survived shutdown"
        );
    }

    #[tokio::test]
    async fn run_returns_when_the_shell_exits_and_kills_pipe_holders() {
        let (_dir, project, config_path) =
            project_with_config(&SH_EXITS_BEFORE_BACKGROUND_JOB.replace("BLOCKING", "true"));
        let runner = runner(&config_path);
        let started = Instant::now();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let result = &summary.blocking[0];
        // The real exit code survives and the run does not wait for the
        // timeout; the job still holding the pipes is killed after the drain.
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert!(started.elapsed() < Duration::from_millis(900));
        let gc = read_pid(&project.join("out/gc.pid"));
        assert!(wait_for_pid_gone(gc), "pipe-holding job {gc} survived");
    }

    #[tokio::test]
    async fn output_detached_background_job_is_left_running() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "sleep 600 >/dev/null 2>&1 & echo $! > out/gc.pid"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        let gc = read_pid(&project.join("out/gc.pid"));
        let alive = pid_alive(gc);
        unsafe { libc::kill(gc, libc::SIGKILL) };
        assert!(
            alive,
            "a job that released the pipes is a daemon, not leaked output"
        );
    }

    #[tokio::test]
    async fn finished_background_runs_are_reaped_from_the_task_set() {
        let (_dir, project, config_path) =
            project_with_config("[[hook]]\nevent = \"turn.completed\"\ncommand = \"true\"\n");
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        let ctx = context(&project, None);
        for _ in 0..60 {
            runner
                .dispatch(HookEvent::turn_completed(serde_json::json!({})), &ctx)
                .await;
            // Wait for the run to finish so the next spawn can reap it.
            tokio::time::timeout(Duration::from_secs(10), recv_run(&mut results))
                .await
                .expect("result published")
                .unwrap();
        }
        // Without reaping this would be 60 runs + the watcher.
        assert!(
            runner.task_count() <= 4,
            "task set grew to {}",
            runner.task_count()
        );
    }

    #[tokio::test]
    async fn inherited_turin_env_is_scrubbed() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "turn.failed"
command = "env > out/env.txt"
blocking = false
"#,
        );
        // SAFETY: test-only; the names are unique to this test.
        unsafe {
            std::env::set_var("TURIN_QC_SECRET", "hunter2");
            std::env::set_var("TURIN_THREAD_ID", "stale-thread");
        }
        let runner = runner(&config_path);
        let mut results = runner.subscribe_results();
        runner
            .dispatch(
                HookEvent::turn_failed(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(10), recv_run(&mut results))
            .await
            .unwrap()
            .unwrap();
        unsafe {
            std::env::remove_var("TURIN_QC_SECRET");
            std::env::remove_var("TURIN_THREAD_ID");
        }
        let env_text = std::fs::read_to_string(project.join("out/env.txt")).unwrap();
        assert!(!env_text.contains("hunter2"), "{env_text}");
        assert!(!env_text.contains("TURIN_QC_SECRET"), "{env_text}");
        assert!(!env_text.contains("stale-thread"), "{env_text}");
        assert!(env_text.contains("TURIN_EVENT=turn.failed"), "{env_text}");
    }

    #[test]
    fn parse_hook_return_reads_the_last_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let wt = cwd.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let line = format!(
            "creating worktree...\n{{\"worktreePath\": {:?}, \"branch\": \"turin/x\"}}\n\n",
            wt.display().to_string()
        );
        assert_eq!(
            parse_hook_return(&line, &cwd),
            Some(HookReturn {
                worktree_path: Some(wt.clone()),
                branch: Some("turin/x".into()),
            })
        );
        // Both fields are optional.
        assert_eq!(
            parse_hook_return("{\"branch\":\"b\"}", &cwd),
            Some(HookReturn {
                worktree_path: None,
                branch: Some("b".into())
            })
        );
        // Relative paths resolve against the hook's cwd, `..` included.
        assert_eq!(
            parse_hook_return("{\"worktreePath\":\"../wt/../wt\"}", &wt)
                .and_then(|r| r.worktree_path),
            Some(wt.clone())
        );
    }

    #[test]
    fn parse_hook_return_treats_bad_output_as_no_change() {
        let cwd = Path::new("/");
        for stdout in [
            "",
            "   \n\n",
            "just text",
            "{not json",
            "[1,2]",
            "{\"worktreePath\": 5}",
            "{}",
            "{\"worktreePath\": \"\", \"branch\": \"  \"}",
            // JSON that is not the last line does not count.
            "{\"branch\":\"b\"}\ndone",
            // A path that is not an existing directory is dropped.
            "{\"worktreePath\": \"/definitely/not/a/real/dir\"}",
        ] {
            assert_eq!(parse_hook_return(stdout, cwd), None, "{stdout:?}");
        }
    }

    #[tokio::test]
    async fn blocking_thread_created_hook_reports_a_worktree() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "mkdir -p wt-new && echo 'making worktree' && printf '{\"worktreePath\":\"%s\",\"branch\":\"turin/t\"}\n' \"$PWD/wt-new\""
blocking = true

[[hook]]
event = "thread.created"
command = "pwd > where.txt; echo $TURIN_WORKTREE_PATH > env-path.txt"
cwd = "thread"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let wt = project.join("wt-new");
        assert_eq!(
            summary.thread_workspace,
            Some(HookReturn {
                worktree_path: Some(wt.clone()),
                branch: Some("turin/t".into()),
            })
        );
        assert_eq!(summary.blocking[0].returned, summary.thread_workspace);
        assert_eq!(summary.blocking[1].returned, None);
        // The next hook with cwd = "thread" already runs in the new worktree.
        assert_eq!(summary.blocking[1].cwd, wt);
        let where_txt = std::fs::read_to_string(wt.join("where.txt")).unwrap();
        assert_eq!(where_txt.trim(), wt.display().to_string());
        let env_path = std::fs::read_to_string(wt.join("env-path.txt")).unwrap();
        assert_eq!(env_path.trim(), wt.display().to_string());
    }

    #[tokio::test]
    async fn failed_or_non_blocking_hooks_do_not_return_a_workspace() {
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "mkdir -p wt && printf '{\"branch\":\"b\"}\n'; exit 3"
blocking = true

[[hook]]
event = "thread.created"
command = "printf '{\"branch\":\"b\"}\n'"
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(3));
        assert_eq!(summary.thread_workspace, None);
        assert_eq!(summary.non_blocking_spawned, 1);
    }

    #[tokio::test]
    async fn script_edit_under_turin_is_caught_on_dispatch_without_a_reload() {
        let (_dir, project, config_path) = project_with_config(
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"sh .turin/hooks/setup.sh\"\nblocking = true\n",
        );
        let script = project.join(".turin/hooks/setup.sh");
        write_config(&script, "touch out/approved.txt\n");
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        assert!(project.join("out/approved.txt").exists());

        // An agent or `git pull` rewrites only the script. Dispatch re-hashes
        // the tree itself, so it does not matter whether the watcher has fired.
        std::fs::write(&script, "touch out/pwned.txt\n").unwrap();
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.blocking.is_empty());
        assert!(!project.join("out/pwned.txt").exists());
        let pending = summary.project_hooks_pending.expect("re-approval needed");
        assert_eq!(pending.reason, crate::PendingReason::Changed);

        // Re-approving the reviewed hash restores it, with the new script.
        runner.trust_project_hooks(&pending.hash).await.unwrap();
        runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(project.join("out/pwned.txt").exists());
    }

    #[tokio::test]
    async fn dispatch_notices_a_project_config_the_watcher_missed() {
        let dir = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(dir.path()).unwrap();
        let config_path = project.join(".turin/hooks.toml");
        let runner = untrusted_runner(&config_path);
        assert_eq!(
            *runner.config().project_status(),
            ProjectHooksStatus::Absent
        );
        write_config(
            &config_path,
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"true\"\n",
        );
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.project_hooks_pending.is_some());
    }

    #[tokio::test]
    async fn untrustable_project_hooks_cannot_be_approved() {
        let (_dir, project, config_path) = project_with_config(
            "[[hook]]\nevent = \"thread.created\"\ncommand = \"true\"\nblocking = true\n",
        );
        std::fs::write(project.join("evil.sh"), "touch out/pwned.txt").unwrap();
        std::os::unix::fs::symlink(project.join("evil.sh"), project.join(".turin/setup.sh"))
            .unwrap();
        let runner = untrusted_runner(&config_path);
        let pending = runner.config().pending_project_hooks().unwrap().clone();
        assert_eq!(pending.reason, crate::PendingReason::Untrustable);
        assert!(matches!(
            runner.trust_project_hooks(&pending.hash).await,
            Err(TrustError::Untrustable(_))
        ));
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert!(summary.blocking.is_empty());
    }

    #[tokio::test]
    async fn pipe_holder_kills_the_whole_group_including_detached_jobs() {
        // Mixed case: one job redirects its output away, a sibling still
        // holds stdout. After the drain the entire group is killed.
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "sleep 600 >/dev/null 2>&1 & echo $! > out/detached.pid; sleep 601 & echo $! > out/holder.pid; exit 0"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        let summary = runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        assert_eq!(summary.blocking[0].exit_code, Some(0));
        assert!(!summary.blocking[0].timed_out);
        for name in ["detached", "holder"] {
            let pid = read_pid(&project.join(format!("out/{name}.pid")));
            assert!(wait_for_pid_gone(pid), "{name} job {pid} survived");
        }
    }

    #[tokio::test]
    async fn job_started_in_its_own_session_survives_the_group_kill() {
        // The documented way to leave a daemon running: leave the group.
        // (`perl` is how the docs spell setsid on macOS.)
        if std::process::Command::new("perl")
            .arg("-v")
            .output()
            .is_err()
        {
            return;
        }
        let (_dir, project, config_path) = project_with_config(
            r#"
[[hook]]
event = "thread.created"
command = "perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' sleep 600 >/dev/null 2>&1 & echo $! > out/daemon.pid; sleep 601 & echo $! > out/holder.pid; exit 0"
blocking = true
"#,
        );
        let runner = runner(&config_path);
        runner
            .dispatch(
                HookEvent::thread_created(serde_json::json!({})),
                &context(&project, None),
            )
            .await;
        let holder = read_pid(&project.join("out/holder.pid"));
        assert!(wait_for_pid_gone(holder), "pipe holder {holder} survived");
        let daemon = read_pid(&project.join("out/daemon.pid"));
        let alive = pid_alive(daemon);
        unsafe { libc::kill(daemon, libc::SIGKILL) };
        assert!(alive, "setsid job {daemon} should have left the group");
    }
}
