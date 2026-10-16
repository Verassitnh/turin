//! Port of upstream/apps/web/src/routes/_chat.pull-requests.tsx `validateSearch`,
//! with upstream/apps/web/src/components/pullRequest/pullRequestsRouteSearch.test.ts.
//! Router/React construction is omitted; this module accepts the router's decoded search map.

use super::pull_request_list_preferences::PullRequestListSort;
use crate::state::contracts::js_trim;
use serde_json::{Map, Value};
use turin_contracts::pull_request::{PullRequestInvolvement, PullRequestListState};

#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestsSearch {
    pub involvement: PullRequestInvolvement,
    pub state: PullRequestListState,
    pub sort: Option<PullRequestListSort>,
    pub repository: Option<String>,
    pub number: Option<u64>,
    pub project_id: Option<String>,
    pub environment_id: Option<String>,
    pub host: Option<String>,
    pub selected_project_id: Option<String>,
    pub selected_host: Option<String>,
    pub selected_environment_id: Option<String>,
    /// Always present as an own key in the router result, even when absent as a value.
    pub q: Option<String>,
    pub draft: Option<String>,
    pub review: Option<String>,
    pub checks: Option<String>,
    pub author: Option<String>,
    pub labels: Option<Vec<String>>,
}

fn js_slice_200(value: &str) -> String {
    String::from_utf16_lossy(&value.encode_utf16().take(200).collect::<Vec<_>>())
}
fn nonempty(raw: &Map<String, Value>, key: &str, bounded: bool) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if bounded {
                js_slice_200(s)
            } else {
                s.to_owned()
            }
        })
}
fn one_of(raw: &Map<String, Value>, key: &str, options: &[&str]) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .filter(|s| options.contains(s))
        .map(str::to_owned)
}

pub fn pull_request_search_labels(raw: Option<&Value>) -> Option<Vec<String>> {
    let candidates: Vec<&Value> = match raw {
        Some(Value::Array(values)) => values.iter().take(100).collect(),
        Some(value @ Value::String(_)) => vec![value],
        _ => vec![],
    };
    let mut labels = vec![];
    let mut seen = std::collections::HashSet::new();
    for raw_value in candidates {
        let Some(value) = raw_value.as_str() else {
            continue;
        };
        let value = js_slice_200(js_trim(value));
        let key = value.to_lowercase();
        if value.is_empty() || !seen.insert(key) {
            continue;
        }
        labels.push(value);
        if labels.len() == 10 {
            break;
        }
    }
    (!labels.is_empty()).then_some(labels)
}

pub fn validate_pull_requests_search(raw: &Map<String, Value>) -> PullRequestsSearch {
    let involvement = match raw.get("involvement").and_then(Value::as_str) {
        Some("reviewing") => PullRequestInvolvement::Reviewing,
        Some("authored") => PullRequestInvolvement::Authored,
        _ => PullRequestInvolvement::All,
    };
    let state = match raw.get("state").and_then(Value::as_str) {
        Some("closed") => PullRequestListState::Closed,
        Some("merged") => PullRequestListState::Merged,
        Some("all") => PullRequestListState::All,
        _ => PullRequestListState::Open,
    };
    let sort = one_of(
        raw,
        "sort",
        &[
            "ready", "blocked", "updated", "newest", "oldest", "largest", "smallest",
        ],
    )
    .map(|s| match s.as_str() {
        "ready" => PullRequestListSort::Ready,
        "blocked" => PullRequestListSort::Blocked,
        "updated" => PullRequestListSort::Updated,
        "newest" => PullRequestListSort::Newest,
        "oldest" => PullRequestListSort::Oldest,
        "largest" => PullRequestListSort::Largest,
        _ => PullRequestListSort::Smallest,
    });
    PullRequestsSearch {
        involvement,
        state,
        sort,
        repository: nonempty(raw, "repository", true),
        number: raw.get("number").and_then(Value::as_u64).filter(|n| *n > 0),
        project_id: nonempty(raw, "projectId", false),
        environment_id: nonempty(raw, "environmentId", false),
        host: nonempty(raw, "host", true),
        selected_project_id: nonempty(raw, "selectedProjectId", false),
        selected_host: nonempty(raw, "selectedHost", true),
        selected_environment_id: nonempty(raw, "selectedEnvironmentId", false),
        // Always an own key: router overlays this on the parent's unvalidated search.
        q: nonempty(raw, "q", true),
        draft: one_of(raw, "draft", &["only", "hide"]),
        review: one_of(
            raw,
            "review",
            &["approved", "changes-requested", "review-required", "none"],
        ),
        checks: one_of(raw, "checks", &["passing", "failing"]),
        author: raw
            .get("author")
            .and_then(Value::as_str)
            .map(js_trim)
            .filter(|s| !s.is_empty())
            .map(js_slice_200),
        labels: pull_request_search_labels(raw.get("labels")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn matched_search(url: &str) -> PullRequestsSearch {
        let mut raw = Map::new();
        for (key, value) in
            url::form_urlencoded::parse(url.split_once('?').map_or("", |(_, q)| q).as_bytes())
        {
            let decoded =
                serde_json::from_str::<Value>(&value).unwrap_or(Value::String(value.into_owned()));
            raw.insert(key.into_owned(), decoded);
        }
        validate_pull_requests_search(&raw)
    }
    #[test]
    fn drops_q_parsed_as_number_or_boolean() {
        assert_eq!(matched_search("/pull-requests?q=4089").q, None);
        assert_eq!(matched_search("/pull-requests?q=true").q, None);
    }
    #[test]
    fn keeps_text_q_including_quoted_digits() {
        assert_eq!(
            matched_search("/pull-requests?q=fix").q.as_deref(),
            Some("fix")
        );
        assert_eq!(
            matched_search("/pull-requests?q=%224089%22").q.as_deref(),
            Some("4089")
        );
    }
}
