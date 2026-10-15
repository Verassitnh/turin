//! Cursor flavour of the ACP provider capability block.
//!
//! Fork, resume, and close are not supported (`-32601`). Load is the resume
//! operation. Model changes stay in the process via `set_config_option`, so
//! model switch is in-session when the agent advertised config options.
//! Runtime-mode changes still reopen the process: Cursor's allowlist is
//! global and `--force` is never passed.

use serde_json::Value;
use turin_acp::compat::InitializeResponse;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ApprovalCapabilities, OrchestrationV2CheckpointCapabilities,
    OrchestrationV2ContextCapabilities, OrchestrationV2IdentityCapabilities,
    OrchestrationV2NativeRefStrength, OrchestrationV2PlanningCapabilities,
    OrchestrationV2ProviderCapabilities, OrchestrationV2RuntimePolicyCapabilities,
    OrchestrationV2RuntimePolicyEnforcement, OrchestrationV2SessionCapabilities,
    OrchestrationV2StreamingCapabilities, OrchestrationV2SubagentCapabilities,
    OrchestrationV2ThreadCapabilities, OrchestrationV2ToolCapabilities,
    OrchestrationV2TurnCapabilities,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTransition {
    /// Model or option change via `session/set_config_option` on the open process.
    KeepSession,
    /// Runtime mode changed, or the agent cannot switch models in place.
    ReopenSession,
}

pub fn cursor_capabilities() -> OrchestrationV2ProviderCapabilities {
    OrchestrationV2ProviderCapabilities {
        sessions: OrchestrationV2SessionCapabilities {
            supports_multiple_provider_threads_per_session: false,
            supports_model_switch_in_session: true,
            supports_provider_switching_via_handoff: true,
            supports_runtime_mode_switch_in_session: false,
            pending_requests_survive_restart: false,
        },
        threads: OrchestrationV2ThreadCapabilities {
            can_create_empty_thread: true,
            // read_thread_snapshot is not implemented. See negotiate_capabilities.
            can_read_thread_snapshot: false,
            can_rollback_thread: false,
            can_fork_thread: false,
            can_fork_from_turn: false,
            can_fork_from_subagent_thread: false,
            exposes_native_thread_id: true,
        },
        turns: OrchestrationV2TurnCapabilities {
            exposes_native_turn_id: false,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_active_steering: false,
            active_steering_interrupts_tools: None,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: OrchestrationV2NativeRefStrength::Strong,
        },
        streaming: OrchestrationV2StreamingCapabilities {
            streams_assistant_text: true,
            streams_reasoning: true,
            streams_tool_output: true,
            streams_plan_text: true,
            emits_message_completed: true,
        },
        tools: OrchestrationV2ToolCapabilities {
            exposes_tool_item_ids: true,
            emits_tool_started: true,
            emits_tool_completed: true,
            emits_tool_output: true,
            supports_mcp_tools: false,
            supports_dynamic_tool_callbacks: false,
        },
        approvals: OrchestrationV2ApprovalCapabilities {
            supports_command_approval: true,
            supports_file_read_approval: true,
            supports_file_change_approval: true,
            supports_apply_patch_approval: false,
            approvals_have_native_request_ids: true,
            approval_callbacks_are_live_only: true,
            approvals_can_originate_from_subagents: false,
        },
        planning: OrchestrationV2PlanningCapabilities {
            emits_plan_updated: true,
            emits_todo_list: true,
            emits_proposed_plan: true,
            supports_structured_questions: true,
            plan_deltas_have_item_ids: false,
        },
        subagents: OrchestrationV2SubagentCapabilities {
            supports_subagents: true,
            exposes_subagent_thread_ids: false,
            emits_subagent_lifecycle: false,
            can_wait_for_subagents: false,
            can_close_subagents: false,
            can_fork_subagent_thread: false,
        },
        context: OrchestrationV2ContextCapabilities {
            accepts_system_context: false,
            accepts_developer_context: false,
            // Handoff text reaches the agent as a prompt, so it consumes handoffs
            // like any synthetic user context. This adapter never authors a
            // summary; summary generation is text generation, which Cursor does not
            // provide here.
            accepts_synthetic_user_context: true,
            can_generate_summaries: false,
            can_consume_handoff_summaries: true,
            supports_delta_handoff: true,
            supports_full_thread_handoff: true,
            max_recommended_handoff_chars: None,
        },
        checkpointing: OrchestrationV2CheckpointCapabilities {
            app_can_checkpoint_filesystem: true,
            supports_nested_checkpoint_scopes: true,
            provider_can_rollback_conversation: false,
            provider_rollback_returns_snapshot: false,
            provider_can_read_conversation_snapshot: false,
        },
        identity: OrchestrationV2IdentityCapabilities {
            native_thread_ids: OrchestrationV2NativeRefStrength::Strong,
            native_turn_ids: OrchestrationV2NativeRefStrength::Weak,
            native_item_ids: OrchestrationV2NativeRefStrength::Weak,
            native_request_ids: OrchestrationV2NativeRefStrength::Weak,
        },
        runtime_policy: OrchestrationV2RuntimePolicyCapabilities {
            enforcement: OrchestrationV2RuntimePolicyEnforcement::ClientBoundary,
        },
    }
}

