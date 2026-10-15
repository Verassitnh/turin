//! Provider-agnostic text generation (thread titles, commit messages, PR bodies,
//! branch names) and the per-provider generators. See `PORTING.md`.
//!
//! Upstream: `packages/provider-core/src/server/textGeneration*`,
//! `apps/server/src/textGeneration/*`, and the text generation files of the Codex,
//! Claude, OpenCode, and Cursor providers.

pub mod claude_text_generation;
pub mod cli_args;
pub mod codex_launch_args;
pub mod codex_text_generation;
pub mod command;
pub mod git_branch_names;
pub mod json_reply;
pub mod opencode2_text_generation;
pub mod opencode_text_generation;
pub mod text_generation;
pub mod text_generation_operations;
pub mod text_generation_policy;
pub mod text_generation_presets;
pub mod text_generation_prompts;
pub mod text_generation_utils;
pub mod thread_title_context;
pub mod thread_title_links;

pub use text_generation::{TextGeneration, TextGenerationInstances};
pub use text_generation_operations::{
    Investigate, Operation, Request, Runner, RunnerTextGeneration, decode_json_reply,
    investigation_unsupported,
};
pub use text_generation_policy::{
    TextGenerationPolicy, TextGenerationPolicyKind, TextGenerationPolicyOverrides,
    custom_text_generation_policy,
};
pub use text_generation_presets::{
    conventional_commits_text_generation_policy, repository_conventions_text_generation_policy,
};
pub use thread_title_links::{
    LinkLookup, LinkSubject, SourceControlLinkResolver, resolve_thread_title_links,
};
