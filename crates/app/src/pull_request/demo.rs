//! The pull request demo hook (`TURIN_PULL_REQUESTS_DEMO`), the way the composer has
//! `TURIN_COMPOSER_DEMO`: wires the mock source into the window so each view can be reached from
//! the command line and photographed.
//!
//! Scenarios (combine with `TURIN_APPEARANCE=light|dark`):
//! - `list` — the pull request page.
//! - `list:<preset>` — `all`, `filters`, `empty`, `loading`, `error`.
//! - `detail[:<tab>[:<number>]]` — the page with the detail panel open on `summary`, `timeline`,
//!   `code` or `guide`.

use gpui_kit::{App, AppContext as _, Entity, Window};

use super::detail_context::DetailTab;
use super::list_page::{PullRequestsPage, PullRequestsPageEvent};
use super::pull_request_list_preferences::{PullRequestListPreferences, PullRequestListSort};
use super::mock_source::{MockPullRequestSource, demo_environment_id, demo_reference};
use super::panel_host::PullRequestPanelHost;
use super::view_model::PullRequestStore;
use crate::components::app_sidebar_layout::AppSidebarLayout;

/// The parsed `TURIN_PULL_REQUESTS_DEMO` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scenario {
    List(Option<String>),
    Detail { tab: Option<DetailTab>, number: Option<u64> },
    /// The thread view's right panel on the linked pull requests of the demo thread.
    Thread,
}

pub fn scenario_from_env() -> Option<Scenario> {
    parse(&std::env::var("TURIN_PULL_REQUESTS_DEMO").ok()?)
}

pub fn parse(value: &str) -> Option<Scenario> {
    let mut parts = value.split(':');
    match parts.next()? {
        "list" => Some(Scenario::List(parts.next().map(str::to_owned))),
        "thread" => Some(Scenario::Thread),
        "detail" => Some(Scenario::Detail {
            tab: parts.next().and_then(DetailTab::parse),
            number: parts.next().and_then(|n| n.parse().ok()),
        }),
        _ => None,
    }
}

/// Installs the scenario into the shell.
pub fn install(
    layout: &Entity<AppSidebarLayout>,
    scenario: Scenario,
    window: &mut Window,
    cx: &mut App,
) {
    let store: Entity<PullRequestStore> = MockPullRequestSource::start(cx);
    if scenario == Scenario::Thread {
        let thread_host = cx.new(|cx| {
            super::panel_host::ThreadPanelHost::new(
                store.clone(),
                super::mock_source::demo_thread_id(),
                window,
                cx,
            )
        });
        layout.update(cx, |layout, cx| {
            layout.set_right_panel_content(Some(thread_host.into()), cx);
        });
        return;
    }
    let host = cx.new(|cx| PullRequestPanelHost::new(store.clone(), cx));
    let page = cx.new(|cx| PullRequestsPage::new(store.clone(), window, cx));
    layout.update(cx, |layout, cx| {
        layout.set_main_content(Some(page.clone().into()), cx);
    });
    // Choosing a row opens its detail in the right panel, as the route does.
    let panel_host = host.clone();
    let panel_layout = layout.clone();
    window
        .subscribe(&page, cx, move |_, event: &PullRequestsPageEvent, window, cx| {
            let PullRequestsPageEvent::Select {
                environment_id,
                reference,
            } = event;
            panel_host.update(cx, |host, cx| {
                host.open(environment_id.clone(), reference.clone(), None, window, cx)
            });
            panel_layout.update(cx, |layout, cx| {
                layout.set_right_panel_content(Some(panel_host.clone().into()), cx);
            });
        })
        .detach();
    match scenario {
        Scenario::List(preset) => apply_list_preset(&page, &store, preset.as_deref(), cx),
        Scenario::Thread => {}
        Scenario::Detail { tab, number } => {
            let mut reference = demo_reference();
            if let Some(number) = number
                && let Some(number) = turin_contracts::PositiveInt::new(number)
            {
                reference.number = number;
            }
            host.update(cx, |host, cx| {
                host.open(demo_environment_id(), reference, tab, window, cx)
            });
            layout.update(cx, |layout, cx| {
                layout.set_right_panel_content(Some(host.clone().into()), cx);
            });
        }
    }
}

/// `list:<preset>`: scopes the page the way a reader would have, for screenshots.
fn apply_list_preset(
    page: &Entity<PullRequestsPage>,
    store: &Entity<PullRequestStore>,
    preset: Option<&str>,
    cx: &mut App,
) {
    use turin_contracts::pull_request::{PullRequestInvolvement, PullRequestListState};
    let mut scope = PullRequestListPreferences::default();
    match preset {
        Some("all") => scope.state = PullRequestListState::All,
        Some("filters") => {
            scope.state = PullRequestListState::All;
            scope.sort = Some(PullRequestListSort::Updated);
            scope.involvement = PullRequestInvolvement::All;
            scope.draft = Some(turin_contracts::pull_request::PullRequestDraftFilter::Hide);
        }
        Some("authored") => scope.involvement = PullRequestInvolvement::Authored,
        Some("empty") => scope.q = Some("zzzz-no-match".into()),
        Some("error") => {
            store.update(cx, |store, cx| {
                store.update_list(cx, |list| {
                    list.merged = None;
                    list.error = Some("gh: could not reach api.github.com. Check your connection.".into());
                })
            });
        }
        Some("loading") => {
            store.update(cx, |store, cx| store.update_list(cx, |list| list.merged = None));
        }
        _ => {}
    }
    page.update(cx, |page, cx| page.set_scope(scope, cx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scenarios() {
        assert_eq!(parse("list"), Some(Scenario::List(None)));
        assert_eq!(parse("list:empty"), Some(Scenario::List(Some("empty".into()))));
        assert_eq!(
            parse("detail:code:12"),
            Some(Scenario::Detail { tab: Some(DetailTab::Code), number: Some(12) })
        );
        assert_eq!(parse("nope"), None);
    }
}
