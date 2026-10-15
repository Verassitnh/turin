//! Ports of upstream/apps/web/src/components/chat.

pub mod changed_files_tree;
pub mod chat_header;
pub mod composer_pending_panels;
pub mod markdown;
pub mod message_copy_button;
pub mod messages_timeline;
pub mod panel_layout_controls;
// The ported timeline read model keeps upstream's full helper surface; the
// view and T14's reducers call into it piecemeal.
#[allow(dead_code)]
pub mod timeline;
pub mod timeline_message_rows;
pub mod timeline_work_rows;
pub mod timestamp_format;
pub mod work_log;
