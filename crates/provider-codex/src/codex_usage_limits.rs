//! Port of upstream/apps/server/src/provider/codexUsageLimits.ts.
//!
//! The `account/rateLimits/read` response and the `account/rateLimits/updated`
//! notification share one snapshot shape, so one mapper serves the status probe
//! and the turn-driven update. Both emit windows with the same ids so they merge.
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use turin_codex_app_server::errors::AppServerError;
use turin_contracts::base_schemas::double_option;
use turin_contracts::provider_usage_limits::{
    ProviderUsageLimitsUpdate, ServerProviderResetCredits, ServerProviderUsageLimits,
    ServerProviderUsageUnavailableReason, ServerProviderUsageWindow, ServerProviderUsageWindowKind,
};
use turin_contracts::{TrimmedNonEmptyString, UtcDateTime};
use turin_provider_core::usage_limits::{
    clamp_percent, make_unavailable_usage_limits, make_usage_limits,
};

const SESSION_MINS: f64 = 5.0 * 60.0;
const WEEK_MINS: f64 = 7.0 * 24.0 * 60.0;
const MONTH_MINS: f64 = 30.0 * 24.0 * 60.0;

/// One `RateLimitWindow`. `resetsAt` is epoch seconds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateLimitWindow {
    pub used_percent: f64,
    #[serde(default)]
    pub resets_at: Option<f64>,
    #[serde(default)]
    pub window_duration_mins: Option<f64>,
}

/// `RateLimitSnapshot`. Both the read response and the update satisfy it.
/// Fields are `Option<Option<_>>` so an update that omits a field (keep the
/// earlier value) is told apart from one that sends `null` (clear it).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRateLimitSnapshot {
    #[serde(default, with = "double_option")]
    pub limit_id: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    pub plan_type: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    pub rate_limit_reached_type: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    pub primary: Option<Option<CodexRateLimitWindow>>,
    #[serde(default, with = "double_option")]
    pub secondary: Option<Option<CodexRateLimitWindow>>,
}

impl CodexRateLimitSnapshot {
    fn limit_id(&self) -> Option<&str> {
        self.limit_id.as_ref().and_then(|id| id.as_deref())
    }

    fn plan_type(&self) -> Option<&str> {
        self.plan_type.as_ref().and_then(|plan| plan.as_deref())
    }

    fn rate_limit_reached_type(&self) -> Option<&str> {
        self.rate_limit_reached_type
            .as_ref()
            .and_then(|kind| kind.as_deref())
    }

    fn primary(&self) -> Option<&CodexRateLimitWindow> {
        self.primary.as_ref().and_then(Option::as_ref)
    }

    fn secondary(&self) -> Option<&CodexRateLimitWindow> {
        self.secondary.as_ref().and_then(Option::as_ref)
    }
}

/// One entry of `rateLimitResetCredits.credits`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCredit {
    pub status: String,
    #[serde(default)]
    pub expires_at: Option<f64>,
}

/// `rateLimitResetCredits`: the account's banked reset credits.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCreditsSummary {
    pub available_count: f64,
    #[serde(default)]
    pub credits: Option<Vec<CodexResetCredit>>,
}

/// `isoFromEpochSeconds`: `None` for absent, non-finite, or non-positive values.
fn iso_from_epoch_seconds(value: Option<f64>) -> Option<String> {
    let seconds = value.filter(|value| value.is_finite() && *value > 0.0)?;
    let datetime = DateTime::<Utc>::from_timestamp_millis((seconds * 1000.0) as i64)?;
    Some(UtcDateTime::from_datetime(datetime).to_string())
}

fn kind_for_duration(mins: f64) -> ServerProviderUsageWindowKind {
    if mins >= MONTH_MINS {
        ServerProviderUsageWindowKind::Monthly
    } else if mins >= WEEK_MINS {
        ServerProviderUsageWindowKind::Weekly
    } else {
        ServerProviderUsageWindowKind::Session
    }
}

