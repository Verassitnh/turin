//! Port of upstream/apps/web/src/components/chat/ContextWindowMeter.logic.ts
//! and the pure helpers of ContextWindowMeter.tsx (`formatPercentage`).
//!
//! Port of upstream/apps/web/src/components/chat/ContextWindowMeter.tsx (pure helpers).

use crate::composer::catalog::ProviderInstanceEntry;
use crate::composer::view_model::ContextWindowCost;

/// Used when the provider did not report its cache TTL: Claude's 1h cache plus slack.
const CLAUDE_RESUME_COMPACTION_FALLBACK_MS: i64 = 70 * 60_000;
const CLAUDE_RESUME_COMPACTION_TOKENS: f64 = 100_000.0;

pub fn provider_supports_manual_compaction(provider: Option<&ProviderInstanceEntry>) -> bool {
    provider.is_some_and(|provider| {
        provider
            .slash_commands
            .iter()
            .any(|command| command.name == "compact")
    })
}

/// `hasAvailableCompactionProvider`: restrict to the locked continuation
/// group, then look for a `/compact` command on the selectable instance.
pub fn has_available_compaction_provider(
    providers: &[ProviderInstanceEntry],
    driver_kind: &str,
    instance_id: Option<&str>,
    locked_instance_id: Option<&str>,
) -> bool {
    let of_driver: Vec<&ProviderInstanceEntry> = providers
        .iter()
        .filter(|provider| provider.driver_kind == driver_kind)
        .collect();
    let group = locked_instance_id
        .and_then(|locked| {
            of_driver
                .iter()
                .find(|provider| provider.instance_id == locked)
        })
        .and_then(|provider| provider.continuation_group_key.as_deref());
    let compatible: Vec<&ProviderInstanceEntry> = match group {
        Some(group) if !group.is_empty() => of_driver
            .into_iter()
            .filter(|p| p.continuation_group_key.as_deref() == Some(group))
            .collect(),
        _ => of_driver,
    };
    // `resolveSelectableProviderInstanceEntry`: the requested instance when it
    // is among the candidates, else the first one.
    let resolved = instance_id
        .and_then(|id| {
            compatible
                .iter()
                .find(|provider| provider.instance_id == id)
        })
        .or_else(|| compatible.first())
        .copied();
    provider_supports_manual_compaction(resolved)
}

/// True once a large Claude context's prompt cache has expired, so the next
/// turn would re-read all of it uncached. Times are epoch milliseconds.
pub fn should_offer_resume_compaction(
    provider: Option<&str>,
    used_tokens: Option<f64>,
    updated_at_ms: Option<i64>,
    prompt_cache_ttl_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    if provider != Some("claudeAgent")
        || used_tokens.unwrap_or(0.0) < CLAUDE_RESUME_COMPACTION_TOKENS
    {
        return false;
    }
    let Some(updated_at) = updated_at_ms else {
        return false;
    };
    now_ms - updated_at >= prompt_cache_ttl_ms.unwrap_or(CLAUDE_RESUME_COMPACTION_FALLBACK_MS)
}

/// `formatContextWindowCompactionMessage`.
pub fn format_context_window_compaction_message(
    model_display_name: Option<&str>,
    auto_compact_threshold: Option<f64>,
) -> String {
    if let Some(threshold) = auto_compact_threshold.filter(|threshold| *threshold > 0.0) {
        return format!(
            "Compacts automatically at {} tokens.",
            crate::composer::submission::format_en_us(threshold.round() as usize)
        );
    }
    match model_display_name.filter(|name| !name.is_empty()) {
        Some(name) => format!("Context for {name} compacts automatically when needed."),
        None => "Context compacts automatically when needed.".to_owned(),
    }
}

/// `shouldReserveContextWindowMeter`: hold the meter's slot while a started
/// thread's activities load, unless the provider is known not to stream usage.
pub fn should_reserve_context_window_meter(
    meter_enabled: bool,
    detail_loading: bool,
    thread_started: bool,
    provider_reports_context_window: Option<bool>,
) -> bool {
    meter_enabled
        && detail_loading
        && thread_started
        && provider_reports_context_window != Some(false)
}

/// `formatContextWindowCost`.
pub fn format_context_window_cost(cost: &ContextWindowCost) -> String {
    let magnitude = cost.amount.abs();
    let digits = if magnitude > 0.0 && magnitude < 0.01 {
        4
    } else {
        2
    };
    format!("{} {:.*}", cost.currency, digits, cost.amount)
}

