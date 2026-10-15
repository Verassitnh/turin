//! Host seam for the launcher: PATH lookup, directory listing, detached
//! spawns, timed command output, and a monotonic clock.
//!
//! Production uses [`SystemHost`]. Tests inject a recording host so no real
//! application is ever launched. The PATH lookup is a stand-in for upstream
//! `@t3tools/shared/shell` (`isCommandAvailable`), which is not on main yet;
//! see PORTING.md Deviations.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Upstream `ProcessEnv`: the environment a lookup or launch sees.
pub type Env = HashMap<String, String>;

static PROCESS_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

#[async_trait]
pub trait LaunchHost: Send + Sync + 'static {
    /// Nanoseconds on a monotonic clock (upstream `Clock.monotonicTimeNanos`).
    fn monotonic_nanos(&self) -> u64;

    /// `isCommandAvailable(command, { env })`: an executable that resolves
    /// through `env`'s PATH, or an absolute path to one.
    fn is_command_available(&self, command: &str, env: &Env) -> bool;

    /// Entry names of a directory. Empty when it cannot be read, matching
    /// upstream `readDirectory(...).pipe(Effect.orElseSucceed(() => []))`.
    fn read_directory(&self, path: &Path) -> Vec<String>;

    /// Starts a child with stdio ignored and does not wait for it. Errors only
    /// when the spawn itself fails.
    async fn spawn_detached(&self, command: &str, args: &[String]) -> io::Result<()>;

    /// Runs a command to completion and returns its stdout. Errors on spawn
    /// failure, non-zero exit, or timeout (the child is killed on timeout).
    async fn command_output(
        &self,
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> io::Result<String>;
}

/// The real macOS host.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHost;

#[async_trait]
impl LaunchHost for SystemHost {
    fn monotonic_nanos(&self) -> u64 {
        u64::try_from(PROCESS_ORIGIN.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn is_command_available(&self, command: &str, env: &Env) -> bool {
        resolve_command_path(command, env).is_some()
    }

    fn read_directory(&self, path: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }

    async fn spawn_detached(&self, command: &str, args: &[String]) -> io::Result<()> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Own process group, so a Ctrl-C aimed at the server does not reach
            // the editor (upstream `detached: true`).
            .process_group(0)
            .spawn()?;
        // Deliberately no `kill_on_drop`: upstream unrefs the child so it
        // outlives the server, and dropping this handle must not close the
        // editor. The reaper task only collects the exit status.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }

    async fn command_output(
        &self,
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> io::Result<String> {
        let mut child = tokio::process::Command::new(command);
        child
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            // Timeout drops the future, which must kill the probe.
            .kill_on_drop(true);
        let output = tokio::time::timeout(timeout, child.output())
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, format!("{command} timed out"))
            })??;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{command} exited with {}",
                output.status
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Command resolution on macOS: a command with a `/` is checked as-is,
/// otherwise each PATH entry from `env` is tried in order.
pub fn resolve_command_path(command: &str, env: &Env) -> Option<PathBuf> {
    if command.contains('/') {
        let candidate = PathBuf::from(command);
        return is_executable_file(&candidate).then_some(candidate);
    }
    let path_value = env.get("PATH")?;
    if path_value.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    for entry in path_value.split(':') {
        let entry = entry.trim();
        if entry.is_empty() || !seen.insert(entry) {
            continue;
        }
        let candidate = Path::new(entry).join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// `is_file` plus any execute bit set. Approximates `access(X_OK)` without
/// the uid check.
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Process environment as an [`Env`] map.
pub fn process_env() -> Env {
    std::env::vars().collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn write_executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn resolves_commands_through_env_path_in_order() {
        let home = tempfile::tempdir().unwrap();
        let first = home.path().join("first");
        let second = home.path().join("second");
        write_executable(&second.join("tool"));
        let env = Env::from([(
            "PATH".to_owned(),
            format!("{}:{}", first.display(), second.display()),
        )]);

        assert_eq!(
            resolve_command_path("tool", &env),
            Some(second.join("tool"))
        );
        assert_eq!(resolve_command_path("missing", &env), None);
    }

    #[test]
    fn non_executable_files_and_empty_path_do_not_resolve() {
        let home = tempfile::tempdir().unwrap();
        let tool = home.path().join("tool");
        fs::write(&tool, "").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
        let env = Env::from([("PATH".to_owned(), home.path().display().to_string())]);

        assert_eq!(resolve_command_path("tool", &env), None);
        assert_eq!(
            resolve_command_path("tool", &Env::from([("PATH".to_owned(), String::new())])),
            None
        );
    }

    #[test]
    fn absolute_paths_skip_path_lookup() {
        let home = tempfile::tempdir().unwrap();
        let tool = home.path().join("bin/tool");
        write_executable(&tool);

        assert_eq!(
            resolve_command_path(tool.to_str().unwrap(), &Env::new()),
            Some(tool)
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A host that delegates filesystem and PATH lookups to the real disk
    //! (tests write fixture executables into temp dirs) but records every
    //! spawn and output request instead of running it.

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Spawn {
        pub command: String,
        pub args: Vec<String>,
    }

    type OutputHandler = Box<dyn Fn(&str, &[String]) -> io::Result<String> + Send + Sync>;
    type AvailabilityHook = Box<dyn Fn(&str) + Send + Sync>;

    #[derive(Default)]
    pub struct FakeHost {
        pub now: AtomicU64,
        pub availability_checks: AtomicUsize,
        pub spawns: Mutex<Vec<Spawn>>,
        pub output: Mutex<Option<OutputHandler>>,
        pub on_availability: Mutex<Option<AvailabilityHook>>,
    }

    impl FakeHost {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn spawned(&self) -> Vec<Spawn> {
            self.spawns.lock().unwrap().clone()
        }

        pub fn last_spawn(&self) -> Option<Spawn> {
            self.spawns.lock().unwrap().last().cloned()
        }

        pub fn advance(&self, duration: Duration) {
            let delta = u64::try_from(duration.as_nanos()).unwrap();
            self.now.fetch_add(delta, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl LaunchHost for FakeHost {
        fn monotonic_nanos(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn is_command_available(&self, command: &str, env: &Env) -> bool {
            self.availability_checks.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = self.on_availability.lock().unwrap().as_ref() {
                hook(command);
            }
            SystemHost.is_command_available(command, env)
        }

        fn read_directory(&self, path: &Path) -> Vec<String> {
            SystemHost.read_directory(path)
        }

        async fn spawn_detached(&self, command: &str, args: &[String]) -> io::Result<()> {
            self.spawns.lock().unwrap().push(Spawn {
                command: command.to_owned(),
                args: args.to_vec(),
            });
            Ok(())
        }

        async fn command_output(
            &self,
            command: &str,
            args: &[String],
            _timeout: Duration,
        ) -> io::Result<String> {
            match self.output.lock().unwrap().as_ref() {
                Some(handler) => handler(command, args),
                None => Ok(String::new()),
            }
        }
    }
}