fn label_for_kind(kind: ServerProviderUsageWindowKind) -> &'static str {
    match kind {
        ServerProviderUsageWindowKind::Session => "Session",
        ServerProviderUsageWindowKind::Weekly => "Weekly",
        ServerProviderUsageWindowKind::Monthly => "Monthly",
        ServerProviderUsageWindowKind::Other => "Other",
    }
}

fn window_label(id: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(id).expect("static window id")
}

/// `codexRateLimitsToWindows`. `primary` and `secondary` are positions, not
/// durations. Codex usually sends `windowDurationMins`; when it does not, paid
/// plans expose the 5-hour and weekly pair and Free/Go expose one monthly allowance.
fn rate_limits_to_windows(snapshot: &CodexRateLimitSnapshot) -> Vec<ServerProviderUsageWindow> {
    // Show the main allowance only. Model-specific notifications (such as Spark)
    // must not replace its primary/secondary rows. Older CLIs omit the limit id.
    if let Some(limit_id) = snapshot.limit_id()
        && !limit_id.is_empty()
        && limit_id != "codex"
    {
        return Vec::new();
    }
    let is_monthly_plan = matches!(snapshot.plan_type(), Some("free" | "go"));
    let positions = [
        (
            "primary",
            snapshot.primary(),
            if is_monthly_plan {
                MONTH_MINS
            } else {
                SESSION_MINS
            },
        ),
        ("secondary", snapshot.secondary(), WEEK_MINS),
    ];
    positions
        .into_iter()
        .filter_map(|(id, window, fallback_mins)| {
            let window = window.filter(|window| window.used_percent.is_finite())?;
            let window_duration_mins = window.window_duration_mins.unwrap_or(fallback_mins);
            let kind = kind_for_duration(window_duration_mins);
            Some(ServerProviderUsageWindow {
                id: window_label(id),
                kind,
                label: window_label(label_for_kind(kind)),
                used_percent: clamp_percent(window.used_percent),
                window_duration_mins: Some(window_duration_mins.max(0.0) as u64),
                resets_at: iso_from_epoch_seconds(window.resets_at),
            })
        })
        .collect()
}

/// `codexResetCreditsToContract`: the count of available credits and the
/// soonest expiry among them.
pub fn codex_reset_credits_to_contract(
    summary: Option<&CodexResetCreditsSummary>,
) -> Option<ServerProviderResetCredits> {
    let summary = summary?;
    let next_expires_at = summary
        .credits
        .iter()
        .flatten()
        .filter(|credit| credit.status == "available")
        .filter_map(|credit| credit.expires_at)
        .reduce(f64::min)
        .and_then(|soonest| iso_from_epoch_seconds(Some(soonest)));
    Some(ServerProviderResetCredits {
        available_count: summary.available_count.max(0.0) as u64,
        next_expires_at,
        next_credit_id: None,
    })
}

/// `codexRateLimitsToLimits`. The main bucket is selected explicitly from
/// `rateLimitsByLimitId.codex`; the legacy snapshot can name another limit.
pub fn codex_rate_limits_to_limits(
    snapshot: &CodexRateLimitSnapshot,
    rate_limits_by_limit_id: Option<&HashMap<String, CodexRateLimitSnapshot>>,
    reset_credits: Option<&CodexResetCreditsSummary>,
    checked_at: &str,
) -> ServerProviderUsageLimits {
    let main = rate_limits_by_limit_id
        .and_then(|by_id| by_id.get("codex"))
        .unwrap_or(snapshot);
    let mut limits = make_usage_limits(checked_at.to_owned(), rate_limits_to_windows(main));
    limits.reset_credits = codex_reset_credits_to_contract(reset_credits);
    limits
}

/// `codexRateLimitsToUpdate`: `None` when the snapshot names no windows.
pub fn codex_rate_limits_to_update(
    snapshot: &CodexRateLimitSnapshot,
) -> Option<ProviderUsageLimitsUpdate> {
    let windows = rate_limits_to_windows(snapshot);
    (!windows.is_empty()).then_some(ProviderUsageLimitsUpdate { windows })
}