/// `formatPercentage` of the meter popup.
pub fn format_percentage(value: Option<f64>) -> Option<String> {
    let value = value.filter(|value| value.is_finite())?;
    if value < 10.0 {
        // JS `toFixed(1)` rounds ties up; Rust's formatter rounds them to even.
        let text = format!("{:.1}", (value * 10.0).round() / 10.0);
        return Some(format!("{}%", text.strip_suffix(".0").unwrap_or(&text)));
    }
    Some(format!("{}%", value.round()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::catalog::ProviderSlashCommand;

    fn provider(
        instance: &str,
        driver: &str,
        group: Option<&str>,
        compact: bool,
    ) -> ProviderInstanceEntry {
        let mut entry = ProviderInstanceEntry::new(instance, driver, instance);
        entry.continuation_group_key = group.map(str::to_owned);
        if compact {
            entry.slash_commands.push(ProviderSlashCommand {
                name: "compact".into(),
                ..Default::default()
            });
        }
        entry
    }

    #[test]
    fn manual_compaction_needs_a_compact_command_on_the_selectable_instance() {
        let providers = [
            provider("claude", "claudeAgent", Some("a"), false),
            provider("claude-b", "claudeAgent", Some("b"), true),
        ];
        assert!(!has_available_compaction_provider(
            &providers,
            "claudeAgent",
            Some("claude"),
            None
        ));
        assert!(has_available_compaction_provider(
            &providers,
            "claudeAgent",
            Some("claude-b"),
            None
        ));
        // A thread locked to group `a` cannot reach the other group's command.
        assert!(!has_available_compaction_provider(
            &providers,
            "claudeAgent",
            Some("claude-b"),
            Some("claude")
        ));
        assert!(!has_available_compaction_provider(
            &providers, "codex", None, None
        ));
    }

    #[test]
    fn resume_compaction_is_offered_once_a_big_claude_cache_expires() {
        let now = 10_000_000_000;
        let stale = now - 71 * 60_000;
        assert!(should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(120_000.0),
            Some(stale),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(99_999.0),
            Some(stale),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("codex"),
            Some(120_000.0),
            Some(stale),
            None,
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(120_000.0),
            Some(now - 60_000),
            None,
            now
        ));
        assert!(should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(120_000.0),
            Some(now - 60_000),
            Some(60_000),
            now
        ));
        assert!(!should_offer_resume_compaction(
            Some("claudeAgent"),
            Some(120_000.0),
            None,
            None,
            now
        ));
    }

    #[test]
    fn compaction_message_prefers_the_threshold() {
        assert_eq!(
            format_context_window_compaction_message(Some("Opus"), Some(160000.0)),
            "Compacts automatically at 160,000 tokens."
        );
        assert_eq!(
            format_context_window_compaction_message(Some("Opus"), None),
            "Context for Opus compacts automatically when needed."
        );
        assert_eq!(
            format_context_window_compaction_message(None, Some(0.0)),
            "Context compacts automatically when needed."
        );
    }

    #[test]
    fn reserves_the_meter_slot_only_while_loading_a_started_thread() {
        assert!(should_reserve_context_window_meter(true, true, true, None));
        assert!(should_reserve_context_window_meter(
            true,
            true,
            true,
            Some(true)
        ));
        assert!(!should_reserve_context_window_meter(
            true,
            true,
            true,
            Some(false)
        ));
        assert!(!should_reserve_context_window_meter(
            false, true, true, None
        ));
        assert!(!should_reserve_context_window_meter(
            true, false, true, None
        ));
        assert!(!should_reserve_context_window_meter(
            true, true, false, None
        ));
    }

    #[test]
    fn formats_cost_and_percentages() {
        let cost = |amount| ContextWindowCost {
            amount,
            currency: "USD".into(),
        };
        assert_eq!(format_context_window_cost(&cost(1.5)), "USD 1.50");
        assert_eq!(format_context_window_cost(&cost(0.004)), "USD 0.0040");
        assert_eq!(format_context_window_cost(&cost(0.0)), "USD 0.00");
        assert_eq!(format_percentage(Some(4.0)), Some("4%".into()));
        assert_eq!(format_percentage(Some(4.25)), Some("4.3%".into()));
        assert_eq!(format_percentage(Some(42.6)), Some("43%".into()));
        assert_eq!(format_percentage(None), None);
        assert_eq!(format_percentage(Some(f64::NAN)), None);
    }
}
