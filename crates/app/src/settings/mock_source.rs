//! Fixture-backed [`SettingsSource`]: realistic settings, providers, projects
//! and archived threads, with every patch applied locally, so the panels run
//! and can be screenshotted before the live services are wired (T22).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui_kit::{App, AppContext as _, Entity};
use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::keybindings::KeybindingRule;
use turin_contracts::provider_setup::{
    ProviderAuthPhase, ProviderAuthRespondInput, ProviderAuthState, ProviderInstallState,
};
use turin_contracts::settings::{
    ClientSettings, ServerSettings, ServerSettingsPatch, default_client_settings,
    default_server_settings,
};
use turin_runtime::settings_secrets::{SecretValue, map_settings_secrets};
use turin_shared::keybindings::{
    ResolvedKeybindingRule, compile_resolved_keybinding_rule, default_keybinding_rules,
    merge_with_default_keybindings,
};
use turin_shared::server_settings::apply_server_settings_patch;

use super::view_model::{
    ArchivedThread, CleanupEntry, CleanupReport, EnvironmentInfo, KeybindingsViewModel,
    LicenseEntry, RemoveKeybinding, SettingsSource, SettingsStore, SettingsViewModel, SetupResult,
    UpsertKeybinding,
};

fn provider(value: Value) -> turin_provider_core::server_provider::ServerProvider {
    serde_json::from_value(value).expect("mock provider decodes")
}

fn mock_provider_auth() -> IndexMap<String, ProviderAuthState> {
    let state: ProviderAuthState = serde_json::from_value(json!({
        "instanceId": "cursor",
        "phase": "idle",
        "methods": [{
            "id": "browser",
            "name": "Browser",
            "type": "agent"
        }]
    }))
    .expect("mock provider auth");
    let codex_work: ProviderAuthState = serde_json::from_value(json!({
        "instanceId": "codex_work",
        "phase": "idle",
        "methods": [
            { "id": "chatgpt", "name": "ChatGPT", "type": "agent" },
            { "id": "chatgpt-profile:work", "name": "work@example.com", "type": "agent", "accountEmail": "work@example.com" }
        ]
    }))
    .expect("mock codex auth");
    IndexMap::from([("cursor".into(), state), ("codex_work".into(), codex_work)])
}

fn mock_provider_install() -> IndexMap<String, ProviderInstallState> {
    let state: ProviderInstallState = serde_json::from_value(json!({
        "driver": "codex", "operationId": null, "phase": "idle", "downloadedBytes": 0,
        "totalBytes": 48_000_000, "version": "0.136.2", "installedVersion": null,
        "executablePath": null, "source": "managed", "canRemove": false,
        "message": "T3 Code downloads and manages Codex for you."
    }))
    .expect("mock codex install");
    IndexMap::from([("codex_work".into(), state)])
}

