//! Port of `fixPath` / `hydratePosixHome` in upstream/apps/server/src/os-jank.ts.
//!
//! A GUI app started from Finder or the Dock inherits launchd's minimal PATH
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), not the one in the user's shell, so
//! `claude`, `codex`, `gh` and friends are "not found". `fix_path` asks the
//! user's login shell for its PATH (`$SHELL -ilc`, 5 s per shell, markers
//! around each value so rc-file noise is ignored) and merges it in front of
//! the inherited one. A shell that fails or times out is logged and skipped;
//! if none yields a PATH, macOS falls back to `launchctl getenv PATH`, and
//! with nothing at all the existing PATH is kept.
//!
//! Upstream works on `process.env`; here [`fix_path`] edits an explicit
//! [`ShellEnv`] so tests inject the shell, and [`fix_process_path`] applies the
//! result to the real process environment.

use crate::shell::{
    ExecFileSync, Platform, ShellEnv, UserInfo, exec_file_sync, list_login_shell_candidates,
    merge_path_entries, read_environment_from_login_shell, read_path_from_launchctl,
    read_user_info,
};

/// Variables read from the login shell. mise's record of the PATH entries it
/// added must travel with the shell's PATH, or `mise x` can rebuild PATH behind
/// a same-named wrapper (`exec mise x claude -- claude`) that then re-runs
/// itself forever.
const LOGIN_SHELL_VARIABLES: [&str; 2] = ["PATH", "__MISE_DIFF"];

/// `fixPath` options. `shell_environment_prepared`: the parent already handed
/// over a hydrated PATH, so no login shell runs (HOME is still hydrated).
#[derive(Debug, Clone, Copy, Default)]
pub struct FixPathOptions {
    pub shell_environment_prepared: bool,
}

/// Everything `fix_path` reads from the host, so tests can replace it.
pub struct FixPathHost<'a> {
    pub platform: Platform,
    /// Runs the login shell and `launchctl`.
    pub exec_file: ExecFileSync<'a>,
    /// The passwd entry (`NodeOS.userInfo()`).
    pub user_info: UserInfo,
}

/// `hydratePosixHome`: sets HOME from the user account when it is unset or
/// blank. An explicitly configured HOME is never replaced.
pub fn hydrate_posix_home(env: &mut ShellEnv, home_dir: impl FnOnce() -> Option<String>) {
    if env.get("HOME").is_some_and(|home| !home.trim().is_empty()) {
        return;
    }
    if let Some(home) = home_dir().filter(|home| !home.is_empty()) {
        env.insert("HOME".to_owned(), home);
    }
}

fn hydrate_posix_path(env: &mut ShellEnv, host: &FixPathHost<'_>) {
    let mut shell_environment = indexmap::IndexMap::new();
    let candidates = list_login_shell_candidates(
        host.platform,
        env.get("SHELL").map(String::as_str),
        host.user_info.shell.as_deref(),
    );
    for shell in &candidates {
        match read_environment_from_login_shell(shell, &LOGIN_SHELL_VARIABLES, host.exec_file) {
            Ok(read) => shell_environment = read,
            Err(error) => {
                tracing::warn!("Failed to read PATH from login shell {shell}. {error}");
            }
        }
        if shell_environment.contains_key("PATH") {
            break;
        }
    }

    let shell_path = shell_environment.get("PATH").cloned();
    let launchctl_path = if host.platform == Platform::Darwin && shell_path.is_none() {
        read_path_from_launchctl(host.exec_file)
    } else {
        None
    };
    let merged_path = merge_path_entries(
        shell_path.as_deref().or(launchctl_path.as_deref()),
        env.get("PATH").map(String::as_str),
        host.platform,
    );
    if let Some(path) = merged_path {
        env.insert("PATH".to_owned(), path);
    }
    if shell_path.is_some()
        && let Some(diff) = shell_environment.get("__MISE_DIFF")
    {
        env.insert("__MISE_DIFF".to_owned(), diff.clone());
    }
}

/// `fixPath` for macOS and Linux (Turin has no Windows build). Failures are
/// logged and leave the existing values in place.
pub fn fix_path(env: &mut ShellEnv, options: FixPathOptions, host: &FixPathHost<'_>) {
    if host.platform != Platform::Darwin && host.platform != Platform::Linux {
        return;
    }
    hydrate_posix_home(env, || host.user_info.homedir.clone());
    if options.shell_environment_prepared {
        return;
    }
    hydrate_posix_path(env, host);
}

