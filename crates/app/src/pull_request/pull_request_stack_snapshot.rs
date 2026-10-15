//! Port of upstream/apps/web/src/components/pullRequest/pullRequestStackSnapshot.ts.
//!
//! This upstream module reads saved thread links; it does not encode or decode storage. The
//! detail snapshot module owns persisted storage. This file only projects saved stack navigation.

use crate::state::time::parse_ms;
use turin_contracts::pull_request::{PullRequestRef, PullRequestStack, PullRequestStackLayer};
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

/// Saved native membership is enough for navigation, but never supplies action head SHAs.
pub fn saved_pull_request_stack(
    links: &[ThreadPullRequestLink],
    reference: &PullRequestRef,
) -> Option<PullRequestStack> {
    let host = reference.host.as_ref()?.as_str().to_lowercase();
    let repository = reference.repository.as_str().to_lowercase();
    let matching: Vec<&ThreadPullRequestLink> = links
        .iter()
        .filter(|link| {
            link.host.as_str().to_lowercase() == host
                && link.repository.as_str().to_lowercase() == repository
        })
        .collect();
    let exact: Vec<_> = matching
        .iter()
        .copied()
        .filter(|link| link.number == reference.number)
        .collect();
    let mut candidates: Vec<_> = if exact.is_empty() {
        matching
            .iter()
            .copied()
            .filter(|link| {
                link.stack.as_ref().is_some_and(|stack| {
                    stack
                        .layers
                        .iter()
                        .any(|layer| layer.number == reference.number)
                })
            })
            .collect()
    } else {
        exact
    };
    candidates.sort_by_key(|link| std::cmp::Reverse(saved_at(link)));
    let stack = candidates.first()?.stack.as_ref()?;
    if !stack
        .layers
        .iter()
        .any(|layer| layer.number == reference.number)
    {
        return None;
    }
    Some(PullRequestStack {
        id: stack.id.clone(),
        number: stack.number,
        url: stack.url.clone(),
        base: stack.base.clone(),
        layers: stack
            .layers
            .iter()
            .map(|layer| {
                let mut layers_links: Vec<_> = matching
                    .iter()
                    .copied()
                    .filter(|link| link.number == layer.number)
                    .collect();
                layers_links.sort_by_key(|link| std::cmp::Reverse(saved_at(link)));
                let snapshot = layers_links.first().and_then(|link| link.snapshot.as_ref());
                PullRequestStackLayer {
                    number: layer.number,
                    head_branch: layer.head_branch.clone(),
                    state: layer.state,
                    title: snapshot.map(|snapshot| snapshot.title.as_str().to_owned()),
                    is_draft: snapshot.map(|snapshot| snapshot.is_draft),
                    head_sha: None,
                }
            })
            .collect(),
    })
}

fn saved_at(link: &ThreadPullRequestLink) -> Option<i64> {
    parse_ms(
        link.snapshot
            .as_ref()
            .map_or(link.linked_at.as_str(), |snapshot| {
                snapshot.synced_at.as_str()
            }),
    )
}

/// A fresh absence overrides saved membership; failed refreshes preserve available navigation.
/// A background refresh keeps the last successful lookup fresh, so stack-dependent actions do
/// not blink out every time the detail revalidates.
#[derive(Debug, Clone)]
pub struct PullRequestStackQuery<'a> {
    pub data: Option<&'a PullRequestStack>,
    pub is_success: bool,
    pub is_pending: bool,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestStackView {
    pub data: Option<PullRequestStack>,
    pub is_fresh: bool,
    pub notice: Option<&'static str>,
}

