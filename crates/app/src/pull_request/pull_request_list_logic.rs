//! Port of upstream/apps/web/src/components/pullRequest/pullRequestList.logic.ts

use super::pull_request_list_preferences::PullRequestListSort;
use crate::state::support::collate::locale_compare;
use crate::state::time::{parse_ms, to_iso_string};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestChecksState, PullRequestInvolvement, PullRequestListState,
    PullRequestMergeability, PullRequestReviewDecision, PullRequestState,
};
use turin_contracts::pull_request::{
    PullRequestListEntry, PullRequestListProjectError, PullRequestListResult,
    PullRequestProviderSummary,
};

/// A listed change request tagged with the environment that read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPullRequestEntry {
    #[serde(flatten)]
    pub entry: PullRequestListEntry,
    pub environment_id: EnvironmentId,
}
impl Deref for EnvironmentPullRequestEntry {
    type Target = PullRequestListEntry;
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}
impl DerefMut for EnvironmentPullRequestEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entry
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPullRequestError {
    #[serde(flatten)]
    pub error: PullRequestListProjectError,
    pub environment_id: EnvironmentId,
}

pub type PullRequestViewers = HashMap<String, String>;
#[derive(Clone, Debug, PartialEq)]
pub struct MergedPullRequestList {
    pub viewers: PullRequestViewers,
    pub providers: Vec<PullRequestProviderSummary>,
    pub entries: Vec<EnvironmentPullRequestEntry>,
    pub errors: Vec<EnvironmentPullRequestError>,
    pub truncated: bool,
    pub next_cursors: HashMap<String, turin_contracts::pull_request::PullRequestListCursors>,
    pub truncated_environments: Vec<String>,
}

pub fn pull_request_label_color(color: Option<&str>) -> Option<String> {
    let raw = crate::state::contracts::js_trim(color?);
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    (hex.len() == 6 && hex.bytes().all(|ch| ch.is_ascii_hexdigit())).then(|| format!("#{hex}"))
}

/// Merge each connected environment's answer into one list.
pub fn merge_pull_request_lists(
    answers: &[(EnvironmentId, PullRequestListResult)],
) -> Option<MergedPullRequestList> {
    if answers.is_empty() {
        return None;
    }
    let mut merged = MergedPullRequestList {
        viewers: HashMap::new(),
        providers: vec![],
        entries: vec![],
        errors: vec![],
        truncated: false,
        next_cursors: HashMap::new(),
        truncated_environments: vec![],
    };
    for (environment_id, answer) in answers {
        for (host, login) in &answer.viewers {
            merged.viewers.insert(
                format!("{environment_id} {host}"),
                login.as_str().to_owned(),
            );
        }
        for provider in &answer.providers {
            if let Some(held) = merged
                .providers
                .iter_mut()
                .find(|p| p.host == provider.host)
            {
                if !held.configured {
                    let mut chosen = provider.clone();
                    chosen.project_count = held.project_count;
                    *held = chosen;
                }
                held.project_count = turin_contracts::PositiveInt::new(
                    held.project_count.get() + provider.project_count.get(),
                )
                .expect("positive sum");
                held.searches_on_host &= provider.searches_on_host;
                held.configured |= provider.configured;
            } else {
                merged.providers.push(provider.clone());
            }
        }
        merged
            .entries
            .extend(
                answer
                    .entries
                    .iter()
                    .cloned()
                    .map(|entry| EnvironmentPullRequestEntry {
                        entry,
                        environment_id: environment_id.clone(),
                    }),
            );
        merged
            .errors
            .extend(
                answer
                    .errors
                    .iter()
                    .cloned()
                    .map(|error| EnvironmentPullRequestError {
                        error,
                        environment_id: environment_id.clone(),
                    }),
            );
        merged.truncated |= answer.truncated;
        if answer.truncated {
            merged
                .truncated_environments
                .push(environment_id.as_str().to_owned());
        }
        if !answer.next_cursors.is_empty() {
            merged.next_cursors.insert(
                environment_id.as_str().to_owned(),
                answer.next_cursors.clone(),
            );
        }
    }
    merged
        .entries
        .sort_by(|a, b| locale_compare(&b.updated_at, &a.updated_at));
    Some(merged)
}

pub fn pull_request_entry_key(
    entry: &PullRequestListEntry,
    environment_id: Option<&str>,
) -> String {
    format!(
        "{}{}:{}#{}",
        environment_id
            .map(|id| format!("{id}:"))
            .unwrap_or_default(),
        entry.host,
        entry.repository,
        entry.number
    )
}

pub fn pull_request_environment_set_key(environment_ids: &[String]) -> String {
    let mut ids = environment_ids.to_vec();
    ids.sort_by(|a, b| locale_compare(a, b));
    ids.join(",")
}

