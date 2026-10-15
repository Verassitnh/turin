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
use super::mock_source::{MockPullRequestSource, demo_environment_id, demo_reference};
use super::panel_host::PullRequestPanelHost;
use super::view_model::PullRequestStore;
use crate::components::app_sidebar_layout::AppSidebarLayout;

/// The parsed `TURIN_PULL_REQUESTS_DEMO` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scenario {
    List(Option<String>),
    Detail { tab: Option<DetailTab>, number: Option<u64> },
}

pub fn scenario_from_env() -> Option<Scenario> {
    parse(&std::env::var("TURIN_PULL_REQUESTS_DEMO").ok()?)
}

pub fn parse(value: &str) -> Option<Scenario> {
    let mut parts = value.split(':');
    match parts.next()? {
        "list" => Some(Scenario::List(parts.next().map(str::to_owned))),
        "detail" => Some(Scenario::Detail {
            tab: parts.next().and_then(DetailTab::parse),
            number: parts.next().and_then(|n| n.parse().ok()),
        }),
        _ => None,
    }
}

/// Installs the scenario into the shell. Returns whether one was requested.
pub fn install(
    layout: &Entity<AppSidebarLayout>,
    scenario: Scenario,
    window: &mut Window,
    cx: &mut App,
) {
    let store: Entity<PullRequestStore> = MockPullRequestSource::start(cx);
    let host = cx.new(|cx| PullRequestPanelHost::new(store.clone(), cx));
    match scenario {
        Scenario::List(_) => {}
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
