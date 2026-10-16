//! Ports of upstream/apps/web/src/components/chat.

pub mod changed_files_tree;
pub mod chat_git_host;
pub mod chat_header;
// Chat-lane geometry for the canvas; the canvas does not call it yet.
#[allow(dead_code)]
pub mod chat_canvas_layout;
// Pure citation-selector text logic; the DOM capture half is not ported yet.
#[allow(dead_code)]
pub mod assistant_text_selection;
pub mod composer_pending_panels;
// Route handoff around the mobile composer morph. No view calls it yet.
#[allow(dead_code)]
pub mod draft_hero_transition;
#[allow(dead_code)]
pub mod folder_drop;
pub mod markdown;
pub mod message_copy_button;
pub mod messages_timeline;
pub mod panel_layout_controls;
// Pure scroll state machine; the timeline view does not drive page keys yet.
#[allow(dead_code)]
pub mod page_scroll_controller;
// Pure answer-draft logic; the pending-user-input card still keeps its drafts
// in element state, so nothing calls these yet.
#[allow(dead_code)]
pub mod pending_user_input;
pub mod thread_details_control;
// Sidebar thread drag into the composer. The sidebar view does not drive it yet.
#[allow(dead_code)]
pub mod thread_context_drag;
// The ported timeline read model keeps upstream's full helper surface; the
// view and T14's reducers call into it piecemeal.
#[allow(dead_code)]
pub mod timeline;
pub mod timeline_message_rows;
pub mod timeline_work_rows;
// Scroll anchoring state for the timeline view; the view does not read it yet.
#[allow(dead_code)]
pub mod timeline_scroll_anchoring;
// Selection-action placement; the DOM observer half is not ported yet.
#[allow(dead_code)]
pub mod selection_actions;
pub mod timestamp_format;
pub mod work_log;
#[allow(dead_code)]
pub mod workspace_file_drop;
