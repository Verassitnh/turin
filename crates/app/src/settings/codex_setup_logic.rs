//! Port of upstream/apps/web/src/components/settings/CodexSetupSection.tsx
//! (`ManagedCodexSetup`'s derived state and effects).
//!
//! React keeps this state in hooks and `useEffect`s. Here the same state lives
//! in [`ManagedCodexState`], [`derive`] is the render-time arithmetic and
//! [`plan_effects`] runs the effect bodies (each one fires on the same
//! condition as upstream and updates the same refs).
//!
//! Not ported (PORTING.md Deviations): the primary-environment handoff
//! (`chatGptHandoffState`, `chatGptReconnectProfile`, `chatGptImportProfile`,
//! `codexAuthHandoffUrl`) and the browser callback bridge
//! (`desktopBridge.receiveProviderAuthCallback`). Turin is one local
//! environment, so `primaryAuthEnvironmentId` is always null,
//! `clientCallback`/`remoteWeb`/`needsManualCallback` are always false and sign-in
//! always uses `callbackMode: "server"`. The `onboarding` presentation belongs
//! to the Welcome wizard, which Turin does not have.

use turin_contracts::provider_setup::{
    ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthPhase, ProviderAuthState,
    ProviderInstallPhase, ProviderInstallSource, ProviderInstallState,
};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};

/// The method id of a plain "Continue with ChatGPT".
pub const CHATGPT_METHOD_ID: &str = "chatgpt";
/// The method id of "Use a different account".
pub const CHATGPT_CHANGE_ACCOUNT_METHOD_ID: &str = "chatgpt-change-account";
/// Saved ChatGPT profiles are listed as `chatgpt-profile:<id>` methods.
pub const CHATGPT_PROFILE_PREFIX: &str = "chatgpt-profile:";

/// What the sign-in attempt waits for after the auth receipt arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitingProvider {
    SignIn,
}

/// `useState`/`useRef` of `ManagedCodexSetup`, plus `requested` of `CodexSetupSection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCodexState {
    pub account_picker_open: bool,
    pub requested_method_id: String,
    pub callback_help_open: bool,
    /// `callbackDraft`: the flow the pasted URL belongs to, and the URL.
    pub callback_flow_id: String,
    pub callback_value: String,
    pub pending: bool,
    pub awaiting_provider: Option<AwaitingProvider>,
    pub error: Option<String>,
    /// `continueWithSignIn` ref: the method to sign in with once the install finishes.
    pub continue_with_sign_in: Option<String>,
    /// `openRequested` ref.
    pub open_requested: bool,
    /// `openedFlow` ref.
    pub opened_flow: Option<String>,
    pub auto_start_handled: bool,
    /// Set once the post-success provider refresh went out for this attempt.
    pub refresh_sent: bool,
}

impl Default for ManagedCodexState {
    fn default() -> Self {
        Self {
            account_picker_open: false,
            requested_method_id: CHATGPT_METHOD_ID.to_owned(),
            callback_help_open: false,
            callback_flow_id: String::new(),
            callback_value: String::new(),
            pending: false,
            awaiting_provider: None,
            error: None,
            continue_with_sign_in: None,
            open_requested: false,
            opened_flow: None,
            auto_start_handled: false,
            refresh_sent: false,
        }
    }
}

/// The props and query results `ManagedCodexSetup` reads each render.
#[derive(Clone, Copy)]
pub struct ManagedInputs<'a> {
    pub provider: Option<&'a ServerProvider>,
    pub auth: Option<&'a ProviderAuthState>,
    /// `installQuery.data`; `None` until the first install state arrives.
    pub installation: Option<&'a ProviderInstallState>,
    pub enabled: bool,
    pub read_only: bool,
    pub auto_start: bool,
}

/// The account row's description (`accountDescription`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountDescription {
    Text(String),
    /// "Signed in as `<redacted email>`."
    SignedInAs(String),
}

/// Everything `ManagedCodexSetup` computes between its hooks and its JSX.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedDerived {
    pub installed: bool,
    pub authenticated: bool,
    pub update_available: bool,
    pub finishing_sign_in: bool,
    pub starting_automatically: bool,
    pub waiting_for_auth_state: bool,
    pub auth_in_progress: bool,
    pub install_active: bool,
    pub auth_active: bool,
    pub unavailable: bool,
    pub busy: bool,
    pub has_saved_account: bool,
    pub url: Option<String>,
    pub flow_id: Option<String>,
    pub phase: Option<ProviderAuthPhase>,
    pub reconnect_email: Option<String>,
    pub requested_account_email: Option<String>,
    pub runtime_description: String,
    pub account_description: AccountDescription,
    pub logout_warning: Option<String>,
    pub callback_available: bool,
}

