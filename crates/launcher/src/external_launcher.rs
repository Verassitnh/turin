//! Port of upstream/apps/server/src/process/externalLauncher.ts, macOS
//! branch only. The Windows and Linux launch paths (PowerShell, Explorer,
//! xdg-open, WSL, command shims) are cut; see PORTING.md Deviations.
//!
//! Upstream launches through an injected Effect spawner. Here every spawn
//! goes through [`LaunchHost`], so tests record launches and never start an
//! application.
//!
//! `ExternalLauncherError` is the wire error from turin-contracts. Boxing it
//! here would make the service error differ from the contract enum.
#![allow(
    clippy::result_large_err,
    reason = "the error is the turin-contracts wire enum; boxing it would diverge from the contract"
)]

use std::io;
use std::sync::{Arc, LazyLock};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use regex::Regex;
use turin_contracts::editor::{
    EDITORS, EditorDefinition, EditorId, EditorLaunchStyle, ExternalLauncherBrowserSpawnError,
    ExternalLauncherCommandNotFoundError, ExternalLauncherEditorSpawnError, ExternalLauncherError,
    ExternalLauncherUnknownEditorError, ExternalLauncherUnsupportedEditorError,
    FileManagerRevealKind, LaunchEditorInput,
};

use crate::editor::resolve_editor_command;
use crate::host::{Env, LaunchHost};

/// Upstream `EDITOR_DISCOVERY_CACHE_TTL_NANOS`. Discovery is memoized for this
/// window so repeat client connects skip the PATH and bundle walk.
const EDITOR_DISCOVERY_CACHE_TTL_NANOS: u64 = 60_000_000_000;

const FILE_MANAGER_COMMAND: &str = "open";

/// `target` split into path, line and optional column, as upstream
/// `TARGET_WITH_POSITION_PATTERN`. JS `.` excludes the four line terminators,
/// so the character class spells them out.
static TARGET_WITH_POSITION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^\n\r\u{2028}\u{2029}]*?):([0-9]+)(?::([0-9]+))?$").expect("static pattern")
});

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorLaunch {
    editor: EditorId,
    target: String,
    command: String,
    args: Vec<String>,
}

/// `None` when the scan task itself failed. Otherwise the editors plus the
/// monotonic time the scan ended, which starts the cache window.
type SharedScan = Shared<BoxFuture<'static, Option<(Vec<EditorId>, u64)>>>;

struct DiscoveryEntry {
    generation: u64,
    scan: SharedScan,
    /// `None` while the scan runs; the cache window starts when it finishes.
    expires_at_nanos: Option<u64>,
}

#[derive(Default)]
struct DiscoveryState {
    generation: u64,
    entry: Option<DiscoveryEntry>,
}

/// Editor discovery memo. Every caller awaits the same scan, and the scan
/// runs in its own task, so a caller that goes away mid-scan does not cancel
/// it for the others. A scan that fails clears its entry so the next caller
/// starts over.
#[derive(Default)]
struct DiscoveryCache {
    state: Mutex<DiscoveryState>,
}

impl DiscoveryCache {
    /// Starts the cache window at `finished_at`, or clears the entry when the
    /// scan failed.
    fn finish(&self, generation: u64, finished_at: Option<u64>) {
        let mut state = self.state.lock();
        let Some(entry) = state.entry.as_mut() else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        match finished_at {
            Some(finished_at) => {
                entry.expires_at_nanos = Some(finished_at + EDITOR_DISCOVERY_CACHE_TTL_NANOS);
            }
            None => state.entry = None,
        }
    }
}

/// Reveal kind for a host. Darwin always reveals through Finder (`open -R`).
pub fn file_manager_reveal_kind() -> FileManagerRevealKind {
    FileManagerRevealKind::Finder
}

/// Launches browser URLs and workspace paths in editors on macOS.
pub struct ExternalLauncher {
    host: Arc<dyn LaunchHost>,
    env: Arc<Env>,
    discovery: Arc<DiscoveryCache>,
}