/// Runs [`fix_path`] against the real process: login shell via `exec_file_sync`
/// and the passwd entry. Call once at startup, before any thread starts and
/// before any provider, git, or launcher process is spawned. It blocks for as
/// long as the login shell takes (5 s per shell at most).
pub fn fix_process_path() {
    let mut env: ShellEnv = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let before = env.clone();
    let host = FixPathHost {
        platform: Platform::current(),
        exec_file: &exec_file_sync,
        user_info: read_user_info(),
    };
    fix_path(&mut env, FixPathOptions::default(), &host);
    for key in ["HOME", "PATH", "__MISE_DIFF"] {
        if let Some(value) = env.get(key)
            && before.get(key) != Some(value)
        {
            // SAFETY: documented to run before any other thread exists, so
            // nothing reads the environment concurrently.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{CommandAvailabilityOptions, CommandResolutionCache, ShellContext};
    use std::cell::RefCell;
    use std::io;
    use std::time::{Duration, Instant};

    fn env(pairs: &[(&str, &str)]) -> ShellEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn user(shell: Option<&str>) -> UserInfo {
        UserInfo {
            shell: shell.map(str::to_owned),
            homedir: Some("/Users/fixture".to_owned()),
        }
    }

    fn run(
        platform: Platform,
        options: FixPathOptions,
        env: &mut ShellEnv,
        user_info: UserInfo,
        exec: ExecFileSync<'_>,
    ) {
        fix_path(
            env,
            options,
            &FixPathHost {
                platform,
                exec_file: exec,
                user_info,
            },
        );
    }

    fn path_output(path: &str) -> String {
        format!("__T3CODE_ENV_PATH_START__\n{path}\n__T3CODE_ENV_PATH_END__\n")
    }

    #[test]
    fn shell_path_goes_first_and_inherited_entries_follow() {
        let mut env = env(&[("SHELL", "/bin/bash"), ("PATH", "/usr/bin")]);
        let calls = RefCell::new(Vec::new());
        run(
            Platform::Linux,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|shell, args, timeout| {
                calls
                    .borrow_mut()
                    .push((shell.to_owned(), args[0].to_owned(), timeout));
                Ok(path_output("/opt/tools/bin"))
            },
        );
        assert_eq!(env["PATH"], "/opt/tools/bin:/usr/bin");
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/bin/bash");
        assert_eq!(calls[0].1, "-ilc");
        assert_eq!(calls[0].2, Duration::from_millis(5000));
    }

    #[test]
    fn rc_file_noise_around_the_markers_is_ignored() {
        let mut env = env(&[("PATH", "/usr/bin:/bin")]);
        run(
            Platform::Darwin,
            FixPathOptions::default(),
            &mut env,
            user(Some("/opt/homebrew/bin/fish")),
            &|_, _, _| {
                Ok(format!(
                    "Last login: today\nwelcome banner\n{}trailing noise\n",
                    path_output("/opt/homebrew/bin:/Users/fixture/.local/bin:/usr/bin")
                ))
            },
        );
        assert_eq!(
            env["PATH"],
            "/opt/homebrew/bin:/Users/fixture/.local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn mise_diff_travels_with_the_shell_path() {
        let mut env = env(&[("SHELL", "/bin/bash"), ("PATH", "/usr/bin")]);
        run(
            Platform::Linux,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|_, _, _| {
                Ok([
                    "__T3CODE_ENV_PATH_START__",
                    "/home/me/.local/bin:/mise/installs/claude/latest:/mise/shims",
                    "__T3CODE_ENV_PATH_END__",
                    "__T3CODE_ENV___MISE_DIFF_START__",
                    "mise-diff",
                    "__T3CODE_ENV___MISE_DIFF_END__",
                ]
                .join("\n"))
            },
        );
        assert_eq!(env["__MISE_DIFF"], "mise-diff");
    }

    #[test]
    fn mise_diff_alone_is_not_imported_without_a_shell_path() {
        let mut env = env(&[("SHELL", "/bin/bash"), ("PATH", "/usr/bin")]);
        run(
            Platform::Linux,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|_, _, _| {
                Ok(
                    "__T3CODE_ENV___MISE_DIFF_START__\nmise-diff\n__T3CODE_ENV___MISE_DIFF_END__\n"
                        .to_owned(),
                )
            },
        );
        assert!(!env.contains_key("__MISE_DIFF"));
        assert_eq!(env["PATH"], "/usr/bin");
    }

    #[test]
    fn missing_markers_keep_the_existing_path_and_try_the_next_shell() {
        let mut env = env(&[("SHELL", "/bin/first"), ("PATH", "/usr/bin")]);
        let tried = RefCell::new(Vec::new());
        run(
            Platform::Linux,
            FixPathOptions::default(),
            &mut env,
            user(Some("/bin/second")),
            &|shell, _, _| {
                tried.borrow_mut().push(shell.to_owned());
                if shell == "/bin/second" {
                    Ok(path_output("/second/bin"))
                } else {
                    Ok("noise without any marker\n".to_owned())
                }
            },
        );
        assert_eq!(*tried.borrow(), ["/bin/first", "/bin/second"]);
        assert_eq!(env["PATH"], "/second/bin:/usr/bin");
    }

    #[test]
    fn timeout_and_failure_keep_the_existing_path() {
        let mut env = env(&[("SHELL", "/bin/zsh"), ("PATH", "/usr/bin:/bin")]);
        let attempts = RefCell::new(0);
        run(
            Platform::Linux,
            FixPathOptions::default(),
            &mut env,
            user(Some("/bin/fish")),
            &|_, _, _| {
                *attempts.borrow_mut() += 1;
                Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"))
            },
        );
        // /bin/zsh, /bin/fish, then the Linux fallback /bin/bash.
        assert_eq!(*attempts.borrow(), 3);
        assert_eq!(env["PATH"], "/usr/bin:/bin");
    }

    #[test]
    fn darwin_falls_back_to_launchctl_when_no_shell_reports_a_path() {
        let mut env = env(&[("PATH", "/usr/bin:/bin")]);
        run(
            Platform::Darwin,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|file, args, timeout| {
                if file == "/bin/launchctl" {
                    assert_eq!(args, ["getenv", "PATH"]);
                    assert_eq!(timeout, Duration::from_millis(2000));
                    Ok("/launchctl/bin\n".to_owned())
                } else {
                    Err(io::Error::other("exit status 1"))
                }
            },
        );
        assert_eq!(env["PATH"], "/launchctl/bin:/usr/bin:/bin");
    }

    #[test]
    fn launchctl_is_not_read_when_the_shell_reported_a_path() {
        let mut env = env(&[("PATH", "/usr/bin")]);
        run(
            Platform::Darwin,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|file, _, _| {
                assert_ne!(file, "/bin/launchctl");
                Ok(path_output("/shell/bin"))
            },
        );
        assert_eq!(env["PATH"], "/shell/bin:/usr/bin");
    }

    #[test]
    fn prepared_environment_launches_no_shell_but_still_hydrates_home() {
        let mut env = env(&[("PATH", "/prepared/bin:/usr/bin")]);
        run(
            Platform::Darwin,
            FixPathOptions {
                shell_environment_prepared: true,
            },
            &mut env,
            user(None),
            &|_, _, _| panic!("the prepared environment must not launch another shell"),
        );
        assert_eq!(env["PATH"], "/prepared/bin:/usr/bin");
        assert_eq!(env["HOME"], "/Users/fixture");
    }

    #[test]
    fn home_is_hydrated_when_blank_and_kept_when_configured() {
        let mut blank = env(&[("HOME", "  ")]);
        hydrate_posix_home(&mut blank, || Some("/Users/fixture".to_owned()));
        assert_eq!(blank["HOME"], "/Users/fixture");

        let mut unset = ShellEnv::new();
        hydrate_posix_home(&mut unset, || Some("/Users/fixture".to_owned()));
        assert_eq!(unset["HOME"], "/Users/fixture");

        let mut configured = env(&[("HOME", "/custom/home")]);
        hydrate_posix_home(&mut configured, || panic!("HOME lookup should not run"));
        assert_eq!(configured["HOME"], "/custom/home");
    }

    #[test]
    fn other_platforms_are_left_alone() {
        let mut env = env(&[("PATH", "/usr/bin")]);
        run(
            Platform::Win32,
            FixPathOptions::default(),
            &mut env,
            user(None),
            &|_, _, _| panic!("no shell on Windows"),
        );
        assert_eq!(env, self::env(&[("PATH", "/usr/bin")]));
    }

    #[cfg(unix)]
    mod real_shell {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;

        fn write_script(path: &Path, body: &str) {
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// A "login shell" that prints noise and a PATH only it knows. Records
        /// that it ran with `-ilc`.
        fn fake_shell(dir: &Path, bin_dir: &Path) -> String {
            let shell = dir.join("fake-shell");
            write_script(
                &shell,
                &format!(
                    "#!/bin/sh\n\
                     [ \"$1\" = \"-ilc\" ] || exit 9\n\
                     echo 'rc noise: oh-my-zsh updating...'\n\
                     printf '%s\\n' '__T3CODE_ENV_PATH_START__'\n\
                     printf '%s\\n' '{}:/usr/bin'\n\
                     printf '%s\\n' '__T3CODE_ENV_PATH_END__'\n",
                    bin_dir.display()
                ),
            );
            shell.display().to_string()
        }

        #[test]
        fn binary_lookup_uses_the_path_only_the_login_shell_provides() {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("shell-only-bin");
            std::fs::create_dir(&bin).unwrap();
            write_script(&bin.join("claude"), "#!/bin/sh\necho fake-claude\n");
            let shell = fake_shell(dir.path(), &bin);

            // The Finder-launched environment: launchd's PATH only.
            let mut env = env(&[("SHELL", &shell), ("PATH", "/usr/bin:/bin")]);
            let cache = CommandResolutionCache::default();
            let options = CommandAvailabilityOptions::default();
            let lookup = |env: &ShellEnv| {
                crate::shell::resolve_command_path(
                    "claude",
                    &options,
                    &ShellContext {
                        platform: Platform::current(),
                        host_env: env,
                        cache: &cache,
                        now: Instant::now(),
                    },
                )
            };
            assert_eq!(
                lookup(&env),
                None,
                "claude must be invisible before the fix"
            );

            fix_path(
                &mut env,
                FixPathOptions::default(),
                &FixPathHost {
                    platform: Platform::current(),
                    exec_file: &crate::shell::exec_file_sync,
                    user_info: UserInfo::default(),
                },
            );
            assert_eq!(
                env["PATH"],
                format!("{}:/usr/bin:/bin", bin.display()),
                "shell PATH first, inherited entries deduplicated behind it"
            );
            // A new cache: the 30 s TTL would otherwise serve the stale miss.
            let cache = CommandResolutionCache::default();
            let found = crate::shell::resolve_command_path(
                "claude",
                &options,
                &ShellContext {
                    platform: Platform::current(),
                    host_env: &env,
                    cache: &cache,
                    now: Instant::now(),
                },
            );
            assert_eq!(found, Some(bin.join("claude").display().to_string()));
        }

        #[test]
        fn a_shell_that_hangs_is_killed_and_the_path_is_kept() {
            let dir = tempfile::tempdir().unwrap();
            let shell = dir.path().join("hang-shell");
            write_script(&shell, "#!/bin/sh\nsleep 30\n");
            let shell = shell.display().to_string();
            // `exec_file_sync` with the production timeout would take 5 s; the
            // same function with a short deadline shows the kill and the error.
            let started = Instant::now();
            let result =
                crate::shell::exec_file_sync(&shell, &["-ilc", "x"], Duration::from_millis(200));
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);

            let mut env = env(&[("SHELL", &shell), ("PATH", "/usr/bin")]);
            run(
                Platform::Linux,
                FixPathOptions::default(),
                &mut env,
                user(None),
                &|file, args, _| {
                    if file == shell {
                        crate::shell::exec_file_sync(file, args, Duration::from_millis(200))
                    } else {
                        Err(io::Error::other("fallback shell not under test"))
                    }
                },
            );
            assert_eq!(env["PATH"], "/usr/bin");
        }

        #[test]
        fn a_shell_that_exits_non_zero_keeps_the_path() {
            let dir = tempfile::tempdir().unwrap();
            let shell = dir.path().join("fail-shell");
            write_script(&shell, "#!/bin/sh\necho partial\nexit 3\n");
            let shell = shell.display().to_string();
            let mut env = env(&[("SHELL", &shell), ("PATH", "/usr/bin")]);
            run(
                Platform::Linux,
                FixPathOptions::default(),
                &mut env,
                user(None),
                &|file, args, timeout| {
                    if file == shell {
                        crate::shell::exec_file_sync(file, args, timeout)
                    } else {
                        Err(io::Error::other("fallback shell not under test"))
                    }
                },
            );
            assert_eq!(env["PATH"], "/usr/bin");
        }
    }
}
