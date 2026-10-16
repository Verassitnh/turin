//! Port of upstream/apps/web/src/state/pullRequests.ts (and `pullRequests.test.ts`): the shared
//! pull request summary every surface that shows one pull request — a list row, its detail, a
//! linked thread's saved snapshot — agrees on, by always keeping the newest of what they saw.
//!
//! The atoms become plain values: [`PullRequestSummaryObservations`] holds what was last observed
//! per pull request, [`saved_pull_request_summaries`] indexes the thread links' snapshots, and
//! [`shared_pull_request_summary`] is `useSharedPullRequestSummary` over both. The merged
//! per-environment list and stats queries are the store's (`view_model`); their `error` is the
//! first environment's failure, and an environment that fails contributes nothing.

use std::collections::HashMap;

use turin_contracts::pull_request::{
    PullRequestDetail, PullRequestListEntry, PullRequestRef, PullRequestSummary,
};
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestSnapshot};
use turin_contracts::{EnvironmentId, ProjectId};

use crate::state::helpers::source_control_clients::SOURCE_CONTROL_CLIENTS;
use crate::state::support::thread_pull_requests::{
    ThreadPullRequestKeySource, thread_pull_request_key_of, visible_thread_pull_requests,
};
use crate::state::time::parse_ms;

/// `ObservedPullRequestSummary`.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedPullRequestSummary {
    pub summary: PullRequestSummary,
    /// Client arrival time, the only ordering older servers leave us for same-dated snapshots.
    pub observed_at: f64,
}

fn parse_date(value: &str) -> f64 {
    parse_ms(value).map_or(f64::NAN, |ms| ms as f64)
}

/// Positive when `incoming` is the newer snapshot. Merged is final. Then the host's own update
/// time, then the server's read-start time, which survives its caches; a snapshot without one
/// never beats a stamped read. Zero when neither side carries a read time. (`NaN` for an
/// unparseable update time, as `Date.parse` gives, which beats nothing.)
fn compare_summaries(current: &PullRequestSummary, incoming: &PullRequestSummary) -> f64 {
    let merged =
        f64::from(incoming.state == turin_contracts::pull_request::PullRequestState::Merged)
            - f64::from(current.state == turin_contracts::pull_request::PullRequestState::Merged);
    if merged != 0. {
        return merged;
    }
    let updated =
        parse_date(incoming.updated_at.as_str()) - parse_date(current.updated_at.as_str());
    if updated != 0. {
        return updated;
    }
    if current.observed_at.is_none() && incoming.observed_at.is_none() {
        return 0.;
    }
    incoming.observed_at.unwrap_or(f64::NEG_INFINITY)
        - current.observed_at.unwrap_or(f64::NEG_INFINITY)
}

/// `newestPullRequestSummary`.
pub fn newest_pull_request_summary(
    current: Option<&PullRequestSummary>,
    observed: Option<&PullRequestSummary>,
) -> Option<PullRequestSummary> {
    match (current, observed) {
        (None, observed) => observed.cloned(),
        (current, None) => current.cloned(),
        (Some(current), Some(observed)) => Some(if compare_summaries(current, observed) >= 0. {
            observed.clone()
        } else {
            current.clone()
        }),
    }
}

/// `pullRequestListEntryToSummary`: reuse list status without treating its deferred line-count
/// placeholders as real stats.
pub fn pull_request_list_entry_to_summary(entry: &PullRequestListEntry) -> PullRequestSummary {
    PullRequestSummary {
        provider: entry.provider.clone(),
        project_id: entry.project_id.clone(),
        repository: entry.repository.clone(),
        number: entry.number,
        title: entry.title.clone(),
        url: entry.url.clone(),
        state: entry.state,
        is_draft: Some(entry.is_draft),
        head_branch: entry.head_branch.clone(),
        base_branch: entry.base_branch.clone(),
        closed_at: None,
        merged_at: None,
        updated_at: entry.updated_at.clone(),
        observed_at: entry.observed_at,
        author: Some(entry.author.clone()),
        additions: None,
        deletions: None,
        changed_files: None,
        review_decision: entry.review_decision.map(Some),
        checks_state: entry.checks_state.map(Some),
        mergeability: Some(entry.mergeability),
        stack: None,
    }
}

