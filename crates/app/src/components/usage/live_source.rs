//! Live [`UsageBackend`] via [`crate::shell::usage_commands::usage_summary`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Task};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditInput, ProviderConsumeResetCreditResult,
};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::settings::ServerSettingsPatch;
use turin_contracts::usage::{UsageSummary, UsageSummaryInput};
use turin_shared::keybindings::{
    ResolvedKeybindingRule, compile_resolved_keybindings_config, default_keybinding_rules,
};

use super::backend::{
    LimitsEnvironment, ResetCreditError, ResetCreditResult, UsageBackend,
};
use super::usage_price_targets::{
    UsagePricePatch, UsagePriceTarget, UsageWriteError, UsageWriteOutcome,
};
use super::view_model::EnvironmentUsageStatus;
use crate::app_runtime::write::bus::CommandBus;
use crate::shell::usage_commands;
use crate::state;
use crate::timestamp_format::TimestampFormat;

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

pub struct LiveUsageBackend {
    bus: CommandBus,
    state: Rc<RefCell<State>>,
}

impl LiveUsageBackend {
    pub fn new(bus: CommandBus) -> Self {
        let id = state::local_environment_id();
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
            state.windows.insert(
                key.clone(),
                WindowCache {
                    pending: true,
                    summary: state.windows.get(&key).and_then(|entry| entry.summary.clone()),
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
            cx.update(|cx| cx.notify());
        })
        .detach();
    }
}

impl UsageBackend for LiveUsageBackend {
    fn usage_environments(&self, window: &UsageSummaryInput) -> Vec<EnvironmentUsageStatus> {
        let key = Self::window_key(window);
        let state = self.state.borrow();
        let cache = state.windows.get(&key);
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
                    needs_cursor_keychain_access: false,
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
        Vec::new()
    }

    fn refresh_limits(
        &self,
        _environment_id: EnvironmentId,
        _automatic: bool,
        _after_pending: bool,
        cx: &mut App,
    ) -> Task<()> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
        })
    }

    fn consume_reset_credit(
        &self,
        _environment_id: EnvironmentId,
        _input: ProviderConsumeResetCreditInput,
        cx: &mut App,
    ) -> Task<ResetCreditResult> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1))
                .await;
            Err(ResetCreditError::Message(
                "Reset credit is not wired yet.".into(),
            ))
        })
    }

    fn can_manage_providers(&self, _environment_id: &EnvironmentId) -> bool {
        true
    }

    fn enable_cursor_keychain(&self, _environment_id: EnvironmentId, cx: &mut App) -> Task<bool> {
        let bus = self.bus.clone();
        cx.spawn(async move |cx| {
            let Ok(commands) = bus.commands() else {
                return false;
            };
            let patch = ServerSettingsPatch {
                cursor_keychain_usage_enabled: Some(true),
                ..Default::default()
            };
            commands
                .update_server_settings(patch)
                .await
                .is_ok()
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
                        let alias = alias
                            .map(|value| TrimmedNonEmptyString::new(value))
                            .transpose()
                            .ok()?;
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
