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
use indexmap::IndexMap;
use serde_json::Value;
use turin_contracts::provider_setup::{
    ProviderAuthRespondInput, ProviderAuthState, ProviderInstallState,
};
use turin_contracts::settings::{ClientSettings, ServerSettings};
use turin_provider_core::server_provider::ServerProvider;
use turin_shared::keybindings::ResolvedKeybindingRule;

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
    /// A `refreshServerProviders` request is in flight (`isRefreshingProviders`).
    pub providers_refreshing: bool,
    /// Live auth flows keyed by provider instance id (`providerAuthState` query).
    pub provider_auth: IndexMap<String, ProviderAuthState>,
    /// Managed runtime installs keyed by provider instance id
    /// (`providerInstallState` query).
    pub provider_install: IndexMap<String, ProviderInstallState>,
    /// `t3:chatgpt-sharing-welcome:v1`: the profile keys whose welcome dialog
    /// the user already dismissed (`ChatGptWelcomeCoordinator`).
    pub chat_gpt_welcome_acknowledged: Vec<String>,
    /// A one-time provider sign-in code handed to the app, waiting for
    /// `ProviderAuthCallbackCoordinator` (`pendingProviderAuthDelivery`).
    pub pending_provider_auth_delivery: Option<ProviderAuthDelivery>,
    /// `sourceControlEnvironment.discovery` of the local environment.
    pub source_control: turin_contracts::source_control::SourceControlDiscoveryResult,
    /// Threads per project id (`useThreadShells`), for the remove confirmation.
    pub project_thread_counts: IndexMap<String, usize>,
    /// The environment's projects as the sidebar groups them
    /// (`useProjects`); the settings scope picks among their logical groups.
    pub environment_projects: Vec<crate::state::models::EnvironmentProject>,
    pub archived_threads: Vec<ArchivedThread>,
    /// `useArchivedThreadSnapshots().isLoading`.
    pub archived_threads_loading: bool,
    /// `useArchivedThreadSnapshots().error`.
    pub archived_threads_error: Option<String>,
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

/// `readCodexAuthDelivery`'s result: the one-time code a sign-in page handed
/// back to the app, for the flow that is still waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAuthDelivery {
    pub instance_id: String,
    pub flow_id: String,
    pub callback_url: String,
}

/// What a setup request answers: accepted, or the message the card shows.
/// Progress itself arrives as a new view model.
pub type SetupResult = Result<(), String>;

/// `usePersistEnvironmentProviderInstanceMutation`'s operations: one instance is
/// added, replaced or removed against the latest stored map, so a stale copy of
/// the whole `providerInstances` map is never sent back.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderInstanceMutation {
    Upsert {
        instance_id: String,
        instance: Value,
    },
    Remove {
        instance_id: String,
    },
}

