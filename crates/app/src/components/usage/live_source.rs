//! Live [`UsageBackend`] via [`crate::shell::usage_commands::usage_summary`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, Task};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::provider_usage_limits::ProviderConsumeResetCreditInput;
use turin_contracts::settings::{ServerSettings, ServerSettingsPatch, UsageModelPriceOverride};
use turin_contracts::usage::{UsageSourceAction, UsageSummary, UsageSummaryInput};
use turin_shared::keybindings::ResolvedKeybindingRule;

use super::backend::{
    LimitsEnvironment, LimitsProvider, ResetCreditError, ResetCreditResult, UsageBackend,
};
use super::usage_price_targets::{
    UsagePricePatch, UsagePriceTarget, UsageWriteError, UsageWriteOutcome,
};
use super::view_model::EnvironmentUsageStatus;
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;
use crate::shell::usage_commands;
use crate::state;
use crate::state::helpers::usage::UsageLimitsRefresher;
use crate::timestamp_format::TimestampFormat;
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderState,
};

#[derive(Clone)]
struct EnvironmentRow {
    id: EnvironmentId,
    label: String,
}

struct WindowCache {
    pending: bool,
    summary: Option<Result<UsageSummary, String>>,
}

struct State {
    environments: Vec<EnvironmentRow>,
    windows: HashMap<String, WindowCache>,
}

fn limits_provider_from_snapshot(provider: &ServerProvider) -> LimitsProvider {
    LimitsProvider {
        instance_id: provider.instance_id.clone(),
        driver: provider.driver.clone(),
        enabled: provider.enabled,
        installed: provider.installed,
        available: provider.is_available(),
        display_name: provider
            .display_name
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        accent_color: provider
            .accent_color
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        authenticated: provider.auth.status == ServerProviderAuthStatus::Authenticated,
        auth_email: provider
            .auth
            .email
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        auth_label: provider
            .auth
            .label
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        auth_workspace_id: provider
            .auth
            .workspace_id
            .as_ref()
            .map(|value| value.as_str().to_owned()),
        subscription_sharing: provider.auth.subscription_sharing.unwrap_or(false),
        usage_limits: provider.usage_limits.clone(),
        slash_commands: provider
            .slash_commands
            .iter()
            .map(|command| command.name.as_str().to_owned())
            .collect(),
    }
}

/// The server settings' custom prices and model mappings: what the price
/// editor lists and what a removal must find to delete.
#[derive(Clone, Debug, Default, PartialEq)]
struct ServerPrices {
    prices: IndexMap<String, UsageModelPriceOverride>,
    aliases: IndexMap<String, String>,
}

impl ServerPrices {
    fn from_settings(settings: &ServerSettings) -> Self {
        Self {
            prices: settings
                .usage_price_overrides
                .iter()
                .map(|(model, price)| (model.as_str().to_owned(), price.clone()))
                .collect(),
            aliases: settings
                .usage_model_aliases
                .iter()
                .map(|(model, alias)| (model.as_str().to_owned(), alias.as_str().to_owned()))
                .collect(),
        }
    }
}

/// The latest server prices, `None` until the settings have been read.
type SharedPrices = Arc<Mutex<Option<ServerPrices>>>;

fn store_prices(shared: &SharedPrices, settings: &ServerSettings) {
    if let Ok(mut prices) = shared.lock() {
        *prices = Some(ServerPrices::from_settings(settings));
    }
}

