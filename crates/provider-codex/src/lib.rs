//! Codex provider adapter, ported from `CodexAdapterV2.ts` and `provider/Codex*.ts`.
pub mod adapter;
pub mod approvals;
pub mod codex_auth_callback;
pub mod codex_auth_callback_page;
pub mod codex_chatgpt_auth;
pub mod codex_chatgpt_handoff;
pub mod codex_chatgpt_models;
pub mod codex_chatgpt_session_lock;
pub mod codex_developer_instructions;
pub mod codex_home_layout;
pub mod codex_input;
pub mod codex_installation;
pub mod codex_launch_args;
pub mod codex_managed_errors;
pub mod codex_managed_provider;
pub mod codex_managed_runtime;
pub mod codex_mcp_elicitation;
pub mod codex_model;
pub mod codex_model_catalog;
pub mod codex_provider;
pub mod codex_skills;
mod codex_snapshot;
pub mod codex_turn_token_usage;
pub mod codex_usage_limits;
pub mod driver;
#[cfg(test)]
mod driver_tests;
pub mod goal;
pub mod items;
pub mod orchestration_adapter;
pub mod params;
pub mod projection;
#[cfg(test)]
mod replay_tests;
pub mod retry;
pub mod runtime;
pub mod subagent;
pub mod tool_presentation;

pub use driver::CodexDriver;
pub use orchestration_adapter::CodexProviderAdapter;

pub use adapter::{CodexAdapter, CodexEvent, CodexEventKind, CodexSession};