/// The user's intent. Implemented by the mock here and by the live services in T22.
pub trait SettingsSource {
    /// `useUpdateSettings` for client keys: a `ClientSettingsPatch` as JSON.
    fn update_client(&self, patch: Value, cx: &mut App);
    /// A `ServerSettingsPatch` as JSON (`updateSettings` RPC).
    fn update_server(&self, patch: Value, cx: &mut App);
    /// Applies one [`ProviderInstanceMutation`], together with `patch` (a
    /// `ServerSettingsPatch` as JSON, e.g. the text generation reset) when set.
    fn mutate_provider_instance(
        &self,
        mutation: ProviderInstanceMutation,
        patch: Option<Value>,
        cx: &mut App,
    );
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
    /// `startProviderAuth`; `method_id` is the chosen sign-in method (the
    /// provider default when `None`). The callback always returns to this app
    /// (`callbackMode: "server"`).
    fn start_provider_auth(
        &self,
        instance_id: &str,
        method_id: Option<&str>,
        cx: &mut App,
    ) -> SetupResult;
    fn cancel_provider_auth(&self, instance_id: &str, flow_id: &str, cx: &mut App) -> SetupResult;
    /// `respondProviderAuth`: answers the live interaction (browser consent,
    /// terminal input, credentials).
    fn respond_provider_auth(&self, input: ProviderAuthRespondInput, cx: &mut App) -> SetupResult;
    fn complete_provider_auth(
        &self,
        instance_id: &str,
        flow_id: &str,
        callback_url: &str,
        cx: &mut App,
    ) -> SetupResult;
    fn open_external_url(&self, url: &str, cx: &mut App);
    fn logout_provider(&self, instance_id: &str, cx: &mut App) -> SetupResult;
    /// `startProviderInstall`: download and verify the managed runtime.
    fn start_provider_install(&self, instance_id: &str, cx: &mut App) -> SetupResult;
    /// `cancelProviderInstall`.
    fn cancel_provider_install(
        &self,
        instance_id: &str,
        operation_id: &str,
        cx: &mut App,
    ) -> SetupResult;
    /// `removeProviderInstallation`: delete the downloaded runtime.
    fn remove_provider_installation(&self, instance_id: &str, cx: &mut App) -> SetupResult;
    /// Writes the dismissed welcome key to `t3:chatgpt-sharing-welcome:v1`.
    fn acknowledge_chat_gpt_welcome(&self, key: &str, cx: &mut App);
    /// `clearProviderAuthDelivery`.
    fn clear_provider_auth_delivery(&self, cx: &mut App);
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
    /// `projectEnvironment.update` with `faviconPath` and `projectIcon` for every
    /// checkout of a project group (`setProjectIcon`).
    fn set_project_icon(
        &self,
        project_ids: &[String],
        favicon_path: Option<String>,
        project_icon: Option<crate::state::models::ProjectIcon>,
        cx: &mut App,
    ) -> SetupResult;
    /// `useProjectFilePickerQuery` with `imageOnly`: the project's entries
    /// matching `query`, ranked.
    fn search_project_files(
        &self,
        cwd: &str,
        query: &str,
        cx: &mut App,
    ) -> Result<Vec<super::project_file_picker_logic::ProjectEntry>, String>;
    /// `desktopBridge.pickProjectFavicon`: the native image picker; `None` when
    /// the user cancels.
    fn pick_project_favicon(&self, cwd: &str, cx: &mut App) -> Result<Option<String>, String>;
    /// `runStorageCleanup`.
    fn run_storage_cleanup(&self, cx: &mut App);
    fn delete_thread(&self, thread_id: &str, cx: &mut App);
}

/// A failed request the user must hear about (`toastManager.add({ type: "error",
/// title, description })` after an atom command fails). The source picks the
/// title, e.g. "Could not update provider instance" or "Unable to save
/// keybinding", and the description, e.g. the error message or "The settings
/// update failed.".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFailure {
    pub title: String,
    pub description: String,
}

pub struct SettingsStore {
    vm: Rc<SettingsViewModel>,
    source: Rc<dyn SettingsSource>,
    failures: std::cell::RefCell<Vec<SettingsFailure>>,
}

impl SettingsStore {
    pub fn new(vm: SettingsViewModel, source: Rc<dyn SettingsSource>) -> Self {
        Self {
            vm: Rc::new(vm),
            source,
            failures: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub fn vm(&self) -> &Rc<SettingsViewModel> {
        &self.vm
    }

    pub fn source(&self) -> Rc<dyn SettingsSource> {
        self.source.clone()
    }

    /// The source calls this when a request failed; the view shows an error toast.
    pub fn report_failure(
        &mut self,
        title: impl Into<String>,
        description: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_failure(title, description);
        cx.notify();
    }

    fn push_failure(&self, title: impl Into<String>, description: impl Into<String>) {
        self.failures.borrow_mut().push(SettingsFailure {
            title: title.into(),
            description: description.into(),
        });
    }

    #[cfg(test)]
    pub(super) fn push_failure_for_tests(&self, title: &str, description: &str) {
        self.push_failure(title, description);
    }

    /// Failures reported since the last call.
    pub fn take_failures(&self) -> Vec<SettingsFailure> {
        std::mem::take(&mut *self.failures.borrow_mut())
    }

    /// The source calls this when anything changed.
    pub fn publish(&mut self, vm: SettingsViewModel, cx: &mut Context<Self>) {
        self.vm = Rc::new(vm);
        cx.notify();
    }
}
