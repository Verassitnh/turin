//! Port of upstream/apps/web/src/components/chat/ThreadDetailsPrRows.tsx
//!
//! The current pull request's row, and the thread's other linked pull requests behind a
//! "Show N more" toggle, newest first with stacks read bottom to top.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, IntoElement, ParentElement as _, RenderOnce, SharedString, Window,
    div,
};
use turin_contracts::pull_request::PullRequestDetail;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

use super::thread_details_control::{ThreadDetailsControl, ThreadDetailsPart, ThreadDetailsTone};
use super::thread_details_pr_row::{OpenHandler, ThreadDetailsPrRow};
use crate::components::sidebar::thread_status_indicators::{
    linked_pull_request_snapshot_status, pr_status_indicator,
};
use crate::pull_request::pull_request_list_lines::pull_request_list_lines;
use crate::state::support::thread_pull_requests::{
    resolve_thread_pull_request_chains, thread_pull_request_key_of,
};
use crate::theme::ActiveAppTheme;
use crate::ui::{Icon, IconName};

type OpenLink = Rc<dyn Fn(&ClickEvent, &str, &mut Window, &mut App)>;
type StopWatching = Rc<dyn Fn(&ThreadPullRequestLink, &mut Window, &mut App)>;
type LinkDetail = Rc<dyn Fn(&ThreadPullRequestLink) -> Option<PullRequestDetail>>;

/// The other linked pull requests, in the order the thread's pull request list shows them.
pub fn rest_pull_requests(
    links: &[ThreadPullRequestLink],
    current: &ThreadPullRequestLink,
) -> Vec<ThreadPullRequestLink> {
    let chains = resolve_thread_pull_request_chains(links);
    let current_key = thread_pull_request_key_of(&current.into());
    pull_request_list_lines(&chains)
        .into_iter()
        .map(|line| line.link)
        .filter(|link| thread_pull_request_key_of(&(*link).into()) != current_key)
        .cloned()
        .collect()
}

/// `Show N more` / `Show less`.
pub fn pr_rows_toggle_label(expanded: bool, rest: usize) -> String {
    if expanded {
        "Show less".to_owned()
    } else {
        format!("Show {rest} more")
    }
}

/// `ThreadDetailsPrLinkRow`: a linked pull request read from its persisted snapshot.
pub fn thread_details_pr_link_row(
    id: impl Into<SharedString>,
    link: &ThreadPullRequestLink,
    on_open: OpenHandler,
    detail: Option<PullRequestDetail>,
) -> ThreadDetailsPrRow {
    let linked = linked_pull_request_snapshot_status(link);
    let pr_state = linked.as_ref().map(|linked| linked.pr.state);
    let status = linked.as_ref().and_then(|linked| {
        pr_status_indicator(Some(&linked.pr), Some(&linked.source_control_provider))
    });
    let label = match &link.snapshot {
        Some(snapshot) => format!("#{}: {}", link.number, snapshot.title),
        None => format!("#{}", link.number),
    };
    ThreadDetailsPrRow::new(id, link.number.get(), label, on_open)
        .pr_state(pr_state)
        .status(status)
        .detail(detail)
}

#[derive(IntoElement)]
pub struct ThreadDetailsPrRows {
    pub id: SharedString,
    pub links: Vec<ThreadPullRequestLink>,
    pub current_link: Option<ThreadPullRequestLink>,
    /// The row for the current pull request, as the owner resolved it.
    pub current_row: ThreadDetailsPrRow,
    pub on_open_link: OpenLink,
    /// Only watched links get the eye; the row hides it once the server records the stop.
    pub stop_watching: Option<StopWatching>,
    pub link_detail: Option<LinkDetail>,
}

