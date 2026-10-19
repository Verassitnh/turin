//! Provider update notifications in the shell: the launch toast
//! (`ProviderUpdatePrimaryNotification`) and the sidebar pill
//! (`SidebarProviderUpdatePill`), both fed from the provider status feed
//! (`CommandBus::providers`).
//!
//! The pure decisions live in `components::provider_update_*`; this host owns
//! the session state (seen keys, the open toast, the visible pill) and the
//! persisted dismissals (`providerUpdateDismissal.ts`, a JSON file in the
//! data dir). The per-environment flow (a local WSL secondary) is not ported:
//! Turin has one local environment, so the primary flow always applies.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Context, Entity, Task, WeakEntity};
use turin_provider_core::server_provider::ServerProvider;
use turin_providers::provider_maintenance_runner::ProviderUpdateTarget;

use crate::components::provider_update_dismissal::{
    PROVIDER_UPDATE_DISMISSALS_STORAGE_KEY, ProviderUpdateDismissals,
    ProviderUpdateDismissalsStore, dismiss_provider_update_notification,
};
use crate::components::provider_update_launch_notification_logic::{
    ProviderUpdateSidebarPillOptions, ProviderUpdateSidebarPillView, ProviderUpdateToastType,
    ProviderUpdateToastView, can_one_click_update_provider_candidate,
    collect_provider_update_candidates, get_provider_update_rejected_toast_view,
    get_provider_update_sidebar_pill_view, provider_update_notification_key,
};
use crate::components::provider_update_primary_notification::{
    PrimaryProviderUpdateToastAction, primary_provider_update_toast_action,
};
use crate::components::sidebar::Sidebar;
use crate::shell::live_settings::{LiveShellHandle, open_provider_settings};
use crate::ui::IconName;
use crate::ui::toast::{ThreadToastData, ToastActionSpec, ToastId, ToastOptions, toast_manager};
use crate::ui::toast_logic::{ToastActionVariant, ToastType};

/// Dismissals persisted as `{ "keys": [...] }` next to the other app state.
pub struct FileProviderUpdateDismissalsStore {
    path: Option<PathBuf>,
}

impl FileProviderUpdateDismissalsStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// `<data dir>/provider-update-dismissals.json`; unpersisted without a data dir.
    pub fn in_data_dir() -> Self {
        Self::new(
            crate::app_runtime::data_dir::resolve_data_dir()
                .map(|dir| dir.join("provider-update-dismissals.json")),
        )
    }
}

impl ProviderUpdateDismissalsStore for FileProviderUpdateDismissalsStore {
    fn read(&self) -> ProviderUpdateDismissals {
        self.path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write(&mut self, document: ProviderUpdateDismissals) {
        let Some(path) = &self.path else {
            return;
        };
        let result = serde_json::to_string(&document)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(path, text));
        if let Err(error) = result {
            tracing::warn!(
                %error,
                key = PROVIDER_UPDATE_DISMISSALS_STORAGE_KEY,
                "could not persist provider update dismissals"
            );
        }
    }
}

struct Prompt {
    toast_id: ToastId,
    key: String,
    /// Instances the "Update" button updates, in candidate order.
    updates: Vec<ProviderUpdateTarget>,
}

pub struct ProviderUpdateHost {
    live: LiveShellHandle,
    sidebar: WeakEntity<Sidebar>,
    providers: std::sync::Arc<Vec<ServerProvider>>,
    dismissals: Box<dyn ProviderUpdateDismissalsStore>,
    /// Pill keys the user closed (or that timed out) this session.
    dismissed_pills: Vec<String>,
    /// Hides terminal update states from before this session.
    visible_after_iso: Option<String>,
    seen_keys: HashSet<String>,
    prompt: Option<Prompt>,
    pill: Option<ProviderUpdateSidebarPillView>,
    _pill_timer: Option<Task<()>>,
    _watch: Task<()>,
}