/// `codexRateLimitsFailureMessage`: a bounded, client-safe reason. The raw
/// error stays in the log.
pub fn codex_rate_limits_failure_message(error: &AppServerError) -> String {
    match error {
        AppServerError::Request { error, .. } => {
            format!("Codex could not read usage (JSON-RPC {}).", error.code)
        }
        AppServerError::Spawn(_) => "Codex could not be started to read usage.".to_owned(),
        AppServerError::ProcessExited(_) => "Codex exited before it could report usage.".to_owned(),
        _ => "Codex did not answer the usage request.".to_owned(),
    }
}

/// `makeUnavailableUsageLimits` for a failed or skipped usage read.
pub fn codex_usage_probe_failed_limits(
    checked_at: &str,
    message: Option<&str>,
) -> ServerProviderUsageLimits {
    make_unavailable_usage_limits(
        checked_at.to_owned(),
        ServerProviderUsageUnavailableReason::ProbeFailed,
        message,
    )
}

/// `mergeCodexRateLimits`: a partial update keeps the fields it omits. A
/// model-specific update never replaces the main allowance.
pub fn merge_codex_rate_limits(
    previous: Option<&CodexRateLimitSnapshot>,
    update: &CodexRateLimitSnapshot,
) -> Option<CodexRateLimitSnapshot> {
    if let Some(limit_id) = update.limit_id()
        && !limit_id.is_empty()
        && limit_id != "codex"
    {
        return previous.cloned();
    }
    let Some(previous) = previous else {
        return Some(update.clone());
    };
    let mut merged = previous.clone();
    if update.limit_id.is_some() {
        merged.limit_id = update.limit_id.clone();
    }
    if update.plan_type.is_some() {
        merged.plan_type = update.plan_type.clone();
    }
    if update.rate_limit_reached_type.is_some() {
        merged.rate_limit_reached_type = update.rate_limit_reached_type.clone();
    }
    if update.primary.is_some() {
        merged.primary = update.primary.clone();
    }
    if update.secondary.is_some() {
        merged.secondary = update.secondary.clone();
    }
    Some(merged)
}

/// `formatCodexUsageLimitWait`: coarse remaining wait, such as `5d 5h`, `3h 20m`, `12m`.
fn format_wait(wait_ms: f64) -> String {
    let total_minutes = (wait_ms / 60_000.0).ceil() as i64;
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes % (24 * 60)) / 60;
    let minutes = total_minutes % 60;
    if days > 0 {
        return if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        };
    }
    if hours == 0 {
        return format!("{total_minutes}m");
    }
    if minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {minutes}m")
    }
}

fn next_step(rate_limit_reached_type: Option<&str>) -> &'static str {
    match rate_limit_reached_type {
        Some("workspace_owner_credits_depleted" | "workspace_member_credits_depleted") => {
            " The workspace has no credits to continue sooner: ask your workspace owner to add credits, or send the message again once the limit resets."
        }
        Some("workspace_owner_usage_limit_reached" | "workspace_member_usage_limit_reached") => {
            " The workspace spend limit is reached: ask your workspace owner to raise it, or send the message again once the limit resets."
        }
        _ => " Send the message again once the limit resets.",
    }
}

fn parse_iso_millis(value: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.timestamp_millis() as f64)
}

