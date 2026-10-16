//! Port of upstream/apps/web/src/components/pullRequest/PullRequestSpeedActions.tsx
//! (labels and merge tooltip copy).

#![allow(dead_code)]

use crate::pull_request::pull_request_list_logic::PullRequestQuickAction;

pub fn pull_request_speed_action_label(action: PullRequestQuickAction) -> &'static str {
    match action {
        PullRequestQuickAction::Close => "Close",
        PullRequestQuickAction::Merge => "Merge",
        PullRequestQuickAction::Ready => "Ready for review",
        PullRequestQuickAction::Reopen => "Reopen",
    }
}

pub fn pull_request_speed_action_tooltip(
    action: PullRequestQuickAction,
    has_stack: bool,
    supports_close_sweep: bool,
) -> String {
    match action {
        PullRequestQuickAction::Merge if has_stack => {
            "Open this pull request to merge its stack".into()
        }
        PullRequestQuickAction::Close if supports_close_sweep => {
            "Close immediately, or drag across rows to close several".into()
        }
        other => format!("{} immediately", pull_request_speed_action_label(other)),
    }
}
