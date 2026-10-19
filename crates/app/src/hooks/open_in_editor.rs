//! Port of the launching half of upstream/apps/web/src/editorPreferences.ts
//! (`useOpenInPreferredEditor`, `usePreferredEditor`) over
//! `turin-launcher`'s `ExternalLauncher` (upstream `shell.openInEditor` and
//! the server's available-editor discovery).
//!
//! One local environment, so the per-environment checks are cut (see
//! `turin_launcher::editor_preferences`). The last-used editor lives in a
//! [`LocalStorage`] under upstream's `t3code:last-editor` key. The launcher
//! spawns on tokio, so callers run these futures on the app's runtime handle
//! ([`EditorLauncherService::spawn_open`]); tests inject a recording
//! `LaunchHost` and never start a real editor.

use std::sync::{Arc, OnceLock};

use gpui_kit::{App, Task};
use tokio::runtime::Handle;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::editor::{EditorId, LaunchEditorInput};
use turin_launcher::editor_preferences::{LAST_EDITOR_KEY, resolve_open_in_preferred_editor};
use turin_launcher::external_launcher::ExternalLauncher;
use turin_launcher::host::{LaunchHost, SystemHost, process_env};

use super::use_local_storage::{FileLocalStorage, SharedLocalStorage};

pub struct EditorLauncherService {
    launcher: ExternalLauncher,
    storage: SharedLocalStorage,
}

impl EditorLauncherService {
    pub fn new(
        host: Arc<dyn LaunchHost>,
        env: turin_launcher::host::Env,
        storage: SharedLocalStorage,
    ) -> Self {
        Self {
            launcher: ExternalLauncher::new(host, env),
            storage,
        }
    }

    /// The real macOS launcher; the last editor persists in the data directory.
    pub fn global() -> Arc<Self> {
        static SERVICE: OnceLock<Arc<EditorLauncherService>> = OnceLock::new();
        SERVICE
            .get_or_init(|| {
                Arc::new(Self::new(
                    Arc::new(SystemHost),
                    process_env(),
                    FileLocalStorage::shared_in_data_dir("local-storage.json"),
                ))
            })
            .clone()
    }

    /// `primaryServerAvailableEditorsAtom`: editors that resolve on this host.
    pub async fn available_editors(&self) -> Vec<EditorId> {
        self.launcher.resolve_available_editors().await
    }

    /// The stored last-used editor (`getLocalStorageItem(LAST_EDITOR_KEY, EditorId)`).
    pub fn stored_editor(&self) -> Option<EditorId> {
        let raw = self.storage.get_item(LAST_EDITOR_KEY).ok()??;
        serde_json::from_str(&raw).ok()
    }

    /// `setLocalStorageItem(LAST_EDITOR_KEY, editor)`.
    pub fn set_preferred_editor(&self, editor: EditorId) {
        if let Ok(encoded) = serde_json::to_string(&editor) {
            let _ = self.storage.set_item(LAST_EDITOR_KEY, &encoded);
        }
    }

