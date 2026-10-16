//! Port of the pull-request helpers of upstream `Orchestrator.ts`
//! (`legacyPullRequestLink`, `withPullRequestWatch`) and the thread-level
//! pull-request arms of `dispatchThreadMutation`.

use turin_contracts::{
    PositiveInt, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::OrchestrationV2AppThread,
    thread_pull_request::{
        ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
    },
};

use crate::projection::shared::thread_pull_requests::{
    legacy_thread_pull_request_key, thread_pull_request_key_of, thread_pull_requests_of,
};

/// `legacyThreadPullRequestKey(linked)` as the `host/repository#number` string
/// that [`pull_request_key_string`] produces for a stored link.
pub(super) fn legacy_pull_request_key_string(linked: &ThreadLinkedPullRequest) -> String {
    let key = legacy_thread_pull_request_key(linked, None);
    format!("{}/{}#{}", key.host, key.repository, key.number)
}

/// The identity of a stored link, in the same form as [`legacy_pull_request_key_string`].
pub(super) fn pull_request_key_string(link: &ThreadPullRequestLink) -> String {
    thread_pull_request_key_of(link)
}

/// `legacyPullRequestLink`: a single-PR link as a link entry. Re-linking a pull
/// request keeps the watch it already had.
pub(super) fn legacy_pull_request_link(
    thread: &OrchestrationV2AppThread,
    linked: &ThreadLinkedPullRequest,
    now: UtcDateTime,
) -> ThreadPullRequestLink {
    let key_string = legacy_pull_request_key_string(linked);
    let watch = thread_pull_requests_of(thread)
        .into_iter()
        .find(|link| pull_request_key_string(link) == key_string)
        .and_then(|link| link.watch);
    let key = legacy_thread_pull_request_key(linked, None);
    ThreadPullRequestLink {
        host: non_blank(key.host),
        repository: non_blank(key.repository),
        number: PositiveInt::new(key.number).unwrap_or(linked.number),
        url: linked.url.clone(),
        source: ThreadPullRequestLinkSource::Manual,
        linked_at: now.to_string(),
        snapshot: None,
        stack: None,
        watch,
    }
}

fn non_blank(value: String) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(&value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("literal is non-empty"))
}