impl ProviderUpdateHost {
    pub fn new(
        live: LiveShellHandle,
        sidebar: WeakEntity<Sidebar>,
        dismissals: Box<dyn ProviderUpdateDismissalsStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut feed = live.bus.providers();
        let watch = cx.spawn(async move |this, cx| {
            loop {
                let providers = feed.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| this.apply(providers, cx))
                    .is_err()
                {
                    break;
                }
                if feed.changed().await.is_err() {
                    break;
                }
            }
        });
        Self {
            live,
            sidebar,
            providers: Default::default(),
            dismissals,
            dismissed_pills: Vec::new(),
            visible_after_iso: None,
            seen_keys: HashSet::new(),
            prompt: None,
            pill: None,
            _pill_timer: None,
            _watch: watch,
        }
    }

    fn apply(&mut self, providers: std::sync::Arc<Vec<ServerProvider>>, cx: &mut Context<Self>) {
        self.providers = providers;
        if self.visible_after_iso.is_none() {
            self.visible_after_iso = self
                .providers
                .iter()
                .map(|provider| provider.checked_at.clone())
                .max();
        }
        self.refresh_pill(cx);
        self.refresh_prompt(cx);
    }

    // ---- sidebar pill ----------------------------------------------------

    fn refresh_pill(&mut self, cx: &mut Context<Self>) {
        let view = get_provider_update_sidebar_pill_view(
            &self.providers,
            &ProviderUpdateSidebarPillOptions {
                visible_after_iso: self.visible_after_iso.as_deref(),
                dismissed_keys: &self.dismissed_pills,
            },
        );
        if view == self.pill {
            return;
        }
        self.pill = view.clone();
        self._pill_timer = None;
        if let Some(view) = view.as_ref()
            && let Some(visible_ms) = view.dismiss_after_visible_ms
        {
            let key = view.key.clone();
            self._pill_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(visible_ms))
                    .await;
                this.update(cx, |this, cx| this.dismiss_pill(&key, cx)).ok();
            }));
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_provider_update_pill(view, cx))
            .ok();
    }

    /// Hides the pill for `key` until the provider status changes.
    pub fn dismiss_pill(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self
            .dismissed_pills
            .iter()
            .any(|dismissed| dismissed == key)
        {
            self.dismissed_pills.push(key.to_owned());
        }
        self.refresh_pill(cx);
    }

    // ---- launch toast ----------------------------------------------------

    fn refresh_prompt(&mut self, cx: &mut Context<Self>) {
        let candidates = collect_provider_update_candidates(&self.providers);
        let key = provider_update_notification_key(&candidates);

        // The offer changed (or cleared): drop a prompt nobody acted on.
        if let Some(prompt) = &self.prompt
            && key.as_deref() != Some(prompt.key.as_str())
        {
            let toast_id = prompt.toast_id;
            self.prompt = None;
            close_toast(toast_id, cx);
        }
        let Some(key) = key else {
            return;
        };
        if self.prompt.is_some() {
            return;
        }
        let Some(PrimaryProviderUpdateToastAction::ShowInitial(view)) =
            primary_provider_update_toast_action(
                &self.providers,
                &mut *self.dismissals,
                Some(&key),
                self.seen_keys.contains(&key),
            )
        else {
            return;
        };
        self.seen_keys.insert(key.clone());
        let updates: Vec<ProviderUpdateTarget> = candidates
            .iter()
            .copied()
            .filter(|candidate| can_one_click_update_provider_candidate(candidate, &self.providers))
            .map(|candidate| ProviderUpdateTarget {
                provider: candidate.driver.clone(),
                instance_id: Some(candidate.instance_id.clone()),
                target_version: None,
            })
            .collect();
        let host = cx.entity().downgrade();
        let settings = {
            let host = host.clone();
            ToastActionSpec::new("provider-update-settings", "Settings").on_click(move |_, cx| {
                host.update(cx, |this, cx| this.open_settings_from_prompt(cx))
                    .ok();
            })
        };
        let mut data = ThreadToastData {
            hide_copy_button: true,
            leading_icon: Some(IconName::Download),
            action_variant: Some(ToastActionVariant::Outline),
            on_close: Some(Rc::new({
                let host = host.clone();
                move |cx| {
                    host.update(cx, |this, cx| this.dismiss_prompt(cx)).ok();
                }
            })),
            ..Default::default()
        };
        let primary = if updates.is_empty() {
            settings
        } else {
            data.secondary_action = Some(settings);
            data.secondary_action_variant = Some(ToastActionVariant::Outline);
            let host = host.clone();
            ToastActionSpec::new("provider-update-run", "Update").on_click(move |_, cx| {
                host.update(cx, |this, cx| this.run_updates(cx)).ok();
            })
        };
        let options = ToastOptions::new(view.title.clone())
            .description(view.description.clone())
            .type_(toast_type(view.toast_type))
            .timeout(0)
            .action(primary)
            .data(data);
        let toast_id = toast_manager(cx).update(cx, |store, cx| store.add(options, cx));
        self.prompt = Some(Prompt {
            toast_id,
            key,
            updates,
        });
    }

    /// The user closed the prompt with the corner button: do not offer this set again.
    fn dismiss_prompt(&mut self, _cx: &mut Context<Self>) {
        if let Some(prompt) = self.prompt.take() {
            dismiss_provider_update_notification(&mut *self.dismissals, Some(&prompt.key));
        }
    }

    /// Closes the prompt and remembers its set as handled.
    fn take_prompt(&mut self, cx: &mut Context<Self>) -> Option<Prompt> {
        let prompt = self.prompt.take()?;
        dismiss_provider_update_notification(&mut *self.dismissals, Some(&prompt.key));
        close_toast(prompt.toast_id, cx);
        Some(prompt)
    }

    fn open_settings_from_prompt(&mut self, cx: &mut Context<Self>) {
        self.take_prompt(cx);
        self.open_settings(cx);
    }

    pub fn open_settings(&self, cx: &mut Context<Self>) {
        open_provider_settings(&self.live, None, cx);
    }

    /// The prompt's "Update" button: runs the one-click updates in order. The
    /// pill follows their progress through the provider feed.
    fn run_updates(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.take_prompt(cx) else {
            return;
        };
        let Some(services) = self.live.try_services(cx) else {
            return;
        };
        let count = prompt.updates.len();
        let maintenance = services.provider_maintenance.clone();
        let (done, outcome) = futures::channel::oneshot::channel();
        self.live.bus.handle().spawn(async move {
            let mut first_failure = None;
            for target in prompt.updates {
                if let Err(error) = maintenance.update_provider(target).await {
                    first_failure.get_or_insert(error.to_string());
                }
            }
            let _ = done.send(first_failure);
        });
        cx.spawn(async move |_, cx| {
            if let Ok(Some(message)) = outcome.await {
                let view = get_provider_update_rejected_toast_view(count, &message);
                cx.update(|cx| show_result_toast(view, cx));
            }
        })
        .detach();
    }
}