impl ExternalLauncher {
    pub fn new(host: Arc<dyn LaunchHost>, env: Env) -> Self {
        Self {
            host,
            env: Arc::new(env),
            discovery: Arc::new(DiscoveryCache::default()),
        }
    }

    /// Editors whose launcher resolves on this host. Memoized for
    /// [`EDITOR_DISCOVERY_CACHE_TTL_NANOS`].
    pub async fn resolve_available_editors(&self) -> Vec<EditorId> {
        self.acquire_scan()
            .await
            .map(|(editors, _)| editors)
            .unwrap_or_default()
    }

    fn acquire_scan(&self) -> SharedScan {
        let now = self.host.monotonic_nanos();
        let mut state = self.discovery.state.lock();
        if let Some(entry) = &state.entry
            && entry.expires_at_nanos.is_none_or(|expires| expires > now)
        {
            return entry.scan.clone();
        }
        state.generation += 1;
        let generation = state.generation;
        let host = Arc::clone(&self.host);
        let env = Arc::clone(&self.env);
        let discovery = Arc::clone(&self.discovery);
        let task = tokio::task::spawn_blocking(move || {
            let editors = scan_editors(host.as_ref(), &env);
            let finished_at = host.monotonic_nanos();
            // Stamp the window before any caller can see the result, so a
            // lookup right after the scan already sees the memoized set.
            discovery.finish(generation, Some(finished_at));
            (editors, finished_at)
        });
        let scan: SharedScan = async move { task.await.ok() }.boxed().shared();

        // A scan task that panicked never reaches `finish` above; clear its
        // entry so the next caller rescans. Driven here, not by the caller, so
        // it runs even if every caller has gone away.
        let driver = scan.clone();
        let discovery = Arc::clone(&self.discovery);
        tokio::spawn(async move {
            if driver.await.is_none() {
                discovery.finish(generation, None);
            }
        });

        state.entry = Some(DiscoveryEntry {
            generation,
            scan: scan.clone(),
            expires_at_nanos: None,
        });
        scan
    }

    /// Reveal kind for this host. Always available on darwin.
    pub fn resolve_file_manager_reveal_kind(&self) -> Option<FileManagerRevealKind> {
        Some(file_manager_reveal_kind())
    }

    /// Opens a URL in the default browser.
    pub async fn launch_browser(&self, target: &str) -> Result<(), ExternalLauncherError> {
        let args = vec![target.to_owned()];
        self.host
            .spawn_detached("open", &args)
            .await
            .map_err(|cause| {
                ExternalLauncherError::BrowserSpawn(
                    ExternalLauncherBrowserSpawnError::BrowserSpawn {
                        command: "open".to_owned(),
                        args,
                        cause: io_cause(&cause),
                        target: target.to_owned(),
                    },
                )
            })
    }

    /// Opens a workspace path in the selected editor. Starts the editor
    /// detached so the server is not blocked.
    pub async fn launch_editor(
        &self,
        input: &LaunchEditorInput,
    ) -> Result<(), ExternalLauncherError> {
        let launch = self.resolve_editor_launch(input)?;
        self.launch_editor_process(launch).await
    }

