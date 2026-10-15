//! Mock [`UsageBackend`] for the screenshot harness and tests (T22 wires the
//! live services). Deterministic fixtures: a month of daily usage across
//! Codex, Claude Code, Cursor and OpenCode (one unpriced model), subscription
//! limits for two Codex accounts and Claude Code, and a few custom prices.
//!
//! `TURIN_USAGE_SCENARIO` picks a variant: `multi` (a second computer),
//! `loading` (nothing answers), `error` (the only environment fails),
//! `partial` (one environment answers, one is still scanning), `empty`,
//! `cursor` (Cursor needs its Keychain enabled).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use gpui_kit::{App, Task};
use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::EnvironmentId;
use turin_contracts::provider_usage_limits::{
    ProviderConsumeResetCreditInput, ProviderConsumeResetCreditOutcome,
    ProviderConsumeResetCreditResult,
};
use turin_contracts::settings::UsageModelPriceOverride;
use turin_contracts::usage::{UsageSummary, UsageSummaryInput};
use turin_shared::keybindings::{
    ResolvedKeybindingRule, compile_resolved_keybindings_config, default_keybinding_rules,
};

use super::backend::{
    LimitsEnvironment, LimitsProvider, ResetCreditError, ResetCreditResult, UsageBackend,
};
use super::usage_price_form::basic_price;
use super::usage_price_targets::{
    UsagePricePatch, UsagePriceTarget, UsageWriteError, UsageWriteOutcome,
};
use super::view_model::EnvironmentUsageStatus;
use crate::timestamp_format::TimestampFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageScenario {
    Default,
    Multi,
    Loading,
    Error,
    Partial,
    Empty,
    Cursor,
}

impl UsageScenario {
    pub fn from_env() -> Self {
        match std::env::var("TURIN_USAGE_SCENARIO").as_deref() {
            Ok("multi") => Self::Multi,
            Ok("loading") => Self::Loading,
            Ok("error") => Self::Error,
            Ok("partial") => Self::Partial,
            Ok("empty") => Self::Empty,
            Ok("cursor") => Self::Cursor,
            _ => Self::Default,
        }
    }
}

struct MockEnvironment {
    id: EnvironmentId,
    label: &'static str,
    /// Rows reported by this environment's scan.
    answers: bool,
    fails: bool,
}

struct State {
    environments: Vec<MockEnvironment>,
    prices: HashMap<EnvironmentId, IndexMap<String, UsageModelPriceOverride>>,
    cursor_enabled: bool,
    credits_redeemed: bool,
    scenario: UsageScenario,
}

pub struct MockUsageBackend {
    state: Rc<RefCell<State>>,
    keybindings: Vec<ResolvedKeybindingRule>,
}

fn env_id(value: &str) -> EnvironmentId {
    EnvironmentId::new(value).expect("static id")
}

impl MockUsageBackend {
    pub fn new(scenario: UsageScenario) -> Self {
        let mut environments = vec![MockEnvironment {
            id: env_id("local"),
            label: "Jackson's MacBook Pro",
            answers: !matches!(scenario, UsageScenario::Loading | UsageScenario::Error),
            fails: scenario == UsageScenario::Error,
        }];
        if matches!(scenario, UsageScenario::Multi | UsageScenario::Partial) {
            environments.push(MockEnvironment {
                id: env_id("studio"),
                label: "Studio Mac mini",
                answers: scenario == UsageScenario::Multi,
                fails: false,
            });
        }
        let prices = environments
            .iter()
            .map(|environment| {
                (
                    environment.id.clone(),
                    IndexMap::from([("gpt-5.5-preview".to_owned(), basic_price(1.25, 10.))]),
                )
            })
            .collect();
        Self {
            state: Rc::new(RefCell::new(State {
                environments,
                prices,
                cursor_enabled: false,
                credits_redeemed: false,
                scenario,
            })),
            keybindings: compile_resolved_keybindings_config(&default_keybinding_rules()),
        }
    }

    pub fn shared(scenario: UsageScenario) -> Rc<dyn UsageBackend> {
        Rc::new(Self::new(scenario))
    }
}