pub fn resolve_project_scope<Id: Clone + PartialEq>(
    project_id: Option<Id>,
    project_ids: &[Id],
    projects_known: bool,
) -> Option<Id> {
    if !projects_known {
        return project_id;
    }
    project_id.filter(|id| project_ids.contains(id))
}

pub fn find_scoped_project<'a, T, F, G>(
    projects: &'a [T],
    environment_id: Option<&str>,
    project_id: Option<&str>,
    id_of: F,
    environment_of: G,
) -> Option<&'a T>
where
    F: Fn(&T) -> &str,
    G: Fn(&T) -> &str,
{
    let project_id = project_id?;
    let matches: Vec<_> = projects
        .iter()
        .filter(|project| id_of(project) == project_id)
        .collect();
    match environment_id {
        Some(environment_id) => matches
            .into_iter()
            .find(|project| environment_of(project) == environment_id),
        None => (matches.len() == 1).then(|| matches[0]),
    }
}

pub fn resolve_query_environment_ids<Id: Clone + PartialEq>(
    environment_ids: &[Id],
    projects: &[(String, Id)],
    scoped_project_environment: Option<&Id>,
    scoped_project_id: Option<&str>,
    projects_known: bool,
) -> Vec<Id> {
    if let Some(scoped) = scoped_project_environment {
        return environment_ids
            .iter()
            .filter(|id| *id == scoped)
            .cloned()
            .collect();
    }
    if scoped_project_id.is_none() || !projects_known {
        return environment_ids.to_vec();
    }
    environment_ids
        .iter()
        .filter(|id| {
            projects.iter().any(|(project_id, env)| {
                Some(project_id.as_str()) == scoped_project_id && env == *id
            })
        })
        .cloned()
        .collect()
}

pub fn resolve_selected_environment_id<Id: Clone + Eq + std::hash::Hash>(
    named: Option<&Id>,
    known: &HashSet<Id>,
    fallback: Option<&Id>,
) -> Option<Id> {
    match named {
        Some(named) if known.contains(named) => Some(named.clone()),
        _ => fallback.cloned(),
    }
}

pub fn score_pull_request_match(entry: &PullRequestListEntry, query: &str) -> u8 {
    let needle = crate::state::contracts::js_trim(query).to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    let number = needle.strip_prefix('#').unwrap_or(&needle);
    if !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()) {
        return if entry.number.to_string() == number {
            100
        } else {
            0
        };
    }
    let title = entry.title.as_str().to_lowercase();
    let terms: Vec<_> = needle.split_whitespace().collect();
    if title == needle {
        90
    } else if title.contains(&needle) {
        80
    } else if terms.len() > 1 && terms.iter().all(|term| title.contains(term)) {
        70
    } else if entry.head_branch.as_str().to_lowercase().contains(&needle) {
        60
    } else if entry
        .author
        .as_ref()
        .is_some_and(|a| a.login.as_str().to_lowercase().contains(&needle))
    {
        50
    } else if entry.repository.as_str().to_lowercase().contains(&needle) {
        40
    } else if terms.iter().any(|term| title.contains(term)) {
        30
    } else {
        10
    }
}

pub fn rank_pull_request_matches(
    entries: &[PullRequestListEntry],
    query: &str,
) -> Vec<PullRequestListEntry> {
    let mut out = entries.to_vec();
    if !crate::state::contracts::js_trim(query).is_empty() {
        out.sort_by(|a, b| {
            score_pull_request_match(b, query)
                .cmp(&score_pull_request_match(a, query))
                .then_with(|| locale_compare(&b.updated_at, &a.updated_at))
        });
    }
    out
}

