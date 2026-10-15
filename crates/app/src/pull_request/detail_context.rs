//! What every tab of the detail panel is constructed with: the store to read, the pull request
//! it shows, and the panel-level navigation upstream threads through props
//! (`openCommit`, `openFileInCode`, `onSelectPullRequest`, `onGuideShown`).

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Entity};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;

use super::view_model::{PullRequestDetailSnapshot, PullRequestSource, PullRequestStore};

/// The tabs of the panel (`DetailTab`); checks are a popover in the header, reviews live in the
/// summary and the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetailTab {
    Summary,
    Guide,
    Timeline,
    Code,
}

impl DetailTab {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "summary" => Some(Self::Summary),
            "guide" => Some(Self::Guide),
            "timeline" => Some(Self::Timeline),
            "code" => Some(Self::Code),
            _ => None,
        }
    }
}

/// Navigation the panel offers its tabs.
#[derive(Clone)]
pub struct DetailNav {
    /// The summary's commit row: scope the Code tab to a commit and show it.
    pub open_commit: Rc<dyn Fn(&str, &mut App)>,
    /// The guide names a file: show it in the whole diff on the Code tab.
    pub open_file_in_code: Rc<dyn Fn(&str, &mut App)>,
    /// Switch tab.
    pub open_tab: Rc<dyn Fn(DetailTab, &mut App)>,
    /// A stack layer or linked pull request was chosen.
    pub select_pull_request: Rc<dyn Fn(&PullRequestRef, &mut App)>,
}

impl DetailNav {
    /// Navigation that does nothing (tests, standalone previews).
    pub fn noop() -> Self {
        Self {
            open_commit: Rc::new(|_, _| {}),
            open_file_in_code: Rc::new(|_, _| {}),
            open_tab: Rc::new(|_, _| {}),
            select_pull_request: Rc::new(|_, _| {}),
        }
    }
}

#[derive(Clone)]
pub struct DetailContext {
    pub store: Entity<PullRequestStore>,
    pub environment_id: EnvironmentId,
    pub reference: PullRequestRef,
    pub nav: DetailNav,
}

impl DetailContext {
    pub fn snapshot<'a>(&self, cx: &'a App) -> Option<&'a PullRequestDetailSnapshot> {
        self.store.read(cx).detail(&self.reference)
    }

    pub fn source(&self, cx: &App) -> Option<Arc<dyn PullRequestSource>> {
        self.store.read(cx).source()
    }
}
