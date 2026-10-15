//! Port of upstream/apps/server/src/provider/claudeUsageLimits.ts.
//!
//! Both sources produce windows with the same ids, so a turn-driven
//! `rate_limit_event` lands on the row the `get_usage` read established:
//!
//! - `get_usage` (on demand, during the capability probe) reports every window
//!   at once as 0–100 percentages with ISO reset times.
//! - `rate_limit_event` (streamed during a turn) names one window at a time with
//!   a 0–1 utilization fraction and an epoch-seconds reset.
use crate::claude_capabilities_probe::ClaudeUsageResponse;
use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_usage_limits::{
        ProviderUsageLimitsUpdate, ServerProviderUsageLimits, ServerProviderUsageUnavailableReason,
        ServerProviderUsageWindow, ServerProviderUsageWindowKind,
    },
};
use turin_provider_core::usage_limits::{
    clamp_percent, make_unavailable_usage_limits, make_usage_limits,
};

const SESSION_MINS: u64 = 5 * 60;
const WEEK_MINS: u64 = 7 * 24 * 60;

/// The streamed event names the overage-included bucket by type
/// (`seven_day_overage_included`), while `get_usage` names it by the model's
/// `display_name`. The probe records that name and the event mapper reuses it,
/// so a mid-turn update lands on the row the probe drew.
const OVERAGE_INCLUDED_EVENT_TYPE: &str = "seven_day_overage_included";

/// `ClaudeScopedLimitNames`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeScopedLimitNames {
    pub overage_included: Option<String>,
}

/// The names the probe recorded, shared with the adapter's event mapper
/// (`Ref<ClaudeScopedLimitNames>` upstream).
pub type SharedClaudeScopedLimitNames = Arc<Mutex<ClaudeScopedLimitNames>>;

/// `rate_limit_info` from a `rate_limit_event` frame (`SDKRateLimitInfo`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeRateLimitInfo {
    #[serde(default)]
    pub rate_limit_type: Option<String>,
    #[serde(default)]
    pub utilization: Option<f64>,
    /// Epoch seconds.
    #[serde(default)]
    pub resets_at: Option<f64>,
}

/// A window the app renders, keyed by the SDK's `rateLimitType`. Model-scoped
/// weeklies are additive: `get_usage` reports them under `rate_limits.model_scoped`.
struct WindowSpec {
    id: &'static str,
    kind: ServerProviderUsageWindowKind,
    label: &'static str,
    window_duration_mins: u64,
}

const WINDOWS: [WindowSpec; 2] = [
    WindowSpec {
        id: "five_hour",
        kind: ServerProviderUsageWindowKind::Session,
        label: "Session",
        window_duration_mins: SESSION_MINS,
    },
    WindowSpec {
        id: "seven_day",
        kind: ServerProviderUsageWindowKind::Weekly,
        label: "Weekly",
        window_duration_mins: WEEK_MINS,
    },
];

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// `scopedWindowId`: `seven_day_` plus the lowercased name with each run of
/// non `[a-z0-9]` characters replaced by one underscore.
fn scoped_window_id(display_name: &str) -> String {
    let mut slug = String::new();
    let mut in_gap = false;
    for ch in display_name.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            slug.push(ch);
            in_gap = false;
        } else if !in_gap {
            slug.push('_');
            in_gap = true;
        }
    }
    format!("seven_day_{slug}")
}

fn scoped_window(
    display_name: &str,
    used_percent: f64,
    resets_at: Option<String>,
) -> Option<ServerProviderUsageWindow> {
    Some(ServerProviderUsageWindow {
        id: tne(&scoped_window_id(display_name))?,
        kind: ServerProviderUsageWindowKind::Weekly,
        label: tne(&format!("Weekly · {display_name}"))?,
        used_percent: clamp_percent(used_percent),
        resets_at,
        window_duration_mins: Some(WEEK_MINS),
    })
}

fn make_window(
    spec: &WindowSpec,
    used_percent: f64,
    resets_at: Option<String>,
) -> Option<ServerProviderUsageWindow> {
    Some(ServerProviderUsageWindow {
        id: tne(spec.id)?,
        kind: spec.kind,
        label: tne(spec.label)?,
        used_percent: clamp_percent(used_percent),
        resets_at,
        window_duration_mins: Some(spec.window_duration_mins),
    })
}

fn window_spec(id: &str) -> Option<&'static WindowSpec> {
    WINDOWS.iter().find(|spec| spec.id == id)
}