/// A detail read as a summary (upstream passes the detail itself where a summary is wanted: it has
/// every summary field, and none of the list's deferred placeholders).
pub fn pull_request_detail_to_summary(detail: &PullRequestDetail) -> PullRequestSummary {
    PullRequestSummary {
        provider: detail.provider.clone(),
        project_id: detail.project_id.clone(),
        repository: detail.repository.clone(),
        number: detail.number,
        title: detail.title.clone(),
        url: detail.url.clone(),
        state: detail.state,
        is_draft: Some(detail.is_draft),
        head_branch: detail.head_branch.clone(),
        base_branch: detail.base_branch.clone(),
        closed_at: Some(detail.closed_at.clone()),
        merged_at: Some(detail.merged_at.clone()),
        updated_at: detail.updated_at.clone(),
        observed_at: detail.observed_at,
        author: Some(detail.author.clone()),
        additions: Some(detail.additions),
        deletions: Some(detail.deletions),
        changed_files: Some(detail.changed_files),
        review_decision: None,
        checks_state: None,
        mergeability: Some(detail.mergeability),
        stack: None,
    }
}

/// `pullRequestSummaryKey`: a project has one remote, so its id already pins the host. Leaving the
/// host out lets a list row, a hostless legacy reference and a URL-derived thread reference share
/// one entry.
pub fn pull_request_summary_key(
    environment_id: &EnvironmentId,
    reference: &PullRequestRef,
) -> String {
    serde_json::json!([
        environment_id.as_str(),
        reference.project_id.as_str(),
        reference.repository.as_str().to_lowercase(),
        reference.number.get(),
    ])
    .to_string()
}

/// `newestPullRequestObservation`: the observation to hold after `incoming` arrives.
pub fn newest_pull_request_observation(
    current: Option<&ObservedPullRequestSummary>,
    incoming: Option<ObservedPullRequestSummary>,
) -> Option<ObservedPullRequestSummary> {
    let (current, incoming) = match (current, incoming) {
        (None, incoming) => return incoming,
        (Some(current), None) => return Some(current.clone()),
        (Some(current), Some(incoming)) => (current, incoming),
    };
    let mut order = compare_summaries(&current.summary, &incoming.summary);
    // Client clocks only break ties between snapshots that both lack a server read time.
    if order == 0.
        && current.summary.observed_at.is_none()
        && incoming.summary.observed_at.is_none()
    {
        order = incoming.observed_at - current.observed_at;
    }
    if order.is_nan() || order <= 0. {
        return Some(current.clone());
    }
    // A sparse summary must not erase known status, or carry old detail stats into a new list
    // read.
    let mut summary = incoming.summary;
    summary.is_draft = summary.is_draft.or(current.summary.is_draft);
    summary.mergeability = summary.mergeability.or(current.summary.mergeability);
    summary.review_decision = summary.review_decision.or(current.summary.review_decision);
    summary.checks_state = summary.checks_state.or(current.summary.checks_state);
    Some(ObservedPullRequestSummary {
        summary,
        observed_at: incoming.observed_at,
    })
}

/// What was last observed of each pull request (`observedPullRequestSummaryAtom`).
#[derive(Debug, Default)]
pub struct PullRequestSummaryObservations {
    by_key: HashMap<String, ObservedPullRequestSummary>,
}

impl PullRequestSummaryObservations {
    pub fn get(
        &self,
        environment_id: &EnvironmentId,
        reference: &PullRequestRef,
    ) -> Option<&ObservedPullRequestSummary> {
        self.by_key
            .get(&pull_request_summary_key(environment_id, reference))
    }

