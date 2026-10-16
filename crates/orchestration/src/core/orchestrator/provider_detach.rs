//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts
//! (`detachSessionIds`). Provider-session detach for thread-row commands: the `detachSessionIds`
//! block of upstream `Orchestrator.ts`. Each live session gets one
//! `provider-session.detached` event and one `provider-session.detach` effect.
//! The effect is what stops the live process (T13e's executor runs it).

use turin_contracts::{
    CommandId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderSessionDetached, OrchestrationV2ProviderSessionStatus,
    },
};

use super::dispatch::Decision;
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};

/// Sessions a detach touches: those not already stopped or errored.
pub(super) fn is_live(session: &OrchestrationV2ProviderSession) -> bool {
    !matches!(
        session.status,
        OrchestrationV2ProviderSessionStatus::Stopped | OrchestrationV2ProviderSessionStatus::Error
    )
}

/// Detaches `sessions` (already filtered to the ones to detach). `revoke_mcp`
/// is set only for terminal detaches (archive), so a re-attaching provider
/// process stays authorized otherwise.
pub(super) fn detach_sessions(
    decision: &mut Decision,
    thread_id: &ThreadId,
    sessions: &[OrchestrationV2ProviderSession],
    reason: &str,
    revoke_mcp: bool,
) {
    let live: Vec<&OrchestrationV2ProviderSession> =
        sessions.iter().filter(|session| is_live(session)).collect();
    let now = decision.now();
    let command_id = decision.command_id().clone();
    for session in live {
        emit_detached(decision, thread_id, session, reason, now);
        decision.effects.push(detach_effect(
            &command_id,
            thread_id,
            &session.id,
            reason,
            revoke_mcp,
        ));
    }
}

fn emit_detached(
    decision: &mut Decision,
    thread_id: &ThreadId,
    session: &OrchestrationV2ProviderSession,
    reason: &str,
    now: UtcDateTime,
) {
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ProviderSessionDetached {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: Some(session.driver.clone()),
        provider_instance_id: Some(session.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2ProviderSessionDetached {
            provider_session_id: session.id.clone(),
            detached_at: now,
            reason: Some(reason.to_owned()),
        },
    });
}

fn detach_effect(
    command_id: &CommandId,
    thread_id: &ThreadId,
    session_id: &turin_contracts::ProviderSessionId,
    reason: &str,
    revoke_mcp: bool,
) -> PendingEffect {
    PendingEffect {
        id: format!("effect:{command_id}:provider-session.detach:{session_id}"),
        command_id: command_id.clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderSessionDetach {
            provider_session_id: session_id.clone(),
            detail: Some(reason.to_owned()),
            revoke_mcp_credential: revoke_mcp.then_some(true),
        },
        available_at: None,
    }
}
