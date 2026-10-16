//! The seam between the services and the settings UI.
//!
//! [`SettingsStore`] is a GPUI model holding the current
//! [`SettingsViewModel`]; the settings view observes it. Whatever feeds the
//! store implements [`SettingsSource`]: it publishes view models and receives
//! the user's intent (setting patches, keybinding edits, archive actions…).
//! [`mock_source::MockSettingsSource`](super::mock_source::MockSettingsSource)
//! backs the demo; T22 replaces it with the live `ServerSettingsService`,
//! `KeybindingsService`, provider registry and projection reads.

use std::rc::Rc;

use gpui_kit::{App, Context};
use serde_json::Value;
use turin_contracts::settings::{ClientSettings, ServerSettings};
use turin_provider_core::server_provider::ServerProvider;
use turin_shared::keybindings::ResolvedKeybindingRule;

/// A project as the settings pages list it (`SidebarProjectSnapshot` subset).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsProject {
    /// `ProjectId` of the project's primary checkout.
    pub id: String,
    /// The project's key; one logical project can have several checkouts.
    pub key: String,
    pub display_name: String,
    pub workspace_root: String,
    pub favicon_color: u32,
    pub monogram: String,
    pub remote_url: Option<String>,
    pub checkout_count: usize,
}

/// One row of `StorageCleanupReport.entries`.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupEntry {
    pub name: String,
    pub thread_title: Option<String>,
    pub reason: String,
    pub failed: bool,
    pub removed: bool,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
}

/// The last storage cleanup run (`StorageCleanupReport`).
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupReport {
    pub finished_at: String,
    pub bytes_freed: u64,
    pub removed: usize,
    pub kept: usize,
    pub failed: usize,
    pub entries: Vec<CleanupEntry>,
    pub omitted: usize,
}

/// One entry of the third-party license manifest (`ThirdPartyLicenseEntry`).
#[derive(Debug, Clone, PartialEq)]
pub struct LicenseEntry {
    pub kind: String,
    pub name: String,
    pub version: Option<String>,
    pub license: String,
    pub bundles: Vec<String>,
    pub source_url: Option<String>,
    pub notice_text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedThread {
    pub id: String,
    pub title: String,
    pub project_name: String,
    pub project_monogram: String,
    pub project_color: u32,
    pub archived_at: String,
    pub created_at: String,
}

/// Why a keybinding rule in `keybindings.json` was skipped (`ServerConfigIssue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingIssue {
    pub kind: String,
    pub message: String,
    pub index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeybindingsViewModel {
    /// The resolved config: defaults merged with the user's rules.
    pub rules: Vec<ResolvedKeybindingRule>,
    pub issues: Vec<KeybindingIssue>,
    pub config_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentInfo {
    pub label: String,
    pub platform_os: String,
    pub app_version: String,
    pub home_dir: String,
    pub worktrees_dir: String,
}

/// Device-local appearance (upstream `useTheme`: localStorage, not settings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearanceState {
    /// `"system" | "light" | "dark"` (the standard look) or a theme id.
    pub theme: String,
    /// `"system" | "light" | "dark"`.
    pub mode: String,
}

impl Default for AppearanceState {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            mode: "system".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsViewModel {
    pub appearance: AppearanceState,
    pub client: ClientSettings,
    pub server: ServerSettings,
    /// `navigator.platform`.
    pub platform: String,
    pub environment: EnvironmentInfo,
    pub keybindings: KeybindingsViewModel,
    pub providers: Vec<ServerProvider>,
    /// `sourceControlEnvironment.discovery` of the local environment.
    pub source_control: turin_contracts::source_control::SourceControlDiscoveryResult,
    pub projects: Vec<SettingsProject>,
    pub archived_threads: Vec<ArchivedThread>,
    pub storage_cleanup: Option<CleanupReport>,
    pub licenses: Vec<LicenseEntry>,
}

impl SettingsViewModel {
    pub fn is_mac(&self) -> bool {
        turin_shared::keybindings::is_mac_platform(&self.platform)
    }

    /// `⌘` or `Ctrl`, the way the General panel labels `mod`.
    pub fn modifier_label(&self) -> &'static str {
        if self.is_mac() { "⌘" } else { "Ctrl" }
    }
}

/// Which keybinding edit the user asked for (`ServerUpsertKeybindingInput`).
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertKeybinding {
    pub command: String,
    pub key: String,
    pub when: Option<String>,
    /// The row being replaced, when editing rather than adding.
    pub replaces: Option<RemoveKeybinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoveKeybinding {
    pub command: String,
    pub key: String,
    pub when: Option<String>,
}

/// The user's intent. Implemented by the mock here and by the live services in T22.
pub trait SettingsSource {
    /// `useUpdateSettings` for client keys: a `ClientSettingsPatch` as JSON.
    fn update_client(&self, patch: Value, cx: &mut App);
    /// A `ServerSettingsPatch` as JSON (`updateSettings` RPC).
    fn update_server(&self, patch: Value, cx: &mut App);
    fn upsert_keybinding(&self, input: UpsertKeybinding, cx: &mut App);
    fn remove_keybinding(&self, input: RemoveKeybinding, cx: &mut App);
    /// `refreshServerProviders({ refreshModels: true })`.
    fn refresh_providers(&self, cx: &mut App);
    /// `sourceControlEnvironment.discovery` refresh (the Rescan button).
    fn rescan_source_control(&self, cx: &mut App);
    /// `updateProvider`: run the provider CLI's update command.
    fn update_provider(&self, instance_id: &str, target_version: Option<String>, cx: &mut App);
    /// Starts / ends the provider account sign-in (the in-app auth flow of
    /// ProviderAuthenticationSection).
    fn start_provider_auth(&self, instance_id: &str, cx: &mut App);
    fn logout_provider(&self, instance_id: &str, cx: &mut App);
    /// `useTheme().setTheme` / `setAppearanceMode`.
    fn set_theme(&self, theme: String, cx: &mut App);
    fn set_appearance_mode(&self, mode: String, cx: &mut App);
    /// Opens `keybindings.json` in the preferred editor.
    fn open_keybindings_file(&self, cx: &mut App);
    fn unarchive_thread(&self, thread_id: &str, cx: &mut App);
    /// `projectEnvironment.update` with a new `title`.
    fn rename_project(&self, project_id: &str, title: &str, cx: &mut App);
    /// `projectEnvironment.delete` (`force: true`).
    fn remove_project(&self, project_id: &str, cx: &mut App);
    /// `runStorageCleanup`.
    fn run_storage_cleanup(&self, cx: &mut App);
    fn delete_thread(&self, thread_id: &str, cx: &mut App);
}

pub struct SettingsStore {
    vm: Rc<SettingsViewModel>,
    source: Rc<dyn SettingsSource>,
}

impl SettingsStore {
    pub fn new(vm: SettingsViewModel, source: Rc<dyn SettingsSource>) -> Self {
        Self {
            vm: Rc::new(vm),
            source,
        }
    }

    pub fn vm(&self) -> &Rc<SettingsViewModel> {
        &self.vm
    }

    pub fn source(&self) -> Rc<dyn SettingsSource> {
        self.source.clone()
    }

    /// The source calls this when anything changed.
    pub fn publish(&mut self, vm: SettingsViewModel, cx: &mut Context<Self>) {
        self.vm = Rc::new(vm);
        cx.notify();
    }
}