/// `loadSession` makes `session/load` available, which is how a thread
/// resumes. It does not make thread snapshot reads work: `read_thread_snapshot`
/// is not implemented, so the flag stays false whatever the agent advertises.
pub fn negotiate_capabilities(
    _initialize: &InitializeResponse,
    session_setup: Option<&Value>,
) -> OrchestrationV2ProviderCapabilities {
    let mut capabilities = cursor_capabilities();
    let has_model_config = session_setup.is_some_and(|setup| {
        setup
            .get("configOptions")
            .and_then(Value::as_array)
            .is_some_and(|options| {
                options.iter().any(|option| {
                    option.get("id").and_then(Value::as_str) == Some("model")
                        || option.get("category").and_then(Value::as_str) == Some("model")
                })
            })
            || setup
                .get("models")
                .and_then(Value::as_array)
                .is_some_and(|models| !models.is_empty())
    });
    capabilities.sessions.supports_model_switch_in_session = has_model_config;
    capabilities.threads.can_fork_thread = false;
    capabilities
}

pub fn plan_selection_transition(
    capabilities: &OrchestrationV2ProviderCapabilities,
    runtime_mode_changed: bool,
) -> SelectionTransition {
    if runtime_mode_changed || !capabilities.sessions.supports_model_switch_in_session {
        SelectionTransition::ReopenSession
    } else {
        SelectionTransition::KeepSession
    }
}

pub fn image_prompts_supported(agent_capabilities: &Value) -> bool {
    match agent_capabilities.pointer("/promptCapabilities/image") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_acp::compat::{InitializeResponse, ProtocolGeneration};

    #[test]
    fn thread_snapshot_reads_stay_off_even_when_load_is_advertised() {
        let initialize = InitializeResponse {
            protocol_version: 1,
            agent_capabilities: json!({"loadSession": true}),
            auth_methods: Vec::new(),
            agent_info: None,
            meta: None,
            generation: ProtocolGeneration::V1,
        };
        let negotiated = negotiate_capabilities(&initialize, None);
        assert!(!negotiated.threads.can_read_thread_snapshot);
        assert!(
            !negotiated
                .checkpointing
                .provider_can_read_conversation_snapshot
        );
        assert!(!cursor_capabilities().threads.can_read_thread_snapshot);
    }

    #[test]
    fn context_handoff_flags_follow_the_handoff_switch() {
        let context = cursor_capabilities().context;
        assert!(
            cursor_capabilities()
                .sessions
                .supports_provider_switching_via_handoff
        );
        assert!(context.accepts_synthetic_user_context);
        assert!(context.can_consume_handoff_summaries);
        assert!(context.supports_delta_handoff);
        assert!(context.supports_full_thread_handoff);
        assert!(!context.can_generate_summaries);
    }

    #[test]
    fn fork_stays_unsupported_and_model_switch_follows_config_options() {
        let initialize = InitializeResponse {
            protocol_version: 1,
            agent_capabilities: json!({"loadSession": true, "promptCapabilities": {"image": {}}}),
            auth_methods: vec![json!({"id": "cursor_login"})],
            agent_info: None,
            meta: None,
            generation: ProtocolGeneration::V1,
        };
        let negotiated = negotiate_capabilities(
            &initialize,
            Some(&json!({"configOptions": [{"id": "model", "category": "model"}]})),
        );
        assert!(!negotiated.threads.can_fork_thread);
        assert!(!negotiated.threads.can_read_thread_snapshot);
        assert!(negotiated.sessions.supports_model_switch_in_session);
        assert!(!negotiated.sessions.supports_runtime_mode_switch_in_session);
        assert_eq!(
            plan_selection_transition(&negotiated, false),
            SelectionTransition::KeepSession
        );
        assert_eq!(
            plan_selection_transition(&negotiated, true),
            SelectionTransition::ReopenSession
        );
        assert!(image_prompts_supported(&initialize.agent_capabilities));
    }
}
