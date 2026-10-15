//! Ports of the composer: upstream/apps/web/src/components/chat/Composer*.tsx,
//! ComposerPromptEditor*.tsx and the lib-level `composer-*.ts` helpers.
//!
//! Pure logic modules come first (each with its upstream tests); the GPUI
//! views build on them. `view_model` is the explicit seam T14's reducers plug
//! into: the composer renders a [`view_model::ComposerViewModel`] and reports
//! user intent through [`view_model::ComposerCallbacks`].

pub mod editor_mentions;
pub mod inline_tokens;
pub mod js_string;
pub mod logic;
pub mod rich_text;
