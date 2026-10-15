//! Port of upstream/packages/client-runtime/src/state/usageProgress.ts

use std::collections::HashSet;

use turin_contracts::usage::{UsageProviderKind, UsageSummary};

/// `UsageProgressEnvironment`.
#[derive(Debug, Clone)]
pub struct UsageProgressEnvironment<'a> {
    pub label: String,
    pub is_connected: bool,
    pub is_pending: bool,
    pub error: Option<String>,
    pub summary: Option<&'a UsageSummary>,
}

/// Where one environment's answer stands for the usage on screen.
///
/// - `Inactive`: disconnected or failed, so waiting on it changes nothing.
/// - `Loading`: no summary yet.
/// - `Stale`: its summary on screen predates the current request.
/// - `Partway`: answered this round while slow sources finish refreshing.
/// - `Ready`: answered.
///
/// While a manual `refreshing` runs, every summary counts as stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageEnvironmentProgress {
    Inactive,
    Loading,
    Stale,
    Partway { providers: Vec<UsageProviderKind> },
    Ready,
}

pub fn usage_environment_progress(
    environment: &UsageProgressEnvironment<'_>,
    refreshing: bool,
) -> UsageEnvironmentProgress {
    if !environment.is_connected || environment.error.is_some() {
        return UsageEnvironmentProgress::Inactive;
    }
    let Some(summary) = environment.summary else {
        return UsageEnvironmentProgress::Loading;
    };
    if refreshing {
        return UsageEnvironmentProgress::Stale;
    }
    // A refreshing source without a pending follow-up has nothing left to wait on.
    if !environment.is_pending {
        return UsageEnvironmentProgress::Ready;
    }
    let mut providers: Vec<UsageProviderKind> = Vec::new();
    for source in &summary.sources {
        if source.refreshing == Some(true) && !providers.contains(&source.fingerprint.provider) {
            providers.push(source.fingerprint.provider);
        }
    }
    if providers.is_empty() {
        UsageEnvironmentProgress::Stale
    } else {
        UsageEnvironmentProgress::Partway { providers }
    }
}

pub fn updating_providers_label(
    providers: &[UsageProviderKind],
    provider_label: impl Fn(UsageProviderKind) -> String,
) -> String {
    if providers.len() == 1 {
        format!("Updating {}…", provider_label(providers[0]))
    } else {
        format!("Updating {} providers…", providers.len())
    }
}

/// `usageProgress` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageProgress {
    pub dimmed: bool,
    pub label: Option<String>,
}

/// Loading state for the selected environments' merged usage.
///
/// The page dims only while old numbers are all that is on screen. Once any
/// environment answers, the label carries what is still coming, so one slow
/// device never holds the page dimmed.
pub fn usage_progress(
    environments: &[UsageProgressEnvironment<'_>],
    refreshing: bool,
    provider_label: impl Fn(UsageProviderKind) -> String,
) -> UsageProgress {
    let progress: Vec<(&str, UsageEnvironmentProgress)> = environments
        .iter()
        .map(|environment| {
            (
                environment.label.as_str(),
                usage_environment_progress(environment, refreshing),
            )
        })
        .collect();
    let waiting: Vec<&str> = progress
        .iter()
        .filter(|(_, phase)| {
            matches!(
                phase,
                UsageEnvironmentProgress::Loading | UsageEnvironmentProgress::Stale
            )
        })
        .map(|(label, _)| *label)
        .collect();
    let mut providers: Vec<UsageProviderKind> = Vec::new();
    for (_, phase) in &progress {
        if let UsageEnvironmentProgress::Partway { providers: entry } = phase {
            for provider in entry {
                if !providers.contains(provider) {
                    providers.push(*provider);
                }
            }
        }
    }
    let answered = progress.iter().any(|(_, phase)| {
        matches!(
            phase,
            UsageEnvironmentProgress::Partway { .. } | UsageEnvironmentProgress::Ready
        )
    });
    let label = if waiting.is_empty() {
        if providers.is_empty() {
            None
        } else {
            Some(updating_providers_label(&providers, provider_label))
        }
    } else if environments.len() == 1 {
        Some("Updating…".to_owned())
    } else if waiting.len() == 1 {
        Some(format!("Updating {}…", waiting[0]))
    } else {
        Some(format!("Updating {} environments…", waiting.len()))
    };
    UsageProgress {
        dimmed: !answered
            && progress
                .iter()
                .any(|(_, phase)| matches!(phase, UsageEnvironmentProgress::Stale)),
        label,
    }
}

/// Which figures in the merged usage on screen are still coming in, so clients
/// can mute just those.
///
/// - `partial`: totals will change because some environment is still answering.
/// - `providers`: providers whose own figures will change. An environment that
///   has not answered yet, or whose whole summary is stale, could change any
///   provider, so it sets `every_provider`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLoadingState {
    pub partial: bool,
    pub every_provider: bool,
    pub providers: HashSet<UsageProviderKind>,
}

