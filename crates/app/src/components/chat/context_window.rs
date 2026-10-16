//! Port of upstream/apps/web/src/lib/contextWindow.ts

use crate::composer::view_model::ContextWindowCost;
use crate::state::contracts::{
    OrchestrationV2ProviderThread, OrchestrationV2ProviderTurnTokenUsage, OrchestrationV2TurnItem,
};

/// Nullable fields mirror upstream's `ContextWindowSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextWindowUsageSnapshot {
    pub used_tokens: f64,
    pub total_processed_tokens: Option<f64>,
    pub max_tokens: Option<f64>,
    pub remaining_tokens: Option<f64>,
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
    pub input_tokens: Option<f64>,
    pub cached_input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub reasoning_output_tokens: Option<f64>,
    pub last_used_tokens: Option<f64>,
    pub last_input_tokens: Option<f64>,
    pub last_cached_input_tokens: Option<f64>,
    pub last_output_tokens: Option<f64>,
    pub last_reasoning_output_tokens: Option<f64>,
    pub tool_uses: Option<f64>,
    pub duration_ms: Option<f64>,
    pub compacts_automatically: Option<bool>,
    pub auto_compact_threshold: Option<f64>,
    pub cost: Option<ContextWindowCost>,
    pub updated_at: String,
}

fn percentages(used_tokens: f64, max_tokens: Option<f64>) -> (Option<f64>, Option<f64>) {
    let used_percentage = max_tokens
        .filter(|max| *max > 0.)
        .map(|max| (used_tokens / max * 100.).min(100.));
    let remaining_percentage = used_percentage.map(|used| (100. - used).max(0.));
    (used_percentage, remaining_percentage)
}

fn remaining_tokens(used_tokens: f64, max_tokens: Option<f64>) -> Option<f64> {
    max_tokens.map(|max| (max - used_tokens).max(0.).round())
}

fn map_cost(cost: &turin_contracts::provider_runtime::ThreadTokenUsageCost) -> ContextWindowCost {
    ContextWindowCost {
        amount: cost.amount,
        currency: cost.currency.as_str().to_owned(),
    }
}

/// Newest usage report across a thread's provider turns. Snapshots order turns
/// by provider thread, not time, so pick by timestamp.
pub fn latest_provider_turn_token_usage<'a>(
    turns: impl IntoIterator<Item = OrchestrationV2ProviderTurnTokenUsageHolder<'a>>,
) -> Option<&'a OrchestrationV2ProviderTurnTokenUsage> {
    let mut latest: Option<&OrchestrationV2ProviderTurnTokenUsage> = None;
    for turn in turns {
        let Some(token_usage) = turn.token_usage else {
            continue;
        };
        // Ties go to the later entry, since live upserts append.
        if latest.is_none() || token_usage.updated_at >= latest.unwrap().updated_at {
            latest = Some(token_usage);
        }
    }
    latest
}

pub struct OrchestrationV2ProviderTurnTokenUsageHolder<'a> {
    pub token_usage: Option<&'a OrchestrationV2ProviderTurnTokenUsage>,
}