/// A small, stable pseudo-random in 0..1 from three integers.
fn noise(a: i64, b: i64, c: i64) -> f64 {
    let mut value =
        (a.wrapping_mul(73856093) ^ b.wrapping_mul(19349663) ^ c.wrapping_mul(83492791)) as u64;
    value ^= value >> 13;
    value = value.wrapping_mul(0x5bd1e995);
    value ^= value >> 15;
    (value % 10_000) as f64 / 10_000.
}

struct ModelSpec {
    provider: &'static str,
    model: &'static str,
    /// Tokens per active day at noise 1.0.
    tokens: f64,
    /// Blended USD per million tokens; `None` is unpriced.
    rate: Option<f64>,
    cache_share: f64,
    activity: f64,
    fast: bool,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        provider: "codex",
        model: "gpt-5.5",
        tokens: 14_000_000.,
        rate: Some(0.62),
        cache_share: 0.82,
        activity: 0.92,
        fast: true,
    },
    ModelSpec {
        provider: "codex",
        model: "gpt-5.5-mini",
        tokens: 3_800_000.,
        rate: Some(0.18),
        cache_share: 0.7,
        activity: 0.7,
        fast: false,
    },
    ModelSpec {
        provider: "claude",
        model: "claude-opus-5-5",
        tokens: 9_500_000.,
        rate: Some(1.9),
        cache_share: 0.9,
        activity: 0.8,
        fast: false,
    },
    ModelSpec {
        provider: "claude",
        model: "claude-sonnet-5-5",
        tokens: 11_000_000.,
        rate: Some(0.55),
        cache_share: 0.88,
        activity: 0.85,
        fast: false,
    },
    ModelSpec {
        provider: "claude",
        model: "claude-haiku-5-5",
        tokens: 2_100_000.,
        rate: Some(0.12),
        cache_share: 0.6,
        activity: 0.5,
        fast: false,
    },
    ModelSpec {
        provider: "cursor",
        model: "composer-2",
        tokens: 1_900_000.,
        rate: Some(0.45),
        cache_share: 0.4,
        activity: 0.45,
        fast: false,
    },
    ModelSpec {
        provider: "opencode",
        model: "deepseek-v4-pro",
        tokens: 1_300_000.,
        rate: None,
        cache_share: 0.5,
        activity: 0.3,
        fast: false,
    },
];

fn bucket(
    spec: &ModelSpec,
    day: &str,
    hour_start: Option<&str>,
    seed: i64,
    scale: f64,
) -> Option<Value> {
    let key = noise(seed, spec.model.len() as i64, day.len() as i64);
    if key > spec.activity {
        return None;
    }
    let tokens =
        (spec.tokens * scale * (0.25 + 0.75 * noise(seed, spec.provider.len() as i64 * 7, 3)))
            .round();
    if tokens < 1. {
        return None;
    }
    let cached = (tokens * spec.cache_share * 0.9).round();
    let creation = (tokens * 0.03).round();
    let output = (tokens * 0.025).round();
    let uncached = (tokens - cached - creation - output).max(0.);
    let cost = spec.rate.map_or(0., |rate| tokens / 1e6 * rate);
    let mut value = json!({
        "day": day,
        "provider": spec.provider,
        "model": spec.model,
        "totals": {
            "uncachedInputTokens": uncached,
            "cachedInputTokens": cached,
            "cacheCreationTokens": creation,
            "outputTokens": output,
            "reasoningTokens": (output * 0.4).round(),
        },
        "costUsd": cost,
        "cacheSavingsUsd": cost * 2.4,
        "costSource": if spec.rate.is_some() { "modelPriced" } else { "unpriced" },
        "records": (tokens / 40_000.).ceil().max(1.),
        "unpricedRecords": if spec.rate.is_some() { 0 } else { (tokens / 40_000.).ceil().max(1.) as i64 },
        "sessions": ((tokens / 2_000_000.).ceil() as i64).max(1),
    });
    if spec.rate.is_some() {
        value["categoryCostUsd"] = json!({
            "input": cost * 0.28,
            "cacheRead": cost * 0.12,
            "cacheWrite": cost * 0.10,
            "output": cost * 0.50,
        });
    }
    if let Some(hour) = hour_start {
        value["hourStart"] = json!(hour);
    }
    if spec.fast {
        let fast = cost * 0.22;
        value["fastCostUsd"] = json!(fast);
        value["ultrafastCostUsd"] = json!(fast * 0.15);
        value["speedPremiumUsd"] = json!(fast * 0.55);
    }
    Some(value)
}