fn method_account_email(methods: Option<&[ProviderAuthMethod]>, id: &str) -> Option<String> {
    methods?
        .iter()
        .find(|method| method.id.as_str() == id)?
        .account_email
        .as_ref()
        .map(|email| email.as_str().to_owned())
}

/// `auth?.interaction?.type === "browser" ? interaction.url : auth?.authorizationUrl`.
pub fn browser_url(auth: Option<&ProviderAuthState>) -> Option<String> {
    let auth = auth?;
    if let Some(Some(ProviderAuthInteraction::Browser { url, .. })) = &auth.interaction {
        return Some(url.as_str().to_owned());
    }
    auth.authorization_url.clone()
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// `installation?.phase === "downloading" ? "Downloading X of Y MB." …`.
pub fn runtime_description(installation: Option<&ProviderInstallState>, installed: bool) -> String {
    match installation.map(|installation| installation.phase) {
        Some(ProviderInstallPhase::Downloading) => {
            let installation = installation.expect("downloading has an installation");
            let total = installation
                .total_bytes
                .map(|total| format!(" of {}", megabytes(total)))
                .unwrap_or_default();
            format!(
                "Downloading {}{} MB.",
                megabytes(installation.downloaded_bytes),
                total
            )
        }
        Some(ProviderInstallPhase::Extracting) => "Installing Codex.".to_owned(),
        Some(ProviderInstallPhase::Verifying) => "Checking Codex.".to_owned(),
        _ if installed => {
            let installation = installation.expect("installed has an installation");
            let source = if matches!(
                installation.source,
                Some(Some(ProviderInstallSource::Local))
            ) {
                "Using your installed Codex"
            } else {
                "Managed by T3 Code"
            };
            let version = installation
                .installed_version
                .as_ref()
                .map(|version| format!(" · v{}", version.as_str()))
                .unwrap_or_default();
            format!("{source}{version}.")
        }
        _ => installation
            .and_then(|installation| installation.message.clone())
            .unwrap_or_else(|| "T3 Code downloads and manages Codex for you.".to_owned()),
    }
}

/// `auth?.phase === "idle" && auth.message?.startsWith("Signed out locally.")`.
pub fn logout_warning(auth: Option<&ProviderAuthState>) -> Option<String> {
    let auth = auth?;
    (auth.phase == ProviderAuthPhase::Idle)
        .then(|| auth.message.clone())
        .flatten()
        .filter(|message| message.starts_with("Signed out locally."))
}

/// The render-time arithmetic of `ManagedCodexSetup`.
pub fn derive(inputs: &ManagedInputs<'_>, state: &ManagedCodexState) -> ManagedDerived {
    let auth = inputs.auth;
    let phase = auth.map(|auth| auth.phase);
    let methods = auth.and_then(|auth| auth.methods.as_deref());
    let installation = inputs.installation;
    let installed =
        installation.is_some_and(|installation| installation.installed_version.is_some());
    let authenticated = inputs
        .provider
        .is_some_and(|provider| provider.auth.status == ServerProviderAuthStatus::Authenticated);
    let update_available = installed
        && installation.is_some_and(|installation| {
            !matches!(
                installation.source,
                Some(Some(ProviderInstallSource::Local))
            ) && installation
                .version
                .as_ref()
                .is_some_and(|version| Some(version) != installation.installed_version.as_ref())
        });
    // Auth receipts and provider snapshots arrive independently. Keep the
    // current attempt pending until its authenticated snapshot arrives, even
    // after success.
    let finishing_sign_in = state.awaiting_provider.is_some()
        && !authenticated
        && phase == Some(ProviderAuthPhase::Succeeded);
    let starting_automatically = inputs.auto_start && !state.auto_start_handled;
    let waiting_for_auth_state = state.awaiting_provider == Some(AwaitingProvider::SignIn)
        && !authenticated
        && (auth.is_none() || phase == Some(ProviderAuthPhase::Idle));
    let auth_in_progress = finishing_sign_in
        || waiting_for_auth_state
        || matches!(
            phase,
            Some(
                ProviderAuthPhase::Starting
                    | ProviderAuthPhase::Waiting
                    | ProviderAuthPhase::Verifying
            )
        );
    let install_active = matches!(
        installation.map(|installation| installation.phase),
        Some(
            ProviderInstallPhase::Downloading
                | ProviderInstallPhase::Extracting
                | ProviderInstallPhase::Verifying
        )
    );
    let auth_active =
        starting_automatically || auth_in_progress || (state.pending && !install_active);
    let unavailable = inputs.read_only || !inputs.enabled || state.pending;
    let busy = state.pending || auth_in_progress || install_active;
    let url = browser_url(auth);
    let flow_id = auth
        .and_then(|auth| auth.flow_id.as_ref())
        .map(|id| id.as_str().to_owned());
    let reconnect_email = method_account_email(methods, CHATGPT_METHOD_ID);
    let requested_account_email = method_account_email(methods, &state.requested_method_id);
    let has_saved_account = methods.is_some_and(|methods| {
        methods
            .iter()
            .any(|method| method.id.as_str().starts_with(CHATGPT_PROFILE_PREFIX))
    });
    let runtime_description = runtime_description(installation, installed);
    let provider_email = inputs
        .provider
        .and_then(|provider| provider.auth.email.as_ref())
        .map(|email| email.as_str().trim().to_owned())
        .filter(|email| !email.is_empty());

    let account_description = if finishing_sign_in {
        AccountDescription::Text("Finishing sign-in...".to_owned())
    } else if install_active {
        AccountDescription::Text(runtime_description.clone())
    } else if auth_active
        || matches!(
            phase,
            Some(ProviderAuthPhase::Failed | ProviderAuthPhase::Cancelled)
        )
    {
        match (phase, &requested_account_email) {
            (Some(ProviderAuthPhase::Waiting), Some(email)) => {
                AccountDescription::Text(format!("Continue as {email} on OpenAI."))
            }
            _ => AccountDescription::Text(
                auth.and_then(|auth| auth.message.clone())
                    .unwrap_or_else(|| "Finish signing in in your browser.".to_owned()),
            ),
        }
    } else if authenticated {
        match provider_email {
            Some(email) => AccountDescription::SignedInAs(email),
            None => AccountDescription::Text("Signed in with ChatGPT.".to_owned()),
        }
    } else {
        AccountDescription::Text(
            reconnect_email
                .clone()
                .unwrap_or_else(|| "Use your ChatGPT subscription.".to_owned()),
        )
    };

    // `!handoff && auth?.phase === "waiting" && url`
    let callback_available = phase == Some(ProviderAuthPhase::Waiting) && url.is_some();

    ManagedDerived {
        installed,
        authenticated,
        update_available,
        finishing_sign_in,
        starting_automatically,
        waiting_for_auth_state,
        auth_in_progress,
        install_active,
        auth_active,
        unavailable,
        busy,
        has_saved_account,
        url,
        flow_id,
        phase,
        reconnect_email,
        requested_account_email,
        runtime_description,
        account_description,
        logout_warning: logout_warning(auth),
        callback_available,
    }
}

/// The `useEffect`s of `ManagedCodexSetup`, as the work the view performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexSetupEffect {
    /// `refreshProviders({ environmentId, input: { instanceId } })`.
    RefreshProviders,
    /// `setAutoStartHandled(true); onAutoStartConsumed(); void setup()`.
    AutoStart,
    /// `void signIn(methodId)` once the install finished.
    ContinueWithSignIn(String),
    /// `void openPage(url)` for the flow that just started waiting.
    OpenPage { url: String },
}