fn mock_providers() -> Vec<turin_provider_core::server_provider::ServerProvider> {
    let effort = json!({
        "id": "effort", "label": "Reasoning", "type": "select", "currentValue": "medium",
        "options": [
            { "id": "low", "label": "Low" },
            { "id": "medium", "label": "Medium", "isDefault": true },
            { "id": "high", "label": "High" },
            { "id": "xhigh", "label": "Extra High" }
        ]
    });
    let fast = json!({ "id": "fastMode", "label": "Fast Mode", "type": "boolean" });
    vec![
        provider(json!({
            "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true,
            "version": "0.134.0", "status": "ready",
            "auth": { "status": "authenticated", "type": "chatgpt", "label": "ChatGPT Pro", "email": "dev@example.com" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [
                { "slug": "gpt-6-sol", "name": "GPT-6 Sol", "isCustom": false, "isDefault": true, "capabilities": { "optionDescriptors": [effort.clone(), fast.clone()] } },
                { "slug": "gpt-6", "name": "GPT-6", "isCustom": false, "capabilities": { "optionDescriptors": [effort.clone()] } },
                { "slug": "my-fine-tune", "name": "my-fine-tune", "isCustom": true, "capabilities": null }
            ],
            "slashCommands": [], "skills": [],
            "versionAdvisory": { "status": "behind_latest", "currentVersion": "0.134.0", "latestVersion": "0.136.2", "updateCommand": "npm install -g @openai/codex@latest", "canUpdate": true, "checkedAt": "2026-10-15T10:00:00.000Z", "message": null },
            "setup": { "canAuthenticate": true, "canInstall": true }
        })),
        provider(json!({
            "instanceId": "codex_work", "driver": "codex", "enabled": true, "installed": false,
            "displayName": "ChatGPT - Work", "version": null, "status": "warning",
            "auth": { "status": "unauthenticated" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": [],
            "setup": { "canAuthenticate": true, "canInstall": true },
            "runtimePaths": { "homePath": "/Users/dev/.codex", "shadowHomePath": "/Users/dev/.t3/codex-shadow/codex_work" }
        })),
        provider(json!({
            "instanceId": "claudeAgent", "driver": "claudeAgent", "enabled": true, "installed": true,
            "version": "2.4.1", "status": "ready",
            "auth": { "status": "authenticated", "type": "claudeAi", "label": "Claude Max Subscription" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [
                { "slug": "claude-fable-5-1", "name": "Claude Fable 5.1", "isCustom": false, "isDefault": true, "capabilities": { "optionDescriptors": [effort.clone()] } },
                { "slug": "claude-sonnet-5-5", "name": "Claude Sonnet 5.5", "isCustom": false, "capabilities": { "optionDescriptors": [effort.clone()] } },
                { "slug": "claude-haiku-5-5", "name": "Claude Haiku 5.5", "isCustom": false, "capabilities": null }
            ],
            "slashCommands": [], "skills": [],
            "setup": { "canAuthenticate": true, "canInstall": true }
        })),
        provider(json!({
            "instanceId": "cursor", "driver": "cursor", "enabled": true, "installed": true,
            "version": "2026.10.09", "status": "warning",
            "auth": { "status": "unauthenticated" },
            "message": "Run cursor-agent login to sign in.",
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": [],
            "setup": { "canAuthenticate": true, "canInstall": true }
        })),
        provider(json!({
            "instanceId": "opencode", "driver": "opencode", "enabled": false, "installed": false,
            "version": null, "status": "disabled",
            "auth": { "status": "unknown" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": [],
            "setup": { "canAuthenticate": false, "canInstall": true }
        })),
    ]
}

/// The mock projects as the sidebar's `EnvironmentProject`s: id, name, root.
const MOCK_PROJECTS: [(&str, &str, &str); 3] = [
    ("project-turin", "turin", "/Users/dev/code/turin"),
    ("project-t3code", "t3code", "/Users/dev/code/t3code"),
    ("project-notes", "notes", "/Users/dev/notes"),
];

fn mock_environment_projects() -> Vec<crate::state::models::EnvironmentProject> {
    use crate::state::contracts::{EnvironmentId, ProjectId};
    MOCK_PROJECTS
        .into_iter()
        .map(
            |(id, title, root)| crate::state::models::EnvironmentProject {
                environment_id: EnvironmentId::new("local").expect("environment id"),
                id: ProjectId::new(id).expect("project id"),
                title: title.to_owned(),
                workspace_root: root.to_owned(),
                favicon_path: None,
                project_icon: None,
                created_at: Some("2026-10-01T09:00:00.000Z".into()),
                updated_at: Some("2026-10-14T18:22:00.000Z".into()),
                is_scratch: false,
                source: None,
            },
        )
        .collect()
}

fn mock_project_thread_counts() -> IndexMap<String, usize> {
    IndexMap::from([
        ("project-turin".to_owned(), 14),
        ("project-t3code".to_owned(), 3),
        ("project-notes".to_owned(), 0),
    ])
}

fn mock_archived_threads() -> Vec<ArchivedThread> {
    let thread =
        |id: &str, title: &str, project: &str, monogram: &str, color: u32, archived: &str| {
            ArchivedThread {
                id: id.into(),
                title: title.into(),
                project_name: project.into(),
                project_monogram: monogram.into(),
                project_color: color,
                archived_at: archived.into(),
                created_at: "2026-10-01T09:00:00.000Z".into(),
            }
        };
    vec![
        thread(
            "t1",
            "Port the sidebar search",
            "turin",
            "T",
            0x155dfc,
            "2026-10-14T18:22:00.000Z",
        ),
        thread(
            "t2",
            "Fix composer paste handling",
            "turin",
            "T",
            0x155dfc,
            "2026-10-12T08:05:00.000Z",
        ),
        thread(
            "t3",
            "Investigate flaky checkpoint test",
            "t3code",
            "T3",
            0x009966,
            "2026-10-09T21:40:00.000Z",
        ),
    ]
}

fn mock_view_model() -> SettingsViewModel {
    SettingsViewModel {
        appearance: Default::default(),
        client: default_client_settings(),
        server: {
            let mut server = default_server_settings();
            server.default_project_scripts = serde_json::from_value(json!([
                { "id": "test", "name": "Test", "command": "cargo test", "icon": "test", "runOnWorktreeCreate": false },
                { "id": "setup", "name": "Install", "command": "bun install", "icon": "configure", "runOnWorktreeCreate": true },
            ]))
            .expect("mock project scripts");
            server.provider_instances = serde_json::from_value(json!({
                "codex_work": {
                    "driver": "codex", "displayName": "ChatGPT - Work", "enabled": true,
                    "config": { "enabled": true, "setupMode": "managed" }
                }
            }))
            .expect("mock provider instances");
            server
        },
        platform: "MacIntel".into(),
        environment: EnvironmentInfo {
            label: "This device".into(),
            platform_os: "darwin".into(),
            app_version: "0.1.0".into(),
            home_dir: "/Users/dev".into(),
            worktrees_dir: "/Users/dev/.turin/worktrees".into(),
        },
        keybindings: KeybindingsViewModel {
            rules: merge_with_default_keybindings(&[]),
            issues: Vec::new(),
            config_path: "/Users/dev/.turin/keybindings.json".into(),
        },
        providers: mock_providers(),
        provider_auth: mock_provider_auth(),
        provider_install: mock_provider_install(),
        chat_gpt_welcome_acknowledged: Vec::new(),
        pending_provider_auth_delivery: None,
        source_control: mock_source_control(),
        project_thread_counts: mock_project_thread_counts(),
        environment_projects: mock_environment_projects(),
        archived_threads: mock_archived_threads(),
        storage_cleanup: None,
        licenses: mock_licenses(),
    }
}

/// The host's secret store, which settings never read back. Keys are the names
/// `settings_secrets` gives each credential, values the plaintext.
type SecretStore = BTreeMap<String, String>;

/// The mutable state behind the mock: the settings, the user's raw keybinding
/// rules (which the resolved config is derived from) and the secret store.
struct MockState {
    vm: SettingsViewModel,
    custom_keybindings: Vec<KeybindingRule>,
    secrets: SecretStore,
    /// Bumped by every install request so a cancelled run's timers stop.
    install_generation: u64,
}

pub struct MockSettingsSource {
    state: Rc<RefCell<MockState>>,
    store: Rc<RefCell<Option<Entity<SettingsStore>>>>,
}

impl MockSettingsSource {
    /// The fixture view model, for logic tests.
    #[cfg(test)]
    pub fn view_model_for_tests() -> SettingsViewModel {
        mock_view_model()
    }

    /// Builds the source and its store.
    pub fn store(cx: &mut App) -> Entity<SettingsStore> {
        let source = Rc::new(Self {
            state: Rc::new(RefCell::new(MockState {
                vm: mock_view_model(),
                custom_keybindings: Vec::new(),
                secrets: SecretStore::new(),
                install_generation: 0,
            })),
            store: Rc::new(RefCell::new(None)),
        });
        let vm = source.state.borrow().vm.clone();
        let store = cx.new(|_| SettingsStore::new(vm, source.clone()));
        *source.store.borrow_mut() = Some(store.clone());
        store
    }

    fn publish(&self, cx: &mut App) {
        publish_parts(&self.state, &self.store, cx);
    }

    fn rebuild_keybindings(&self) {
        let mut state = self.state.borrow_mut();
        let custom: Vec<ResolvedKeybindingRule> = state
            .custom_keybindings
            .iter()
            .filter_map(compile_resolved_keybinding_rule)
            .collect();
        state.vm.keybindings.rules = merge_with_default_keybindings(&custom);
    }
}

/// Merges `patch` into one instance's install state.
fn set_install(vm: &mut SettingsViewModel, instance_id: &str, patch: Value) {
    let Some(current) = vm.provider_install.get(instance_id) else {
        return;
    };
    let mut value = serde_json::to_value(current).expect("install state encodes");
    if let (Some(object), Some(patch)) = (value.as_object_mut(), patch.as_object()) {
        for (key, entry) in patch {
            object.insert(key.clone(), entry.clone());
        }
    }
    if let Ok(next) = serde_json::from_value(value) {
        vm.provider_install.insert(instance_id.to_owned(), next);
    }
}

fn publish_parts(
    state: &RefCell<MockState>,
    store: &RefCell<Option<Entity<SettingsStore>>>,
    cx: &mut App,
) {
    let vm = state.borrow().vm.clone();
    apply_appearance(&vm, cx);
    if let Some(store) = store.borrow().clone() {
        store.update(cx, |store, cx| store.publish(vm, cx));
    }
}

/// What the live app does on every settings change: re-resolve the theme from
/// the theme preference, contrast and font settings.
pub fn apply_appearance(vm: &SettingsViewModel, cx: &mut App) {
    use crate::theme::theme_palette::ThemePreferenceMode;
    use crate::theme::{AppearancePreferences, appearance_fonts::AppearanceFontPreferences};
    let client = &vm.client;
    let fonts = AppearanceFontPreferences {
        sans: client.font_family_sans.as_str().to_owned(),
        code: client.font_family_code.as_str().to_owned(),
        composer: client.font_family_composer.as_str().to_owned(),
        size_interface: client.font_size_interface.get() as u32,
        size_prompt: client.font_size_prompt.get() as u32,
        size_code: client.font_size_code.get() as u32,
        smoothing: client.font_smoothing,
    };
    let mode = match vm.appearance.mode.as_str() {
        "light" => ThemePreferenceMode::Light,
        "dark" => ThemePreferenceMode::Dark,
        _ => ThemePreferenceMode::System,
    };
    crate::theme::set_preferences(
        AppearancePreferences {
            theme: vm.appearance.theme.clone(),
            appearance_mode: Some(mode),
            contrast: client.appearance_contrast.get() as u32,
            fonts,
        },
        cx,
    );
}

/// `{...target, ...patch}` over the settings' JSON, the way the client-settings
/// store applies a patch.
fn merge_client_patch(client: &ClientSettings, patch: &Value) -> ClientSettings {
    let mut value = serde_json::to_value(client).expect("client settings encode");
    if let (Some(object), Some(patch)) = (value.as_object_mut(), patch.as_object()) {
        for (key, entry) in patch {
            object.insert(key.clone(), entry.clone());
        }
    }
    serde_json::from_value(value).unwrap_or_else(|_| client.clone())
}

/// `persistSettingsSecrets` as the mock host runs it, on the store above. Every
/// credential in `next` is visited with `map_settings_secrets`: a plain value is
/// written to the store and settings keep only the redaction marker, a blank
/// value clears its secret, and an echoed marker keeps the stored one. Stored
/// credentials that `next` no longer carries (a removed token, a dropped
/// instance) are dropped.
fn persist_settings_secrets(
    store: &mut SecretStore,
    next: &ServerSettings,
) -> serde_json::Result<ServerSettings> {
    let mut present: BTreeSet<String> = BTreeSet::new();
    let persisted = map_settings_secrets(next, |secret| {
        let name = secret.secret_name.clone();
        if !secret.sensitive {
            store.remove(&name);
            return SecretValue {
                value: secret.value.clone(),
                redacted: false,
            };
        }
        present.insert(name.clone());
        let value = if secret.redacted {
            store.get(&name).cloned()
        } else {
            Some(secret.value.clone())
        };
        match value {
            // An echoed marker with nothing stored: keep the marker, store nothing.
            None => SecretValue {
                value: String::new(),
                redacted: true,
            },
            Some(value) if value.is_empty() => {
                store.remove(&name);
                SecretValue {
                    value: String::new(),
                    redacted: false,
                }
            }
            Some(value) => {
                store.insert(name, value);
                SecretValue {
                    value: String::new(),
                    redacted: true,
                }
            }
        }
    })?;
    store.retain(|name, _| present.contains(name));
    Ok(persisted)
}

fn apply_server_patch(server: &ServerSettings, patch: Value) -> ServerSettings {
    match serde_json::from_value::<ServerSettingsPatch>(patch) {
        Ok(patch) => apply_server_settings_patch(server, &patch).unwrap_or_else(|_| server.clone()),
        Err(error) => {
            tracing::warn!(%error, "mock settings: ignoring undecodable server patch");
            server.clone()
        }
    }
}

impl SettingsSource for MockSettingsSource {
    fn update_client(&self, patch: Value, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            state.vm.client = merge_client_patch(&state.vm.client, &patch);
        }
        self.publish(cx);
    }

    fn update_server(&self, patch: Value, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let next = apply_server_patch(&state.vm.server, patch);
            match persist_settings_secrets(&mut state.secrets, &next) {
                Ok(persisted) => state.vm.server = persisted,
                Err(error) => {
                    // Keep the saved settings: `next` still holds the plaintext.
                    tracing::warn!(%error, "mock settings: could not persist settings secrets");
                }
            }
        }
        self.publish(cx);
    }

    fn upsert_keybinding(&self, input: UpsertKeybinding, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            if let Some(replaces) = &input.replaces {
                state.custom_keybindings.retain(|rule| {
                    !(rule.command.as_str() == replaces.command
                        && rule.key.as_str() == replaces.key
                        && rule.when.as_ref().map(|w| w.as_str()) == replaces.when.as_deref())
                });
            }
            state
                .custom_keybindings
                .retain(|rule| rule.command.as_str() != input.command || input.replaces.is_some());
            if let Ok(rule) = serde_json::from_value::<KeybindingRule>(json!({
                "key": input.key, "command": input.command,
                "when": input.when.filter(|when| !when.is_empty()),
            })) {
                state.custom_keybindings.push(rule);
            }
        }
        self.rebuild_keybindings();
        self.publish(cx);
    }

    fn remove_keybinding(&self, input: RemoveKeybinding, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            state.custom_keybindings.retain(|rule| {
                !(rule.command.as_str() == input.command
                    && rule.key.as_str() == input.key
                    && rule.when.as_ref().map(|w| w.as_str()) == input.when.as_deref())
            });
            // Removing a default shadows it with an empty override upstream; the
            // mock just hides the default rule by listing the command as removed.
            let defaults = default_keybinding_rules();
            let is_default = defaults.iter().any(|rule| {
                rule.command.as_str() == input.command && rule.key.as_str() == input.key
            });
            if is_default {
                state.vm.keybindings.rules.retain(|rule| {
                    !(rule.command.as_str() == input.command
                        && super::keybindings_logic::shortcut_to_keybinding_input(&rule.shortcut)
                            == input.key)
                });
                drop(state);
                self.publish(cx);
                return;
            }
        }
        self.rebuild_keybindings();
        self.publish(cx);
    }

    fn rescan_source_control(&self, cx: &mut App) {
        self.publish(cx);
    }

    fn refresh_providers(&self, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            let now = "2026-10-15T10:05:00.000Z";
            for provider in &mut state.vm.providers {
                provider.checked_at = now.to_owned();
            }
        }
        self.publish(cx);
    }

    fn update_provider(&self, instance_id: &str, _target_version: Option<String>, _cx: &mut App) {
        tracing::info!(instance_id, "mock settings: update provider");
    }

    fn start_provider_auth(
        &self,
        instance_id: &str,
        method_id: Option<&str>,
        cx: &mut App,
    ) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            let methods = state
                .vm
                .provider_auth
                .get(instance_id)
                .and_then(|s| s.methods.clone());
            tracing::info!(
                instance_id,
                ?method_id,
                "mock settings: start provider auth"
            );
            state.vm.provider_auth.insert(
                instance_id.to_owned(),
                serde_json::from_value(json!({
                    "instanceId": instance_id,
                    "phase": "waiting",
                    "flowId": "mock-flow",
                    "authorizationUrl": "https://example.com/oauth",
                    "message": "Complete sign-in in your browser.",
                    "methods": methods,
                    "interaction": {
                        "type": "browser",
                        "id": "mock-interaction",
                        "url": "https://example.com/oauth",
                        "requiresConsent": false,
                        "acceptsCallback": true
                    }
                }))
                .expect("mock waiting auth"),
            );
        }
        self.publish(cx);
        Ok(())
    }

    fn cancel_provider_auth(&self, instance_id: &str, _flow_id: &str, cx: &mut App) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            if let Some(entry) = state.vm.provider_auth.get_mut(instance_id) {
                entry.phase = ProviderAuthPhase::Cancelled;
                entry.flow_id = None;
                entry.interaction = None;
            }
        }
        self.publish(cx);
        Ok(())
    }

    fn respond_provider_auth(&self, input: ProviderAuthRespondInput, _cx: &mut App) -> SetupResult {
        tracing::info!(instance_id = %input.instance_id.as_str(), "mock settings: respond provider auth");
        Ok(())
    }

    fn complete_provider_auth(
        &self,
        instance_id: &str,
        _flow_id: &str,
        _callback_url: &str,
        cx: &mut App,
    ) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            let methods = state
                .vm
                .provider_auth
                .get(instance_id)
                .and_then(|s| s.methods.clone());
            state.vm.provider_auth.insert(
                instance_id.to_owned(),
                serde_json::from_value(json!({
                    "instanceId": instance_id,
                    "phase": "succeeded",
                    "methods": methods,
                }))
                .expect("mock succeeded auth"),
            );
            if let Some(provider) = state
                .vm
                .providers
                .iter_mut()
                .find(|p| p.instance_id.as_str() == instance_id)
            {
                provider.auth.status =
                    turin_provider_core::server_provider::ServerProviderAuthStatus::Authenticated;
                provider.auth.label =
                    Some(TrimmedNonEmptyString::new("Signed in (mock)").expect("label"));
                provider.auth.email =
                    Some(TrimmedNonEmptyString::new("work@example.com").expect("email"));
                if provider.driver.as_str() == "codex" {
                    provider.auth.subscription_sharing = Some(true);
                }
            }
        }
        self.publish(cx);
        Ok(())
    }

    fn open_external_url(&self, url: &str, _cx: &mut App) {
        tracing::info!(url, "mock settings: open external url");
    }

    fn logout_provider(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            if let Some(provider) = state
                .vm
                .providers
                .iter_mut()
                .find(|p| p.instance_id.as_str() == instance_id)
            {
                provider.auth.status =
                    turin_provider_core::server_provider::ServerProviderAuthStatus::Unauthenticated;
                provider.auth.email = None;
                provider.auth.subscription_sharing = None;
            }
            state.vm.provider_auth.shift_remove(instance_id);
        }
        tracing::info!(instance_id, "mock settings: provider logout");
        self.publish(cx);
        Ok(())
    }

    fn start_provider_install(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        const TOTAL: u64 = 48_000_000;
        let generation = {
            let mut state = self.state.borrow_mut();
            state.install_generation += 1;
            let generation = state.install_generation;
            set_install(
                &mut state.vm,
                instance_id,
                json!({
                    "operationId": "mock-install", "phase": "downloading",
                    "downloadedBytes": 0, "totalBytes": TOTAL, "message": null
                }),
            );
            generation
        };
        self.publish(cx);
        let state = self.state.clone();
        let store = self.store.clone();
        let instance_id = instance_id.to_owned();
        cx.spawn(async move |cx| {
            let step = |ms: u64| {
                let cx = cx.clone();
                async move {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(ms))
                        .await;
                }
            };
            let alive =
                |state: &RefCell<MockState>| state.borrow().install_generation == generation;
            let mut downloaded = 0;
            while downloaded < TOTAL {
                step(250).await;
                downloaded = (downloaded + 12_000_000).min(TOTAL);
                if !alive(&state) {
                    return;
                }
                set_install(
                    &mut state.borrow_mut().vm,
                    &instance_id,
                    json!({ "downloadedBytes": downloaded }),
                );
                cx.update(|cx| publish_parts(&state, &store, cx));
            }
            for phase in ["extracting", "verifying"] {
                step(400).await;
                if !alive(&state) {
                    return;
                }
                set_install(
                    &mut state.borrow_mut().vm,
                    &instance_id,
                    json!({ "phase": phase }),
                );
                cx.update(|cx| publish_parts(&state, &store, cx));
            }
            step(400).await;
            if !alive(&state) {
                return;
            }
            {
                let mut state = state.borrow_mut();
                set_install(
                    &mut state.vm,
                    &instance_id,
                    json!({
                        "phase": "succeeded", "operationId": null,
                        "installedVersion": "0.136.2",
                        "executablePath": "/Users/dev/.t3/runtimes/codex/0.136.2/codex",
                        "canRemove": true, "message": null
                    }),
                );
                if let Some(provider) = state
                    .vm
                    .providers
                    .iter_mut()
                    .find(|p| p.instance_id.as_str() == instance_id)
                {
                    provider.installed = true;
                }
            }
            cx.update(|cx| publish_parts(&state, &store, cx));
        })
        .detach();
        Ok(())
    }

    fn cancel_provider_install(
        &self,
        instance_id: &str,
        _operation_id: &str,
        cx: &mut App,
    ) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            state.install_generation += 1;
            set_install(
                &mut state.vm,
                instance_id,
                json!({ "phase": "cancelled", "operationId": null, "message": "Installation cancelled." }),
            );
        }
        self.publish(cx);
        Ok(())
    }

    fn remove_provider_installation(&self, instance_id: &str, cx: &mut App) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            set_install(
                &mut state.vm,
                instance_id,
                json!({
                    "phase": "idle", "installedVersion": null, "executablePath": null,
                    "canRemove": false, "downloadedBytes": 0
                }),
            );
            if let Some(provider) = state
                .vm
                .providers
                .iter_mut()
                .find(|p| p.instance_id.as_str() == instance_id)
            {
                provider.installed = false;
            }
        }
        self.publish(cx);
        Ok(())
    }

    fn acknowledge_chat_gpt_welcome(&self, key: &str, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            if !state
                .vm
                .chat_gpt_welcome_acknowledged
                .iter()
                .any(|k| k == key)
            {
                state.vm.chat_gpt_welcome_acknowledged.push(key.to_owned());
            }
        }
        self.publish(cx);
    }

    fn clear_provider_auth_delivery(&self, cx: &mut App) {
        self.state.borrow_mut().vm.pending_provider_auth_delivery = None;
        self.publish(cx);
    }

    fn set_theme(&self, theme: String, cx: &mut App) {
        self.state.borrow_mut().vm.appearance.theme = theme;
        self.publish(cx);
    }

    fn set_appearance_mode(&self, mode: String, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            // Standard-look preferences are named after the mode.
            if matches!(
                state.vm.appearance.theme.as_str(),
                "system" | "light" | "dark"
            ) {
                state.vm.appearance.theme = mode.clone();
            }
            state.vm.appearance.mode = mode;
        }
        self.publish(cx);
    }

    fn open_keybindings_file(&self, _cx: &mut App) {
        tracing::info!("mock settings: open keybindings.json");
    }

    fn unarchive_thread(&self, thread_id: &str, cx: &mut App) {
        self.state
            .borrow_mut()
            .vm
            .archived_threads
            .retain(|thread| thread.id != thread_id);
        self.publish(cx);
    }

    fn rename_project(&self, project_id: &str, title: &str, cx: &mut App) {
        if let Some(project) = self
            .state
            .borrow_mut()
            .vm
            .environment_projects
            .iter_mut()
            .find(|p| p.id.as_str() == project_id)
        {
            project.title = title.to_owned();
        }
        self.publish(cx);
    }

    fn remove_project(&self, project_id: &str, cx: &mut App) {
        self.state
            .borrow_mut()
            .vm
            .environment_projects
            .retain(|p| p.id.as_str() != project_id);
        self.publish(cx);
    }

    fn set_project_icon(
        &self,
        project_ids: &[String],
        favicon_path: Option<String>,
        project_icon: Option<crate::state::models::ProjectIcon>,
        cx: &mut App,
    ) -> SetupResult {
        {
            let mut state = self.state.borrow_mut();
            for project in &mut state.vm.environment_projects {
                if project_ids.iter().any(|id| id == project.id.as_str()) {
                    project.favicon_path = favicon_path.clone();
                    project.project_icon = project_icon.clone();
                }
            }
        }
        self.publish(cx);
        Ok(())
    }

    fn search_project_files(
        &self,
        _cwd: &str,
        query: &str,
        _cx: &mut App,
    ) -> Result<Vec<super::project_file_picker_logic::ProjectEntry>, String> {
        use super::project_file_picker_logic::{ProjectEntry, ProjectEntryKind};
        let query = query.trim().to_lowercase();
        Ok([
            "assets/icon.png",
            "assets/logo.svg",
            "docs/screenshots/overview.png",
            "public/favicon.ico",
        ]
        .into_iter()
        .filter(|path| query.is_empty() || path.contains(query.as_str()))
        .map(|path| ProjectEntry {
            kind: ProjectEntryKind::File,
            path: path.to_owned(),
        })
        .collect())
    }

    fn pick_project_favicon(&self, _cwd: &str, _cx: &mut App) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn run_storage_cleanup(&self, cx: &mut App) {
        self.state.borrow_mut().vm.storage_cleanup = Some(CleanupReport {
            finished_at: "2026-10-15T10:00:00.000Z".into(),
            bytes_freed: 482_344_960,
            removed: 2,
            kept: 1,
            failed: 0,
            entries: vec![
                CleanupEntry {
                    name: "turin-fix-composer".into(),
                    thread_title: Some("Fix composer paste handling".into()),
                    reason: "Thread inactive for 12 days.".into(),
                    failed: false,
                    removed: true,
                    files: None,
                    bytes: Some(401_604_608),
                },
                CleanupEntry {
                    name: "Rotated logs".into(),
                    thread_title: None,
                    reason: "Rotated log files older than 14 days.".into(),
                    failed: false,
                    removed: true,
                    files: Some(18),
                    bytes: Some(80_740_352),
                },
            ],
            omitted: 0,
        });
        self.publish(cx);
    }

    fn delete_thread(&self, thread_id: &str, cx: &mut App) {
        self.unarchive_thread(thread_id, cx);
    }
}

