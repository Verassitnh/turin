//! Port of upstream/apps/web/src/components/pullRequest/pullRequestListPreferences.ts
//! React hooks and browser `localStorage` plumbing are omitted; callers persist the JSON string.

use crate::state::contracts::{EnvironmentId, ProjectId};
use serde::{Deserialize, Serialize};
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestDraftFilter, PullRequestInvolvement, PullRequestListState,
    PullRequestReviewFilter,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestListSort {
    Ready,
    Blocked,
    Updated,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListPreferences {
    pub involvement: PullRequestInvolvement,
    pub state: PullRequestListState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<EnvironmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<PullRequestDraftFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReviewFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecksFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<PullRequestListSort>,
}

impl Default for PullRequestListPreferences {
    fn default() -> Self {
        Self {
            involvement: PullRequestInvolvement::All,
            state: PullRequestListState::Open,
            environment_id: None,
            project_id: None,
            host: None,
            q: None,
            draft: None,
            review: None,
            checks: None,
            author: None,
            labels: None,
            sort: None,
        }
    }
}

pub const PULL_REQUEST_LIST_PREFERENCES_STORAGE_KEY: &str = "t3.pullRequests.preferences";

fn bounded(value: &str) -> bool {
    value.encode_utf16().count() <= 200
}
fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

/// Only list controls are remembered. The selected row is a URL and right-panel concern.
pub fn pull_request_list_preferences(
    search: &PullRequestListPreferences,
) -> PullRequestListPreferences {
    PullRequestListPreferences {
        involvement: search.involvement,
        state: search.state,
        environment_id: search.environment_id.clone(),
        project_id: search.project_id.clone(),
        host: nonempty(search.host.clone()),
        q: nonempty(search.q.clone()),
        draft: search.draft,
        review: search.review,
        checks: search.checks,
        author: nonempty(search.author.clone()),
        labels: search
            .labels
            .as_ref()
            .filter(|labels| !labels.is_empty())
            .cloned(),
        sort: search
            .sort
            .filter(|sort| *sort != PullRequestListSort::Ready),
    }
}

/// Parse stored JSON, returning the upstream defaults for malformed or out-of-bounds data.
pub fn read_pull_request_list_preferences(raw: Option<&str>) -> PullRequestListPreferences {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return PullRequestListPreferences::default();
    };
    let Ok(decoded) = serde_json::from_str::<PullRequestListPreferences>(raw) else {
        return PullRequestListPreferences::default();
    };
    if decoded.host.as_deref().is_some_and(|s| !bounded(s))
        || decoded.q.as_deref().is_some_and(|s| !bounded(s))
        || decoded.author.as_deref().is_some_and(|s| !bounded(s))
        || decoded
            .labels
            .as_ref()
            .is_some_and(|labels| labels.len() > 10 || labels.iter().any(|s| !bounded(s)))
    {
        return PullRequestListPreferences::default();
    }
    pull_request_list_preferences(&decoded)
}

/// Serialize the caller's preferences as written; upstream normalizes on read, not write.
pub fn write_pull_request_list_preferences(preferences: &PullRequestListPreferences) -> String {
    serde_json::to_string(preferences).expect("preferences serialize")
}