/// `setup()`'s decision: sign in straight away, or install the runtime first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStep {
    Skip,
    SignIn,
    Install,
}

/// `setup(methodId)`'s first branch.
pub fn setup_step(derived: &ManagedDerived) -> SetupStep {
    if derived.unavailable || derived.busy {
        SetupStep::Skip
    } else if derived.installed && !derived.update_available {
        SetupStep::SignIn
    } else {
        SetupStep::Install
    }
}

/// Runs the effect bodies against `state` and returns what the view must do.
pub fn plan_effects(
    inputs: &ManagedInputs<'_>,
    state: &mut ManagedCodexState,
) -> Vec<CodexSetupEffect> {
    let mut effects = Vec::new();
    let derived = derive(inputs, state);

    // `if (authenticated || auth?.phase === "failed" || … "cancelled") setAwaitingProvider(null)`
    if derived.authenticated
        || matches!(
            derived.phase,
            Some(ProviderAuthPhase::Failed | ProviderAuthPhase::Cancelled)
        )
    {
        state.awaiting_provider = None;
        state.refresh_sent = false;
    }
    // `if (awaitingProvider && auth?.phase === "succeeded") void refreshProviders(…)`
    if state.awaiting_provider.is_some() {
        if derived.phase == Some(ProviderAuthPhase::Succeeded) && !state.refresh_sent {
            state.refresh_sent = true;
            effects.push(CodexSetupEffect::RefreshProviders);
        }
    } else {
        state.refresh_sent = false;
    }

    // Auto start.
    if inputs.auto_start
        && !state.auto_start_handled
        && !derived.unavailable
        && !derived.busy
        && inputs.installation.is_some()
        && inputs
            .provider
            .and_then(|provider| provider.setup.as_ref())
            .is_some_and(|setup| setup.can_install)
    {
        state.auto_start_handled = true;
        effects.push(CodexSetupEffect::AutoStart);
    }

    // Continue with sign-in once the install settles.
    if state.continue_with_sign_in.is_some() && !state.pending && !derived.install_active {
        let phase = inputs.installation.map(|installation| installation.phase);
        if matches!(
            phase,
            Some(ProviderInstallPhase::Failed | ProviderInstallPhase::Cancelled)
        ) {
            state.continue_with_sign_in = None;
        } else if derived.installed {
            let method = state.continue_with_sign_in.take().unwrap_or_default();
            effects.push(CodexSetupEffect::ContinueWithSignIn(method));
        }
    }

    // Open the sign-in page once for the flow that was requested.
    if state.open_requested
        && derived.phase == Some(ProviderAuthPhase::Waiting)
        && derived.flow_id.is_some()
        && let Some(url) = derived.url.clone()
        && state.opened_flow.as_deref() != derived.flow_id.as_deref()
    {
        state.opened_flow = derived.flow_id.clone();
        state.open_requested = false;
        effects.push(CodexSetupEffect::OpenPage { url });
    }
    effects
}