fn toast_type(kind: ProviderUpdateToastType) -> ToastType {
    match kind {
        ProviderUpdateToastType::Warning => ToastType::Warning,
        ProviderUpdateToastType::Loading => ToastType::Loading,
        ProviderUpdateToastType::Error => ToastType::Error,
        ProviderUpdateToastType::Success => ToastType::Success,
    }
}

fn close_toast(toast_id: ToastId, cx: &mut App) {
    toast_manager(cx).update(cx, |store, cx| store.close(toast_id, cx));
}

fn show_result_toast(view: ProviderUpdateToastView, cx: &mut App) {
    let options = ToastOptions::new(view.title)
        .description(view.description)
        .type_(toast_type(view.toast_type))
        .timeout(0)
        .data(ThreadToastData {
            hide_copy_button: true,
            ..Default::default()
        });
    toast_manager(cx).update(cx, |store, cx| {
        store.add(options, cx);
    });
}

/// Mounts the host for the live shell.
pub fn start(
    live: LiveShellHandle,
    sidebar: WeakEntity<Sidebar>,
    cx: &mut App,
) -> Entity<ProviderUpdateHost> {
    cx.new(|cx| {
        ProviderUpdateHost::new(
            live,
            sidebar,
            Box::new(FileProviderUpdateDismissalsStore::in_data_dir()),
            cx,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissals_persist_across_stores() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-update-dismissals.json");
        let mut first = FileProviderUpdateDismissalsStore::new(Some(path.clone()));
        assert!(first.read().keys.is_empty());
        dismiss_provider_update_notification(&mut first, Some("codex:1.2.3"));
        let second = FileProviderUpdateDismissalsStore::new(Some(path));
        assert_eq!(second.read().keys, vec!["codex:1.2.3".to_owned()]);
    }

    fn outdated_codex() -> ServerProvider {
        serde_json::from_value(serde_json::json!({
            "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true,
            "version": "0.134.0", "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": [],
            "versionAdvisory": {
                "status": "behind_latest", "currentVersion": "0.134.0",
                "latestVersion": "0.136.2",
                "updateCommand": "npm install -g @openai/codex@latest",
                "canUpdate": true, "checkedAt": "2026-10-15T10:00:00.000Z", "message": null
            }
        }))
        .expect("provider")
    }

    #[test]
    fn an_outdated_provider_prompts_once_and_a_dismissal_sticks() {
        let providers = vec![outdated_codex()];
        let candidates = collect_provider_update_candidates(&providers);
        let key = provider_update_notification_key(&candidates).expect("key");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-update-dismissals.json");
        let mut store = FileProviderUpdateDismissalsStore::new(Some(path.clone()));

        let action =
            primary_provider_update_toast_action(&providers, &mut store, Some(&key), false);
        assert!(matches!(
            action,
            Some(PrimaryProviderUpdateToastAction::ShowInitial(view))
                if view.title.contains("Codex")
        ));
        // Seen this session: no second prompt.
        assert!(
            primary_provider_update_toast_action(&providers, &mut store, Some(&key), true)
                .is_none()
        );
        // Dismissed in an earlier session (read back from disk): no prompt at launch.
        dismiss_provider_update_notification(&mut store, Some(&key));
        let mut reopened = FileProviderUpdateDismissalsStore::new(Some(path));
        assert!(
            primary_provider_update_toast_action(&providers, &mut reopened, Some(&key), false)
                .is_none()
        );
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dismissals.json");
        std::fs::write(&path, "not json").unwrap();
        let store = FileProviderUpdateDismissalsStore::new(Some(path));
        assert!(store.read().keys.is_empty());
        assert!(
            FileProviderUpdateDismissalsStore::new(None)
                .read()
                .keys
                .is_empty()
        );
    }
}