/// `codexUsageLimitMessage`: the message a usage-limit stop shows. It names the
/// exhausted window that has yet to reset, latest first; `at_iso` is the
/// stopping event's timestamp, not the wall clock.
pub fn codex_usage_limit_message(
    snapshot: Option<&CodexRateLimitSnapshot>,
    at_iso: &str,
) -> String {
    let at_ms = parse_iso_millis(at_iso);
    let windows = match (snapshot, at_ms) {
        (Some(snapshot), Some(_)) => rate_limits_to_windows(snapshot),
        _ => Vec::new(),
    };
    let mut reset = String::new();
    let mut latest_reset_ms = f64::NEG_INFINITY;
    if let Some(at_ms) = at_ms {
        for window in &windows {
            if window.used_percent < 100.0 {
                continue;
            }
            let Some(resets_at) = window.resets_at.as_deref() else {
                continue;
            };
            let Some(reset_ms) = parse_iso_millis(resets_at) else {
                continue;
            };
            if reset_ms <= at_ms || reset_ms <= latest_reset_ms {
                continue;
            }
            latest_reset_ms = reset_ms;
            reset = format!(
                " The {} limit resets in {}.",
                kind_word(window.kind),
                format_wait(reset_ms - at_ms)
            );
        }
    }
    format!(
        "Codex usage limit reached.{reset}{}",
        next_step(snapshot.and_then(CodexRateLimitSnapshot::rate_limit_reached_type))
    )
}

fn kind_word(kind: ServerProviderUsageWindowKind) -> &'static str {
    match kind {
        ServerProviderUsageWindowKind::Session => "session",
        ServerProviderUsageWindowKind::Weekly => "weekly",
        ServerProviderUsageWindowKind::Monthly => "monthly",
        ServerProviderUsageWindowKind::Other => "other",
    }
}