pub fn pull_request_stack_view(
    query: PullRequestStackQuery<'_>,
    saved: Option<&PullRequestStack>,
) -> PullRequestStackView {
    let data = if query.is_success {
        query.data
    } else {
        query.data.or(saved)
    }
    .cloned();
    let notice = if data.is_none() {
        None
    } else if query.error.is_some() {
        Some("Stack data may be stale. We couldn’t refresh it.")
    } else if !query.is_success || query.is_pending {
        Some("Refreshing stack… Showing saved data.")
    } else {
        None
    };
    PullRequestStackView {
        data,
        is_fresh: query.is_success,
        notice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    fn link() -> ThreadPullRequestLink {
        serde_json::from_value(json!({
          "host":"github.com", "repository":"acme/web", "number":2,
          "url":"https://github.com/acme/web/pull/2", "source":"manual",
          "linkedAt":"2026-09-09T10:00:00Z",
          "snapshot":{"title":"Top layer","headBranch":"top","baseBranch":"bottom",
            "state":"open","isDraft":false,"updatedAt":null,"syncedAt":"2026-09-09T10:00:00Z"},
          "stack":{"kind":"native","id":"stack","number":3,
            "url":"https://github.com/acme/web/pull/3","base":"main","layers":[
            {"number":1,"headBranch":"bottom","state":"open"},
            {"number":2,"headBranch":"top","state":"open"}]}
        }))
        .unwrap()
    }
    fn reference() -> PullRequestRef {
        serde_json::from_value(json!({"projectId":"project","host":"github.com",
            "repository":"acme/web","number":2}))
        .unwrap()
    }
    #[test]
    fn preserves_all_native_layers_without_action_shas() {
        let saved = saved_pull_request_stack(&[link()], &reference()).unwrap();
        assert_eq!(
            serde_json::to_value(&saved.layers).unwrap(),
            json!([
                {"number":1,"headBranch":"bottom","state":"open"},
                {"number":2,"headBranch":"top","state":"open","title":"Top layer","isDraft":false}
            ])
        );
        let mut other = reference();
        other.number = turin_contracts::PositiveInt::new(1).unwrap();
        assert_eq!(
            saved_pull_request_stack(&[link()], &other)
                .unwrap()
                .number
                .get(),
            3
        );
    }
    #[test]
    fn does_not_borrow_stacks_across_hosts_or_repositories() {
        let mut other = reference();
        other.host =
            Some(turin_contracts::TrimmedNonEmptyString::new("enterprise.example").unwrap());
        assert!(saved_pull_request_stack(&[link()], &other).is_none());
        other = reference();
        other.repository = turin_contracts::TrimmedNonEmptyString::new("other/web").unwrap();
        assert!(saved_pull_request_stack(&[link()], &other).is_none());
        other = reference();
        other.host = None;
        assert!(saved_pull_request_stack(&[link()], &other).is_none());
    }
    #[test]
    fn honors_newer_saved_removal_across_linked_threads() {
        let mut removed = link();
        removed.stack = None;
        removed.snapshot.as_mut().unwrap().synced_at = "2026-09-09T11:00:00Z".into();
        assert!(saved_pull_request_stack(&[link(), removed], &reference()).is_none());
    }
    #[test]
    fn shows_saved_data_during_loading_and_marks_failed_refresh_stale() {
        let saved = saved_pull_request_stack(&[link()], &reference()).unwrap();
        let view = pull_request_stack_view(
            PullRequestStackQuery {
                data: None,
                is_success: false,
                is_pending: true,
                error: None,
            },
            Some(&saved),
        );
        assert_eq!(view.data, Some(saved.clone()));
        assert!(!view.is_fresh);
        assert!(view.notice.unwrap().contains("Refreshing"));
        let view = pull_request_stack_view(
            PullRequestStackQuery {
                data: None,
                is_success: false,
                is_pending: false,
                error: Some("Rate limited"),
            },
            Some(&saved),
        );
        assert!(view.notice.unwrap().contains("stale"));
    }
    #[test]
    fn prefers_refreshed_data_and_honors_successful_absence() {
        let saved = saved_pull_request_stack(&[link()], &reference()).unwrap();
        let query = PullRequestStackQuery {
            data: Some(&saved),
            is_success: true,
            is_pending: false,
            error: None,
        };
        assert_eq!(
            pull_request_stack_view(query, None).data,
            Some(saved.clone())
        );
        let absent = pull_request_stack_view(
            PullRequestStackQuery {
                data: None,
                is_success: true,
                is_pending: false,
                error: None,
            },
            Some(&saved),
        );
        assert_eq!(absent.data, None);
        assert!(absent.is_fresh);
        assert_eq!(absent.notice, None);
        let pending = pull_request_stack_view(
            PullRequestStackQuery {
                data: Some(&saved),
                is_success: true,
                is_pending: true,
                error: None,
            },
            Some(&saved),
        );
        assert!(pending.is_fresh);
        let absent_pending = pull_request_stack_view(
            PullRequestStackQuery {
                data: None,
                is_success: true,
                is_pending: true,
                error: None,
            },
            Some(&saved),
        );
        assert!(absent_pending.is_fresh);
        assert_eq!(absent_pending.data, None);
    }
}