    /// `observePullRequestSummary`; whether the observation changed anything.
    pub fn observe(
        &mut self,
        environment_id: &EnvironmentId,
        reference: &PullRequestRef,
        summary: PullRequestSummary,
        observed_at: f64,
    ) -> bool {
        let key = pull_request_summary_key(environment_id, reference);
        let previous = self.by_key.get(&key);
        let next = newest_pull_request_observation(
            previous,
            Some(ObservedPullRequestSummary {
                summary,
                observed_at,
            }),
        );
        if next.as_ref() == previous {
            return false;
        }
        if let Some(next) = next {
            self.by_key.insert(key, next);
        }
        true
    }

    /// Rows of a merged listing, each observed under its own environment.
    pub fn observe_entries(
        &mut self,
        entries: &[super::pull_request_list_logic::EnvironmentPullRequestEntry],
        observed_at: f64,
    ) {
        for entry in entries {
            self.observe_list(
                &entry.environment_id,
                std::slice::from_ref(&entry.entry),
                observed_at,
            );
        }
    }

    /// The listing's own rows, observed with when the answer arrived (`usePullRequestList`'s
    /// layout effect).
    pub fn observe_list(
        &mut self,
        environment_id: &EnvironmentId,
        entries: &[PullRequestListEntry],
        observed_at: f64,
    ) {
        for entry in entries {
            let reference = PullRequestRef {
                project_id: entry.project_id.clone(),
                host: Some(entry.host.clone()),
                expected_account_id: None,
                allow_stale: None,
                repository: entry.repository.clone(),
                number: entry.number,
            };
            self.observe(
                environment_id,
                &reference,
                pull_request_list_entry_to_summary(entry),
                observed_at,
            );
        }
    }
}

/// `threadPullRequestLinkToSummary`: without the host update time a snapshot cannot be ordered
/// against a fresh read, so it sits out.
pub fn thread_pull_request_link_to_summary(
    link: &ThreadPullRequestLink,
    project_id: &ProjectId,
) -> Option<PullRequestSummary> {
    let snapshot: &ThreadPullRequestSnapshot = link.snapshot.as_ref()?;
    let updated_at = snapshot.updated_at.clone()?;
    let provider = SOURCE_CONTROL_CLIENTS
        .find_by_change_request_url(link.url.as_str())
        .unwrap_or_else(|| SOURCE_CONTROL_CLIENTS.get(None))
        .kind
        .clone();
    Some(PullRequestSummary {
        provider,
        project_id: project_id.clone(),
        repository: link.repository.clone(),
        number: link.number,
        title: snapshot.title.clone(),
        url: link.url.clone(),
        state: snapshot.state,
        is_draft: Some(snapshot.is_draft),
        head_branch: snapshot.head_branch.clone(),
        base_branch: snapshot.base_branch.clone(),
        closed_at: snapshot.closed_at.clone(),
        merged_at: snapshot.merged_at.clone(),
        updated_at,
        observed_at: None,
        author: snapshot.author.clone(),
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        changed_files: snapshot.changed_files,
        review_decision: snapshot.review_decision,
        checks_state: snapshot.checks_state,
        mergeability: snapshot.mergeability,
        stack: None,
    })
}

/// `savedPullRequestKey`: environment and host-qualified link. The host keeps a link to another
/// forge from matching the project's pull request.
pub fn saved_pull_request_key(
    environment_id: &EnvironmentId,
    reference: &PullRequestRef,
) -> String {
    match &reference.host {
        None => String::new(),
        Some(host) => format!(
            "{}:{}",
            environment_id.as_str(),
            thread_pull_request_key_of(&ThreadPullRequestKeySource {
                host: host.as_str(),
                repository: reference.repository.as_str(),
                number: reference.number.get(),
                authority: None,
                url: None,
            })
        ),
    }
}

/// One thread's links, as the saved index reads them.
pub struct SavedThreadLinks<'a> {
    pub environment_id: &'a EnvironmentId,
    pub project_id: &'a ProjectId,
    pub pull_requests: &'a [ThreadPullRequestLink],
}

