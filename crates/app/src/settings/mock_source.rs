//! Fixture-backed [`SettingsSource`]: realistic settings, providers, projects
//! and archived threads, with every patch applied locally, so the panels run
//! and can be screenshotted before the live services are wired (T22).

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{App, AppContext as _, Entity};
use serde_json::{Value, json};
use turin_contracts::keybindings::KeybindingRule;
use turin_contracts::settings::{
    ClientSettings, ServerSettings, ServerSettingsPatch, default_client_settings,
    default_server_settings,
};
use turin_shared::keybindings::{
    ResolvedKeybindingRule, compile_resolved_keybinding_rule, default_keybinding_rules,
    merge_with_default_keybindings,
};
use turin_shared::server_settings::apply_server_settings_patch;

use super::view_model::{
    ArchivedThread, CleanupEntry, CleanupReport, EnvironmentInfo, KeybindingsViewModel,
    LicenseEntry, RemoveKeybinding, SettingsProject, SettingsSource, SettingsStore,
    SettingsViewModel, UpsertKeybinding,
};

fn provider(value: Value) -> turin_provider_core::server_provider::ServerProvider {
    serde_json::from_value(value).expect("mock provider decodes")
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

fn mock_projects() -> Vec<SettingsProject> {
    vec![
        SettingsProject {
            id: "project-turin".into(),
            key: "turin".into(),
            display_name: "turin".into(),
            workspace_root: "/Users/dev/code/turin".into(),
            favicon_color: 0x155dfc,
            monogram: "T".into(),
            remote_url: Some("github.com/example/turin".into()),
            checkout_count: 3,
        },
        SettingsProject {
            id: "project-t3code".into(),
            key: "t3code".into(),
            display_name: "t3code".into(),
            workspace_root: "/Users/dev/code/t3code".into(),
            favicon_color: 0x009966,
            monogram: "T3".into(),
            remote_url: Some("github.com/pingdotgg/t3code".into()),
            checkout_count: 1,
        },
        SettingsProject {
            id: "project-notes".into(),
            key: "notes".into(),
            display_name: "notes".into(),
            workspace_root: "/Users/dev/notes".into(),
            favicon_color: 0xe17100,
            monogram: "N".into(),
            remote_url: None,
            checkout_count: 1,
        },
    ]
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
        source_control: mock_source_control(),
        projects: mock_projects(),
        archived_threads: mock_archived_threads(),
        storage_cleanup: None,
        licenses: mock_licenses(),
    }
}

/// The mutable state behind the mock: the settings plus the user's raw
/// keybinding rules, which the resolved config is derived from.
struct MockState {
    vm: SettingsViewModel,
    custom_keybindings: Vec<KeybindingRule>,
}

pub struct MockSettingsSource {
    state: RefCell<MockState>,
    store: RefCell<Option<Entity<SettingsStore>>>,
}

impl MockSettingsSource {
    /// Builds the source and its store.
    pub fn store(cx: &mut App) -> Entity<SettingsStore> {
        let source = Rc::new(Self {
            state: RefCell::new(MockState {
                vm: mock_view_model(),
                custom_keybindings: Vec::new(),
            }),
            store: RefCell::new(None),
        });
        let vm = source.state.borrow().vm.clone();
        let store = cx.new(|_| SettingsStore::new(vm, source.clone()));
        *source.store.borrow_mut() = Some(store.clone());
        store
    }

    fn publish(&self, cx: &mut App) {
        let vm = self.state.borrow().vm.clone();
        apply_appearance(&vm, cx);
        if let Some(store) = self.store.borrow().clone() {
            store.update(cx, |store, cx| store.publish(vm, cx));
        }
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
            state.vm.server = apply_server_patch(&state.vm.server, patch);
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

    fn start_provider_auth(&self, instance_id: &str, _cx: &mut App) {
        tracing::info!(instance_id, "mock settings: start provider auth");
    }

    fn logout_provider(&self, instance_id: &str, _cx: &mut App) {
        tracing::info!(instance_id, "mock settings: provider logout");
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
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
        {
            project.display_name = title.to_owned();
        }
        self.publish(cx);
    }

    fn remove_project(&self, project_id: &str, cx: &mut App) {
        self.state
            .borrow_mut()
            .vm
            .projects
            .retain(|p| p.id != project_id);
        self.publish(cx);
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
