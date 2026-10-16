//! Port of upstream/packages/shared/src/codexMarkdownDirectives.ts
//!
//! The directive scanner moved to `turin_shared::codex_markdown_directives` so the thread find
//! extractor shares it; the copy adapter is re-exported for the timeline.

pub use turin_shared::codex_markdown_directives::render_codex_directives_for_copy;