pub fn usage_loading_state(
    environments: &[UsageProgressEnvironment<'_>],
    refreshing: bool,
) -> UsageLoadingState {
    let mut providers = HashSet::new();
    let mut every_provider = false;
    for environment in environments {
        match usage_environment_progress(environment, refreshing) {
            UsageEnvironmentProgress::Loading | UsageEnvironmentProgress::Stale => {
                every_provider = true;
            }
            UsageEnvironmentProgress::Partway { providers: entry } => providers.extend(entry),
            UsageEnvironmentProgress::Inactive | UsageEnvironmentProgress::Ready => {}
        }
    }
    UsageLoadingState {
        partial: every_provider || !providers.is_empty(),
        every_provider,
        providers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::usage::{
        USAGE_CONTRACT_VERSION, UsageDay, UsagePricing, UsagePricingStatus, UsageSource,
        UsageSourceFingerprint, UsageSourceStatus,
    };

    fn summary(refreshing: &[UsageProviderKind]) -> UsageSummary {
        let providers = std::iter::once(UsageProviderKind::Codex).chain(refreshing.iter().copied());
        UsageSummary {
            contract_version: USAGE_CONTRACT_VERSION,
            read_at: "2026-09-05T12:00:00Z".into(),
            time_zone: TrimmedNonEmptyString::new("UTC").unwrap(),
            since_day: UsageDay::new("2026-09-05").unwrap(),
            until_day: UsageDay::new("2026-09-05").unwrap(),
            buckets: vec![],
            sources: providers
                .map(|provider| UsageSource {
                    fingerprint: UsageSourceFingerprint {
                        host_id: TrimmedNonEmptyString::new("host").unwrap(),
                        provider,
                        resolved_home_path: TrimmedNonEmptyString::new("/home").unwrap(),
                        volume_id: "1:1".into(),
                    },
                    status: UsageSourceStatus::Ok,
                    scanned_files: 1,
                    skipped_files: 0,
                    malformed_records: 0,
                    distinct_sessions: 1,
                    message: None,
                    action: None,
                    refreshing: (provider != UsageProviderKind::Codex).then_some(true),
                })
                .collect(),
            pricing: UsagePricing {
                status: UsagePricingStatus::Fresh,
                source: TrimmedNonEmptyString::new("test").unwrap(),
                fetched_at: None,
                known_models: 1,
            },
            scan_duration_ms: 1,
        }
    }

    fn environment<'a>(
        label: &str,
        summary: Option<&'a UsageSummary>,
    ) -> UsageProgressEnvironment<'a> {
        UsageProgressEnvironment {
            label: label.into(),
            is_connected: true,
            is_pending: false,
            error: None,
            summary,
        }
    }

    fn pending<'a>(label: &str, summary: Option<&'a UsageSummary>) -> UsageProgressEnvironment<'a> {
        UsageProgressEnvironment {
            is_pending: true,
            ..environment(label, summary)
        }
    }

    fn provider_label(provider: UsageProviderKind) -> String {
        let name = serde_json::to_value(provider).unwrap();
        format!("{} label", name.as_str().unwrap())
    }

    fn progress(environments: &[UsageProgressEnvironment<'_>], refreshing: bool) -> UsageProgress {
        usage_progress(environments, refreshing, provider_label)
    }

    fn expected(dimmed: bool, label: Option<&str>) -> UsageProgress {
        UsageProgress {
            dimmed,
            label: label.map(str::to_owned),
        }
    }

    #[test]
    fn dims_a_lone_environment_while_it_refetches_over_its_old_totals() {
        let plain = summary(&[]);
        assert_eq!(
            progress(&[pending("a", Some(&plain))], false),
            expected(true, Some("Updating…"))
        );
        assert_eq!(
            progress(&[environment("a", Some(&plain))], false),
            expected(false, None)
        );
    }

    #[test]
    fn undims_once_one_device_answers_and_names_the_slow_one() {
        let plain = summary(&[]);
        let waiting = [pending("a", Some(&plain)), pending("b", Some(&plain))];
        assert_eq!(
            progress(&waiting, false),
            expected(true, Some("Updating 2 environments…"))
        );
        assert_eq!(
            progress(
                &[
                    environment("a", Some(&plain)),
                    waiting[1].clone(),
                    environment("c", Some(&plain))
                ],
                false
            ),
            expected(false, Some("Updating b…"))
        );
        assert_eq!(
            progress(&[environment("a", Some(&plain)), pending("b", None)], false),
            expected(false, Some("Updating b…"))
        );
    }

    #[test]
    fn names_a_slow_source_without_dimming_once_its_environment_answers_from_cache() {
        let plain = summary(&[]);
        let cursor = summary(&[UsageProviderKind::Cursor]);
        let both = summary(&[UsageProviderKind::Cursor, UsageProviderKind::Grok]);
        let partway = pending("a", Some(&cursor));
        assert_eq!(
            usage_environment_progress(&partway, false),
            UsageEnvironmentProgress::Partway {
                providers: vec![UsageProviderKind::Cursor]
            }
        );
        assert_eq!(
            progress(&[partway.clone(), environment("b", Some(&plain))], false),
            expected(false, Some("Updating cursor label…"))
        );
        assert_eq!(
            progress(&[partway.clone(), pending("b", Some(&both))], false),
            expected(false, Some("Updating 2 providers…"))
        );
        // Without a follow-up in flight, a leftover refreshing source is not waited on.
        let settled = UsageProgressEnvironment {
            is_pending: false,
            ..partway
        };
        assert_eq!(progress(&[settled], false), expected(false, None));
    }

    #[test]
    fn ignores_reconnecting_and_failed_environments() {
        let plain = summary(&[]);
        let reconnecting = UsageProgressEnvironment {
            is_connected: false,
            ..pending("reconnecting", Some(&plain))
        };
        let failed = UsageProgressEnvironment {
            error: Some("Offline".into()),
            ..environment("failed", None)
        };
        let ignored = [reconnecting.clone(), failed];
        let mut with_healthy = vec![environment("a", Some(&plain))];
        with_healthy.extend(ignored.iter().cloned());
        assert_eq!(progress(&with_healthy, false), expected(false, None));
        with_healthy.push(pending("slow", None));
        assert_eq!(
            progress(&with_healthy, false),
            expected(false, Some("Updating slow…"))
        );
        assert_eq!(
            usage_environment_progress(&reconnecting, false),
            UsageEnvironmentProgress::Inactive
        );
    }

    #[test]
    fn dims_every_answered_environment_while_a_manual_refresh_runs() {
        // Pricing refreshes first, so neither query is pending yet.
        let plain = summary(&[]);
        assert_eq!(
            progress(
                &[
                    environment("a", Some(&plain)),
                    environment("b", Some(&plain))
                ],
                true
            ),
            expected(true, Some("Updating 2 environments…"))
        );
    }

    #[test]
    fn loading_state_marks_only_refreshing_providers_while_the_rest_have_answered() {
        let plain = summary(&[]);
        let cursor = summary(&[UsageProviderKind::Cursor]);
        let state = usage_loading_state(
            &[pending("a", Some(&cursor)), environment("b", Some(&plain))],
            false,
        );
        assert_eq!(
            state,
            UsageLoadingState {
                partial: true,
                every_provider: false,
                providers: HashSet::from([UsageProviderKind::Cursor]),
            }
        );
    }

    #[test]
    fn loading_state_marks_every_provider_while_an_environment_has_not_answered() {
        let plain = summary(&[]);
        let state = usage_loading_state(
            &[environment("a", Some(&plain)), environment("b", None)],
            false,
        );
        assert!(state.every_provider);
        assert!(state.partial);
    }

    #[test]
    fn loading_state_ignores_environments_that_cannot_answer_and_settles_once_all_have() {
        let plain = summary(&[]);
        let state = usage_loading_state(
            &[
                environment("a", Some(&plain)),
                UsageProgressEnvironment {
                    is_connected: false,
                    ..environment("b", None)
                },
                UsageProgressEnvironment {
                    error: Some("Offline".into()),
                    ..environment("c", None)
                },
            ],
            false,
        );
        assert_eq!(
            state,
            UsageLoadingState {
                partial: false,
                every_provider: false,
                providers: HashSet::new(),
            }
        );
    }
}
