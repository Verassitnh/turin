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

use crate::config::{HookConfigSource, HookCwd, HookPaths, HookSpec, HooksConfig};
use crate::event::{HookEvent, HookEventKind};

/// Per-stream output capture limit, mirroring upstream's bounded capture.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Appended when a stream's output was cut at the limit.
pub const TRUNCATED_MARKER: &str = "\n...[turin: output truncated]";
/// Coalescing window for config-file change events before a hot reload.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);
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
}

/// What [`HookRunner::dispatch`] did: awaited results for blocking hooks and
/// the number of non-blocking runs it spawned into the background.
#[derive(Debug, Clone, Default)]
pub struct DispatchSummary {
    pub blocking: Vec<HookRunResult>,
    pub non_blocking_spawned: usize,
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
    let config = runner.config();
    let mut summary = DispatchSummary::default();

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
        };
        if should_block {
            let result = run_hook(&plan).await;
            let _ = runner.inner.results.send(result.clone());
            summary.blocking.push(result);
        } else {
            let results = runner.inner.results.clone();
            let spawned = runner.try_spawn(async move {
                let result = run_hook(&plan).await;
                let _ = results.send(result);
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
    };

    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(&plan.spec.command);
    command.current_dir(&plan.cwd);
    // Login-shell-equivalent environment: the full parent env plus TURIN_*.
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
    match tokio::time::timeout(timeout, guard.run(plan.stdin.as_bytes())).await {
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

/// Reads up to `max_bytes`, then keeps draining and discarding so the child
/// can never block on a full pipe. Mirrors upstream's truncate mode.
async fn read_bounded<R: AsyncRead + Unpin>(reader: Option<R>, max_bytes: usize) -> StreamCapture {
    let Some(mut reader) = reader else {
        return StreamCapture::default();
    };
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if !truncated {
                    if bytes.len() + n > max_bytes {
                        let keep = max_bytes.saturating_sub(bytes.len());
                        bytes.extend_from_slice(&chunk[..keep]);
                        truncated = true;
                    } else {
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if truncated {
        bytes.extend_from_slice(TRUNCATED_MARKER.as_bytes());
    }
    match String::from_utf8(bytes) {
        Ok(text) => StreamCapture {
            text,
            truncated,
            invalid_utf8: false,
        },
        Err(err) => StreamCapture {
            text: String::from_utf8_lossy(err.as_bytes()).into_owned(),
            truncated,
            invalid_utf8: true,
        },
    }
}

/// Owns the hook child for its whole run. Dropping it before the run
/// finishes (timeout path handled explicitly; task abort path via Drop)
/// kills the child's whole process group, so grandchildren cannot survive.
struct ChildGroupGuard {
    child: Option<Child>,
    finished: bool,
}

impl ChildGroupGuard {
    fn new(child: Child) -> Self {
        Self {
            child: Some(child),
            finished: false,
        }
    }

    fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Writes stdin, drains both output streams, and waits for exit.
    async fn run(&mut self, stdin_bytes: &[u8]) -> Result<RunCapture, String> {
        let child = self
            .child
            .as_mut()
            .expect("the guard owns the child for the whole run");
        let stdin_handle = child.stdin.take();
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();
        let write = async {
            if let Some(mut handle) = stdin_handle {
                // A hook that never reads stdin gets EPIPE here; that is not
                // an error (upstream only fails on spawn/read/exit).
                let _ = handle.write_all(stdin_bytes).await;
                let _ = handle.shutdown().await;
            }
        };
        let stdout = read_bounded(stdout_handle, MAX_OUTPUT_BYTES);
        let stderr = read_bounded(stderr_handle, MAX_OUTPUT_BYTES);
        let wait = child.wait();
        let (_, stdout, stderr, status) = tokio::join!(write, stdout, stderr, wait);
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
        if let Some(pid) = self.id() {
            let _ = kill_process_group(pid);
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

/// SIGKILL to `-pid`: every member of the group the child leads. The child
/// was spawned with `process_group(0)`, so it is the group leader.
#[cfg(unix)]
fn kill_process_group(pid: u32) -> bool {
    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) -> bool {
    false
}

/// Owns hook runs and the results channel. Clone it freely; shutdown drains
/// every clone through the shared inner state.
#[derive(Clone)]
pub struct HookRunner {
    inner: Arc<HookRunnerInner>,
}

struct HookRunnerInner {
    paths: HookPaths,
    config: RwLock<Arc<HooksConfig>>,
    /// Background hook runs and the hot-reload task. `None` after shutdown.
    tasks: Mutex<Option<JoinSet<()>>>,
    results: broadcast::Sender<HookRunResult>,
    /// Dropped on shutdown to stop file watching.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
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
        });
        let runner = Self { inner };
        runner.start_watcher();
        runner
    }

    /// Subscribes to every completed hook run (blocking and non-blocking).
    /// A lagging subscriber loses old results; it never slows hooks down.
    pub fn subscribe_results(&self) -> broadcast::Receiver<HookRunResult> {
        self.inner.results.subscribe()
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
        let mut watcher = match notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!(%err, "hooks hot reload disabled: no filesystem watcher available");
                return;
            }
        };
        // Watch the parent directories, not the files: editors save
        // atomically (temp file + rename), which replaces the inode a
        // file watch points at.
        let mut watched: Vec<PathBuf> = Vec::new();
        for path in [&self.inner.paths.global, &self.inner.paths.project]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = path.parent()
                && parent.is_dir()
                && !watched.contains(&parent.to_path_buf())
            {
                match watcher.watch(parent, notify::RecursiveMode::NonRecursive) {
                    Ok(()) => watched.push(parent.to_path_buf()),
                    Err(err) => {
                        tracing::warn!(%err, dir = %parent.display(), "hooks hot reload: cannot watch directory")
                    }
                }
            }
        }
        if watched.is_empty() {
            return;
        }
        *self.inner.watcher.lock() = Some(watcher);

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
                        if !event.paths.iter().any(|path| paths.contains_file(path)) {
                            continue;
                        }
                        // Coalesce bursts (atomic saves emit several events),
                        // then reload once.
                        tokio::time::sleep(RELOAD_DEBOUNCE).await;
                        while rx.try_recv().is_ok() {}
                        if let Some(inner) = weak.upgrade() {
                            inner.reload_config();
                        } else {
                            return;
                        }
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
    fn reload_config(&self) -> Arc<HooksConfig> {
        let fresh = Arc::new(HooksConfig::load(&self.paths));
        log_config(&fresh);
        *self.config.write() = fresh.clone();
        fresh
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

    fn runner(project_config: &Path) -> HookRunner {
        HookRunner::with_paths(HookPaths {
            global: None,
            project: Some(project_config.to_path_buf()),
        })
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
        let published = tokio::time::timeout(Duration::from_secs(5), results.recv())
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
        let published = tokio::time::timeout(Duration::from_secs(30), results.recv())
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
        let published = tokio::time::timeout(Duration::from_secs(30), results.recv())
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
        let runner = HookRunner::with_paths(HookPaths {
            global: Some(global_path),
            project: Some(project_config),
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

    #[tokio::test]
    async fn watcher_hot_reloads_on_file_change() {
        let (_dir, _project, config_path) = project_with_config(
            "[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/one.txt\"\n",
        );
        let runner = runner(&config_path);
        assert_eq!(
            runner
                .config()
                .hooks_for(HookEventKind::TurnCompleted)
                .len(),
            1
        );

        write_config(
            &config_path,
            "[[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/one.txt\"\n\n\
             [[hook]]\nevent = \"turn.completed\"\ncommand = \"touch out/two.txt\"\n",
        );
        // Filesystem event latency varies by platform; poll the snapshot.
        let mut updated = false;
        for _ in 0..200 {
            if runner
                .config()
                .hooks_for(HookEventKind::TurnCompleted)
                .len()
                == 2
            {
                updated = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(updated, "watcher did not reload the config in time");
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
            tokio::time::timeout(Duration::from_millis(500), results.recv())
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
}
