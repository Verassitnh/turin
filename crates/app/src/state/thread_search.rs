//! Port of upstream/packages/client-runtime/src/state/threadSearch.ts
//!
//! Search is a method on the in-process orchestrator, so the per-environment
//! query atoms collapse to folding the results the caller already has.

use crate::state::contracts::{
    EnvironmentId, OrchestrationSearchThreadsResult, OrchestrationThreadSearchMatch, ThreadId,
};

/// `OrchestrationThreadSearchMatch` plus its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentThreadSearchMatch {
    pub environment_id: EnvironmentId,
    pub r#match: OrchestrationThreadSearchMatch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadSearchResultsState {
    pub matches: Vec<EnvironmentThreadSearchMatch>,
    pub is_loading: bool,
}

/// The longest query the server accepts (`OrchestrationSearchThreadsInput.query`).
pub const THREAD_SEARCH_QUERY_MAX_LENGTH: usize = 200;

/// A stable key for one search across environments (order-independent).
pub fn make_thread_search_key(environment_ids: &[EnvironmentId], query: &str) -> String {
    let mut sorted: Vec<&str> = environment_ids.iter().map(EnvironmentId::as_str).collect();
    // `localeCompare`; environment ids are ASCII-ish slugs.
    sorted.sort_by(|left, right| crate::state::support::collate::locale_compare(left, right));
    serde_json::to_string(&(sorted, query)).expect("strings serialize")
}

/// Decodes a key from [`make_thread_search_key`]. The query is trimmed and must
/// fit the server's bound; anything else is an invalid key.
pub fn parse_thread_search_key(key: &str) -> Option<(Vec<EnvironmentId>, String)> {
    let (environment_ids, query): (Vec<String>, String) = serde_json::from_str(key).ok()?;
    let query = crate::state::contracts::js_trim(&query).to_owned();
    if query.is_empty() || crate::state::contracts::js_len(&query) > THREAD_SEARCH_QUERY_MAX_LENGTH
    {
        return None;
    }
    let environment_ids = environment_ids
        .into_iter()
        .map(EnvironmentId::new)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some((environment_ids, query))
}

pub fn thread_search_match_key(environment_id: &EnvironmentId, thread_id: &ThreadId) -> String {
    serde_json::to_string(&(environment_id.as_str(), thread_id.as_str()))
        .expect("strings serialize")
}

/// Combines one search result per environment. Invalid search keys, failed
/// requests, and disconnected environments contribute no content matches,
/// preserving local title search as the compatibility fallback.
pub fn thread_search_results_state(
    key: &str,
    mut result_of: impl FnMut(&EnvironmentId, &str) -> ThreadSearchEnvironmentResult,
) -> ThreadSearchResultsState {
    let Some((environment_ids, query)) = parse_thread_search_key(key) else {
        return ThreadSearchResultsState::default();
    };
    let mut state = ThreadSearchResultsState::default();
    for environment_id in &environment_ids {
        let result = result_of(environment_id, &query);
        state.is_loading |= result.waiting;
        if let Some(value) = result.value {
            state
                .matches
                .extend(
                    value
                        .matches
                        .into_iter()
                        .map(|r#match| EnvironmentThreadSearchMatch {
                            environment_id: environment_id.clone(),
                            r#match,
                        }),
                );
        }
    }
    state
}

/// `AsyncResult<OrchestrationSearchThreadsResult, E>` flattened; a failure is
/// `value: None`.
pub struct ThreadSearchEnvironmentResult {
    pub value: Option<OrchestrationSearchThreadsResult>,
    pub waiting: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::from_json;
    use serde_json::json;

    fn env(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).unwrap()
    }

    #[test]
    fn creates_stable_keys_regardless_of_environment_order() {
        assert_eq!(
            make_thread_search_key(&[env("env-b"), env("env-a")], "needle"),
            make_thread_search_key(&[env("env-a"), env("env-b")], "needle")
        );
        assert_eq!(
            make_thread_search_key(&[env("env-b"), env("env-a")], "needle"),
            r#"[["env-a","env-b"],"needle"]"#
        );
    }

    #[test]
    fn encodes_scoped_thread_keys_without_delimiter_collisions() {
        let first = thread_search_match_key(&env("env\u{0}thread"), &ThreadId::new("id").unwrap());
        let second = thread_search_match_key(&env("env"), &ThreadId::new("thread\u{0}id").unwrap());
        assert_ne!(first, second);
    }

    #[test]
    fn accepts_search_keys_at_the_maximum_decoded_query_length() {
        let query = "a".repeat(200);
        let mut queries = Vec::new();
        for key_query in [query.clone(), format!(" {query} ")] {
            thread_search_results_state(
                &make_thread_search_key(&[env("env-a")], &key_query),
                |_, q| {
                    queries.push(q.to_owned());
                    ThreadSearchEnvironmentResult {
                        value: Some(OrchestrationSearchThreadsResult { matches: vec![] }),
                        waiting: false,
                    }
                },
            );
        }
        assert_eq!(queries, vec![query.clone(), query]);
    }

    #[test]
    fn ignores_invalid_search_keys() {
        let mut search_count = 0;
        let mut search = |_: &EnvironmentId, _: &str| {
            search_count += 1;
            ThreadSearchEnvironmentResult {
                value: None,
                waiting: false,
            }
        };
        let too_long = make_thread_search_key(&[env("env-a")], &"a".repeat(201));
        assert_eq!(
            thread_search_results_state(&too_long, &mut search),
            ThreadSearchResultsState::default()
        );
        assert_eq!(
            thread_search_results_state("not json{{", &mut search),
            ThreadSearchResultsState::default()
        );
        assert_eq!(search_count, 0);
    }

    #[test]
    fn merges_successful_environments_and_silently_ignores_failures() {
        let result: OrchestrationSearchThreadsResult = from_json(json!({
            "matches": [{
                "threadId": "thread-a", "projectId": "project-a", "source": "user",
                "snippet": "needle", "messageCreatedAt": "2026-07-30T00:00:00.000Z",
            }],
        }));
        let state = thread_search_results_state(
            &make_thread_search_key(&[env("env-b"), env("env-a")], "needle"),
            |environment_id, _| ThreadSearchEnvironmentResult {
                value: (environment_id.as_str() == "env-a").then(|| result.clone()),
                waiting: false,
            },
        );
        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.matches[0].environment_id, env("env-a"));
        assert_eq!(state.matches[0].r#match, result.matches[0]);
        assert!(!state.is_loading);
        assert_eq!(
            thread_search_match_key(
                &state.matches[0].environment_id,
                &state.matches[0].r#match.thread_id
            ),
            r#"["env-a","thread-a"]"#
        );
    }
}