/// Prefers the provider's live usage report (#8144); falls back to the last compaction item.
pub fn derive_latest_context_window_snapshot(
    entries: &[OrchestrationV2TurnItem],
    live_usage: Option<&OrchestrationV2ProviderTurnTokenUsage>,
    provider_thread: Option<&OrchestrationV2ProviderThread>,
) -> Option<ContextWindowUsageSnapshot> {
    if let Some(live_usage) = live_usage {
        let used_tokens = live_usage.used_tokens as f64;
        let used_tokens = used_tokens.max(0.);
        let max_tokens = live_usage.max_tokens.flatten().map(|max| max as f64);
        let (used_percentage, remaining_percentage) = percentages(used_tokens, max_tokens);
        let remaining_tokens = remaining_tokens(used_tokens, max_tokens);
        return Some(ContextWindowUsageSnapshot {
            used_tokens,
            total_processed_tokens: None,
            max_tokens,
            remaining_tokens,
            used_percentage,
            remaining_percentage,
            input_tokens: live_usage.input_tokens.map(|v| v as f64),
            cached_input_tokens: live_usage.cached_input_tokens.map(|v| v as f64),
            output_tokens: live_usage.output_tokens.map(|v| v as f64),
            reasoning_output_tokens: live_usage.reasoning_output_tokens.map(|v| v as f64),
            last_used_tokens: None,
            last_input_tokens: None,
            last_cached_input_tokens: None,
            last_output_tokens: None,
            last_reasoning_output_tokens: None,
            tool_uses: None,
            duration_ms: None,
            compacts_automatically: Some(true),
            auto_compact_threshold: None,
            cost: None,
            updated_at: live_usage.updated_at.clone(),
        });
    }
    let provider_usage = provider_thread.and_then(|thread| thread.context_usage.as_ref());
    let provider_usage_updated_at = provider_thread.map(|thread| &thread.updated_at);
    if let (Some(provider_usage), Some(updated_at)) = (provider_usage, provider_usage_updated_at) {
        let max_tokens = provider_usage.max_tokens.map(|max| u64::from(max) as f64);
        let used_tokens = provider_usage.used_tokens as f64;
        let (used_percentage, remaining_percentage) = percentages(used_tokens, max_tokens);
        return Some(ContextWindowUsageSnapshot {
            used_tokens,
            total_processed_tokens: provider_usage.total_processed_tokens.map(|v| v as f64),
            max_tokens,
            remaining_tokens: remaining_tokens(used_tokens, max_tokens),
            used_percentage,
            remaining_percentage,
            input_tokens: provider_usage.input_tokens.map(|v| v as f64),
            cached_input_tokens: provider_usage.cached_input_tokens.map(|v| v as f64),
            output_tokens: provider_usage.output_tokens.map(|v| v as f64),
            reasoning_output_tokens: provider_usage.reasoning_output_tokens.map(|v| v as f64),
            last_used_tokens: provider_usage.last_used_tokens.map(|v| v as f64),
            last_input_tokens: provider_usage.last_input_tokens.map(|v| v as f64),
            last_cached_input_tokens: provider_usage.last_cached_input_tokens.map(|v| v as f64),
            last_output_tokens: provider_usage.last_output_tokens.map(|v| v as f64),
            last_reasoning_output_tokens: provider_usage
                .last_reasoning_output_tokens
                .map(|v| v as f64),
            tool_uses: provider_usage.tool_uses.map(|v| v as f64),
            duration_ms: provider_usage.duration_ms.map(|v| v as f64),
            compacts_automatically: provider_usage.compacts_automatically,
            auto_compact_threshold: provider_usage
                .auto_compact_threshold
                .map(|v| u64::from(v) as f64),
            cost: provider_usage.cost.as_ref().map(map_cost),
            updated_at: updated_at.to_string(),
        });
    }
    for entry in entries.iter().rev() {
        let OrchestrationV2TurnItem::Compaction {
            before_token_count,
            after_token_count,
            started_at,
            updated_at,
            ..
        } = entry
        else {
            continue;
        };
        let used_tokens = after_token_count.map(|v| v as f64);
        let Some(used_tokens) = used_tokens.filter(|used| used.is_finite() && *used >= 0.) else {
            continue;
        };
        let max_tokens = None;
        let (used_percentage, remaining_percentage) = percentages(used_tokens, max_tokens);
        let remaining_tokens = remaining_tokens(used_tokens, max_tokens);
        let updated_at = started_at
            .as_ref()
            .map(|at| at.to_string())
            .unwrap_or_else(|| updated_at.to_string());
        return Some(ContextWindowUsageSnapshot {
            used_tokens,
            total_processed_tokens: before_token_count.map(|v| v as f64),
            max_tokens,
            remaining_tokens,
            used_percentage,
            remaining_percentage,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            last_used_tokens: None,
            last_input_tokens: None,
            last_cached_input_tokens: None,
            last_output_tokens: None,
            last_reasoning_output_tokens: None,
            tool_uses: None,
            duration_ms: None,
            compacts_automatically: Some(true),
            auto_compact_threshold: None,
            cost: None,
            updated_at,
        });
    }
    None
}

