//! Port of upstream/apps/web/src/onboarding/providerReadiness.logic.ts
//!
//! Upstream reads the full `ServerProvider`; the wizard only needs the fields
//! below, so [`OnboardingProvider`] is the seam the live provider registry
//! (T22) maps its snapshots onto.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_contracts::settings::{ClaudeSettings, CodexSettings};

use crate::state::helpers::provider_types::ServerProviderState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

/// `getProviderSummary(provider)` (settings/providerStatus.ts): the one-line
/// state the agent card shows when a provider is not ready.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSummary {
    pub headline: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OnboardingProvider {
    pub instance_id: String,
    /// `ProviderDriverKind` slug (`codex`, `claudeAgent`, …).
    pub driver: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub installed: bool,
    pub status: ServerProviderState,
    pub auth_status: ProviderAuthStatus,
    pub summary: ProviderSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingProviderState {
    Checking,
    Disabled,
    Install,
    Attention,
    SignIn,
    Ready,
}

impl OnboardingProviderState {
    /// `PROVIDER_STATE_PRIORITY`: higher is more usable.
    fn priority(self) -> u8 {
        match self {
            Self::Checking => 0,
            Self::Disabled => 1,
            Self::Install => 2,
            Self::Attention => 3,
            Self::SignIn => 4,
            Self::Ready => 5,
        }
    }
}

static SAFE_SHELL_BINARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_./:\\-]+$").expect("static pattern"));

fn quote_provider_binary(
    binary_path: &str,
    fallback: &str,
    platform: ExecutionEnvironmentPlatformOs,
) -> String {
    use ExecutionEnvironmentPlatformOs::{Darwin, Linux, Windows};
    if SAFE_SHELL_BINARY.is_match(binary_path)
        && (platform == Windows || !binary_path.contains('\\'))
    {
        return binary_path.to_owned();
    }
    match platform {
        Windows => format!("& '{}'", binary_path.replace('\'', "''")),
        Darwin | Linux => {
            if let Some(rest) = binary_path
                .strip_prefix("~/")
                .or_else(|| binary_path.strip_prefix("~\\"))
            {
                return format!("~/'{}'", rest.replace('\'', "'\"'\"'"));
            }
            format!("'{}'", binary_path.replace('\'', "'\"'\"'"))
        }
        _ => fallback.to_owned(),
    }
}

pub fn get_onboarding_provider_state(
    provider: Option<&OnboardingProvider>,
) -> OnboardingProviderState {
    let Some(provider) = provider else {
        return OnboardingProviderState::Checking;
    };
    if !provider.enabled || provider.status == ServerProviderState::Disabled {
        return OnboardingProviderState::Disabled;
    }
    if !provider.installed
        && provider.status == ServerProviderState::Warning
        && provider.auth_status == ProviderAuthStatus::Unknown
    {
        return OnboardingProviderState::Checking;
    }
    if !provider.installed {
        return OnboardingProviderState::Install;
    }
    if provider.auth_status == ProviderAuthStatus::Unauthenticated {
        return OnboardingProviderState::SignIn;
    }
    if provider.status == ServerProviderState::Ready {
        return OnboardingProviderState::Ready;
    }
    OnboardingProviderState::Attention
}

/// Select the most usable configured instance for each provider driver. Ties
/// keep the earlier instance.
pub fn select_onboarding_providers_by_driver(
    providers: Option<&[OnboardingProvider]>,
) -> HashMap<&str, &OnboardingProvider> {
    let mut by_driver: HashMap<&str, &OnboardingProvider> = HashMap::new();
    for provider in providers.unwrap_or_default() {
        let better = by_driver
            .get(provider.driver.as_str())
            .is_none_or(|existing| {
                get_onboarding_provider_state(Some(provider)).priority()
                    > get_onboarding_provider_state(Some(existing)).priority()
            });
        if better {
            by_driver.insert(provider.driver.as_str(), provider);
        }
    }
    by_driver
}

