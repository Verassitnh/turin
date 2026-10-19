//! Live [`UsageBackend`] via [`crate::shell::usage_commands::usage_summary`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Task};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::provider_usage_limits::ProviderConsumeResetCreditInput;
use turin_contracts::settings::ServerSettingsPatch;
use turin_contracts::usage::{UsageSourceAction, UsageSummary, UsageSummaryInput};
use turin_shared::keybindings::{
    ResolvedKeybindingRule, compile_resolved_keybindings_config, default_keybinding_rules,
};

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
    keybindings: Vec<ResolvedKeybindingRule>,
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
    limits_refresher: UsageLimitsRefresher<Vec<ServerProvider>, String>,
}

impl LiveUsageBackend {
    pub fn new(bus: CommandBus) -> Self {
        let id = state::local_environment_id();
        // The refresh future is polled on the GPUI executor, which has no tokio reactor.
        let runtime = bus.handle().clone();
        Self {
            bus,
            state: Rc::new(RefCell::new(State {
                environments: vec![EnvironmentRow {
                    id: id.clone(),
                    label: "This device".to_owned(),
                }],
                windows: HashMap::new(),
                keybindings: compile_resolved_keybindings_config(&default_keybinding_rules()),
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

    /// Reads `window` and stores the answer. The task resolves once it is stored.
    fn fetch(&self, window: UsageSummaryInput, cx: &mut App) -> Task<()> {
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
        cx.spawn(async move |_cx| {
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
        })
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
                    // A rescan keeps the previous answer on screen while it
                    // waits, as `AsyncResult.value` does upstream.
                    Some(entry) => match &entry.summary {
                        None => (None, None, true),
                        Some(Ok(summary)) => (Some(summary.clone()), None, entry.pending),
                        Some(Err(message)) => (None, Some(message.clone()), entry.pending),
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

    fn load_usage(&self, window: &UsageSummaryInput, cx: &mut App) -> Option<Task<()>> {
        if self
            .state
            .borrow()
            .windows
            .contains_key(&Self::window_key(window))
        {
            return None;
        }
        Some(self.fetch(window.clone(), cx))
    }

    fn refresh_usage(
        &self,
        _environment_ids: Vec<EnvironmentId>,
        input: UsageSummaryInput,
        cx: &mut App,
    ) -> Task<()> {
        self.fetch(input, cx)
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
        self.state
            .borrow()
            .environments
            .iter()
            .map(|environment| UsagePriceTarget {
                environment_id: environment.id.clone(),
                label: environment.label.clone(),
                prices: None,
                aliases: Some(IndexMap::new()),
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
            commands
                .update_server_settings(settings_patch)
                .await
                .map(|_| UsageWriteOutcome::Success)
                .map_err(|_| UsageWriteError)
        })
    }

    fn open_external(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }

    fn keybindings(&self) -> Vec<ResolvedKeybindingRule> {
        self.state.borrow().keybindings.clone()
    }

    fn timestamp_format(&self) -> TimestampFormat {
        TimestampFormat::Locale
    }
}
