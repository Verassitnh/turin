//! Ports of the composer: upstream/apps/web/src/components/chat/Composer*.tsx,
//! ComposerPromptEditor*.tsx and the lib-level `composer-*.ts` helpers.
//!
//! Pure logic modules come first (each with its upstream tests); the GPUI
//! views build on them. `view_model` is the explicit seam T14's reducers plug
//! into: the composer renders a [`view_model::ComposerViewModel`] and reports
//! user intent through [`view_model::ComposerCallbacks`].

pub mod code_block;
pub mod code_languages;
pub mod editor_state;
pub mod editor_mentions;
pub mod inline_tokens;
pub mod js_string;
pub mod list_continuation;
pub mod logic;
pub mod menu_highlight;
pub mod prompt_history;
pub mod rich_doc;
pub mod rich_text;
pub mod submission;
pub mod typing_guard;
pub mod file_link;
pub mod inline_chip;
pub mod mention_drag;
pub mod scroll_gesture;
pub mod search_ranking;
pub mod selection;
pub mod dispatch;
pub mod primary_actions;
pub mod footer_layout;
pub mod provider_options;
pub mod provider_state;
pub mod model_display;
pub mod model_picker_keys;
pub mod model_picker_search;
pub mod catalog;
pub mod model_picker;
pub mod attachment_files;
pub mod draft_store;
pub mod skills;
pub mod command_items;
pub mod views;
pub mod view_model;
pub mod mock_source;