fn merge_readiness_tier(entry: &PullRequestListEntry) -> u8 {
    if entry.mergeability == PullRequestMergeability::Conflicting {
        4
    } else if entry.state != PullRequestState::Open {
        3
    } else if entry.is_draft {
        2
    } else if entry.checks_state == Some(PullRequestChecksState::Passing)
        && entry.review_decision == Some(PullRequestReviewDecision::Approved)
    {
        0
    } else if entry.checks_state == Some(PullRequestChecksState::Passing) {
        1
    } else {
        2
    }
}
fn size(entry: &PullRequestListEntry) -> u64 {
    entry.additions + entry.deletions
}
pub fn rank_pull_requests_by_merge_readiness(
    entries: &[PullRequestListEntry],
    has_measured_size: impl Fn(&PullRequestListEntry) -> bool,
) -> Vec<PullRequestListEntry> {
    let mut out = entries.to_vec();
    out.sort_by(|a, b| {
        merge_readiness_tier(a)
            .cmp(&merge_readiness_tier(b))
            .then_with(|| has_measured_size(b).cmp(&has_measured_size(a)))
            .then_with(|| size(a).cmp(&size(b)))
            .then_with(|| locale_compare(&b.updated_at, &a.updated_at))
    });
    out
}
fn rank_by_tier_then_recency(
    entries: &[PullRequestListEntry],
    tier: impl Fn(&PullRequestListEntry) -> u8,
) -> Vec<PullRequestListEntry> {
    let mut out = entries.to_vec();
    out.sort_by(|a, b| {
        tier(a).cmp(&tier(b)).then_with(|| {
            match (parse_ms(&a.updated_at), parse_ms(&b.updated_at)) {
                (Some(a), Some(b)) => b.cmp(&a),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        })
    });
    out
}
pub fn rank_pull_requests_blocked_on_author(
    entries: &[PullRequestListEntry],
) -> Vec<PullRequestListEntry> {
    rank_by_tier_then_recency(entries, |entry| {
        if entry.state != PullRequestState::Open {
            6
        } else if entry.mergeability == PullRequestMergeability::Conflicting {
            0
        } else if entry.review_decision == Some(PullRequestReviewDecision::ChangesRequested) {
            1
        } else if entry.checks_state == Some(PullRequestChecksState::Failing) {
            2
        } else if entry.is_draft {
            3
        } else if entry.checks_state == Some(PullRequestChecksState::Passing)
            && entry.review_decision == Some(PullRequestReviewDecision::Approved)
        {
            5
        } else {
            4
        }
    })
}
pub fn rank_pull_requests_blocked_on_reviewer(
    entries: &[PullRequestListEntry],
) -> Vec<PullRequestListEntry> {
    rank_by_tier_then_recency(entries, |entry| {
        u8::from(entry.state != PullRequestState::Open)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestGroupKey {
    ReviewRequested,
    Authored,
    Others,
}
impl PullRequestGroupKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReviewRequested => "Review requested",
            Self::Authored => "Authored",
            Self::Others => "Others",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestGroup<T> {
    pub key: PullRequestGroupKey,
    pub label: &'static str,
    pub entries: Vec<T>,
}
impl<T> PullRequestGroup<T> {
    fn new(key: PullRequestGroupKey, entries: Vec<T>) -> Self {
        Self {
            key,
            label: key.label(),
            entries,
        }
    }
}

pub fn sort_pull_request_groups(
    groups: &[PullRequestGroup<PullRequestListEntry>],
    sort: PullRequestListSort,
    search_text: &str,
    has_measured_size: impl Fn(&PullRequestListEntry) -> bool,
    involvement: PullRequestInvolvement,
) -> Vec<PullRequestGroup<PullRequestListEntry>> {
    let mut out = groups.to_vec();
    if sort == PullRequestListSort::Ready {
        if crate::state::contracts::js_trim(search_text).is_empty() {
            for group in &mut out {
                group.entries =
                    rank_pull_requests_by_merge_readiness(&group.entries, &has_measured_size)
            }
        }
        return out;
    }
    if sort == PullRequestListSort::Blocked {
        if crate::state::contracts::js_trim(search_text).is_empty() {
            for group in &mut out {
                let role = match group.key {
                    PullRequestGroupKey::Others => involvement,
                    PullRequestGroupKey::Authored => PullRequestInvolvement::Authored,
                    PullRequestGroupKey::ReviewRequested => PullRequestInvolvement::Reviewing,
                };
                group.entries = match role {
                    PullRequestInvolvement::All => group.entries.clone(),
                    PullRequestInvolvement::Authored => {
                        rank_pull_requests_blocked_on_author(&group.entries)
                    }
                    PullRequestInvolvement::Reviewing => {
                        rank_pull_requests_blocked_on_reviewer(&group.entries)
                    }
                }
            }
        }
        return out;
    }
    if sort == PullRequestListSort::Updated {
        return out;
    }
    for group in &mut out {
        group.entries.sort_by(|a, b| {
            let timestamp = |entry: &PullRequestListEntry| {
                parse_ms(&entry.updated_at)
                    .or_else(|| parse_ms(&entry.created_at))
                    .unwrap_or(0)
            };
            let recency = || timestamp(b).cmp(&timestamp(a));
            match sort {
                PullRequestListSort::Newest | PullRequestListSort::Oldest => {
                    let left = parse_ms(&a.created_at);
                    let right = parse_ms(&b.created_at);
                    right
                        .is_some()
                        .cmp(&left.is_some())
                        .then_with(|| {
                            if sort == PullRequestListSort::Newest {
                                right.unwrap_or(0).cmp(&left.unwrap_or(0))
                            } else {
                                left.unwrap_or(0).cmp(&right.unwrap_or(0))
                            }
                        })
                        .then_with(recency)
                }
                PullRequestListSort::Largest | PullRequestListSort::Smallest => {
                    has_measured_size(b)
                        .cmp(&has_measured_size(a))
                        .then_with(|| {
                            if sort == PullRequestListSort::Largest {
                                size(b).cmp(&size(a))
                            } else {
                                size(a).cmp(&size(b))
                            }
                        })
                        .then_with(recency)
                }
                _ => Ordering::Equal,
            }
        })
    }
    out
}

#[cfg(test)]
#[path = "pull_request_list_logic_tests/mod.rs"]
mod tests;
