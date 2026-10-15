//! The diff panel (upstream `DiffPanel.tsx` and friends), its pure logic and
//! the ports of the `@pierre/diffs` pieces it renders with.

// The ports are complete ahead of their callers (the real orchestrator and git
// sources, the composer, settings persistence); unused pieces are intentional.
#![allow(dead_code)]

pub mod annotation;
pub mod base_ref_choices;
pub mod comment_submit_shortcut;
pub mod diff_collapse;
pub mod diff_file_actions;
pub mod diff_file_tree_logic;
pub mod diff_panel_store;
pub mod diff_rendering;
pub mod file_tree;
pub mod highlight;
pub mod menu;
pub mod mock_fixtures;
pub mod mock_source;
pub mod natural;
pub mod panel;
mod panel_view;
pub mod pierre;
pub mod review_comment_context;
pub mod rows;
pub mod shell;
pub mod theme;
pub mod view_model;

use gpui_kit::{App, Global};

/// "Open diff" requests from elsewhere in the app (the changed-files card of
/// a turn): the panel selects the turn, the shell opens the right panel.
#[derive(Default)]
struct DiffOpenRequests {
    turn: Option<(String, Option<String>)>,
    open_panel: bool,
}

impl Global for DiffOpenRequests {}

/// `onOpenTurnDiff`: select `run_id` (and `file_path`) in the diff panel and
/// ask the shell to show it.
pub fn request_open_turn_diff(run_id: &str, file_path: Option<&str>, cx: &mut App) {
    let requests = cx.default_global::<DiffOpenRequests>();
    requests.turn = Some((run_id.to_string(), file_path.map(str::to_string)));
    requests.open_panel = true;
    cx.refresh_windows();
}

/// Consumed by the shell: whether the right panel should open for a diff.
pub fn take_open_panel_request(cx: &mut App) -> bool {
    std::mem::take(&mut cx.default_global::<DiffOpenRequests>().open_panel)
}

/// Consumed by the panel: the turn selection to apply.
pub(crate) fn take_turn_request(cx: &mut App) -> Option<(String, Option<String>)> {
    cx.default_global::<DiffOpenRequests>().turn.take()
}
