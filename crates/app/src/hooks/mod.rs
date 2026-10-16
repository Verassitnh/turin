//! Ports of upstream/apps/web/src/hooks (the pure parts; React state wiring
//! lives on the owning GPUI entity).

pub mod chat_thread_actions;
pub mod thread_undo;
pub mod use_active_project_target;
pub mod use_active_thread_ref;
pub mod use_file_metadata;
pub mod use_handle_new_thread;
pub mod use_local_storage;
pub mod use_navigate_back;
pub mod use_preview_panel_inline_size;
pub mod use_resizable_width;
pub mod use_thread_action_menu;
pub mod use_thread_actions;
