//! Chat markdown: upstream's `ChatMarkdown` and its remark pipeline, rendered
//! with an inline-flow element that reproduces the browser's line boxes.

pub mod chat_markdown;
pub mod code_highlight;
pub mod file_link_meta;
pub mod inline_flow;
pub mod markdown_links;
pub mod markdown_pipeline;

pub use chat_markdown::ChatMarkdown;
