//! Port of upstream/apps/server/src/orchestration-v2/NotificationMailbox.ts

use turin_contracts::MessageId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2ConversationMessage, OrchestrationV2CreationSource,
    OrchestrationV2ProviderTurnStatus, OrchestrationV2RunStatus, OrchestrationV2ThreadProjection,
    latest_provider_turn_for_attempt,
};

pub fn is_buffered_provider_notification(message: &OrchestrationV2ConversationMessage) -> bool {
    message.created_by == OrchestrationV2Actor::Agent
        && message.creation_source == OrchestrationV2CreationSource::Provider
        && message.notification.is_some()
        && message.delegated_completion.is_none()
}

pub fn is_undelivered_mailbox_steer(
    projection: &OrchestrationV2ThreadProjection,
    message_id: &MessageId,
) -> bool {
    let Some(message) = projection
        .messages
        .iter()
        .find(|message| &message.id == message_id)
    else {
        return false;
    };
    if message.delegated_completion.is_none() {
        return false;
    }
    let Some(run) = projection
        .runs
        .iter()
        .find(|run| Some(&run.id) == message.run_id.as_ref())
    else {
        return false;
    };
    if run.user_message_id == message.id {
        return false;
    }
    if matches!(
        run.status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::Cancelled
            | OrchestrationV2RunStatus::RolledBack
    ) {
        return true;
    }
    run.active_attempt_id
        .as_ref()
        .and_then(|id| latest_provider_turn_for_attempt(&projection.provider_turns, Some(id)))
        .is_some_and(|turn| turn.status == OrchestrationV2ProviderTurnStatus::Completed)
}