/// The pasted redirect URL for the current flow (`callbackDraft.flowId === auth?.flowId`).
pub fn callback_url(state: &ManagedCodexState, flow_id: Option<&str>) -> String {
    if flow_id == Some(state.callback_flow_id.as_str()) {
        state.callback_value.clone()
    } else {
        String::new()
    }
}

/// The primary button's label while the managed runtime is installed or signing in
/// (`waitingControl`).
pub fn waiting_control_label(derived: &ManagedDerived) -> &'static str {
    if derived.finishing_sign_in {
        "Finishing sign-in..."
    } else if derived.phase == Some(ProviderAuthPhase::Waiting) {
        "Open sign-in page"
    } else {
        "Signing in..."
    }
}

/// `waitingControl`'s `disabled`.
pub fn waiting_control_disabled(derived: &ManagedDerived) -> bool {
    derived.url.is_none() || derived.phase != Some(ProviderAuthPhase::Waiting)
}

/// The label of the connect button in the idle row.
pub fn connect_button_label(derived: &ManagedDerived, pending: bool) -> &'static str {
    if pending {
        "Setting up..."
    } else if derived.has_saved_account {
        "Reconnect account"
    } else {
        "Continue with ChatGPT"
    }
}

/// `disabled` of the connect button.
pub fn connect_button_disabled(
    derived: &ManagedDerived,
    provider: Option<&ServerProvider>,
) -> bool {
    derived.unavailable
        || derived.busy
        || if derived.installed {
            !provider
                .and_then(|provider| provider.setup.as_ref())
                .is_some_and(|setup| setup.can_authenticate)
        } else {
            !provider
                .and_then(|provider| provider.setup.as_ref())
                .is_some_and(|setup| setup.can_install)
        }
}

/// The method `ChatGptAccountPicker` hands to `setup()`:
/// `auth.methods.some(m => m.id === methodId) ? methodId : "chatgpt-change-account"`.
pub fn resolve_picked_method(methods: &[ProviderAuthMethod], picked: &str) -> String {
    if methods.iter().any(|method| method.id.as_str() == picked) {
        picked.to_owned()
    } else {
        CHATGPT_CHANGE_ACCOUNT_METHOD_ID.to_owned()
    }
}

