//! Login-shell environment hydration at startup (upstream `fixPath`, `os-jank.ts`).
//!
//! A bundle opened from Finder or the Dock inherits launchd's PATH, which does not hold
//! `claude`, `codex`, `cursor-agent`, `opencode`, `git` or `gh` when they live in
//! `~/.local/bin`, `/opt/homebrew/bin`, nvm or bun. Every provider probe and session spawn
//! reads the process environment, so the PATH is repaired once, before anything else starts.

use turin_shared::shell::{
    Platform, exec_file_sync, hydrate_posix_environment, hydrate_posix_home, read_user_login_shell,
};

/// Repairs `HOME` and `PATH` (and mise's `__MISE_DIFF`) from the user's login shell.
///
/// Call it first thing in `main`, before any thread exists: it writes the process environment.
pub fn hydrate_process_environment() {
    let platform = Platform::current();
    if !matches!(platform, Platform::Darwin | Platform::Linux) {
        return;
    }
    let home = hydrate_posix_home(std::env::var("HOME").ok().as_deref(), || {
        Some(turin_provider_core::host_process::HostProcess::current().home_directory)
    });
    let hydrated = hydrate_posix_environment(
        platform,
        std::env::var("SHELL").ok().as_deref(),
        read_user_login_shell().as_deref(),
        std::env::var("PATH").ok().as_deref(),
        &exec_file_sync,
        &|message| eprintln!("[turin] {message}"),
    );
    // SAFETY: `main` calls this before it creates the tokio runtime, the UI or any other thread
    // that could read the environment concurrently.
    unsafe {
        if let Some(home) = home {
            std::env::set_var("HOME", home);
        }
        if let Some(path) = hydrated.path {
            std::env::set_var("PATH", path);
        }
        if let Some(mise_diff) = hydrated.mise_diff {
            std::env::set_var("__MISE_DIFF", mise_diff);
        }
    }
}
