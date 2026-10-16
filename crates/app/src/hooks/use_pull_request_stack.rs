//! Port of upstream/apps/web/src/state/usePullRequestStack.ts.

#![allow(dead_code)]

use turin_contracts::pull_request::PullRequestRef;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

use crate::pull_request::pull_request_stack_snapshot::{
    PullRequestStackQuery, PullRequestStackView, pull_request_stack_view, saved_pull_request_stack,
};

#[derive(Clone, Debug)]
pub struct UsePullRequestStackResult {
    pub view: PullRequestStackView,
}

/// Combines saved thread links with a live stack query (`usePullRequestStack`).
pub fn use_pull_request_stack(
    links: &[ThreadPullRequestLink],
    reference: Option<&PullRequestRef>,
    query: PullRequestStackQuery<'_>,
) -> UsePullRequestStackResult {
    let saved = reference.and_then(|reference| saved_pull_request_stack(links, reference));
    UsePullRequestStackResult {
        view: pull_request_stack_view(query, saved.as_ref()),
    }
}
