//! The seam between the Usage page and the services behind it.
//!
//! Upstream reads Effect atoms (`useUsage`, `environmentPresentations`,
//! `serverEnvironment.*` commands). Turin's page takes an [`UsageBackend`]
//! instead: reads are synchronous snapshots the page re-queries on every
//! render, commands return a [`Task`] the page awaits and then re-renders.
//! `MockUsageBackend` (mock_source.rs) backs the screenshot harness and the
//! tests; T22 wires `turin_usage::UsageService`, the provider registry and
//! settings.

use gpui_kit::{App, Task};
use turin_contracts::EnvironmentId;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditInput, ProviderConsumeResetCreditResult, ServerProviderUsageLimits,
};
use turin_contracts::usage::UsageSummaryInput;
use turin_shared::keybindings::ResolvedKeybindingRule;
use turin_shared::usage_limits::{LimitPresentations, UsageProviderInfo};

use super::usage_price_targets::{
    UsagePricePatch, UsagePriceTarget, UsageWriteError, UsageWriteOutcome,
};
use super::view_model::EnvironmentUsageStatus;
use crate::timestamp_format::TimestampFormat;

/// What the Limits views read from one provider instance (`ServerProvider`):
/// enough for [`UsageProviderInfo`], which the pooled-limits logic in
/// `turin-shared` is written against.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitsProvider {
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriverKind,
    pub enabled: bool,
    pub installed: bool,
    pub available: bool,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub authenticated: bool,
    pub auth_email: Option<String>,
    pub auth_label: Option<String>,
    pub auth_workspace_id: Option<String>,
    pub subscription_sharing: bool,
    pub usage_limits: Option<ServerProviderUsageLimits>,
    pub slash_commands: Vec<String>,
}

impl UsageProviderInfo for LimitsProvider {
    type SlashCommand = String;

    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn installed(&self) -> bool {
        self.installed
    }
    fn is_available(&self) -> bool {
        self.available
    }
    fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }
    fn accent_color(&self) -> Option<&str> {
        self.accent_color.as_deref()
    }
    fn is_authenticated(&self) -> bool {
        self.authenticated
    }
    fn auth_email(&self) -> Option<&str> {
        self.auth_email.as_deref()
    }
    fn auth_label(&self) -> Option<&str> {
        self.auth_label.as_deref()
    }
    fn auth_workspace_id(&self) -> Option<&str> {
        self.auth_workspace_id.as_deref()
    }
    fn has_subscription_sharing(&self) -> bool {
        self.subscription_sharing
    }
    fn usage_limits(&self) -> Option<&ServerProviderUsageLimits> {
        self.usage_limits.as_ref()
    }
    fn command_name(command: &String) -> &str {
        command
    }
    fn make_slash_command(name: &str, _description: &str) -> String {
        name.to_owned()
    }
    fn slash_commands(&self) -> &[String] {
        &self.slash_commands
    }
    fn set_slash_commands(&mut self, commands: Vec<String>) {
        self.slash_commands = commands;
    }
    fn map_workspace_snapshot_commands<F>(&mut self, _map: F)
    where
        F: FnMut(&[String]) -> Vec<String>,
    {
    }
}

/// One entry of `environmentPresentations.presentationsAtom` as Usage reads it.
#[derive(Clone, Debug)]
pub struct LimitsEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
    /// `connection.phase === "connected"`.
    pub connected: bool,
    /// `serverConfig !== null`.
    pub has_server_config: bool,
    pub providers: Vec<LimitsProvider>,
    pub usage_limit_sources: turin_contracts::provider_usage_limits::UsageLimitSourceSnapshots,
}

impl LimitsEnvironment {
    /// The entry the pooled logic consumes.
    pub fn presentation(&self) -> turin_shared::usage_limits::LimitPresentation<LimitsProvider> {
        turin_shared::usage_limits::LimitPresentation {
            label: self.label.clone(),
            providers: self.providers.clone(),
            usage_limit_sources: self.usage_limit_sources.clone(),
        }
    }
}

pub fn limit_presentations(
    environments: &[LimitsEnvironment],
) -> LimitPresentations<LimitsProvider> {
    environments
        .iter()
        .map(|environment| {
            (
                environment.environment_id.clone(),
                environment.presentation(),
            )
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResetCreditError {
    /// The message a thrown failure carries.
    Message(String),
    Unknown,
}

pub type ResetCreditResult = Result<ProviderConsumeResetCreditResult, ResetCreditError>;

pub trait UsageBackend {
    /// `usageByWindowAtom(window)`: every environment's status for one window.
    fn usage_environments(&self, window: &UsageSummaryInput) -> Vec<EnvironmentUsageStatus>;

    /// `refreshUsage`: rescan the given environments for `input`.
    fn refresh_usage(
        &self,
        environment_ids: Vec<EnvironmentId>,
        input: UsageSummaryInput,
        cx: &mut App,
    ) -> Task<()>;

    /// Connected environments with their provider snapshots.
    fn limits_environments(&self) -> Vec<LimitsEnvironment>;

    /// `refreshUsageLimits` (single-flighted, with a cooldown for automatic checks).
    fn refresh_limits(
        &self,
        environment_id: EnvironmentId,
        automatic: bool,
        after_pending: bool,
        cx: &mut App,
    ) -> Task<()>;

    /// `serverEnvironment.consumeResetCredit`.
    fn consume_reset_credit(
        &self,
        environment_id: EnvironmentId,
        input: ProviderConsumeResetCreditInput,
        cx: &mut App,
    ) -> Task<ResetCreditResult>;

    /// `AuthProvidersManageScope` on the environment.
    fn can_manage_providers(&self, environment_id: &EnvironmentId) -> bool;

    /// Updates settings with `cursorKeychainUsageEnabled: true`; `true` on success.
    fn enable_cursor_keychain(&self, environment_id: EnvironmentId, cx: &mut App) -> Task<bool>;

    /// `priceTargetsAtom`.
    fn price_targets(&self) -> Vec<UsagePriceTarget>;

    /// `updateSettings` with the price/alias patch.
    fn write_price_patch(
        &self,
        environment_id: EnvironmentId,
        patch: UsagePricePatch,
        cx: &mut App,
    ) -> Task<Result<UsageWriteOutcome, UsageWriteError>>;

    /// `ensureLocalApi().shell.openExternal`.
    fn open_external(&self, url: &str, cx: &mut App);

    /// The primary environment's resolved keybindings.
    fn keybindings(&self) -> Vec<ResolvedKeybindingRule>;

    /// `usePrimarySettings(timestampFormat)`.
    fn timestamp_format(&self) -> TimestampFormat;
}
