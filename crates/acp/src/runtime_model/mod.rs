//! Port of upstream/packages/provider-acp/src/server/runtimeModel.ts

mod content;
mod initialize;
mod mcp_identity;
mod parse_update;
mod replay;
mod session_setup;
mod terminal;
mod tool_call;
mod util;

#[cfg(test)]
mod port_tests;

pub use content::acp_content_block_display_text;
pub use initialize::{
    SyntheticLoadSessionResponse, session_model_state_from_initialize,
    synthetic_load_session_response_from_initialize,
};
pub use mcp_identity::{AcpMcpToolCallIdentity, extract_mcp_tool_call_identity};
pub use parse_update::{
    AcpParsedSessionEvent, AcpPermissionRequest, AcpPlanUpdate, ParseSessionUpdateResult,
    embedded_terminal_ids_from_session_update, parse_permission_request,
    parse_session_update_event,
};
pub use replay::{session_update_counts_as_load_replay_activity, session_update_is_replay};
pub use session_setup::{
    AcpSessionMode, AcpSessionModeState, AcpSessionModeStateWire, AcpSessionModeWire,
    AcpSessionSetupResponse, collect_session_config_option_values, extract_model_config_id,
    find_session_config_option, parse_session_mode_state,
};
pub use terminal::{AcpAgentTerminalState, apply_acp_agent_terminal_update};
pub use tool_call::{
    AcpToolCallEmitDecision, AcpToolCallEmitDecisionInput, AcpToolCallState, AcpToolCallStatus,
    canonical_item_type_from_acp_tool_kind, decide_tool_call_update_emission,
    merge_tool_call_state, tool_call_progress_length, tool_call_visible_output_changed,
};
pub use util::SessionLoadGate;