/// Discovery of a typical machine: git installed, jj not supported yet, GitHub
/// through `gh` with two logins.
fn mock_source_control() -> turin_contracts::source_control::SourceControlDiscoveryResult {
    let some = |value: &str| json!({ "_tag": "Some", "value": value });
    let none = || json!({ "_tag": "None" });
    serde_json::from_value(json!({
        "versionControlSystems": [
            {
                "kind": "git",
                "implemented": true,
                "label": "Git",
                "executable": "git",
                "status": "available",
                "version": some("2.47.0"),
                "installHint": "Install Git from git-scm.com.",
                "detail": none(),
            },
            {
                "kind": "jj",
                "implemented": false,
                "label": "Jujutsu",
                "executable": "jj",
                "status": "missing",
                "version": none(),
                "installHint": "Install Jujutsu with your package manager.",
                "detail": none(),
            },
        ],
        "sourceControlProviders": [
            {
                "kind": "github",
                "label": "GitHub",
                "executable": "gh",
                "status": "available",
                "version": some("2.62.0"),
                "installHint": "Install the GitHub CLI from cli.github.com.",
                "detail": none(),
                "auth": {
                    "status": "authenticated",
                    "account": some("octocat"),
                    "host": some("github.com"),
                    "detail": none(),
                    "accounts": [
                        { "host": "github.com", "account": "octocat", "active": true, "authenticated": true },
                        { "host": "github.com", "account": "work-account", "active": false, "authenticated": true },
                        { "host": "git.example.com", "account": "octo-enterprise", "active": true, "authenticated": true },
                    ],
                },
            },
        ],
    }))
    .expect("mock source control discovery")
}