/// Drivers with an official standalone installer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingInstallDriver {
    ClaudeAgent,
    Codex,
}

/// Official standalone installers. Neither needs Node or npm, and both land in
/// the paths the provider maintenance recognizes as native, so the one-click
/// updater in Settings keeps working after install.
///
/// Install command for the setup step, keyed on the platform (not the client's):
/// unknown platforms get the shell script too.
pub fn resolve_onboarding_provider_install_command(
    driver: OnboardingInstallDriver,
    platform: ExecutionEnvironmentPlatformOs,
) -> &'static str {
    let windows = platform == ExecutionEnvironmentPlatformOs::Windows;
    match (driver, windows) {
        (OnboardingInstallDriver::ClaudeAgent, true) => "irm https://claude.ai/install.ps1 | iex",
        (OnboardingInstallDriver::ClaudeAgent, false) => {
            "curl -fsSL https://claude.ai/install.sh | bash"
        }
        (OnboardingInstallDriver::Codex, true) => "irm https://chatgpt.com/codex/install.ps1 | iex",
        (OnboardingInstallDriver::Codex, false) => {
            "curl -fsSL https://chatgpt.com/codex/install.sh | sh"
        }
    }
}

/// Use the selected provider instance's binary for its login command.
/// `instance_config` is the instance's `config` from the server settings
/// (`settings.providerInstances[instanceId]?.config`).
pub fn resolve_onboarding_provider_login_command(
    driver: &str,
    instance_config: Option<&Value>,
    platform: ExecutionEnvironmentPlatformOs,
) -> String {
    let empty = Value::Object(Default::default());
    let config = instance_config.unwrap_or(&empty);
    match driver {
        "claudeAgent" => {
            let binary_path = serde_json::from_value::<ClaudeSettings>(config.clone())
                .map(|settings| settings.binary_path.as_str().to_owned())
                .unwrap_or_else(|_| "claude".to_owned());
            format!(
                "{} auth login",
                quote_provider_binary(&binary_path, "claude", platform)
            )
        }
        "codex" => {
            let binary_path = serde_json::from_value::<CodexSettings>(config.clone())
                .map(|settings| settings.binary_path.as_str().to_owned())
                .unwrap_or_else(|_| "codex".to_owned());
            format!(
                "{} login",
                quote_provider_binary(&binary_path, "codex", platform)
            )
        }
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ExecutionEnvironmentPlatformOs::{Darwin, Linux, Unknown, Windows};
    use OnboardingProviderState::{Attention, Checking, Disabled, Install, Ready, SignIn};
    use serde_json::json;

    fn ready_codex() -> OnboardingProvider {
        OnboardingProvider {
            instance_id: "codex".to_owned(),
            driver: "codex".to_owned(),
            display_name: None,
            enabled: true,
            installed: true,
            status: ServerProviderState::Ready,
            auth_status: ProviderAuthStatus::Unknown,
            summary: ProviderSummary::default(),
        }
    }

    fn state(provider: &OnboardingProvider) -> OnboardingProviderState {
        get_onboarding_provider_state(Some(provider))
    }

    fn edit(
        mut provider: OnboardingProvider,
        change: impl FnOnce(&mut OnboardingProvider),
    ) -> OnboardingProvider {
        change(&mut provider);
        provider
    }

    #[test]
    fn treats_an_enabled_provider_with_ready_status_and_unknown_auth_as_ready() {
        assert_eq!(state(&ready_codex()), Ready);
    }

    #[test]
    fn authenticated_providers_are_ready_only_when_their_status_is_ready() {
        let authenticated = edit(ready_codex(), |p| {
            p.auth_status = ProviderAuthStatus::Authenticated;
        });
        assert_eq!(state(&authenticated), Ready);
        for status in [ServerProviderState::Error, ServerProviderState::Warning] {
            assert_eq!(
                state(&edit(authenticated.clone(), |p| p.status = status)),
                Attention
            );
        }
    }

    #[test]
    fn offers_sign_in_only_when_the_server_reports_an_authentication_failure() {
        let signed_out = edit(ready_codex(), |p| {
            p.status = ServerProviderState::Error;
            p.auth_status = ProviderAuthStatus::Unauthenticated;
        });
        assert_eq!(state(&signed_out), SignIn);
        assert_eq!(
            state(&edit(ready_codex(), |p| p.status = ServerProviderState::Error)),
            Attention
        );
        assert_eq!(
            state(&edit(ready_codex(), |p| p.status = ServerProviderState::Warning)),
            Attention
        );
    }

    #[test]
    fn does_not_offer_installation_or_sign_in_for_disabled_providers() {
        assert_eq!(
            state(&edit(ready_codex(), |p| {
                p.enabled = false;
                p.installed = false;
            })),
            Disabled
        );
        assert_eq!(
            state(&edit(ready_codex(), |p| p.status = ServerProviderState::Disabled)),
            Disabled
        );
    }

    #[test]
    fn offers_installation_only_when_an_enabled_provider_is_missing() {
        assert_eq!(
            state(&edit(ready_codex(), |p| {
                p.installed = false;
                p.status = ServerProviderState::Error;
            })),
            Install
        );
        // A warning with unknown auth is a probe still settling.
        assert_eq!(
            state(&edit(ready_codex(), |p| {
                p.installed = false;
                p.status = ServerProviderState::Warning;
            })),
            Checking
        );
    }

    #[test]
    fn waits_for_a_provider_snapshot_before_offering_an_action() {
        assert_eq!(get_onboarding_provider_state(None), Checking);
    }

    fn instance(provider: OnboardingProvider, id: &str) -> OnboardingProvider {
        edit(provider, |p| p.instance_id = id.to_owned())
    }

    fn picked(providers: &[OnboardingProvider]) -> String {
        select_onboarding_providers_by_driver(Some(providers))["codex"]
            .instance_id
            .clone()
    }

    #[test]
    fn prefers_a_ready_instance_with_unknown_auth_to_an_unauthenticated_instance() {
        let signed_out = edit(instance(ready_codex(), "codex_work"), |p| {
            p.status = ServerProviderState::Error;
            p.auth_status = ProviderAuthStatus::Unauthenticated;
        });
        assert_eq!(picked(&[signed_out, ready_codex()]), "codex");
    }

    #[test]
    fn prefers_a_provider_with_an_actionable_sign_in_over_a_failed_provider() {
        let failed = edit(ready_codex(), |p| p.status = ServerProviderState::Error);
        let signed_out = edit(instance(ready_codex(), "codex_work"), |p| {
            p.status = ServerProviderState::Error;
            p.auth_status = ProviderAuthStatus::Unauthenticated;
        });
        assert_eq!(picked(&[failed, signed_out]), "codex_work");
    }

    #[test]
    fn prefers_installed_providers_over_missing_or_disabled_instances() {
        let disabled = edit(ready_codex(), |p| p.enabled = false);
        let missing = edit(instance(ready_codex(), "codex_work"), |p| {
            p.installed = false;
            p.status = ServerProviderState::Error;
        });
        let ready = instance(ready_codex(), "ready_one");
        assert_eq!(picked(&[disabled, missing, ready]), "ready_one");
    }

    #[test]
    fn handles_provider_snapshots_that_have_not_arrived() {
        assert!(select_onboarding_providers_by_driver(None).is_empty());
    }

    #[test]
    fn keeps_a_ready_custom_account_when_the_default_account_is_signed_out() {
        let signed_out_default = edit(ready_codex(), |p| {
            p.status = ServerProviderState::Error;
            p.auth_status = ProviderAuthStatus::Unauthenticated;
        });
        let ready_custom = instance(ready_codex(), "codex_work");
        assert_eq!(picked(&[signed_out_default, ready_custom]), "codex_work");
    }

    fn login(
        driver: &str,
        binary_path: Option<&str>,
        platform: ExecutionEnvironmentPlatformOs,
    ) -> String {
        let config = binary_path.map(|path| json!({ "binaryPath": path }));
        resolve_onboarding_provider_login_command(driver, config.as_ref(), platform)
    }

    #[test]
    fn uses_the_selected_account_binary() {
        assert_eq!(
            login("codex", Some("/opt/codex-work/bin/codex"), Linux),
            "/opt/codex-work/bin/codex login"
        );
        assert_eq!(
            login("claudeAgent", Some("/opt/claude-work/bin/claude"), Linux),
            "/opt/claude-work/bin/claude auth login"
        );
        assert_eq!(login("codex", None, Linux), "codex login");
        assert_eq!(login("claudeAgent", None, Linux), "claude auth login");
    }

    #[test]
    fn quotes_a_codex_path_with_spaces_for_powershell() {
        assert_eq!(
            login(
                "codex",
                Some("C:\\Program Files\\Codex & Tools\\codex.exe"),
                Windows
            ),
            "& 'C:\\Program Files\\Codex & Tools\\codex.exe' login"
        );
    }

    #[test]
    fn quotes_a_claude_path_with_shell_metacharacters_on_posix() {
        assert_eq!(
            login(
                "claudeAgent",
                Some("/opt/Claude Tools/$current/claude"),
                Linux
            ),
            "'/opt/Claude Tools/$current/claude' auth login"
        );
    }

    #[test]
    fn keeps_the_home_prefix_expandable_while_quoting() {
        for (binary_path, expected) in [
            ("~/my tools/codex", "~/'my tools/codex' login"),
            ("~\\my tools/codex", "~/'my tools/codex' login"),
            (
                "~/tools/codex's build",
                "~/'tools/codex'\"'\"'s build' login",
            ),
            (
                "~\\tools\\codex's build",
                "~/'tools\\codex'\"'\"'s build' login",
            ),
            (
                "~/tools/codex; echo unsafe",
                "~/'tools/codex; echo unsafe' login",
            ),
        ] {
            assert_eq!(
                login("codex", Some(binary_path), Linux),
                expected,
                "{binary_path}"
            );
        }
    }

    #[test]
    fn quotes_backslashes_in_a_codex_path_on_posix() {
        for platform in [Darwin, Linux] {
            assert_eq!(
                login("codex", Some("/opt/codex\\work/codex"), platform),
                "'/opt/codex\\work/codex' login"
            );
        }
    }

    #[test]
    fn keeps_a_plain_windows_path_unquoted() {
        assert_eq!(
            login("codex", Some("C:\\Tools\\codex.exe"), Windows),
            "C:\\Tools\\codex.exe login"
        );
    }

    #[test]
    fn uses_the_default_command_when_an_old_server_reports_an_unknown_shell() {
        assert_eq!(
            login("codex", Some("/opt/Codex Tools/codex"), Unknown),
            "codex login"
        );
    }

    #[test]
    fn install_commands_follow_the_environment_platform() {
        use OnboardingInstallDriver::{ClaudeAgent, Codex};
        assert_eq!(
            resolve_onboarding_provider_install_command(Codex, Windows),
            "irm https://chatgpt.com/codex/install.ps1 | iex"
        );
        assert_eq!(
            resolve_onboarding_provider_install_command(ClaudeAgent, Windows),
            "irm https://claude.ai/install.ps1 | iex"
        );
        for platform in [Darwin, Linux, Unknown] {
            assert_eq!(
                resolve_onboarding_provider_install_command(Codex, platform),
                "curl -fsSL https://chatgpt.com/codex/install.sh | sh"
            );
            assert_eq!(
                resolve_onboarding_provider_install_command(ClaudeAgent, platform),
                "curl -fsSL https://claude.ai/install.sh | bash"
            );
        }
    }
}
