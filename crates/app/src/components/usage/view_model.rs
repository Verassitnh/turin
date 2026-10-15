//! Port of upstream/apps/web/src/state/usage.ts (the pure merge and selection
//! logic of `useUsage`; the atom plumbing is the [`UsageBackend`] seam).
//!
//! Every connected environment answers the same typed query; the client merges
//! the results. Raw transcripts never leave the machine that produced them.

use std::collections::HashSet;

use turin_contracts::EnvironmentId;
use turin_contracts::usage::{
    USAGE_CONTRACT_VERSION, UsageBucket, UsageProviderKind, UsageSummary, UsageSummaryInput,
};
use turin_shared::usage_merge::{EnvironmentUsage, MergedUsage, merge_usage};

use crate::state::helpers::usage_progress::UsageProgressEnvironment;

#[derive(Clone, Debug)]
pub struct EnvironmentUsageStatus {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub is_pending: bool,
    pub can_read_diagnostics: bool,
    pub is_connected: bool,
    pub error: Option<String>,
    pub summary: Option<UsageSummary>,
    pub needs_cursor_keychain_access: bool,
}

impl EnvironmentUsageStatus {
    pub fn progress_environment(&self) -> UsageProgressEnvironment<'_> {
        UsageProgressEnvironment {
            label: self.label.clone(),
            is_connected: self.is_connected,
            is_pending: self.is_pending,
            error: self.error.clone(),
            summary: self.summary.as_ref(),
        }
    }
}

/// Merges every environment that has answered. `keep_bucket` narrows the merge,
/// for example to one model; source ownership still applies, so the result
/// matches that slice of the full merge. Session counts are per directory and
/// are not narrowed.
pub fn merge_answered_usage(
    environments: &[EnvironmentUsageStatus],
    keep_bucket: Option<&dyn Fn(&UsageBucket) -> bool>,
) -> MergedUsage {
    let answered: Vec<EnvironmentUsage> = environments
        .iter()
        .filter_map(|environment| {
            let summary = environment.summary.as_ref()?;
            let summary = match keep_bucket {
                None => summary.clone(),
                Some(keep) => UsageSummary {
                    buckets: summary
                        .buckets
                        .iter()
                        .filter(|bucket| keep(bucket))
                        .cloned()
                        .collect(),
                    ..summary.clone()
                },
            };
            Some(EnvironmentUsage {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
                summary,
            })
        })
        .collect();
    merge_usage(&answered, USAGE_CONTRACT_VERSION)
}