    /// `openInEditor` with an explicit editor.
    pub async fn open_in(&self, target_path: &str, editor: EditorId) -> Result<(), String> {
        let cwd = TrimmedNonEmptyString::new(target_path)
            .map_err(|_| format!("Cannot open an empty path ({target_path:?})."))?;
        self.launcher
            .launch_editor(&LaunchEditorInput {
                cwd,
                editor,
                reveal: None,
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// `useOpenInPreferredEditor`: resolves (and persists) the preferred
    /// editor, then opens `target_path` in it.
    pub async fn open_in_preferred_editor(&self, target_path: &str) -> Result<EditorId, String> {
        let available = self.available_editors().await;
        let choice =
            resolve_open_in_preferred_editor(target_path, &available, self.stored_editor())
                .map_err(|error| error.to_string())?;
        if choice.save {
            self.set_preferred_editor(choice.editor);
        }
        self.open_in(target_path, choice.editor).await?;
        Ok(choice.editor)
    }

    /// An explicit pick from the "Open in" menu or its primary button: opens
    /// the target and remembers the editor (`setPreferredEditor`).
    pub async fn open_and_remember(
        &self,
        target_path: &str,
        editor: EditorId,
    ) -> Result<(), String> {
        let opened = self.open_in(target_path, editor).await;
        self.set_preferred_editor(editor);
        opened
    }

    /// [`Self::available_editors`] as a GPUI task.
    pub fn spawn_available(
        self: &Arc<Self>,
        runtime: &Handle,
        cx: &mut App,
    ) -> Task<Vec<EditorId>> {
        let service = Arc::clone(self);
        let (done, outcome) = futures::channel::oneshot::channel();
        runtime.spawn(async move {
            let _ = done.send(service.available_editors().await);
        });
        cx.spawn(async move |_| outcome.await.unwrap_or_default())
    }

    /// [`Self::open_and_remember`] as a GPUI task.
    pub fn spawn_open_in(
        self: &Arc<Self>,
        runtime: &Handle,
        target_path: String,
        editor: EditorId,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let service = Arc::clone(self);
        let (done, outcome) = futures::channel::oneshot::channel();
        runtime.spawn(async move {
            let _ = done.send(service.open_and_remember(&target_path, editor).await);
        });
        cx.spawn(async move |_| {
            outcome
                .await
                .unwrap_or_else(|_| Err("The editor launch was interrupted.".to_owned()))
        })
    }

    /// Runs [`Self::open_in_preferred_editor`] on `runtime` and hands the
    /// result back as a GPUI task.
    pub fn spawn_open(
        self: &Arc<Self>,
        runtime: &Handle,
        target_path: String,
        cx: &mut App,
    ) -> Task<Result<EditorId, String>> {
        let service = Arc::clone(self);
        let (done, outcome) = futures::channel::oneshot::channel();
        runtime.spawn(async move {
            let _ = done.send(service.open_in_preferred_editor(&target_path).await);
        });
        cx.spawn(async move |_| {
            outcome
                .await
                .unwrap_or_else(|_| Err("The editor launch was interrupted.".to_owned()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use turin_launcher::host::Env;

    use super::*;
    use crate::hooks::use_local_storage::MemoryLocalStorage;

    /// A host that records spawns and only knows the commands it is given:
    /// no editor is ever started.
    #[derive(Default)]
    struct FakeHost {
        commands: Vec<&'static str>,
        spawns: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl LaunchHost for FakeHost {
        fn monotonic_nanos(&self) -> u64 {
            0
        }
        fn is_command_available(&self, command: &str, _: &Env) -> bool {
            self.commands.contains(&command)
        }
        fn read_directory(&self, _: &Path) -> Vec<String> {
            Vec::new()
        }
        fn application_roots(&self, _: &Env) -> Vec<PathBuf> {
            Vec::new()
        }
        async fn spawn_detached(&self, command: &str, args: &[String]) -> io::Result<()> {
            self.spawns
                .lock()
                .unwrap()
                .push((command.to_owned(), args.to_vec()));
            Ok(())
        }
        async fn command_output(&self, _: &str, _: &[String], _: Duration) -> io::Result<String> {
            Ok(String::new())
        }
    }

    fn service(commands: Vec<&'static str>) -> (Arc<FakeHost>, EditorLauncherService) {
        let host = Arc::new(FakeHost {
            commands,
            ..FakeHost::default()
        });
        let service =
            EditorLauncherService::new(host.clone(), Env::new(), MemoryLocalStorage::shared());
        (host, service)
    }

    #[tokio::test]
    async fn opens_the_target_in_the_first_available_editor_and_remembers_it() {
        let (host, service) = service(vec!["zed"]);
        let editor = service
            .open_in_preferred_editor("/work/app/src/main.rs")
            .await
            .unwrap();
        assert_eq!(editor, EditorId::Zed);
        assert_eq!(service.stored_editor(), Some(EditorId::Zed));
        let spawns = host.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].0, "zed");
        assert!(spawns[0].1.contains(&"/work/app/src/main.rs".to_owned()));
    }

    #[tokio::test]
    async fn a_stored_available_editor_wins_over_editor_order() {
        let (host, service) = service(vec!["code", "zed"]);
        service.set_preferred_editor(EditorId::Zed);
        let editor = service.open_in_preferred_editor("/work/app").await.unwrap();
        assert_eq!(editor, EditorId::Zed);
        assert_eq!(host.spawns.lock().unwrap()[0].0, "zed");
    }

    #[tokio::test]
    async fn no_available_editor_reports_upstreams_message_and_spawns_nothing() {
        let (host, service) = service(Vec::new());
        let error = service
            .open_in_preferred_editor("/work/app/a.rs")
            .await
            .unwrap_err();
        assert_eq!(error, "No available editor can open /work/app/a.rs.");
        assert!(host.spawns.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn picking_an_editor_opens_it_and_makes_it_the_preference() {
        let (host, service) = service(vec!["code", "zed"]);
        service
            .open_and_remember("/work/app", EditorId::Zed)
            .await
            .unwrap();
        assert_eq!(service.stored_editor(), Some(EditorId::Zed));
        assert_eq!(host.spawns.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_blank_target_is_refused_before_launching() {
        let (host, service) = service(vec!["zed"]);
        assert!(service.open_in("  ", EditorId::Zed).await.is_err());
        assert!(host.spawns.lock().unwrap().is_empty());
    }
}