/// Reads the server settings once, then follows their changes for as long as
/// the backend (the only other owner of `shared`) is alive.
fn follow_server_prices(bus: &CommandBus, shared: &SharedPrices) {
    let weak = Arc::downgrade(shared);
    let bus = bus.clone();
    bus.handle().clone().spawn(async move {
        let commands = loop {
            if weak.strong_count() == 0 {
                return;
            }
            match bus.commands() {
                Ok(commands) => break commands,
                Err(error) if error.message.starts_with("Turin could not start") => return,
                Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        };
        let mut changes = commands.runtime().server_settings.subscribe();
        if let Ok(settings) = commands.server_settings().await {
            let Some(shared) = weak.upgrade() else { return };
            store_prices(&shared, &settings);
        }
        while let Some(settings) = changes.recv().await {
            let Some(shared) = weak.upgrade() else { return };
            store_prices(&shared, &settings);
        }
    });
}

fn provider_snapshots(bus: &CommandBus) -> Vec<ServerProvider> {
    let cached = bus.providers().borrow().iter().cloned().collect::<Vec<_>>();
    if !cached.is_empty() {
        return cached;
    }
    bus.commands()
        .map(|commands| commands.provider_snapshots())
        .unwrap_or_default()
}

pub struct LiveUsageBackend {
    bus: CommandBus,
    state: Rc<RefCell<State>>,
    prices: SharedPrices,
    limits_refresher: UsageLimitsRefresher<Vec<ServerProvider>, String>,
}

impl LiveUsageBackend {
    pub fn new(bus: CommandBus) -> Self {
        let id = state::local_environment_id();
        // The refresh future is polled on the GPUI executor, which has no tokio reactor.
        let runtime = bus.handle().clone();
        let prices = SharedPrices::default();
        follow_server_prices(&bus, &prices);
        Self {
            bus,
            prices,
            state: Rc::new(RefCell::new(State {
                environments: vec![EnvironmentRow {
                    id: id.clone(),
                    label: "This device".to_owned(),
                }],
                windows: HashMap::new(),
            })),
            limits_refresher: UsageLimitsRefresher::new(|| chrono::Utc::now().timestamp_millis())
                .with_runtime(runtime),
        }
    }

    pub fn shared(bus: CommandBus) -> Rc<dyn UsageBackend> {
        Rc::new(Self::new(bus))
    }

    fn window_key(window: &UsageSummaryInput) -> String {
        serde_json::to_string(window).unwrap_or_default()
    }

    fn fetch(&self, window: UsageSummaryInput, cx: &mut App) {
        let key = Self::window_key(&window);
        {
            let mut state = self.state.borrow_mut();
            let summary = state
                .windows
                .get(&key)
                .and_then(|entry| entry.summary.clone());
            state.windows.insert(
                key.clone(),
                WindowCache {
                    pending: true,
                    summary,
                },
            );
        }
        let bus = self.bus.clone();
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            let summary = match bus.commands() {
                Ok(commands) => usage_commands::usage_summary(&commands, &window)
                    .await
                    .map_err(|error| error.message),
                Err(error) => Err(error.message),
            };
            state.borrow_mut().windows.insert(
                key,
                WindowCache {
                    pending: false,
                    summary: Some(summary),
                },
            );
            cx.update(|_| ());
        })
        .detach();
    }
}

impl UsageBackend for LiveUsageBackend {
    fn usage_environments(&self, window: &UsageSummaryInput) -> Vec<EnvironmentUsageStatus> {
        let key = Self::window_key(window);
        let state = self.state.borrow();
        let cache = state.windows.get(&key);
        let providers = provider_snapshots(&self.bus);
        state
            .environments
            .iter()
            .map(|environment| {
                let (summary, error, is_pending) = match cache {
                    None => (None, None, true),
                    Some(entry) if entry.pending => (None, None, true),
                    Some(entry) => match &entry.summary {
                        None => (None, None, true),
                        Some(Ok(summary)) => (Some(summary.clone()), None, false),
                        Some(Err(message)) => (None, Some(message.clone()), false),
                    },
                };
                EnvironmentUsageStatus {
                    environment_id: environment.id.clone(),
                    label: environment.label.clone(),
                    is_pending,
                    can_read_diagnostics: true,
                    is_connected: true,
                    error,
                    needs_cursor_keychain_access: summary.as_ref().is_some_and(|summary| {
                        summary.sources.iter().any(|source| {
                            source.action == Some(UsageSourceAction::EnableCursorKeychain)
                        })
                    }) && providers.iter().any(|provider| {
                        provider.driver.as_str() == "cursor"
                            && provider.status == ServerProviderState::Ready
                    }),
                    summary,
                }
            })
            .collect()
    }