impl RenderOnce for ThreadDetailsPrRows {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let expanded_state = window.use_keyed_state(
            SharedString::from(format!("pr-rows-expanded-{}", self.id)),
            cx,
            |_, _| false,
        );
        let expanded = *expanded_state.read(cx);
        let stop_for = |link: Option<&ThreadPullRequestLink>| {
            let link = link.filter(|link| link.watch.is_some())?.clone();
            let stop = self.stop_watching.clone()?;
            Some(
                Rc::new(move |window: &mut Window, cx: &mut App| stop(&link, window, cx))
                    as Rc<dyn Fn(&mut Window, &mut App)>,
            )
        };
        let current_row = self
            .current_row
            .on_stop_watching(stop_for(self.current_link.as_ref()));
        let rest = match &self.current_link {
            Some(current) => rest_pull_requests(&self.links, current),
            None => Vec::new(),
        };
        if rest.is_empty() {
            return current_row.into_any_element();
        }
        let rest_len = rest.len();
        let rows: Vec<AnyElement> = if expanded {
            rest.iter()
                .map(|link| {
                    let on_open_link = self.on_open_link.clone();
                    let url = link.url.as_str().to_owned();
                    let detail = self.link_detail.as_ref().and_then(|detail| detail(link));
                    thread_details_pr_link_row(
                        SharedString::from(format!(
                            "{}-{}",
                            self.id,
                            thread_pull_request_key_of(&link.into())
                        )),
                        link,
                        Rc::new(move |event, window, cx| on_open_link(event, &url, window, cx)),
                        detail,
                    )
                    .on_stop_watching(stop_for(Some(link)))
                    .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        div()
            .child(current_row)
            .children(rows)
            .child(
                ThreadDetailsControl::new(SharedString::from(format!("{}-toggle", self.id)))
                    .part(ThreadDetailsPart::Row)
                    .tone(ThreadDetailsTone::Muted)
                    .child(
                        Icon::new(if expanded {
                            IconName::Minus
                        } else {
                            IconName::Plus
                        })
                        .size(gpui_kit::rems(1.))
                        .color(theme.muted_foreground),
                    )
                    .child(pr_rows_toggle_label(expanded, rest_len))
                    .on_click(move |_, _, cx| {
                        expanded_state.update(cx, |expanded, cx| {
                            *expanded = !*expanded;
                            cx.notify();
                        });
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(number: u64, head: &str, base: &str, updated_at: &str) -> ThreadPullRequestLink {
        serde_json::from_value(json!({
            "host": "github.com",
            "repository": "pingdotgg/t3code",
            "number": number,
            "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
            "source": "manual",
            "linkedAt": updated_at,
            "snapshot": {
                "state": "open",
                "title": format!("Change {number}"),
                "headBranch": head,
                "baseBranch": base,
                "isDraft": false,
                "updatedAt": updated_at,
                "syncedAt": updated_at,
            },
            "stack": null,
        }))
        .unwrap()
    }

    fn watched(mut link: ThreadPullRequestLink) -> ThreadPullRequestLink {
        link.watch = Some(
            serde_json::from_value(json!({
                "startedAt": "2026-01-01T00:00:30.000Z",
                "headSha": null,
                "failedChecks": [],
                "passed": false,
                "passedChecks": [],
                "remarksThrough": "2026-01-01T00:00:30.000Z",
                "remarkIds": [],
                "conflicting": false,
                "wakes": 0,
            }))
            .unwrap(),
        );
        link
    }

    fn numbers(links: &[ThreadPullRequestLink]) -> Vec<u64> {
        links.iter().map(|link| link.number.get()).collect()
    }

    fn fixtures() -> (
        ThreadPullRequestLink,
        ThreadPullRequestLink,
        ThreadPullRequestLink,
    ) {
        (
            link(1, "layer-one", "main", "2026-01-01T00:00:10.000Z"),
            link(2, "layer-two", "layer-one", "2026-01-01T00:00:20.000Z"),
            link(3, "unrelated", "main", "2026-01-01T00:00:05.000Z"),
        )
    }

    // ThreadDetailsPrRows.test.tsx: "shows only the current pull request until the rest are
    // asked for".
    #[test]
    fn shows_only_the_current_pull_request_until_the_rest_are_asked_for() {
        let (bottom, top, other) = fixtures();
        let rest = rest_pull_requests(&[other, bottom, top.clone()], &top);
        // Collapsed: just the current row, with the toggle offering the rest.
        assert_eq!(pr_rows_toggle_label(false, rest.len()), "Show 2 more");
        // Expanded: the stack below the current layer, then the unrelated one.
        assert_eq!(numbers(&rest), vec![1, 3]);
        assert_eq!(pr_rows_toggle_label(true, rest.len()), "Show less");
        // Collapsing again restores the first label.
        assert_eq!(pr_rows_toggle_label(false, rest.len()), "Show 2 more");
    }

    // "keeps the single row untouched when the thread links one pull request".
    #[test]
    fn keeps_the_single_row_untouched_when_the_thread_links_one_pull_request() {
        let (bottom, _, _) = fixtures();
        assert!(rest_pull_requests(std::slice::from_ref(&bottom), &bottom).is_empty());
    }

    // "lets only watched pull requests stop their watch".
    #[test]
    fn lets_only_watched_pull_requests_stop_their_watch() {
        let (bottom, top, other) = fixtures();
        let bottom = watched(bottom);
        let rest = rest_pull_requests(&[other, bottom, top.clone()], &top);
        let watched_rows: Vec<u64> = rest
            .iter()
            .filter(|link| link.watch.is_some())
            .map(|link| link.number.get())
            .collect();
        assert_eq!(watched_rows, vec![1]);
        let row = rest.iter().find(|link| link.number.get() == 1).unwrap();
        assert_eq!(row.host.as_str(), "github.com");
        assert_eq!(row.repository.as_str(), "pingdotgg/t3code");
    }
}