/// `formatContextWindowTokens`.
pub fn format_context_window_tokens(value: Option<f64>) -> String {
    let Some(value) = value.filter(|v| v.is_finite()) else {
        return "0".to_owned();
    };
    // JS `toFixed(1)` rounds ties up, where Rust's formatter rounds them to even.
    let fixed = |value: f64| format!("{:.1}", (value * 10.0).round() / 10.0);
    let trim = |text: String| text.strip_suffix(".0").map(str::to_owned).unwrap_or(text);
    if value < 1_000.0 {
        return format!("{}", value.round());
    }
    if value < 10_000.0 {
        return format!("{}k", trim(fixed(value / 1_000.0)));
    }
    // Anything from 999,500 rounds to 1000k, so it belongs to the next unit.
    if value < 999_500.0 {
        return format!("{}k", (value / 1_000.0).round());
    }
    format!("{}m", trim(fixed(value / 1_000_000.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2TurnItem;
    use serde_json::json;

    fn compaction(after: u64, before: u64) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "type": "compaction",
            "id": "compaction-1",
            "threadId": "thread-1",
            "runId": null,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "completed",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": "2026-06-20T00:00:00.000Z",
            "driver": null,
            "beforeTokenCount": before,
            "afterTokenCount": after,
        }))
        .unwrap()
    }

    #[test]
    fn uses_retained_compaction_token_data_when_available() {
        let snapshot =
            derive_latest_context_window_snapshot(&[compaction(2_000, 10_000)], None, None)
                .unwrap();
        assert_eq!(snapshot.used_tokens, 2_000.);
        assert_eq!(snapshot.total_processed_tokens, Some(10_000.));
    }

    #[test]
    fn prefers_current_provider_usage_and_preserves_acp_cost() {
        let thread: OrchestrationV2ProviderThread = serde_json::from_value(json!({
            "id": "provider-thread:1",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": null,
            "appThreadId": "thread-1",
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "active",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "pendingBackgroundTasks": [],
            "contextUsage": {
                "usedTokens": 2500,
                "maxTokens": 10000,
                "cost": { "amount": 0.42, "currency": "USD" },
            },
            "createdAt": "2026-08-23T00:00:00.000Z",
            "updatedAt": "2026-08-23T00:00:00.000Z",
        }))
        .unwrap();
        let snapshot = derive_latest_context_window_snapshot(&[], None, Some(&thread)).unwrap();
        assert_eq!(snapshot.used_tokens, 2_500.);
        assert_eq!(snapshot.max_tokens, Some(10_000.));
        assert_eq!(snapshot.remaining_tokens, Some(7_500.));
        assert_eq!(snapshot.used_percentage, Some(25.));
        assert_eq!(snapshot.cost.as_ref().unwrap().amount, 0.42);
    }

    #[test]
    fn formats_compact_token_values() {
        assert_eq!(format_context_window_tokens(Some(1_500.)), "1.5k");
    }

    #[test]
    fn formats_million_boundary() {
        for (tokens, expected) in [
            (999_499., "999k"),
            (999_500., "1m"),
            (999_999., "1m"),
            (1_000_000., "1m"),
            (1_100_000., "1.1m"),
        ] {
            assert_eq!(
                format_context_window_tokens(Some(tokens)),
                expected,
                "{tokens}"
            );
        }
    }

    #[test]
    fn prefers_live_report_over_compaction_items() {
        let live = OrchestrationV2ProviderTurnTokenUsage {
            used_tokens: 42_000,
            max_tokens: Some(Some(200_000)),
            input_tokens: Some(40_000),
            cached_input_tokens: None,
            output_tokens: Some(2_000),
            reasoning_output_tokens: None,
            prompt_cache_ttl_ms: None,
            updated_at: "2026-08-27T00:00:00.000Z".into(),
        };
        let snapshot =
            derive_latest_context_window_snapshot(&[compaction(1, 1)], Some(&live), None).unwrap();
        assert_eq!(snapshot.used_tokens, 42_000.);
        assert_eq!(snapshot.max_tokens, Some(200_000.));
        assert_eq!(snapshot.remaining_tokens, Some(158_000.));
        assert_eq!(snapshot.used_percentage, Some(21.));
    }

    #[test]
    fn handles_live_report_without_known_context_window() {
        let live = OrchestrationV2ProviderTurnTokenUsage {
            used_tokens: 42_000,
            max_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            prompt_cache_ttl_ms: None,
            updated_at: "2026-08-27T00:00:00.000Z".into(),
        };
        let snapshot = derive_latest_context_window_snapshot(&[], Some(&live), None).unwrap();
        assert!(snapshot.max_tokens.is_none());
        assert!(snapshot.used_percentage.is_none());
    }

    #[test]
    fn latest_provider_turn_token_usage_picks_newest_report() {
        let a = OrchestrationV2ProviderTurnTokenUsage {
            used_tokens: 300_000,
            max_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            prompt_cache_ttl_ms: None,
            updated_at: "2026-06-20T12:00:00.000Z".into(),
        };
        let b = OrchestrationV2ProviderTurnTokenUsage {
            used_tokens: 90_000,
            max_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            prompt_cache_ttl_ms: None,
            updated_at: "2026-06-20T09:00:00.000Z".into(),
        };
        let latest = latest_provider_turn_token_usage([
            OrchestrationV2ProviderTurnTokenUsageHolder {
                token_usage: Some(&a),
            },
            OrchestrationV2ProviderTurnTokenUsageHolder { token_usage: None },
            OrchestrationV2ProviderTurnTokenUsageHolder {
                token_usage: Some(&b),
            },
        ])
        .unwrap();
        assert_eq!(latest.used_tokens, 300_000);
    }
}