    fn refresh_usage(
        &self,
        _environment_ids: Vec<EnvironmentId>,
        input: UsageSummaryInput,
        cx: &mut App,
    ) -> Task<()> {
        self.fetch(input, cx);
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
        })
    }

    fn limits_environments(&self) -> Vec<LimitsEnvironment> {
        let providers: Vec<LimitsProvider> = provider_snapshots(&self.bus)
            .into_iter()
            .map(|provider| limits_provider_from_snapshot(&provider))
            .collect();
        self.state
            .borrow()
            .environments
            .iter()
            .map(|environment| LimitsEnvironment {
                environment_id: environment.id.clone(),
                label: environment.label.clone(),
                connected: true,
                has_server_config: true,
                providers: providers.clone(),
                usage_limit_sources: Default::default(),
            })
            .collect()
    }

    fn refresh_limits(
        &self,
        environment_id: EnvironmentId,
        automatic: bool,
        after_pending: bool,
        cx: &mut App,
    ) -> Task<()> {
        let bus = self.bus.clone();
        let refresher = self.limits_refresher.clone();
        cx.spawn(async move |cx| {
            let _ = refresher
                .refresh(
                    &environment_id,
                    move || {
                        let bus = bus.clone();
                        async move {
                            let commands = bus
                                .commands()
                                .map_err(|error: CommandError| error.message)?;
                            Ok(commands.refresh_usage_limits().await)
                        }
                    },
                    automatic,
                    after_pending,
                )
                .await;
            cx.update(|_| ());
        })
    }

    fn consume_reset_credit(
        &self,
        _environment_id: EnvironmentId,
        input: ProviderConsumeResetCreditInput,
        cx: &mut App,
    ) -> Task<ResetCreditResult> {
        let bus = self.bus.clone();
        cx.spawn(async move |_cx| {
            let commands = bus
                .commands()
                .map_err(|error| ResetCreditError::Message(error.message))?;
            commands
                .consume_reset_credit(input)
                .await
                .map_err(|error| ResetCreditError::Message(error.message))
        })
    }

    fn can_manage_providers(&self, _environment_id: &EnvironmentId) -> bool {
        true
    }

    fn enable_cursor_keychain(&self, _environment_id: EnvironmentId, cx: &mut App) -> Task<bool> {
        let bus = self.bus.clone();
        cx.spawn(async move |_cx| {
            let Ok(commands) = bus.commands() else {
                return false;
            };
            let patch = ServerSettingsPatch {
                cursor_keychain_usage_enabled: Some(true),
                ..Default::default()
            };
            commands.update_server_settings(patch).await.is_ok()
        })
    }

    fn price_targets(&self) -> Vec<UsagePriceTarget> {
        let server = self.prices.lock().ok().and_then(|prices| prices.clone());
        self.state
            .borrow()
            .environments
            .iter()
            .map(|environment| UsagePriceTarget {
                environment_id: environment.id.clone(),
                label: environment.label.clone(),
                prices: server.as_ref().map(|server| server.prices.clone()),
                aliases: server.as_ref().map(|server| server.aliases.clone()),
                unavailable: None,
            })
            .collect()
    }

    fn write_price_patch(
        &self,
        _environment_id: EnvironmentId,
        patch: UsagePricePatch,
        cx: &mut App,
    ) -> Task<Result<UsageWriteOutcome, UsageWriteError>> {
        let bus = self.bus.clone();
        let shared = self.prices.clone();
        cx.spawn(async move |_cx| {
            let commands = bus.commands().map_err(|_| UsageWriteError)?;
            let mut settings_patch = ServerSettingsPatch::default();
            if let Some(overrides) = patch.usage_price_overrides {
                let mapped: indexmap::IndexMap<_, _> = overrides
                    .into_iter()
                    .filter_map(|(model, price)| {
                        TrimmedNonEmptyString::new(model)
                            .ok()
                            .map(|model| (model, price))
                    })
                    .collect();
                if !mapped.is_empty() {
                    settings_patch.usage_price_overrides = Some(mapped);
                }
            }
            if let Some(aliases) = patch.usage_model_aliases {
                let mapped: indexmap::IndexMap<_, _> = aliases
                    .into_iter()
                    .filter_map(|(model, alias)| {
                        let model = TrimmedNonEmptyString::new(model).ok()?;
                        let alias = alias.map(TrimmedNonEmptyString::new).transpose().ok()?;
                        Some((model, alias))
                    })
                    .collect();
                if !mapped.is_empty() {
                    settings_patch.usage_model_aliases = Some(mapped);
                }
            }
            if settings_patch.usage_price_overrides.is_none()
                && settings_patch.usage_model_aliases.is_none()
            {
                return Ok(UsageWriteOutcome::Success);
            }
            let settings = commands
                .update_server_settings(settings_patch)
                .await
                .map_err(|_| UsageWriteError)?;
            // The next read of the targets sees the saved prices without
            // waiting for the settings change notification.
            store_prices(&shared, &settings);
            Ok(UsageWriteOutcome::Success)
        })
    }

    fn open_external(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }

    fn keybindings(&self) -> Vec<ResolvedKeybindingRule> {
        crate::shell::user_keybindings::active_rules()
    }

    fn timestamp_format(&self) -> TimestampFormat {
        TimestampFormat::Locale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_prices_list_the_settings_overrides_and_mappings() {
        let settings: ServerSettings = serde_json::from_value(serde_json::json!({
            "usagePriceOverrides": {
                "gpt-x": {
                    "inputCostPerMillionTokens": 1.5,
                    "outputCostPerMillionTokens": 6
                }
            },
            "usageModelAliases": {"gpt-x-preview": "gpt-x"}
        }))
        .expect("settings");
        let server = ServerPrices::from_settings(&settings);
        assert_eq!(server.prices.keys().collect::<Vec<_>>(), ["gpt-x"]);
        assert_eq!(
            server.aliases.get("gpt-x-preview").map(String::as_str),
            Some("gpt-x")
        );
        assert_eq!(
            ServerPrices::from_settings(&ServerSettings::default()),
            ServerPrices::default()
        );
    }

    #[test]
    fn stored_prices_replace_the_previous_snapshot() {
        let shared = SharedPrices::default();
        assert!(shared.lock().expect("lock").is_none());
        let settings: ServerSettings = serde_json::from_value(serde_json::json!({
            "usageModelAliases": {"a": "b"}
        }))
        .expect("settings");
        store_prices(&shared, &settings);
        store_prices(&shared, &ServerSettings::default());
        assert_eq!(
            shared.lock().expect("lock").clone(),
            Some(ServerPrices::default())
        );
    }
}