    fn resolve_editor_launch(
        &self,
        input: &LaunchEditorInput,
    ) -> Result<EditorLaunch, ExternalLauncherError> {
        let cwd = input.cwd.as_str();
        let editor_def = EDITORS
            .iter()
            .find(|editor| editor.id == input.editor)
            .ok_or_else(|| {
                ExternalLauncherError::UnknownEditor(
                    ExternalLauncherUnknownEditorError::UnknownEditor {
                        editor: input.editor.to_string(),
                    },
                )
            })?;

        if let Some(commands) = editor_def.commands {
            let (command, base_args) =
                match resolve_editor_command(editor_def, self.host.as_ref(), &self.env) {
                    Some(resolved) => (resolved.command, resolved.base_args),
                    // Not found: fall back to the bare command so the launch
                    // reports CommandNotFound with the name the user would type.
                    None => (
                        commands[0].to_owned(),
                        editor_def
                            .base_args
                            .iter()
                            .map(|arg| (*arg).to_owned())
                            .collect(),
                    ),
                };
            let mut args = base_args;
            args.extend(resolve_command_editor_args(editor_def, cwd));
            return Ok(EditorLaunch {
                editor: editor_def.id,
                target: cwd.to_owned(),
                command,
                args,
            });
        }

        if editor_def.id != EditorId::FileManager {
            return Err(ExternalLauncherError::UnsupportedEditor(
                ExternalLauncherUnsupportedEditorError::UnsupportedEditor {
                    editor: input.editor,
                },
            ));
        }
        let command = self.usable_file_manager_command().ok_or_else(|| {
            ExternalLauncherError::UnsupportedEditor(
                ExternalLauncherUnsupportedEditorError::UnsupportedEditor {
                    editor: input.editor,
                },
            )
        })?;

        if input.reveal == Some(true) {
            return Ok(EditorLaunch {
                editor: EditorId::FileManager,
                target: cwd.to_owned(),
                command: command.to_owned(),
                args: vec!["-R".to_owned(), cwd.to_owned()],
            });
        }
        Ok(EditorLaunch {
            editor: EditorId::FileManager,
            target: cwd.to_owned(),
            command: command.to_owned(),
            args: vec![cwd.to_owned()],
        })
    }

    fn usable_file_manager_command(&self) -> Option<&'static str> {
        self.host
            .is_command_available(FILE_MANAGER_COMMAND, &self.env)
            .then_some(FILE_MANAGER_COMMAND)
    }

    async fn launch_editor_process(
        &self,
        launch: EditorLaunch,
    ) -> Result<(), ExternalLauncherError> {
        if !self.host.is_command_available(&launch.command, &self.env) {
            return Err(ExternalLauncherError::CommandNotFound(
                ExternalLauncherCommandNotFoundError::CommandNotFound {
                    editor: launch.editor,
                    command: launch.command,
                },
            ));
        }
        self.host
            .spawn_detached(&launch.command, &launch.args)
            .await
            .map_err(|cause| {
                ExternalLauncherError::EditorSpawn(ExternalLauncherEditorSpawnError::EditorSpawn {
                    command: launch.command.clone(),
                    args: launch.args.clone(),
                    cause: io_cause(&cause),
                    editor: launch.editor,
                    target: launch.target.clone(),
                })
            })
    }
}

/// Discovery scan: every editor whose launcher resolves, in `EDITORS` order.
fn scan_editors(host: &dyn LaunchHost, env: &Env) -> Vec<EditorId> {
    EDITORS
        .iter()
        .filter(|editor| match editor.commands {
            None => host.is_command_available(FILE_MANAGER_COMMAND, env),
            Some(_) => resolve_editor_command(editor, host, env).is_some(),
        })
        .map(|editor| editor.id)
        .collect()
}

