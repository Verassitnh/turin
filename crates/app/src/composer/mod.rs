//! Ports of the composer: upstream/apps/web/src/components/chat/Composer*.tsx,
//! ComposerPromptEditor*.tsx and the lib-level `composer-*.ts` helpers.
//!
//! Pure logic modules come first (each with its upstream tests); the GPUI
//! views build on them. `view_model` is the explicit seam T14's reducers plug
//! into: the composer renders a [`view_model::ComposerViewModel`] and reports
//! user intent through [`view_model::ComposerCallbacks`].

pub mod attachment_files;
pub mod attachment_upload_state;
pub mod banner_stack;
pub mod catalog;
pub mod code_block;
pub mod code_languages;
pub mod command_items;
pub mod context_meter;
pub mod dispatch;
pub mod draft_store;
pub mod editor_mentions;
pub mod editor_state;
pub mod file_link;
pub mod footer_layout;
pub mod inline_chip;
pub mod inline_tokens;
pub mod js_string;
pub mod list_continuation;
pub mod logic;
pub mod mention_drag;
pub mod menu_highlight;
pub mod mock_source;
pub mod model_display;
pub mod model_picker;
pub mod model_picker_keys;
pub mod model_picker_search;
pub mod primary_actions;
pub mod prompt_history;
pub mod provider_options;
pub mod provider_state;
pub mod queued_edit;
pub mod rich_doc;
pub mod rich_text;
pub mod scroll_gesture;
pub mod search_ranking;
pub mod selection;
pub mod skills;
pub mod stash;
pub mod submission;
pub mod typing_guard;
pub mod view_model;
pub mod views;