/// `savedPullRequestSummariesAtom`: saved thread snapshots by environment and host-qualified link,
/// the newest one winning per key.
pub fn saved_pull_request_summaries<'a>(
    threads: impl IntoIterator<Item = SavedThreadLinks<'a>>,
) -> HashMap<String, PullRequestSummary> {
    let mut summaries: HashMap<String, PullRequestSummary> = HashMap::new();
    for thread in threads {
        for link in visible_thread_pull_requests(thread.pull_requests) {
            let Some(summary) = thread_pull_request_link_to_summary(link, thread.project_id) else {
                continue;
            };
            let key = format!(
                "{}:{}",
                thread.environment_id.as_str(),
                thread_pull_request_key_of(&link.into())
            );
            let newest =
                newest_pull_request_summary(summaries.get(&key), Some(&summary)).unwrap_or(summary);
            summaries.insert(key, newest);
        }
    }
    summaries
}

/// `useSharedPullRequestSummary`: the newest of what this surface holds, what any surface
/// observed, and what a linked thread saved.
pub fn shared_pull_request_summary(
    observations: &PullRequestSummaryObservations,
    saved: &HashMap<String, PullRequestSummary>,
    environment_id: Option<&EnvironmentId>,
    reference: Option<&PullRequestRef>,
    current: Option<&PullRequestSummary>,
    observed_at: Option<f64>,
) -> Option<PullRequestSummary> {
    let (observed, saved) = match (environment_id, reference) {
        (Some(environment_id), Some(reference)) => (
            observations.get(environment_id, reference),
            saved.get(&saved_pull_request_key(environment_id, reference)),
        ),
        _ => (None, None),
    };
    let incoming = match (current, observed_at) {
        (Some(summary), Some(observed_at)) => Some(ObservedPullRequestSummary {
            summary: summary.clone(),
            observed_at,
        }),
        _ => None,
    };
    let summary = newest_pull_request_observation(observed, incoming)
        .map(|observation| observation.summary)
        .or_else(|| current.cloned());
    newest_pull_request_summary(summary.as_ref(), saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::pull_request::{
        PullRequestChecksState, PullRequestMergeability, PullRequestReviewDecision,
        PullRequestState,
    };

    fn summary(overrides: serde_json::Value) -> PullRequestSummary {
        let mut base = json!({
            "provider": "github",
            "projectId": "pull-request-cache-test",
            "repository": "acme/widget",
            "number": 7,
            "title": "Improve widget",
            "url": "https://github.com/acme/widget/pull/7",
            "state": "open",
            "isDraft": false,
            "headBranch": "improve-widget",
            "baseBranch": "main",
            "updatedAt": "2026-09-10T00:00:00Z",
            "author": { "login": "oliver", "name": null, "avatarUrl": null },
            "mergeability": "mergeable",
            "checksState": "passing",
        });
        for (key, value) in overrides.as_object().cloned().unwrap_or_default() {
            base[key] = value;
        }
        serde_json::from_value(base).expect("summary")
    }

    fn observed(value: PullRequestSummary, observed_at: f64) -> ObservedPullRequestSummary {
        ObservedPullRequestSummary {
            summary: value,
            observed_at,
        }
    }

    #[test]
    fn orders_same_dated_snapshots_by_the_servers_read_time_not_by_arrival() {
        let stale = observed(summary(json!({ "observedAt": 200 })), 500.);
        let held = observed(
            summary(
                json!({ "mergeability": "conflicting", "checksState": "failing", "observedAt": 300 }),
            ),
            100.,
        );
        let newest = newest_pull_request_observation(Some(&stale), Some(held.clone())).unwrap();
        assert_eq!(
            newest.summary.mergeability,
            Some(PullRequestMergeability::Conflicting)
        );
        assert_eq!(
            newest.summary.checks_state,
            Some(Some(PullRequestChecksState::Failing))
        );
        assert_eq!(newest.summary.observed_at, Some(300.));
        // An older filtered or server-cached response finishing last must not roll status back.
        assert_eq!(
            newest_pull_request_observation(Some(&held), Some(stale)),
            Some(held.clone())
        );
        // The same read seen again is not a new observation.
        let again = ObservedPullRequestSummary {
            observed_at: 900.,
            ..held.clone()
        };
        assert_eq!(
            newest_pull_request_observation(Some(&held), Some(again)),
            Some(held)
        );
    }

    #[test]
    fn uses_arrival_order_only_when_neither_snapshot_carries_a_server_read_time() {
        let first = observed(summary(json!({})), 100.);
        let later = observed(summary(json!({ "checksState": "failing" })), 200.);
        assert_eq!(
            newest_pull_request_observation(Some(&first), Some(later.clone()))
                .unwrap()
                .observed_at,
            200.
        );
        assert_eq!(
            newest_pull_request_observation(Some(&later), Some(first)),
            Some(later.clone())
        );
        // A stamped read beats an unstamped one regardless of which arrived last.
        let stamped = observed(summary(json!({ "observedAt": 50 })), 1.);
        assert_eq!(
            newest_pull_request_observation(Some(&later), Some(stamped.clone()))
                .unwrap()
                .summary
                .observed_at,
            Some(50.)
        );
        assert_eq!(
            newest_pull_request_observation(Some(&stamped), Some(later)),
            Some(stamped)
        );
    }

    #[test]
    fn keeps_known_status_when_a_newer_snapshot_omits_it_and_clears_it_when_told_to() {
        let list = observed(
            summary(json!({ "reviewDecision": "approved", "observedAt": 100 })),
            100.,
        );
        let mut sparse = summary(json!({ "observedAt": 200 }));
        sparse.checks_state = None;
        sparse.review_decision = None;
        let merged = newest_pull_request_observation(Some(&list), Some(observed(sparse, 200.)))
            .unwrap()
            .summary;
        assert_eq!(
            merged.checks_state,
            Some(Some(PullRequestChecksState::Passing))
        );
        assert_eq!(
            merged.review_decision,
            Some(Some(PullRequestReviewDecision::Approved))
        );
        let cleared = observed(
            summary(json!({ "checksState": null, "observedAt": 300 })),
            300.,
        );
        assert_eq!(
            newest_pull_request_observation(Some(&list), Some(cleared))
                .unwrap()
                .summary
                .checks_state,
            Some(None)
        );
    }

    #[test]
    fn treats_merged_as_final_and_otherwise_prefers_the_later_host_update() {
        let merged = summary(json!({ "state": "merged", "updatedAt": "2026-09-01T00:00:00Z" }));
        let reopened = summary(json!({ "updatedAt": "2026-09-12T00:00:00Z", "observedAt": 900 }));
        assert_eq!(
            newest_pull_request_summary(Some(&merged), Some(&reopened)),
            Some(merged.clone())
        );
        assert_eq!(
            newest_pull_request_summary(Some(&reopened), Some(&merged)),
            Some(merged)
        );
        let older = summary(json!({ "updatedAt": "2026-09-11T00:00:00Z", "observedAt": 999 }));
        assert_eq!(
            newest_pull_request_summary(Some(&older), Some(&reopened)),
            Some(reopened.clone())
        );
        assert_eq!(
            newest_pull_request_summary(Some(&reopened), Some(&older)),
            Some(reopened)
        );
    }

    fn link(snapshot: serde_json::Value) -> ThreadPullRequestLink {
        let stale = summary(json!({}));
        serde_json::from_value(json!({
            "host": "github.com",
            "repository": stale.repository,
            "number": stale.number,
            "url": stale.url,
            "source": "manual",
            "linkedAt": stale.updated_at,
            "stack": null,
            "snapshot": snapshot,
        }))
        .expect("link")
    }

    fn snapshot(updated_at: serde_json::Value) -> serde_json::Value {
        json!({
            "state": "closed",
            "title": "Improve widget",
            "headBranch": "improve-widget",
            "baseBranch": "main",
            "isDraft": false,
            "updatedAt": updated_at,
            "syncedAt": "2026-09-12T00:00:00Z",
        })
    }

    #[test]
    fn reuses_saved_thread_state_while_detail_is_stale_and_lets_newer_detail_replace_it() {
        let stale = summary(json!({}));
        let saved = thread_pull_request_link_to_summary(
            &link(snapshot(json!("2026-09-11T00:00:00Z"))),
            &stale.project_id,
        );
        assert_eq!(
            newest_pull_request_summary(Some(&stale), saved.as_ref())
                .unwrap()
                .state,
            PullRequestState::Closed
        );
        let fresh = summary(json!({ "isDraft": true, "updatedAt": "2026-09-13T00:00:00Z" }));
        assert_eq!(
            newest_pull_request_summary(Some(&fresh), saved.as_ref()),
            Some(fresh)
        );
        assert_eq!(
            thread_pull_request_link_to_summary(&link(json!(null)), &stale.project_id),
            None
        );
        assert_eq!(
            thread_pull_request_link_to_summary(&link(snapshot(json!(null))), &stale.project_id),
            None
        );
    }

    #[test]
    fn observations_hold_the_newest_per_pull_request_and_say_when_nothing_changed() {
        let environment = EnvironmentId::new("local").unwrap();
        let reference: PullRequestRef = serde_json::from_value(json!({
            "projectId": "pull-request-cache-test",
            "host": "github.com",
            "repository": "Acme/Widget",
            "number": 7,
        }))
        .unwrap();
        let mut observations = PullRequestSummaryObservations::default();
        assert!(observations.observe(
            &environment,
            &reference,
            summary(json!({ "observedAt": 10 })),
            1.
        ));
        // Same read again, and an older one: nothing to hold.
        assert!(!observations.observe(
            &environment,
            &reference,
            summary(json!({ "observedAt": 10 })),
            2.
        ));
        assert!(!observations.observe(
            &environment,
            &reference,
            summary(json!({ "observedAt": 5 })),
            3.
        ));
        // The repository's case is not part of the identity.
        let lowered: PullRequestRef = serde_json::from_value(json!({
            "projectId": "pull-request-cache-test",
            "repository": "acme/widget",
            "number": 7,
        }))
        .unwrap();
        assert_eq!(
            observations
                .get(&environment, &lowered)
                .unwrap()
                .summary
                .observed_at,
            Some(10.)
        );
    }

    #[test]
    fn the_shared_summary_prefers_a_newer_saved_snapshot_over_a_stale_current_one() {
        let environment = EnvironmentId::new("local").unwrap();
        let project = ProjectId::new("pull-request-cache-test").unwrap();
        let links = [link(snapshot(json!("2026-09-11T00:00:00Z")))];
        let saved = saved_pull_request_summaries([SavedThreadLinks {
            environment_id: &environment,
            project_id: &project,
            pull_requests: &links,
        }]);
        let reference: PullRequestRef = serde_json::from_value(json!({
            "projectId": "pull-request-cache-test",
            "host": "github.com",
            "repository": "acme/widget",
            "number": 7,
        }))
        .unwrap();
        let current = summary(json!({}));
        let shared = shared_pull_request_summary(
            &PullRequestSummaryObservations::default(),
            &saved,
            Some(&environment),
            Some(&reference),
            Some(&current),
            Some(1.),
        )
        .unwrap();
        assert_eq!(shared.state, PullRequestState::Closed);
        // Without a reference nothing is saved to read: the surface's own summary stands.
        let alone = shared_pull_request_summary(
            &PullRequestSummaryObservations::default(),
            &saved,
            None,
            None,
            Some(&current),
            None,
        );
        assert_eq!(alone, Some(current));
    }
}