/// Upstream `resolveCommandEditorArgs` for a resolved command.
fn resolve_command_editor_args(editor: &EditorDefinition, target: &str) -> Vec<String> {
    let parsed = parse_target_path_and_position(target);
    match editor.launch_style {
        EditorLaunchStyle::DirectPath => vec![target.to_owned()],
        EditorLaunchStyle::Goto => match parsed {
            Some(_) => vec!["--goto".to_owned(), target.to_owned()],
            None => vec![target.to_owned()],
        },
        EditorLaunchStyle::LineColumn => match parsed {
            None => vec![target.to_owned()],
            Some(position) => {
                let mut args = vec!["--line".to_owned(), position.line];
                if let Some(column) = position.column {
                    args.push("--column".to_owned());
                    args.push(column);
                }
                args.push(position.path);
                args
            }
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TargetPathAndPosition {
    path: String,
    line: String,
    column: Option<String>,
}

fn parse_target_path_and_position(target: &str) -> Option<TargetPathAndPosition> {
    let captures = TARGET_WITH_POSITION_PATTERN.captures(target)?;
    let path = captures.get(1)?.as_str();
    if path.is_empty() {
        return None;
    }
    Some(TargetPathAndPosition {
        path: path.to_owned(),
        line: captures.get(2)?.as_str().to_owned(),
        column: captures.get(3).map(|column| column.as_str().to_owned()),
    })
}

/// The error `cause` field: upstream carries the Effect error, which the wire
/// encodes as a value. The message is what a client can show.
fn io_cause(cause: &io::Error) -> serde_json::Value {
    serde_json::Value::String(cause.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex as StdMutex, mpsc};
    use std::time::Duration;

    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    use super::*;
    use crate::host::test_support::{FakeHost, Spawn};

    fn write_executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn env_for(home: &Path, path: &str) -> Env {
        Env::from([
            ("HOME".to_owned(), home.display().to_string()),
            ("PATH".to_owned(), path.to_owned()),
        ])
    }

    fn launcher(host: &Arc<FakeHost>, env: Env) -> ExternalLauncher {
        ExternalLauncher::new(Arc::clone(host) as Arc<dyn LaunchHost>, env)
    }

    fn input(editor: EditorId, cwd: &str) -> LaunchEditorInput {
        LaunchEditorInput {
            cwd: TrimmedNonEmptyString::new(cwd).unwrap(),
            editor,
            reveal: None,
        }
    }

    fn spawn(command: &str, args: &[&str]) -> Spawn {
        Spawn {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn launches_the_default_browser_through_open() {
        let host = Arc::new(FakeHost::new());
        launcher(&host, Env::new())
            .launch_browser("https://example.com/some path")
            .await
            .unwrap();

        assert_eq!(
            host.last_spawn(),
            Some(spawn("open", &["https://example.com/some path"]))
        );
    }

    #[tokio::test]
    async fn launches_cursor_in_classic_ide_mode_on_macos() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        write_executable(&bin.join("cursor"));
        let host = Arc::new(FakeHost::new());
        let launcher = launcher(&host, env_for(home.path(), bin.to_str().unwrap()));

        for cwd in [
            "/workspace with spaces",
            "/workspace with spaces/src/index.ts",
            "/workspace with spaces/src/index.ts:12",
            "/workspace with spaces/src/index.ts:12:4",
        ] {
            launcher
                .launch_editor(&input(EditorId::Cursor, cwd))
                .await
                .unwrap();
        }

        assert_eq!(
            host.spawned(),
            vec![
                spawn("cursor", &["--classic", "/workspace with spaces"]),
                spawn(
                    "cursor",
                    &["--classic", "/workspace with spaces/src/index.ts"]
                ),
                spawn(
                    "cursor",
                    &[
                        "--classic",
                        "--goto",
                        "/workspace with spaces/src/index.ts:12"
                    ]
                ),
                spawn(
                    "cursor",
                    &[
                        "--classic",
                        "--goto",
                        "/workspace with spaces/src/index.ts:12:4"
                    ]
                ),
            ]
        );
    }

    #[tokio::test]
    async fn discovers_and_launches_editors_outside_path_on_macos() {
        let cases: Vec<(EditorId, &str, Vec<&str>)> = vec![
            (
                EditorId::Antigravity,
                "Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide",
                vec!["--goto", "/workspace with spaces/file.ts:12:4"],
            ),
            (
                EditorId::Cursor,
                "Applications/Cursor.app/Contents/Resources/app/bin/code",
                vec!["--classic", "--goto", "/workspace with spaces/file.ts:12:4"],
            ),
            (
                EditorId::Kiro,
                "Applications/Kiro.app/Contents/Resources/app/bin/code",
                vec!["--goto", "/workspace with spaces/file.ts:12:4"],
            ),
            (
                EditorId::Webstorm,
                "Applications/WebStorm.app/Contents/MacOS/webstorm",
                vec![
                    "--line",
                    "12",
                    "--column",
                    "4",
                    "/workspace with spaces/file.ts",
                ],
            ),
            (
                EditorId::Zed,
                "Applications/Zed.app/Contents/MacOS/cli",
                vec!["/workspace with spaces/file.ts:12:4"],
            ),
        ];
        for (editor, install_path, args) in cases {
            let home = tempfile::tempdir().unwrap();
            let executable = home.path().join(install_path);
            write_executable(&executable);
            let host = Arc::new(FakeHost::new());
            let launcher = launcher(
                &host,
                env_for(home.path(), home.path().join("empty").to_str().unwrap()),
            );

            assert!(launcher.resolve_available_editors().await.contains(&editor));
            launcher
                .launch_editor(&input(editor, "/workspace with spaces/file.ts:12:4"))
                .await
                .unwrap();

            let executable = executable.to_str().unwrap();
            assert_eq!(
                host.last_spawn(),
                Some(Spawn {
                    command: executable.to_owned(),
                    args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                }),
                "{editor}"
            );
        }
    }

    #[tokio::test]
    async fn zed_prefers_the_macos_bundle_over_a_path_entry() {
        for cwd in [
            "/workspace with spaces/",
            "/workspace with spaces/file.ts:12:4",
        ] {
            let home = tempfile::tempdir().unwrap();
            let executable = home.path().join("Applications/Zed.app/Contents/MacOS/cli");
            let bin_dir = home.path().join("zfs/bin");
            write_executable(&executable);
            write_executable(&bin_dir.join("zed"));
            let host = Arc::new(FakeHost::new());
            let launcher = launcher(&host, env_for(home.path(), bin_dir.to_str().unwrap()));

            assert!(
                launcher
                    .resolve_available_editors()
                    .await
                    .contains(&EditorId::Zed)
            );
            launcher
                .launch_editor(&input(EditorId::Zed, cwd))
                .await
                .unwrap();

            assert_eq!(
                host.last_spawn(),
                Some(Spawn {
                    command: executable.to_str().unwrap().to_owned(),
                    args: vec![cwd.to_owned()],
                })
            );
        }
    }

    #[tokio::test]
    async fn discovers_and_launches_versioned_jetbrains_bundles() {
        for (editor, install_path) in [
            (
                EditorId::Webstorm,
                "Applications/WebStorm 2026.2.app/Contents/MacOS/webstorm",
            ),
            (
                EditorId::Idea,
                "Applications/IntelliJ IDEA 2026.1.4.app/Contents/MacOS/idea",
            ),
        ] {
            let home = tempfile::tempdir().unwrap();
            let executable = home.path().join(install_path);
            write_executable(&executable);
            let host = Arc::new(FakeHost::new());
            let launcher = launcher(
                &host,
                env_for(home.path(), home.path().join("empty").to_str().unwrap()),
            );

            assert!(launcher.resolve_available_editors().await.contains(&editor));
            launcher
                .launch_editor(&input(editor, "/workspace with spaces/file.ts:12:4"))
                .await
                .unwrap();

            assert_eq!(
                host.last_spawn(),
                Some(Spawn {
                    command: executable.to_str().unwrap().to_owned(),
                    args: [
                        "--line",
                        "12",
                        "--column",
                        "4",
                        "/workspace with spaces/file.ts"
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                })
            );
        }
    }

    #[tokio::test]
    async fn matches_macos_bundles_on_the_name_boundary_only() {
        let unrelated_home = tempfile::tempdir().unwrap();
        write_executable(
            &unrelated_home
                .path()
                .join("Applications/WebStormHelper.app/Contents/MacOS/webstorm"),
        );
        let host = Arc::new(FakeHost::new());
        let unrelated = launcher(
            &host,
            env_for(
                unrelated_home.path(),
                unrelated_home.path().join("bin").to_str().unwrap(),
            ),
        );
        assert!(
            !unrelated
                .resolve_available_editors()
                .await
                .contains(&EditorId::Webstorm)
        );

        let exact_home = tempfile::tempdir().unwrap();
        write_executable(
            &exact_home
                .path()
                .join("Applications/WebStorm.app/Contents/MacOS/webstorm"),
        );
        let exact = launcher(
            &host,
            env_for(
                exact_home.path(),
                exact_home.path().join("bin").to_str().unwrap(),
            ),
        );
        assert!(
            exact
                .resolve_available_editors()
                .await
                .contains(&EditorId::Webstorm)
        );
    }

    #[tokio::test]
    async fn keeps_the_macos_vs_code_insiders_bundle_out_of_stable_vs_code() {
        let home = tempfile::tempdir().unwrap();
        let bin = home
            .path()
            .join("Applications/Visual Studio Code - Insiders.app/Contents/Resources/app/bin");
        // The real Insiders bundle ships both `code-insiders` and `code`.
        write_executable(&bin.join("code"));
        write_executable(&bin.join("code-insiders"));
        let host = Arc::new(FakeHost::new());
        let launcher = launcher(
            &host,
            env_for(home.path(), home.path().join("empty").to_str().unwrap()),
        );

        let editors = launcher.resolve_available_editors().await;
        assert!(!editors.contains(&EditorId::Vscode));
        assert!(editors.contains(&EditorId::VscodeInsiders));

        launcher
            .launch_editor(&input(EditorId::VscodeInsiders, "/workspace/file.ts"))
            .await
            .unwrap();
        assert_eq!(
            host.last_spawn().map(|spawn| spawn.command),
            Some(bin.join("code-insiders").to_str().unwrap().to_owned())
        );
    }

    #[tokio::test]
    async fn does_not_report_the_agy_cli_as_the_antigravity_ide() {
        let home = tempfile::tempdir().unwrap();
        write_executable(&home.path().join(".local/bin/agy"));
        let host = Arc::new(FakeHost::new());
        let launcher = launcher(
            &host,
            env_for(
                home.path(),
                home.path().join(".local/bin").to_str().unwrap(),
            ),
        );

        assert!(
            !launcher
                .resolve_available_editors()
                .await
                .contains(&EditorId::Antigravity)
        );
    }

    #[tokio::test]
    async fn ignores_unusable_app_bundles_and_keeps_path_launchers_first() {
        let home = tempfile::tempdir().unwrap();
        let executable = home
            .path()
            .join("Applications/Cursor.app/Contents/Resources/app/bin/code");
        let bin = home.path().join("bin");
        let host = Arc::new(FakeHost::new());
        let env = env_for(home.path(), bin.to_str().unwrap());

        let before = launcher(&host, env.clone())
            .resolve_available_editors()
            .await;
        // A directory where the launcher should be.
        fs::create_dir_all(&executable).unwrap();
        assert_eq!(
            launcher(&host, env.clone())
                .resolve_available_editors()
                .await,
            before
        );
        // A launcher without the execute bit.
        fs::remove_dir_all(&executable).unwrap();
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            launcher(&host, env.clone())
                .resolve_available_editors()
                .await,
            before
        );

        // A cursor on PATH wins, and its bare name is what gets spawned.
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        write_executable(&bin.join("cursor"));
        let with_path = Env::from([
            ("PATH".to_owned(), format!("{}:/usr/bin", bin.display())),
            ("HOME".to_owned(), home.path().display().to_string()),
        ]);
        launcher(&host, with_path)
            .launch_editor(&input(EditorId::Cursor, "/workspace"))
            .await
            .unwrap();
        assert_eq!(
            host.last_spawn().map(|spawn| spawn.command),
            Some("cursor".to_owned())
        );
    }

    #[tokio::test]
    async fn passes_line_breaks_through_to_editors_started_without_a_shell() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        write_executable(&bin.join("zed"));
        let host = Arc::new(FakeHost::new());
        launcher(&host, env_for(home.path(), bin.to_str().unwrap()))
            .launch_editor(&input(EditorId::Zed, "/workspace/odd\nname.ts"))
            .await
            .unwrap();

        assert_eq!(
            host.last_spawn(),
            Some(Spawn {
                command: "zed".to_owned(),
                args: vec!["/workspace/odd\nname.ts".to_owned()],
            })
        );
    }

    #[tokio::test]
    async fn file_manager_is_offered_and_reveals_through_open_when_available() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        write_executable(&bin.join("open"));
        let host = Arc::new(FakeHost::new());
        let launcher = launcher(&host, env_for(home.path(), bin.to_str().unwrap()));

        assert!(
            launcher
                .resolve_available_editors()
                .await
                .contains(&EditorId::FileManager)
        );
        assert_eq!(
            launcher.resolve_file_manager_reveal_kind(),
            Some(FileManagerRevealKind::Finder)
        );

        launcher
            .launch_editor(&input(EditorId::FileManager, "/workspace/file.ts"))
            .await
            .unwrap();
        assert_eq!(
            host.last_spawn(),
            Some(spawn("open", &["/workspace/file.ts"]))
        );

        let reveal = LaunchEditorInput {
            reveal: Some(true),
            ..input(EditorId::FileManager, "/workspace/file.ts")
        };
        launcher.launch_editor(&reveal).await.unwrap();
        assert_eq!(
            host.last_spawn(),
            Some(spawn("open", &["-R", "/workspace/file.ts"]))
        );
    }

    #[tokio::test]
    async fn missing_editor_command_is_reported_without_spawning() {
        let home = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::new());
        let error = launcher(
            &host,
            env_for(home.path(), home.path().join("empty").to_str().unwrap()),
        )
        .launch_editor(&input(EditorId::Cursor, "/workspace"))
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ExternalLauncherError::CommandNotFound(
                ExternalLauncherCommandNotFoundError::CommandNotFound {
                    editor: EditorId::Cursor,
                    ..
                }
            )
        ));
        assert!(host.spawned().is_empty());
    }

    #[tokio::test]
    async fn memoizes_editor_discovery_and_refreshes_after_the_cache_window() {
        let home = tempfile::tempdir().unwrap();
        write_executable(
            &home
                .path()
                .join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
        );
        let host = Arc::new(FakeHost::new());
        let launcher = launcher(
            &host,
            env_for(home.path(), home.path().join("empty").to_str().unwrap()),
        );

        let first = launcher.resolve_available_editors().await;
        assert!(first.contains(&EditorId::Vscode));
        let checks_after_first_scan = host.availability_checks.load(Ordering::SeqCst);
        assert!(checks_after_first_scan > 0);

        // Within the cache window the memoized set is reused without a scan.
        host.advance(Duration::from_secs(31));
        assert_eq!(launcher.resolve_available_editors().await, first);
        assert_eq!(
            host.availability_checks.load(Ordering::SeqCst),
            checks_after_first_scan
        );

        // Past the window the next call rescans.
        host.advance(Duration::from_secs(30));
        launcher.resolve_available_editors().await;
        assert!(host.availability_checks.load(Ordering::SeqCst) > checks_after_first_scan);
    }

    #[tokio::test]
    async fn keeps_scanning_after_the_caller_is_interrupted_and_shares_that_scan() {
        let home = tempfile::tempdir().unwrap();
        write_executable(
            &home
                .path()
                .join("Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
        );
        let host = Arc::new(FakeHost::new());
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = StdMutex::new(release_rx);
        let parked = AtomicBool::new(false);
        let scans_started = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&scans_started);
        // The first availability check of the first scan parks until released.
        *host.on_availability.lock().unwrap() = Some(Box::new(move |_| {
            if !parked.swap(true, Ordering::SeqCst) {
                counter.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
        }));
        let launcher = Arc::new(launcher(
            &host,
            env_for(home.path(), home.path().join("empty").to_str().unwrap()),
        ));

        let interrupted = tokio::spawn({
            let launcher = Arc::clone(&launcher);
            async move { launcher.resolve_available_editors().await }
        });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        interrupted.abort();
        assert!(interrupted.await.unwrap_err().is_cancelled());

        // The next connect joins the scan that is still running.
        let next = tokio::spawn({
            let launcher = Arc::clone(&launcher);
            async move { launcher.resolve_available_editors().await }
        });
        release_tx.send(()).unwrap();
        let editors = next.await.unwrap();

        assert!(editors.contains(&EditorId::Vscode));
        assert_eq!(scans_started.load(Ordering::SeqCst), 1);
        assert_eq!(launcher.discovery.state.lock().generation, 1);
    }
}
