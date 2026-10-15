//! turin-hooks: user-defined shell hooks that run on orchestrator events.
//!
//! New functionality (PORTING.md "Hooks (new)"): replaces upstream's
//! `ThreadLaunchService` / worktree creation. Process-running conventions
//! follow upstream `apps/server/src/processRunner.ts` and
//! `apps/server/src/project/ProjectSetupScriptRunner.ts`.
//!
//! Overview:
//! - [`HooksConfig`] loads `~/.turin/hooks.toml` and
//!   `<project>/.turin/hooks.toml` (both optional; project hooks run after
//!   global ones). Invalid entries are skipped and reported with file and
//!   hook index — never fatal.
//! - [`HookRunner::dispatch`] runs matching hooks as `/bin/sh -c <command>`
//!   in their own process group with the event JSON on stdin and `TURIN_*`
//!   env vars. Timeout kills the whole group. Blocking hooks are awaited
//!   (thread.created only); everything else runs in a background `JoinSet`.
//!   Results are published on a broadcast channel for the UI.
//! - Config files are watched for changes; call [`HookRunner::reload`] to
//!   force a re-read.
//!
//! See `docs/hooks.md` for the user guide.

pub mod config;
pub mod event;
pub mod runner;

pub use config::{
    DEFAULT_TIMEOUT_SECS, HookConfigError, HookConfigSource, HookCwd, HookPaths, HookSpec,
    HooksConfig,
};
pub use event::{HookEvent, HookEventKind, UnknownHookEventError};
pub use runner::{
    DispatchSummary, HookDispatchContext, HookRunResult, HookRunner, MAX_OUTPUT_BYTES,
    TRUNCATED_MARKER, dispatch, hook_env_vars, hook_stdin_json, resolve_blocking, resolve_hook_cwd,
};