fn source(provider: &str, status: &str, message: Option<&str>, action: Option<&str>) -> Value {
    let mut value = json!({
        "fingerprint": {
            "hostId": if provider == "cursor" && action.is_none() { "cursor.com" } else { "local" },
            "provider": provider,
            "resolvedHomePath": format!("/Users/demo/.{provider}"),
            "volumeId": "",
        },
        "status": status,
        "scannedFiles": 240,
        "skippedFiles": 0,
        "malformedRecords": 0,
        "distinctSessions": 120,
        "message": message,
    });
    if let Some(action) = action {
        value["action"] = json!(action);
    }
    value
}

fn summary_for(
    window: &UsageSummaryInput,
    scenario: UsageScenario,
    cursor_enabled: bool,
) -> UsageSummary {
    let hourly = window.since_time.is_some();
    let mut buckets = Vec::new();
    if scenario != UsageScenario::Empty {
        if hourly {
            let hours = turin_shared::usage_format::enumerate_hour_starts(
                window.since_time.as_ref().map_or("", |t| t.as_str()),
                window.until_time.as_ref().map_or("", |t| t.as_str()),
            );
            for (index, hour) in hours.iter().enumerate() {
                let day = DateTime::parse_from_rfc3339(hour)
                    .map(|date| date.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                // Quiet at night.
                let clock = (index as i64 + 6) % 24;
                let scale = if (9..=19).contains(&clock) {
                    0.09
                } else {
                    0.015
                };
                for spec in MODELS {
                    if let Some(value) = bucket(spec, &day, Some(hour), index as i64 + 1000, scale)
                    {
                        buckets.push(value);
                    }
                }
            }
        } else {
            let since = NaiveDate::parse_from_str(window.since_day.as_str(), "%Y-%m-%d")
                .unwrap_or_default();
            let until = NaiveDate::parse_from_str(window.until_day.as_str(), "%Y-%m-%d")
                .unwrap_or_default();
            let mut cursor = since;
            while cursor <= until {
                let ordinal = i64::from(cursor.num_days_from_ce());
                // Weekends are lighter.
                let weekday = cursor.weekday().num_days_from_monday();
                let scale = if weekday >= 5 { 0.35 } else { 1. };
                let day = cursor.format("%Y-%m-%d").to_string();
                for spec in MODELS {
                    if spec.provider == "cursor"
                        && !cursor_enabled
                        && scenario != UsageScenario::Default
                        && scenario != UsageScenario::Multi
                    {
                        continue;
                    }
                    if let Some(value) = bucket(spec, &day, None, ordinal, scale) {
                        buckets.push(value);
                    }
                }
                cursor = cursor
                    .checked_add_days(Days::new(1))
                    .unwrap_or(until + Days::new(1));
            }
        }
    }
    let cursor_source = if scenario == UsageScenario::Cursor && !cursor_enabled {
        source(
            "cursor",
            "partial",
            Some("Requires access to your Cursor login in macOS Keychain."),
            Some("enableCursorKeychain"),
        )
    } else {
        source("cursor", "ok", None, None)
    };
    let sources = vec![
        source("codex", "ok", None, None),
        source("claude", "ok", None, None),
        cursor_source,
        source(
            "opencode",
            "partial",
            Some("Some OpenCode sessions could not be read."),
            None,
        ),
    ];
    serde_json::from_value(json!({
        "contractVersion": 6,
        "readAt": "2026-10-15T12:00:00.000Z",
        "timeZone": window.time_zone.as_str(),
        "sinceDay": window.since_day.as_str(),
        "untilDay": window.until_day.as_str(),
        "buckets": buckets,
        "sources": sources,
        "pricing": { "status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 1200 },
        "scanDurationMs": 420,
    }))
    .expect("mock usage summary decodes")
}

fn iso(now: DateTime<Utc>, offset_minutes: i64) -> String {
    (now + chrono::Duration::minutes(offset_minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn limits_window(
    id: &str,
    kind: &str,
    label: &str,
    used: f64,
    mins: i64,
    resets_in_minutes: i64,
    now: DateTime<Utc>,
) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "label": label,
        "usedPercent": used,
        "windowDurationMins": mins,
        "resetsAt": iso(now, resets_in_minutes),
    })
}

fn provider(overrides: Value) -> LimitsProvider {
    let base = json!({
        "instanceId": "codex",
        "driver": "codex",
        "enabled": true,
        "installed": true,
        "available": true,
        "displayName": null,
        "accentColor": null,
        "authenticated": true,
        "authEmail": null,
        "authLabel": null,
        "authWorkspaceId": null,
        "subscriptionSharing": false,
    });
    let mut merged = base;
    for (key, value) in overrides.as_object().expect("object").iter() {
        merged[key] = value.clone();
    }
    let limits = merged.get("usageLimits").cloned().filter(|v| !v.is_null());
    LimitsProvider {
        instance_id: turin_contracts::provider_instance::ProviderInstanceId::new(
            merged["instanceId"].as_str().expect("id"),
        )
        .expect("instance id"),
        driver: turin_contracts::provider_instance::ProviderDriverKind::new(
            merged["driver"].as_str().expect("driver"),
        )
        .expect("driver"),
        enabled: merged["enabled"].as_bool().unwrap_or(true),
        installed: merged["installed"].as_bool().unwrap_or(true),
        available: merged["available"].as_bool().unwrap_or(true),
        display_name: merged["displayName"].as_str().map(str::to_owned),
        accent_color: merged["accentColor"].as_str().map(str::to_owned),
        authenticated: merged["authenticated"].as_bool().unwrap_or(true),
        auth_email: merged["authEmail"].as_str().map(str::to_owned),
        auth_label: merged["authLabel"].as_str().map(str::to_owned),
        auth_workspace_id: merged["authWorkspaceId"].as_str().map(str::to_owned),
        subscription_sharing: merged["subscriptionSharing"].as_bool().unwrap_or(false),
        usage_limits: limits.map(|value| serde_json::from_value(value).expect("limits decode")),
        slash_commands: Vec::new(),
    }
}

fn limits_environment(
    id: &EnvironmentId,
    label: &str,
    scenario: UsageScenario,
    redeemed: bool,
) -> LimitsEnvironment {
    let now = Utc::now();
    let checked_at = iso(now, 0);
    let credits = if redeemed {
        json!({ "availableCount": 0 })
    } else {
        json!({ "availableCount": 2, "nextExpiresAt": iso(now, 60 * 24 * 27 + 60 * 23) })
    };
    let mut providers = vec![
        provider(json!({
            "instanceId": "codex",
            "driver": "codex",
            "authEmail": "jackson@samwisepsm.com",
            "subscriptionSharing": true,
            "usageLimits": {
                "checkedAt": checked_at,
                "windows": [
                    limits_window("primary", "session", "5h", 62., 300, 133, now),
                    limits_window("secondary", "weekly", "Weekly", 38., 10080, 60 * 24 * 3 + 240, now),
                ],
                "resetCredits": credits,
            },
        })),
        provider(json!({
            "instanceId": "claudeAgent",
            "driver": "claudeAgent",
            "authEmail": "jackson@samwisepsm.com",
            "usageLimits": {
                "checkedAt": checked_at,
                "windows": [
                    limits_window("five_hour", "session", "Session", 24., 300, 205, now),
                    limits_window("seven_day", "weekly", "Weekly", 71., 10080, 60 * 24 * 2 + 600, now),
                ],
            },
        })),
    ];
    if scenario == UsageScenario::Multi {
        providers.push(provider(json!({
            "instanceId": "codex_work",
            "driver": "codex",
            "displayName": "Work",
            "authEmail": "jackson@acme.dev",
            "usageLimits": {
                "checkedAt": checked_at,
                "windows": [
                    limits_window("primary", "session", "5h", 18., 300, 48, now),
                    limits_window("secondary", "weekly", "Weekly", 55., 10080, 60 * 24 * 5, now),
                ],
            },
        })));
    }
    LimitsEnvironment {
        environment_id: id.clone(),
        label: label.to_owned(),
        connected: true,
        has_server_config: true,
        providers,
        usage_limit_sources: Vec::new(),
    }
}

impl UsageBackend for MockUsageBackend {
    fn usage_environments(&self, window: &UsageSummaryInput) -> Vec<EnvironmentUsageStatus> {
        let state = self.state.borrow();
        // Everything but the `loading` scenario answers at once; a refresh only
        // plays the spinner (the live backend answers per window).
        let ready = state.scenario != UsageScenario::Loading;
        state
            .environments
            .iter()
            .map(|environment| {
                let summary = (ready && environment.answers && !environment.fails)
                    .then(|| summary_for(window, state.scenario, state.cursor_enabled));
                let needs_cursor = state.scenario == UsageScenario::Cursor && !state.cursor_enabled;
                EnvironmentUsageStatus {
                    environment_id: environment.id.clone(),
                    label: environment.label.to_owned(),
                    is_pending: summary.is_none() && !environment.fails,
                    can_read_diagnostics: true,
                    is_connected: true,
                    error: environment
                        .fails
                        .then(|| "This environment could not report usage.".to_owned()),
                    needs_cursor_keychain_access: summary.is_some() && needs_cursor,
                    summary,
                }
            })
            .collect()
    }

    fn refresh_usage(
        &self,
        _environment_ids: Vec<EnvironmentId>,
        _input: UsageSummaryInput,
        cx: &mut App,
    ) -> Task<()> {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
        })
    }

    fn limits_environments(&self) -> Vec<LimitsEnvironment> {
        let state = self.state.borrow();
        state
            .environments
            .iter()
            .filter(|environment| !environment.fails)
            .map(|environment| {
                limits_environment(
                    &environment.id,
                    environment.label,
                    state.scenario,
                    state.credits_redeemed,
                )
            })
            .collect()
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
                .timer(Duration::from_millis(300))
                .await;
        })
    }

    fn consume_reset_credit(
        &self,
        _environment_id: EnvironmentId,
        _input: ProviderConsumeResetCreditInput,
        cx: &mut App,
    ) -> Task<ResetCreditResult> {
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            state.borrow_mut().credits_redeemed = true;
            Ok::<_, ResetCreditError>(ProviderConsumeResetCreditResult {
                outcome: ProviderConsumeResetCreditOutcome::Reset,
                warning: None,
            })
        })
    }

    fn can_manage_providers(&self, _environment_id: &EnvironmentId) -> bool {
        true
    }

    fn enable_cursor_keychain(&self, _environment_id: EnvironmentId, cx: &mut App) -> Task<bool> {
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            state.borrow_mut().cursor_enabled = true;
            true
        })
    }

    fn price_targets(&self) -> Vec<UsagePriceTarget> {
        let state = self.state.borrow();
        state
            .environments
            .iter()
            .map(|environment| UsagePriceTarget {
                environment_id: environment.id.clone(),
                label: environment.label.to_owned(),
                prices: state.prices.get(&environment.id).cloned(),
                aliases: Some(IndexMap::new()),
                unavailable: None,
            })
            .collect()
    }

    fn write_price_patch(
        &self,
        environment_id: EnvironmentId,
        patch: UsagePricePatch,
        cx: &mut App,
    ) -> Task<Result<UsageWriteOutcome, UsageWriteError>> {
        let state = self.state.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;
            let mut state = state.borrow_mut();
            if let (Some(prices), Some(overrides)) = (
                state.prices.get_mut(&environment_id),
                patch.usage_price_overrides,
            ) {
                for (model, price) in overrides {
                    match price {
                        Some(price) => {
                            prices.insert(model, price);
                        }
                        None => {
                            prices.shift_remove(&model);
                        }
                    }
                }
            }
            Ok(UsageWriteOutcome::Success)
        })
    }

    fn open_external(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }

    fn keybindings(&self) -> Vec<ResolvedKeybindingRule> {
        self.keybindings.clone()
    }

    fn timestamp_format(&self) -> TimestampFormat {
        TimestampFormat::Locale
    }
}