/// `ChatGptAccountPicker`'s selection: the explicit change-account choice, a
/// still-listed profile, the first profile, else change-account.
pub fn account_picker_selection(methods: &[ProviderAuthMethod], selection: Option<&str>) -> String {
    let profiles: Vec<&ProviderAuthMethod> = methods
        .iter()
        .filter(|method| method.id.as_str().starts_with(CHATGPT_PROFILE_PREFIX))
        .collect();
    if selection == Some(CHATGPT_CHANGE_ACCOUNT_METHOD_ID) {
        return CHATGPT_CHANGE_ACCOUNT_METHOD_ID.to_owned();
    }
    profiles
        .iter()
        .find(|profile| Some(profile.id.as_str()) == selection)
        .or(profiles.first())
        .map(|profile| profile.id.as_str().to_owned())
        .unwrap_or_else(|| CHATGPT_CHANGE_ACCOUNT_METHOD_ID.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(auth_status: &str, can_install: bool, can_authenticate: bool) -> ServerProvider {
        serde_json::from_value(json!({
            "instanceId": "codex_work", "driver": "codex", "enabled": true, "installed": true,
            "version": "0.134.0", "status": "ready",
            "auth": { "status": auth_status, "email": "dev@example.com" },
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": [],
            "setup": { "canAuthenticate": can_authenticate, "canInstall": can_install }
        }))
        .expect("provider")
    }

    fn auth(phase: &str, extra: serde_json::Value) -> ProviderAuthState {
        let mut value = json!({ "instanceId": "codex_work", "phase": phase });
        if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            object.extend(extra.clone());
        }
        serde_json::from_value(value).expect("auth state")
    }

    fn installation(
        phase: &str,
        installed_version: Option<&str>,
        extra: serde_json::Value,
    ) -> ProviderInstallState {
        let mut value = json!({
            "driver": "codex", "operationId": null, "phase": phase, "downloadedBytes": 0,
            "totalBytes": null, "version": null, "installedVersion": installed_version,
            "canRemove": false, "message": null
        });
        if let (Some(object), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            object.extend(extra.clone());
        }
        serde_json::from_value(value).expect("install state")
    }

    fn inputs<'a>(
        provider: Option<&'a ServerProvider>,
        auth: Option<&'a ProviderAuthState>,
        installation: Option<&'a ProviderInstallState>,
    ) -> ManagedInputs<'a> {
        ManagedInputs {
            provider,
            auth,
            installation,
            enabled: true,
            read_only: false,
            auto_start: false,
        }
    }

    #[test]
    fn describes_the_runtime_by_install_phase() {
        let downloading = installation(
            "downloading",
            None,
            json!({ "downloadedBytes": 12_500_000, "totalBytes": 50_000_000 }),
        );
        assert_eq!(
            runtime_description(Some(&downloading), false),
            "Downloading 12.5 of 50.0 MB."
        );
        let unknown_total =
            installation("downloading", None, json!({ "downloadedBytes": 3_000_000 }));
        assert_eq!(
            runtime_description(Some(&unknown_total), false),
            "Downloading 3.0 MB."
        );
        assert_eq!(
            runtime_description(Some(&installation("extracting", None, json!({}))), false),
            "Installing Codex."
        );
        assert_eq!(
            runtime_description(Some(&installation("verifying", None, json!({}))), false),
            "Checking Codex."
        );
        let managed = installation("succeeded", Some("0.134.0"), json!({ "source": "managed" }));
        assert_eq!(
            runtime_description(Some(&managed), true),
            "Managed by T3 Code · v0.134.0."
        );
        let local = installation("succeeded", Some("0.130.0"), json!({ "source": "local" }));
        assert_eq!(
            runtime_description(Some(&local), true),
            "Using your installed Codex · v0.130.0."
        );
        let failed = installation("failed", None, json!({ "message": "Network down" }));
        assert_eq!(runtime_description(Some(&failed), false), "Network down");
        assert_eq!(
            runtime_description(None, false),
            "T3 Code downloads and manages Codex for you."
        );
    }

    #[test]
    fn keeps_the_attempt_pending_until_the_authenticated_snapshot_arrives() {
        let provider = provider("unauthenticated", true, true);
        let succeeded = auth("succeeded", json!({}));
        let install = installation("succeeded", Some("0.134.0"), json!({}));
        let state = ManagedCodexState {
            awaiting_provider: Some(AwaitingProvider::SignIn),
            ..Default::default()
        };
        let derived = derive(
            &inputs(Some(&provider), Some(&succeeded), Some(&install)),
            &state,
        );
        assert!(derived.finishing_sign_in);
        assert!(derived.auth_in_progress && derived.auth_active && derived.busy);
        assert_eq!(
            derived.account_description,
            AccountDescription::Text("Finishing sign-in...".into())
        );
        assert_eq!(waiting_control_label(&derived), "Finishing sign-in...");

        let provider = self::provider("authenticated", true, true);
        let derived = derive(
            &inputs(Some(&provider), Some(&succeeded), Some(&install)),
            &state,
        );
        assert!(!derived.finishing_sign_in && !derived.auth_active);
        assert_eq!(
            derived.account_description,
            AccountDescription::SignedInAs("dev@example.com".into())
        );
    }

    #[test]
    fn describes_a_waiting_flow_with_the_requested_account() {
        let waiting = auth(
            "waiting",
            json!({
                "flowId": "flow-1",
                "authorizationUrl": "https://auth.openai.com/authorize",
                "methods": [
                    { "id": "chatgpt", "name": "ChatGPT", "type": "agent", "accountEmail": "me@example.com" },
                    { "id": "chatgpt-profile:abc", "name": "Work", "type": "agent", "accountEmail": "work@example.com" }
                ]
            }),
        );
        let provider = provider("unauthenticated", true, true);
        let install = installation("succeeded", Some("0.134.0"), json!({}));
        let mut state = ManagedCodexState::default();
        let derived = derive(
            &inputs(Some(&provider), Some(&waiting), Some(&install)),
            &state,
        );
        assert_eq!(
            derived.account_description,
            AccountDescription::Text("Continue as me@example.com on OpenAI.".into())
        );
        assert!(derived.has_saved_account);
        assert!(!waiting_control_disabled(&derived));
        assert_eq!(waiting_control_label(&derived), "Open sign-in page");

        state.requested_method_id = "chatgpt-profile:abc".into();
        let derived = derive(
            &inputs(Some(&provider), Some(&waiting), Some(&install)),
            &state,
        );
        assert_eq!(
            derived.account_description,
            AccountDescription::Text("Continue as work@example.com on OpenAI.".into())
        );
    }

    #[test]
    fn offers_the_saved_account_email_when_idle() {
        let idle = auth(
            "idle",
            json!({ "methods": [{ "id": "chatgpt", "name": "ChatGPT", "type": "agent", "accountEmail": "me@example.com" }] }),
        );
        let provider = provider("unauthenticated", true, true);
        let install = installation("succeeded", Some("0.134.0"), json!({}));
        let derived = derive(
            &inputs(Some(&provider), Some(&idle), Some(&install)),
            &ManagedCodexState::default(),
        );
        assert_eq!(
            derived.account_description,
            AccountDescription::Text("me@example.com".into())
        );
        let derived = derive(
            &inputs(Some(&provider), None, Some(&install)),
            &ManagedCodexState::default(),
        );
        assert_eq!(
            derived.account_description,
            AccountDescription::Text("Use your ChatGPT subscription.".into())
        );
    }

    #[test]
    fn setup_signs_in_when_installed_and_installs_otherwise() {
        let provider = provider("unauthenticated", true, true);
        let installed = installation("succeeded", Some("0.134.0"), json!({ "source": "managed" }));
        let state = ManagedCodexState::default();
        let derived = derive(&inputs(Some(&provider), None, Some(&installed)), &state);
        assert_eq!(setup_step(&derived), SetupStep::SignIn);

        // A newer managed version reinstalls first; a local install never updates.
        let update = installation(
            "succeeded",
            Some("0.134.0"),
            json!({ "source": "managed", "version": "0.136.0" }),
        );
        let derived = derive(&inputs(Some(&provider), None, Some(&update)), &state);
        assert!(derived.update_available);
        assert_eq!(setup_step(&derived), SetupStep::Install);
        let local = installation(
            "succeeded",
            Some("0.134.0"),
            json!({ "source": "local", "version": "0.136.0" }),
        );
        let derived = derive(&inputs(Some(&provider), None, Some(&local)), &state);
        assert!(!derived.update_available);
        assert_eq!(setup_step(&derived), SetupStep::SignIn);

        let none = installation("idle", None, json!({}));
        let derived = derive(&inputs(Some(&provider), None, Some(&none)), &state);
        assert_eq!(setup_step(&derived), SetupStep::Install);

        let busy = ManagedCodexState {
            pending: true,
            ..Default::default()
        };
        let derived = derive(&inputs(Some(&provider), None, Some(&none)), &busy);
        assert_eq!(setup_step(&derived), SetupStep::Skip);
    }

    #[test]
    fn auto_start_waits_for_the_install_state_and_fires_once() {
        let provider = provider("unauthenticated", true, true);
        let mut state = ManagedCodexState::default();
        let mut input = inputs(Some(&provider), None, None);
        input.auto_start = true;
        assert!(plan_effects(&input, &mut state).is_empty());
        let install = installation("idle", None, json!({}));
        input.installation = Some(&install);
        assert_eq!(
            plan_effects(&input, &mut state),
            vec![CodexSetupEffect::AutoStart]
        );
        assert!(state.auto_start_handled);
        assert!(plan_effects(&input, &mut state).is_empty());

        // Without install support nothing starts.
        let provider = self::provider("unauthenticated", false, true);
        let mut state = ManagedCodexState::default();
        let mut input = inputs(Some(&provider), None, Some(&install));
        input.auto_start = true;
        assert!(plan_effects(&input, &mut state).is_empty());
    }

    #[test]
    fn continues_with_sign_in_after_the_install_finishes() {
        let provider = provider("unauthenticated", true, true);
        let mut state = ManagedCodexState {
            continue_with_sign_in: Some("chatgpt".into()),
            ..Default::default()
        };
        let downloading = installation("downloading", None, json!({}));
        assert!(
            plan_effects(
                &inputs(Some(&provider), None, Some(&downloading)),
                &mut state
            )
            .is_empty()
        );
        assert_eq!(state.continue_with_sign_in.as_deref(), Some("chatgpt"));

        let done = installation("succeeded", Some("0.134.0"), json!({}));
        assert_eq!(
            plan_effects(&inputs(Some(&provider), None, Some(&done)), &mut state),
            vec![CodexSetupEffect::ContinueWithSignIn("chatgpt".into())]
        );
        assert_eq!(state.continue_with_sign_in, None);

        let mut state = ManagedCodexState {
            continue_with_sign_in: Some("chatgpt".into()),
            ..Default::default()
        };
        let failed = installation("failed", None, json!({}));
        assert!(plan_effects(&inputs(Some(&provider), None, Some(&failed)), &mut state).is_empty());
        assert_eq!(state.continue_with_sign_in, None);
    }

    #[test]
    fn opens_the_sign_in_page_once_per_flow() {
        let provider = provider("unauthenticated", true, true);
        let install = installation("succeeded", Some("0.134.0"), json!({}));
        let waiting = auth(
            "waiting",
            json!({ "flowId": "flow-1", "authorizationUrl": "https://auth.openai.com/authorize" }),
        );
        let mut state = ManagedCodexState {
            open_requested: true,
            ..Default::default()
        };
        assert_eq!(
            plan_effects(
                &inputs(Some(&provider), Some(&waiting), Some(&install)),
                &mut state
            ),
            vec![CodexSetupEffect::OpenPage {
                url: "https://auth.openai.com/authorize".into()
            }]
        );
        assert!(!state.open_requested);
        assert_eq!(state.opened_flow.as_deref(), Some("flow-1"));
        state.open_requested = true;
        assert!(
            plan_effects(
                &inputs(Some(&provider), Some(&waiting), Some(&install)),
                &mut state
            )
            .is_empty()
        );
    }

    #[test]
    fn refreshes_providers_once_the_receipt_succeeds_and_stops_when_authenticated() {
        let unauthenticated = provider("unauthenticated", true, true);
        let install = installation("succeeded", Some("0.134.0"), json!({}));
        let succeeded = auth("succeeded", json!({}));
        let mut state = ManagedCodexState {
            awaiting_provider: Some(AwaitingProvider::SignIn),
            ..Default::default()
        };
        assert_eq!(
            plan_effects(
                &inputs(Some(&unauthenticated), Some(&succeeded), Some(&install)),
                &mut state
            ),
            vec![CodexSetupEffect::RefreshProviders]
        );
        assert!(
            plan_effects(
                &inputs(Some(&unauthenticated), Some(&succeeded), Some(&install)),
                &mut state
            )
            .is_empty()
        );
        let authenticated = provider("authenticated", true, true);
        assert!(
            plan_effects(
                &inputs(Some(&authenticated), Some(&succeeded), Some(&install)),
                &mut state
            )
            .is_empty()
        );
        assert_eq!(state.awaiting_provider, None);
    }

    #[test]
    fn clears_the_wait_when_the_flow_fails_or_is_cancelled() {
        let provider = provider("unauthenticated", true, true);
        for phase in ["failed", "cancelled"] {
            let mut state = ManagedCodexState {
                awaiting_provider: Some(AwaitingProvider::SignIn),
                ..Default::default()
            };
            let ended = auth(phase, json!({ "message": "No." }));
            plan_effects(&inputs(Some(&provider), Some(&ended), None), &mut state);
            assert_eq!(state.awaiting_provider, None, "{phase}");
        }
    }

    #[test]
    fn reads_the_logout_warning_only_from_an_idle_signed_out_locally_receipt() {
        let idle = auth(
            "idle",
            json!({ "message": "Signed out locally. Revoke it in ChatGPT." }),
        );
        assert_eq!(
            logout_warning(Some(&idle)).as_deref(),
            Some("Signed out locally. Revoke it in ChatGPT.")
        );
        let other = auth("idle", json!({ "message": "Something else." }));
        assert_eq!(logout_warning(Some(&other)), None);
        let waiting = auth("waiting", json!({ "message": "Signed out locally." }));
        assert_eq!(logout_warning(Some(&waiting)), None);
    }

    #[test]
    fn disables_the_connect_button_for_the_missing_capability() {
        let state = ManagedCodexState::default();
        let none = installation("idle", None, json!({}));
        let installed = installation("succeeded", Some("0.134.0"), json!({}));

        let cannot_install = provider("unauthenticated", false, true);
        let derived = derive(&inputs(Some(&cannot_install), None, Some(&none)), &state);
        assert!(connect_button_disabled(&derived, Some(&cannot_install)));

        let cannot_authenticate = provider("unauthenticated", true, false);
        let derived = derive(
            &inputs(Some(&cannot_authenticate), None, Some(&installed)),
            &state,
        );
        assert!(connect_button_disabled(
            &derived,
            Some(&cannot_authenticate)
        ));

        let capable = provider("unauthenticated", true, true);
        let derived = derive(&inputs(Some(&capable), None, Some(&installed)), &state);
        assert!(!connect_button_disabled(&derived, Some(&capable)));
        assert_eq!(
            connect_button_label(&derived, false),
            "Continue with ChatGPT"
        );
        assert_eq!(connect_button_label(&derived, true), "Setting up...");
    }

    #[test]
    fn the_account_picker_defaults_to_the_first_saved_profile() {
        let waiting = auth(
            "idle",
            json!({ "methods": [
                { "id": "chatgpt", "name": "ChatGPT", "type": "agent" },
                { "id": "chatgpt-profile:a", "name": "a@x.com", "type": "agent" },
                { "id": "chatgpt-profile:b", "name": "b@x.com", "type": "agent" }
            ] }),
        );
        let methods = waiting.methods.as_deref().unwrap();
        assert_eq!(account_picker_selection(methods, None), "chatgpt-profile:a");
        assert_eq!(
            account_picker_selection(methods, Some("chatgpt-profile:b")),
            "chatgpt-profile:b"
        );
        assert_eq!(
            account_picker_selection(methods, Some("chatgpt-profile:gone")),
            "chatgpt-profile:a"
        );
        assert_eq!(
            account_picker_selection(methods, Some("chatgpt-change-account")),
            "chatgpt-change-account"
        );
        assert_eq!(
            account_picker_selection(&[], None),
            "chatgpt-change-account"
        );
        assert_eq!(
            resolve_picked_method(methods, "chatgpt-profile:b"),
            "chatgpt-profile:b"
        );
        assert_eq!(
            resolve_picked_method(methods, "bogus"),
            "chatgpt-change-account"
        );
    }
}