fn mock_licenses() -> Vec<LicenseEntry> {
    const MIT: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the \"Software\"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:\n\nThe above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.";
    let entry = |name: &str, version: &str, license: &str, url: &str, notice: &str| LicenseEntry {
        kind: "cargo".into(),
        name: name.into(),
        version: Some(version.into()),
        license: license.into(),
        bundles: vec!["desktop".into()],
        source_url: Some(url.into()),
        notice_text: notice.into(),
    };
    vec![
        entry(
            "gpui",
            "0.3.9",
            "Apache-2.0",
            "https://github.com/zed-industries/zed",
            "Licensed under the Apache License, Version 2.0 (the \"License\"); you may not use this file except in compliance with the License.",
        ),
        entry(
            "serde",
            "1.0.228",
            "MIT OR Apache-2.0",
            "https://github.com/serde-rs/serde",
            &format!("Copyright (c) David Tolnay\n\n{MIT}"),
        ),
        entry(
            "serde_json",
            "1.0.145",
            "MIT OR Apache-2.0",
            "https://github.com/serde-rs/json",
            &format!("Copyright (c) David Tolnay\n\n{MIT}"),
        ),
        entry(
            "tokio",
            "1.48.0",
            "MIT",
            "https://github.com/tokio-rs/tokio",
            &format!("Copyright (c) Tokio Contributors\n\n{MIT}"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::usage_limit_source_id::UsageLimitSourceId;
    use turin_runtime::settings_secrets::{SECRET_REDACTED, source_control_host_secret_name};

    /// One save: the patch applied to `server`, then its secrets persisted.
    fn save(store: &mut SecretStore, server: &ServerSettings, patch: Value) -> ServerSettings {
        let next = apply_server_patch(server, patch);
        persist_settings_secrets(store, &next).expect("settings encode")
    }

    fn cursor_environment(environment: Value) -> Value {
        json!({ "providerInstances": { "cursor": { "driver": "cursor", "environment": environment } } })
    }

    fn cursor_variable(server: &ServerSettings, name: &str) -> Option<(String, Option<bool>)> {
        let instance = server.provider_instances.get("cursor")?;
        let variable = instance
            .environment
            .as_ref()?
            .iter()
            .find(|v| v.name.as_str() == name)?;
        Some((variable.value.clone(), variable.value_redacted))
    }

    fn github_tokens(tokens: Value) -> Value {
        json!({ "sourceControlHosts": { "github": { "tokens": tokens } } })
    }

    fn token_of(server: &ServerSettings, host: &str) -> Option<String> {
        server
            .source_control_hosts
            .get("github")?
            .get("tokens")?
            .get(host)?
            .as_str()
            .map(str::to_owned)
    }

    fn token_name(host: &str) -> String {
        source_control_host_secret_name("github", "tokens", Some(host))
    }

    #[test]
    fn sensitive_environment_value_moves_to_the_store() {
        let mut store = SecretStore::new();
        let server = save(
            &mut store,
            &default_server_settings(),
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "sk-1", "sensitive": true }]),
            ),
        );

        assert_eq!(
            cursor_variable(&server, "CURSOR_API_KEY"),
            Some((String::new(), Some(true)))
        );
        assert_eq!(store.values().collect::<Vec<_>>(), vec!["sk-1"]);
    }

    #[test]
    fn echoed_marker_keeps_the_stored_environment_secret() {
        let mut store = SecretStore::new();
        let saved = save(
            &mut store,
            &default_server_settings(),
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "sk-1", "sensitive": true }]),
            ),
        );
        let echoed = save(
            &mut store,
            &saved,
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "", "sensitive": true, "valueRedacted": true }]),
            ),
        );

        assert_eq!(
            cursor_variable(&echoed, "CURSOR_API_KEY"),
            Some((String::new(), Some(true)))
        );
        assert_eq!(store.values().collect::<Vec<_>>(), vec!["sk-1"]);
    }

    #[test]
    fn blank_sensitive_value_clears_its_secret() {
        let mut store = SecretStore::new();
        let saved = save(
            &mut store,
            &default_server_settings(),
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "sk-1", "sensitive": true }]),
            ),
        );
        let cleared = save(
            &mut store,
            &saved,
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "", "sensitive": true, "valueRedacted": false }]),
            ),
        );

        assert_eq!(
            cursor_variable(&cleared, "CURSOR_API_KEY"),
            Some((String::new(), None))
        );
        assert!(store.is_empty());
    }

    #[test]
    fn plain_value_stays_in_settings() {
        let mut store = SecretStore::new();
        let server = save(
            &mut store,
            &default_server_settings(),
            cursor_environment(json!([{ "name": "EXTRA_FLAG", "value": "1", "sensitive": false }])),
        );

        assert_eq!(
            cursor_variable(&server, "EXTRA_FLAG"),
            Some(("1".to_owned(), None))
        );
        assert!(store.is_empty());
    }

    #[test]
    fn dropped_instance_takes_its_secrets_with_it() {
        let mut store = SecretStore::new();
        let saved = save(
            &mut store,
            &default_server_settings(),
            cursor_environment(
                json!([{ "name": "CURSOR_API_KEY", "value": "sk-1", "sensitive": true }]),
            ),
        );
        save(&mut store, &saved, json!({ "providerInstances": {} }));

        assert!(store.is_empty());
    }

    #[test]
    fn github_token_is_stored_per_host_and_settings_keep_the_marker() {
        let mut store = SecretStore::new();
        let server = save(
            &mut store,
            &default_server_settings(),
            github_tokens(json!({ "github.com": "ghp_dotcom" })),
        );

        assert_eq!(
            token_of(&server, "github.com"),
            Some(SECRET_REDACTED.to_owned())
        );
        assert_eq!(
            store.get(&token_name("github.com")),
            Some(&"ghp_dotcom".to_owned())
        );
        let settings_json = serde_json::to_string(&server).expect("settings encode");
        assert!(!settings_json.contains("ghp_dotcom"));
    }

    #[test]
    fn echoed_github_tokens_are_kept_and_a_new_host_is_added_beside_them() {
        let mut store = SecretStore::new();
        let saved = save(
            &mut store,
            &default_server_settings(),
            github_tokens(json!({ "github.com": "ghp_dotcom" })),
        );
        let echoed = save(
            &mut store,
            &saved,
            github_tokens(json!({ "github.com": SECRET_REDACTED, "ghe.acme.test": "ghp_ghe" })),
        );

        assert_eq!(
            token_of(&echoed, "github.com"),
            Some(SECRET_REDACTED.to_owned())
        );
        assert_eq!(
            token_of(&echoed, "ghe.acme.test"),
            Some(SECRET_REDACTED.to_owned())
        );
        assert_eq!(
            store.get(&token_name("github.com")),
            Some(&"ghp_dotcom".to_owned())
        );
        assert_eq!(
            store.get(&token_name("ghe.acme.test")),
            Some(&"ghp_ghe".to_owned())
        );
    }

    #[test]
    fn blank_github_token_removes_only_its_host() {
        let mut store = SecretStore::new();
        let saved = save(
            &mut store,
            &default_server_settings(),
            github_tokens(json!({ "github.com": "ghp_dotcom", "ghe.acme.test": "ghp_ghe" })),
        );
        let cleared = save(
            &mut store,
            &saved,
            github_tokens(json!({ "github.com": "", "ghe.acme.test": SECRET_REDACTED })),
        );

        assert_eq!(token_of(&cleared, "github.com"), Some(String::new()));
        assert_eq!(
            token_of(&cleared, "ghe.acme.test"),
            Some(SECRET_REDACTED.to_owned())
        );
        assert_eq!(store.get(&token_name("github.com")), None);
        assert_eq!(
            store.get(&token_name("ghe.acme.test")),
            Some(&"ghp_ghe".to_owned())
        );
    }

    #[test]
    fn management_key_moves_aside_and_removing_a_hub_drops_it() {
        let mut store = SecretStore::new();
        let hub = json!({ "usageLimitSources": {
            "cliproxy-hub": { "kind": "cliproxy", "url": "https://hub", "managementKey": " mk " },
        } });
        let saved = save(&mut store, &default_server_settings(), hub);

        let key = saved
            .usage_limit_sources
            .get(&UsageLimitSourceId::new("cliproxy-hub"))
            .expect("hub saved");
        assert!(
            !serde_json::to_string(key)
                .expect("encode")
                .contains(r#""mk""#)
        );
        assert_eq!(store.values().collect::<Vec<_>>(), vec!["mk"]);

        save(
            &mut store,
            &saved,
            json!({ "usageLimitSources": { "cliproxy-hub": null } }),
        );
        assert!(store.is_empty());
    }
}