/// Epoch seconds to the ISO form the snapshot carries, or `None` when absent or not positive.
fn iso_from_epoch_seconds(value: Option<f64>) -> Option<String> {
    let value = value.filter(|value| value.is_finite() && *value > 0.0)?;
    let date = DateTime::<Utc>::from_timestamp_millis((value * 1000.0).trunc() as i64)?;
    Some(date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// An RFC 3339 string to the ISO form the snapshot carries, or `None` when blank or invalid.
fn iso_from_string(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    let date = DateTime::parse_from_rfc3339(value)
        .ok()?
        .with_timezone(&Utc);
    Some(date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// `claudeRateLimitEventToUpdate`. Utilization is a 0–1 fraction on the
/// streamed event. An overage-included event before any probe has named the
/// bucket is dropped, since guessing a name would draw a row the next probe
/// cannot reconcile.
pub fn claude_rate_limit_event_to_update(
    info: &ClaudeRateLimitInfo,
    names: &ClaudeScopedLimitNames,
) -> Option<ProviderUsageLimitsUpdate> {
    let kind = info
        .rate_limit_type
        .as_deref()
        .filter(|kind| !kind.is_empty())?;
    let utilization = info.utilization?;
    let used_percent = utilization * 100.0;
    let resets_at = iso_from_epoch_seconds(info.resets_at);
    if let Some(spec) = window_spec(kind) {
        return Some(ProviderUsageLimitsUpdate {
            windows: make_window(spec, used_percent, resets_at)
                .into_iter()
                .collect(),
        });
    }
    if kind == OVERAGE_INCLUDED_EVENT_TYPE
        && let Some(name) = names.overage_included.as_deref()
    {
        return Some(ProviderUsageLimitsUpdate {
            windows: scoped_window(name, used_percent, resets_at)
                .into_iter()
                .collect(),
        });
    }
    None
}

/// `claudeUsageResponseToLimits`. Percentages on the `get_usage` response are
/// already 0–100. The second value is the scoped-bucket name the event mapper reuses.
pub fn claude_usage_response_to_limits(
    response: &ClaudeUsageResponse,
    checked_at: &str,
) -> (ServerProviderUsageLimits, ClaudeScopedLimitNames) {
    let rate_limits = response
        .rate_limits
        .as_ref()
        .filter(|value| !value.is_null());
    let (true, Some(rate_limits)) = (response.rate_limits_available == Some(true), rate_limits)
    else {
        return (
            make_unavailable_usage_limits(
                checked_at.to_owned(),
                ServerProviderUsageUnavailableReason::Unsupported,
                None,
            ),
            ClaudeScopedLimitNames::default(),
        );
    };

    let mut windows = Vec::new();
    for spec in &WINDOWS {
        let Some(window) = rate_limits.get(spec.id) else {
            continue;
        };
        let Some(utilization) = window.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let resets_at = iso_from_string(window.get("resets_at").and_then(Value::as_str));
        windows.extend(make_window(spec, utilization, resets_at));
    }

    // The CLI filters `model_scoped` to the overage-included allowlist, which
    // today holds one model. The first entry that drew a row is the one the
    // event refers to.
    let mut overage_included = None;
    for entry in read_model_scoped(rate_limits) {
        let Some(utilization) = entry.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let Some(display_name) = entry.get("display_name").and_then(Value::as_str) else {
            continue;
        };
        let resets_at = iso_from_string(entry.get("resets_at").and_then(Value::as_str));
        windows.extend(scoped_window(display_name, utilization, resets_at));
        // Only a bucket that drew a row may receive events. Naming one that was
        // skipped would let a mid-turn event open a row the probe never showed.
        if overage_included.is_none() {
            overage_included = Some(display_name.to_owned());
        }
    }

    (
        make_usage_limits(checked_at.to_owned(), windows),
        ClaudeScopedLimitNames { overage_included },
    )
}

/// `readModelScoped`: the `model_scoped` entries that carry a `display_name`.
fn read_model_scoped(rate_limits: &Value) -> Vec<&Value> {
    rate_limits
        .get("model_scoped")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("display_name").and_then(Value::as_str).is_some())
                .collect()
        })
        .unwrap_or_default()
}

/// Receives a streamed usage update and the time it was observed, the
/// `onUsageLimits` hook upstream passes the adapter.
pub type UsageLimitsSink = Arc<dyn Fn(ProviderUsageLimitsUpdate, String) + Send + Sync>;

/// What the adapter needs to report `rate_limit_event` frames: the probe's
/// scoped names and where updates go.
#[derive(Clone)]
pub struct ClaudeUsageLimitHooks {
    pub scoped_names: SharedClaudeScopedLimitNames,
    pub sink: UsageLimitsSink,
}

impl ClaudeUsageLimitHooks {
    /// Maps one `rate_limit_event` frame (`raw` is the whole message) and reports it
    /// when it names a window the page renders. Frames without `rate_limit_info` are ignored.
    pub fn emit_rate_limit_event(&self, raw: &Value) {
        let Some(info) = raw.get("rate_limit_info") else {
            return;
        };
        let Ok(info) = serde_json::from_value::<ClaudeRateLimitInfo>(info.clone()) else {
            return;
        };
        let names = self.scoped_names.lock().clone();
        let Some(update) = claude_rate_limit_event_to_update(&info, &names) else {
            return;
        };
        let checked_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        (self.sink)(update, checked_at);
    }
}

/// `recordClaudeUsageResponse`: maps the response and remembers the scoped
/// names for the event mapper.
pub fn record_claude_usage_response(
    names: &SharedClaudeScopedLimitNames,
    response: &ClaudeUsageResponse,
    checked_at: &str,
) -> ServerProviderUsageLimits {
    let (limits, next_names) = claude_usage_response_to_limits(response, checked_at);
    *names.lock() = next_names;
    limits
}

#[cfg(test)]
mod tests;