/// `codexUsageLimitResetAt`: when every exhausted window has reset. `None`
/// when no window is exhausted or any exhausted window lacks a reset time.
pub fn codex_usage_limit_reset_at(snapshot: Option<&CodexRateLimitSnapshot>) -> Option<String> {
    let windows: Vec<ServerProviderUsageWindow> = rate_limits_to_windows(snapshot?)
        .into_iter()
        .filter(|window| window.used_percent >= 100.0)
        .collect();
    if windows.is_empty() || windows.iter().any(|window| window.resets_at.is_none()) {
        return None;
    }
    windows
        .iter()
        .filter_map(|window| window.resets_at.clone())
        .reduce(|latest, candidate| {
            if parse_iso_millis(&candidate) > parse_iso_millis(&latest) {
                candidate
            } else {
                latest
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(value: serde_json::Value) -> CodexRateLimitSnapshot {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn primary_and_secondary_map_to_session_and_weekly() {
        let limits = codex_rate_limits_to_limits(
            &snapshot(json!({
                "primary": {"usedPercent": 42.0, "resetsAt": 1_800_000_000, "windowDurationMins": 300},
                "secondary": {"usedPercent": 7.5, "windowDurationMins": 10080}
            })),
            None,
            None,
            "2026-01-01T00:00:00.000Z",
        );
        let rows: Vec<(&str, &str, f64)> = limits
            .windows
            .iter()
            .map(|window| {
                (
                    window.id.as_str(),
                    window.label.as_str(),
                    window.used_percent,
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![("primary", "Session", 42.0), ("secondary", "Weekly", 7.5)]
        );
        assert_eq!(
            limits.windows[0].resets_at.as_deref(),
            Some("2027-01-15T08:00:00.000Z")
        );
    }

    #[test]
    fn duration_less_primary_is_monthly_on_free_and_go() {
        let limits = codex_rate_limits_to_limits(
            &snapshot(json!({"planType": "free", "primary": {"usedPercent": 10}})),
            None,
            None,
            "2026-01-01T00:00:00.000Z",
        );
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(
            limits.windows[0].kind,
            ServerProviderUsageWindowKind::Monthly
        );
        assert_eq!(limits.windows[0].window_duration_mins, Some(43_200));
    }

    #[test]
    fn main_bucket_wins_and_spark_stays_out() {
        let spark = snapshot(json!({"limitId": "spark", "primary": {"usedPercent": 99}}));
        assert!(codex_rate_limits_to_update(&spark).is_none());
        let main = snapshot(json!({"limitId": "codex", "primary": {"usedPercent": 5}}));
        let by_id = HashMap::from([("codex".to_owned(), main)]);
        let limits =
            codex_rate_limits_to_limits(&spark, Some(&by_id), None, "2026-01-01T00:00:00.000Z");
        assert_eq!(limits.windows[0].used_percent, 5.0);
    }

    #[test]
    fn reset_credits_count_available_and_report_soonest_expiry() {
        let summary: CodexResetCreditsSummary = serde_json::from_value(json!({
            "availableCount": -1,
            "credits": [
                {"status": "available", "expiresAt": 1_800_000_000},
                {"status": "available", "expiresAt": 1_700_000_000},
                {"status": "spent", "expiresAt": 1_600_000_000}
            ]
        }))
        .unwrap();
        let contract = codex_reset_credits_to_contract(Some(&summary)).unwrap();
        assert_eq!(contract.available_count, 0);
        assert_eq!(
            contract.next_expires_at.as_deref(),
            Some("2023-11-14T22:13:20.000Z")
        );
    }

    #[test]
    fn partial_update_keeps_fields_it_omits_and_honours_explicit_null() {
        let previous = snapshot(json!({
            "planType": "plus",
            "primary": {"usedPercent": 10},
            "secondary": {"usedPercent": 20}
        }));
        let merged =
            merge_codex_rate_limits(Some(&previous), &snapshot(json!({"primary": null}))).unwrap();
        assert_eq!(merged.plan_type(), Some("plus"));
        assert!(merged.primary().is_none());
        assert_eq!(merged.secondary().map(|w| w.used_percent), Some(20.0));
    }

    #[test]
    fn failure_messages_keep_only_the_category_and_code() {
        let request = AppServerError::Request {
            method: "account/rateLimits/read".into(),
            request_id: "1".into(),
            error: Box::new(turin_codex_app_server::errors::ProtocolError {
                code: -32000,
                message: "secret detail".into(),
                data: None,
            }),
        };
        assert_eq!(
            codex_rate_limits_failure_message(&request),
            "Codex could not read usage (JSON-RPC -32000)."
        );
        assert_eq!(
            codex_rate_limits_failure_message(&AppServerError::ProcessExited(Some(1))),
            "Codex exited before it could report usage."
        );
    }

    #[test]
    fn exhausted_window_names_reset_and_workspace_next_step() {
        let snapshot = snapshot(json!({
            "rateLimitReachedType": "workspace_owner_credits_depleted",
            "primary": {"usedPercent": 100, "resetsAt": 1_767_229_200}
        }));
        let message = codex_usage_limit_message(Some(&snapshot), "2026-01-01T00:00:00.000Z");
        assert!(message.starts_with("Codex usage limit reached. The session limit resets in 1h."));
        assert!(message.contains("ask your workspace owner to add credits"));
        assert_eq!(
            codex_usage_limit_reset_at(Some(&snapshot)).as_deref(),
            Some("2026-01-01T01:00:00.000Z")
        );
        assert_eq!(
            codex_usage_limit_message(None, "2026-01-01T00:00:00.000Z"),
            "Codex usage limit reached. Send the message again once the limit resets."
        );
    }

    #[test]
    fn reset_waits_for_every_exhausted_window() {
        let unknown = snapshot(json!({
            "primary": {"usedPercent": 100, "resetsAt": 1_767_229_200},
            "secondary": {"usedPercent": 100}
        }));
        assert!(codex_usage_limit_reset_at(Some(&unknown)).is_none());
    }

    #[test]
    fn wait_formatting_rounds_up_and_drops_zero_parts() {
        assert_eq!(format_wait(12.0 * 60_000.0), "12m");
        assert_eq!(format_wait(3.0 * 3_600_000.0 + 20.0 * 60_000.0), "3h 20m");
        assert_eq!(format_wait(5.0 * 86_400_000.0 + 5.0 * 3_600_000.0), "5d 5h");
        assert_eq!(format_wait(2.0 * 86_400_000.0), "2d");
    }
}
