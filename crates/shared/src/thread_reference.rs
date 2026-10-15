//! Port of upstream/packages/shared/src/threadReference.ts

use turin_contracts::thread_pull_request::ThreadPullRequestLink;

use crate::thread_pull_requests::resolve_thread_current_pull_request_link;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadReferenceKind {
    PullRequest,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadReferenceCopyTarget {
    pub kind: ThreadReferenceKind,
    pub value: String,
    pub clipboard_target: &'static str,
    pub success_title: &'static str,
    pub failure_title: &'static str,
}

/// What the copy-reference action copies. `open_panel_pull_request_url` is `None` when no PR panel
/// is open, and `Some(None)` when it is open but its URL is not available yet.
pub fn resolve_thread_reference_copy_target(
    thread_id: &str,
    open_panel_pull_request_url: Option<Option<&str>>,
    pull_requests: &[ThreadPullRequestLink],
    linked_pull_request_url: Option<&str>,
) -> Option<ThreadReferenceCopyTarget> {
    // Upstream `??` chain: an open panel URL wins even when empty, and only then does it fall back.
    let pull_request_url = match open_panel_pull_request_url {
        Some(None) => return None,
        Some(Some(url)) => Some(url),
        None => resolve_thread_current_pull_request_link(pull_requests)
            .map(|link| link.url.as_str())
            .or(linked_pull_request_url),
    };
    match pull_request_url.filter(|url| !url.is_empty()) {
        Some(url) => Some(ThreadReferenceCopyTarget {
            kind: ThreadReferenceKind::PullRequest,
            value: url.to_owned(),
            clipboard_target: "pull request link",
            success_title: "PR link copied",
            failure_title: "Failed to copy PR link",
        }),
        None => Some(ThreadReferenceCopyTarget {
            kind: ThreadReferenceKind::Thread,
            value: thread_id.to_owned(),
            clipboard_target: "thread ID",
            success_title: "Thread ID copied",
            failure_title: "Failed to copy thread ID",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString as Tne};
    use turin_contracts::pull_request::PullRequestState;
    use turin_contracts::thread_pull_request::{
        ThreadNativeStackKind, ThreadPullRequestLinkSource, ThreadPullRequestStack,
        ThreadPullRequestStackLayer,
    };

    fn cross_repository(number: u64) -> ThreadPullRequestLink {
        ThreadPullRequestLink {
            host: Tne::new("github.com").unwrap(),
            repository: Tne::new("other/repo").unwrap(),
            number: PositiveInt::new(number).unwrap(),
            url: Tne::new(format!("https://github.com/other/repo/pull/{number}")).unwrap(),
            source: ThreadPullRequestLinkSource::Manual,
            linked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            snapshot: None,
            stack: None,
            watch: None,
        }
    }

    fn with_source(
        mut link: ThreadPullRequestLink,
        source: ThreadPullRequestLinkSource,
    ) -> ThreadPullRequestLink {
        link.source = source;
        link
    }

    fn kind_and_value(
        target: Option<ThreadReferenceCopyTarget>,
    ) -> Option<(ThreadReferenceKind, String)> {
        target.map(|target| (target.kind, target.value))
    }

    const FALLBACK: &str = "https://github.com/t3/pr/12";

    #[test]
    fn copies_nothing_while_the_open_panel_url_is_unavailable() {
        let target = resolve_thread_reference_copy_target(
            "thread-1",
            Some(None),
            &[cross_repository(42)],
            Some(FALLBACK),
        );
        assert_eq!(target, None);
    }

    #[test]
    fn prefers_the_open_panel_pull_request_over_the_thread_pull_request() {
        let target = resolve_thread_reference_copy_target(
            "thread-1",
            Some(Some("https://github.com/t3/pr/14")),
            &[cross_repository(42)],
            Some(FALLBACK),
        )
        .unwrap();
        assert_eq!(target.kind, ThreadReferenceKind::PullRequest);
        assert_eq!(target.value, "https://github.com/t3/pr/14");
        assert_eq!(target.success_title, "PR link copied");
    }

    #[test]
    fn copies_a_native_cross_repository_link_before_the_fallback_url() {
        for fallback in [None, Some(FALLBACK)] {
            assert_eq!(
                kind_and_value(resolve_thread_reference_copy_target(
                    "thread-1",
                    None,
                    &[cross_repository(42)],
                    fallback,
                )),
                Some((
                    ThreadReferenceKind::PullRequest,
                    "https://github.com/other/repo/pull/42".to_owned(),
                ))
            );
        }
    }

    #[test]
    fn copies_the_highest_open_stack_layer_instead_of_the_first_link() {
        let stack = ThreadPullRequestStack {
            kind: ThreadNativeStackKind,
            id: Tne::new("stack-1").unwrap(),
            number: PositiveInt::new(1).unwrap(),
            url: Tne::new("https://github.com/other/repo/stacks/1").unwrap(),
            base: Tne::new("main").unwrap(),
            layers: [42, 43]
                .into_iter()
                .map(|number| ThreadPullRequestStackLayer {
                    number: PositiveInt::new(number).unwrap(),
                    head_branch: Tne::new(format!("b{number}")).unwrap(),
                    state: PullRequestState::Open,
                })
                .collect(),
        };
        let with_stack = |mut link: ThreadPullRequestLink| {
            link.stack = Some(stack.clone());
            link
        };
        let top = with_stack(cross_repository(43));
        let target = resolve_thread_reference_copy_target(
            "thread-1",
            None,
            &[with_stack(cross_repository(42)), top.clone()],
            None,
        )
        .unwrap();
        assert_eq!(target.value, top.url.as_str());
    }

    #[test]
    fn uses_the_fallback_url_when_native_links_are_dismissed() {
        let dismissed = with_source(
            cross_repository(42),
            ThreadPullRequestLinkSource::StackDismissed,
        );
        assert_eq!(
            kind_and_value(resolve_thread_reference_copy_target(
                "thread-1",
                None,
                &[dismissed],
                Some(FALLBACK),
            )),
            Some((ThreadReferenceKind::PullRequest, FALLBACK.to_owned()))
        );
    }

    #[test]
    fn uses_the_thread_pull_request_when_no_panel_is_open() {
        let target =
            resolve_thread_reference_copy_target("thread-1", None, &[], Some(FALLBACK)).unwrap();
        assert_eq!(target.value, FALLBACK);
        assert_eq!(target.success_title, "PR link copied");
    }

    #[test]
    fn falls_back_to_the_thread_id() {
        assert_eq!(
            resolve_thread_reference_copy_target("thread-1", None, &[], None),
            Some(ThreadReferenceCopyTarget {
                kind: ThreadReferenceKind::Thread,
                value: "thread-1".to_owned(),
                clipboard_target: "thread ID",
                success_title: "Thread ID copied",
                failure_title: "Failed to copy thread ID",
            })
        );
    }
}