/// Drops hidden providers' buckets and sources before merging, so totals,
/// shares, and session counts all describe only the visible providers.
fn without_providers(
    environments: &[EnvironmentUsageStatus],
    hidden_providers: &HashSet<UsageProviderKind>,
) -> Vec<EnvironmentUsageStatus> {
    if hidden_providers.is_empty() {
        return environments.to_vec();
    }
    environments
        .iter()
        .map(|environment| match &environment.summary {
            None => environment.clone(),
            Some(summary) => EnvironmentUsageStatus {
                summary: Some(UsageSummary {
                    buckets: summary
                        .buckets
                        .iter()
                        .filter(|bucket| !hidden_providers.contains(&bucket.provider))
                        .cloned()
                        .collect(),
                    sources: summary
                        .sources
                        .iter()
                        .filter(|source| !hidden_providers.contains(&source.fingerprint.provider))
                        .cloned()
                        .collect(),
                    ..summary.clone()
                }),
                ..environment.clone()
            },
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageShown {
    pub window: UsageSummaryInput,
    pub merged: MergedUsage,
}

#[derive(Clone, Debug)]
pub struct UsageView {
    pub merged: MergedUsage,
    pub environments: Vec<EnvironmentUsageStatus>,
    pub selected_environments: Vec<EnvironmentUsageStatus>,
    /// True until at least one selected environment has answered.
    pub is_pending: bool,
    /// The usage to draw: this window's once a selected environment answers,
    /// until then the last window answered for the same selection. `None` while
    /// nothing has answered and something still could.
    pub shown: Option<UsageShown>,
    /// True while environments that have not failed are still answering. Failed
    /// environments are reported through their own error rows: totals will not
    /// improve by waiting on them, so they must not read as "still reporting".
    pub is_partial: bool,
}

pub type EnvironmentSelection = Option<HashSet<EnvironmentId>>;

/// `lastAnswered` of `useUsage`, kept across renders by the page.
#[derive(Clone, Debug, Default)]
pub struct UsageViewMemory {
    last_answered: Option<LastAnswered>,
}

#[derive(Clone, Debug)]
struct LastAnswered {
    shown: UsageShown,
    selection: EnvironmentSelection,
    hidden: HashSet<UsageProviderKind>,
}

/// `useUsage`'s derivation from the environments' statuses for one window.
pub fn compute_usage_view(
    environments: Vec<EnvironmentUsageStatus>,
    selected_environment_ids: &EnvironmentSelection,
    hidden_providers: &HashSet<UsageProviderKind>,
    input: &UsageSummaryInput,
    memory: &mut UsageViewMemory,
) -> UsageView {
    let selected_environments: Vec<EnvironmentUsageStatus> = match selected_environment_ids {
        None => environments.clone(),
        Some(ids) => environments
            .iter()
            .filter(|environment| ids.contains(&environment.environment_id))
            .cloned()
            .collect(),
    };

    let merged = merge_answered_usage(
        &without_providers(&selected_environments, hidden_providers),
        None,
    );

    let answered_count = selected_environments
        .iter()
        .filter(|environment| environment.summary.is_some())
        .count();
    let still_reporting = selected_environments
        .iter()
        .filter(|environment| environment.summary.is_none() && environment.error.is_none())
        .count();
    let is_pending = answered_count == 0 && still_reporting > 0;

    // Remembered per render, so the kept usage is on screen in the same frame
    // the new window starts pending.
    if answered_count > 0 {
        let shown = UsageShown {
            window: input.clone(),
            merged: merged.clone(),
        };
        let unchanged = memory.last_answered.as_ref().is_some_and(|last| {
            last.shown == shown
                && last.selection == *selected_environment_ids
                && last.hidden == *hidden_providers
        });
        if !unchanged {
            memory.last_answered = Some(LastAnswered {
                shown,
                selection: selected_environment_ids.clone(),
                hidden: hidden_providers.clone(),
            });
        }
    }
    // Kept usage only stands in for the same environments and provider filter.
    let kept = memory
        .last_answered
        .as_ref()
        .filter(|last| {
            last.selection == *selected_environment_ids && last.hidden == *hidden_providers
        })
        .map(|last| last.shown.clone());
    // With no answers, even failed ones keep the last answered usage on screen.
    let shown = if answered_count > 0 {
        Some(UsageShown {
            window: input.clone(),
            merged: merged.clone(),
        })
    } else {
        kept.or_else(|| {
            (!is_pending).then(|| UsageShown {
                window: input.clone(),
                merged: merged.clone(),
            })
        })
    };

    UsageView {
        merged,
        environments,
        selected_environments,
        is_pending,
        shown,
        is_partial: answered_count > 0 && still_reporting > 0,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use serde_json::json;
    use turin_contracts::TrimmedNonEmptyString;

    pub fn id(value: &str) -> EnvironmentId {
        EnvironmentId::new(value).unwrap()
    }

    pub fn input(since: &str, until: &str) -> UsageSummaryInput {
        UsageSummaryInput {
            since_day: turin_contracts::usage::UsageDay::new(since).unwrap(),
            until_day: turin_contracts::usage::UsageDay::new(until).unwrap(),
            time_zone: TrimmedNonEmptyString::new("UTC").unwrap(),
            resolution: None,
            since_time: None,
            until_time: None,
            await_refresh: None,
        }
    }

    pub fn summary(buckets: &[(&str, &str, &str, u64, f64)]) -> UsageSummary {
        let buckets: Vec<_> = buckets
            .iter()
            .map(|(day, provider, model, tokens, cost)| {
                json!({
                    "day": day, "provider": provider, "model": model,
                    "totals": {
                        "uncachedInputTokens": tokens, "cachedInputTokens": 0,
                        "cacheCreationTokens": 0, "outputTokens": 0, "reasoningTokens": 0,
                    },
                    "costUsd": cost, "cacheSavingsUsd": 0,
                    "costSource": "modelPriced", "records": 1, "unpricedRecords": 0, "sessions": 1,
                })
            })
            .collect();
        let source = |provider: &str| {
            json!({
                "fingerprint": {
                    "hostId": "h", "provider": provider, "resolvedHomePath": "/home", "volumeId": "",
                },
                "status": "ok", "scannedFiles": 1, "skippedFiles": 0, "malformedRecords": 0,
                "distinctSessions": 1, "message": null,
            })
        };
        serde_json::from_value(json!({
            "contractVersion": 6,
            "readAt": "2026-08-22T00:00:00.000Z",
            "timeZone": "UTC",
            "sinceDay": "2026-08-01",
            "untilDay": "2026-08-22",
            "buckets": buckets,
            "sources": [source("codex"), source("claude")],
            "pricing": { "status": "fresh", "source": "litellm", "fetchedAt": null, "knownModels": 3 },
            "scanDurationMs": 1,
        }))
        .unwrap()
    }

    pub fn status(
        environment: &str,
        summary: Option<UsageSummary>,
        error: Option<&str>,
    ) -> EnvironmentUsageStatus {
        EnvironmentUsageStatus {
            environment_id: id(environment),
            label: environment.to_owned(),
            is_pending: summary.is_none() && error.is_none(),
            can_read_diagnostics: true,
            is_connected: true,
            error: error.map(str::to_owned),
            summary,
            needs_cursor_keychain_access: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn day_summary() -> UsageSummary {
        summary(&[
            ("2026-08-20", "codex", "gpt-5", 100, 1.0),
            ("2026-08-21", "claude", "opus", 300, 3.0),
        ])
    }

    #[test]
    fn merges_every_answered_environment_and_ignores_the_rest() {
        let environments = vec![
            status("a", Some(day_summary()), None),
            status("b", None, Some("This environment could not report usage.")),
        ];
        let mut memory = UsageViewMemory::default();
        let view = compute_usage_view(
            environments,
            &None,
            &HashSet::new(),
            &input("2026-08-01", "2026-08-22"),
            &mut memory,
        );
        assert_eq!(view.merged.total_tokens, 400);
        assert_eq!(view.merged.cost_usd, 4.0);
        assert!(!view.is_pending);
        // The failed environment is not "still reporting".
        assert!(!view.is_partial);
        assert!(view.shown.is_some());
    }

    #[test]
    fn partial_while_a_healthy_environment_is_still_answering() {
        let environments = vec![
            status("a", Some(day_summary()), None),
            status("b", None, None),
        ];
        let view = compute_usage_view(
            environments,
            &None,
            &HashSet::new(),
            &input("2026-08-01", "2026-08-22"),
            &mut UsageViewMemory::default(),
        );
        assert!(view.is_partial);
        assert!(!view.is_pending);
    }

    #[test]
    fn nothing_shown_while_the_first_answer_is_pending() {
        let view = compute_usage_view(
            vec![status("a", None, None)],
            &None,
            &HashSet::new(),
            &input("2026-08-01", "2026-08-22"),
            &mut UsageViewMemory::default(),
        );
        assert!(view.is_pending);
        assert!(view.shown.is_none());
    }

    #[test]
    fn keeps_the_last_window_on_screen_until_the_new_one_answers() {
        let mut memory = UsageViewMemory::default();
        let first = input("2026-08-01", "2026-08-22");
        compute_usage_view(
            vec![status("a", Some(day_summary()), None)],
            &None,
            &HashSet::new(),
            &first,
            &mut memory,
        );
        let second = input("2026-07-01", "2026-08-22");
        let view = compute_usage_view(
            vec![status("a", None, None)],
            &None,
            &HashSet::new(),
            &second,
            &mut memory,
        );
        let shown = view.shown.expect("kept usage");
        assert_eq!(shown.window, first);
        assert_eq!(shown.merged.total_tokens, 400);
    }

    #[test]
    fn kept_usage_does_not_stand_in_for_another_selection_or_filter() {
        let mut memory = UsageViewMemory::default();
        let window = input("2026-08-01", "2026-08-22");
        compute_usage_view(
            vec![
                status("a", Some(day_summary()), None),
                status("b", None, None),
            ],
            &None,
            &HashSet::new(),
            &window,
            &mut memory,
        );
        let only_b = Some(HashSet::from([id("b")]));
        let view = compute_usage_view(
            vec![
                status("a", Some(day_summary()), None),
                status("b", None, None),
            ],
            &only_b,
            &HashSet::new(),
            &window,
            &mut memory,
        );
        assert!(view.shown.is_none());
        let hidden = HashSet::from([UsageProviderKind::Codex]);
        let view = compute_usage_view(
            vec![status("a", None, None)],
            &None,
            &hidden,
            &window,
            &mut memory,
        );
        assert!(view.shown.is_none());
    }

    #[test]
    fn without_any_answer_even_failures_keep_the_last_usage() {
        let mut memory = UsageViewMemory::default();
        let window = input("2026-08-01", "2026-08-22");
        compute_usage_view(
            vec![status("a", Some(day_summary()), None)],
            &None,
            &HashSet::new(),
            &window,
            &mut memory,
        );
        let view = compute_usage_view(
            vec![status("a", None, Some("down"))],
            &None,
            &HashSet::new(),
            &window,
            &mut memory,
        );
        assert_eq!(view.shown.expect("kept").merged.total_tokens, 400);
        // A fresh page with only failures shows the (empty) window itself.
        let view = compute_usage_view(
            vec![status("a", None, Some("down"))],
            &None,
            &HashSet::new(),
            &window,
            &mut UsageViewMemory::default(),
        );
        assert_eq!(view.shown.expect("empty").merged.total_tokens, 0);
    }

    #[test]
    fn hidden_providers_drop_out_of_totals() {
        let hidden = HashSet::from([UsageProviderKind::Codex]);
        let view = compute_usage_view(
            vec![status("a", Some(day_summary()), None)],
            &None,
            &hidden,
            &input("2026-08-01", "2026-08-22"),
            &mut UsageViewMemory::default(),
        );
        assert_eq!(view.merged.total_tokens, 300);
        assert_eq!(view.merged.providers.len(), 1);
    }

    #[test]
    fn narrowing_to_one_model_keeps_that_models_buckets() {
        let environments = vec![status("a", Some(day_summary()), None)];
        let only_opus = |bucket: &UsageBucket| bucket.model.as_str() == "opus";
        let merged = merge_answered_usage(&environments, Some(&only_opus));
        assert_eq!(merged.total_tokens, 300);
        assert_eq!(merged.models.len(), 1);
    }
}
