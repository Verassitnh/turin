//! Envelope access for [`OrchestrationV2DomainEvent`]. Upstream reads
//! `event.type`, `event.threadId` and `event.occurredAt` off the TS union;
//! the Rust enum repeats the envelope on each of its 44 variants.

use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_contracts::{ThreadId, UtcDateTime};

macro_rules! envelope {
    ($($variant:ident => $tag:literal),+ $(,)?) => {
        /// The event's `type` literal.
        pub fn event_type(event: &OrchestrationV2DomainEvent) -> &'static str {
            match event {
                $(OrchestrationV2DomainEvent::$variant { .. } => $tag,)+
            }
        }

        /// `event.threadId`.
        pub fn event_thread_id(event: &OrchestrationV2DomainEvent) -> &ThreadId {
            match event {
                $(OrchestrationV2DomainEvent::$variant { thread_id, .. })|+ => thread_id,
            }
        }

        /// `event.occurredAt`.
        pub fn event_occurred_at(event: &OrchestrationV2DomainEvent) -> &UtcDateTime {
            match event {
                $(OrchestrationV2DomainEvent::$variant { occurred_at, .. })|+ => occurred_at,
            }
        }
    };
}

envelope! {
    ThreadCreated => "thread.created",
    ThreadArchived => "thread.archived",
    ThreadUnarchived => "thread.unarchived",
    ThreadDeleted => "thread.deleted",
    ThreadSettled => "thread.settled",
    ThreadUnsettled => "thread.unsettled",
    ThreadSnoozed => "thread.snoozed",
    ThreadUnsnoozed => "thread.unsnoozed",
    ThreadPinned => "thread.pinned",
    ThreadAutoSettleSet => "thread.auto-settle-set",
    ThreadUnpinned => "thread.unpinned",
    ThreadPinReordered => "thread.pin-reordered",
    ThreadActiveReordered => "thread.active-reordered",
    ThreadVisited => "thread.visited",
    ThreadMarkedUnread => "thread.marked-unread",
    ThreadMetadataUpdated => "thread.metadata-updated",
    ThreadPullRequestSynced => "thread.pull-request-synced",
    ThreadRuntimeModeUpdated => "thread.runtime-mode-updated",
    ThreadInteractionModeUpdated => "thread.interaction-mode-updated",
    ThreadModelSelectionUpdated => "thread.model-selection-updated",
    ThreadProviderSwitched => "thread.provider-switched",
    ThreadPullRequestLinkUpdated => "thread.pull-request-link-updated",
    RunCreated => "run.created",
    RunUpdated => "run.updated",
    RunBackgroundWorkCancelled => "run.background-work-cancelled",
    RunAttemptCreated => "run-attempt.created",
    RunAttemptUpdated => "run-attempt.updated",
    NodeUpdated => "node.updated",
    SubagentUpdated => "subagent.updated",
    ProviderSessionAttached => "provider-session.attached",
    ProviderSessionUpdated => "provider-session.updated",
    ProviderSessionDetached => "provider-session.detached",
    ProviderThreadUpdated => "provider-thread.updated",
    ProviderTurnUpdated => "provider-turn.updated",
    RuntimeRequestUpdated => "runtime-request.updated",
    MessageUpdated => "message.updated",
    TurnItemUpdated => "turn-item.updated",
    PlanUpdated => "plan.updated",
    CheckpointScopeCreated => "checkpoint-scope.created",
    CheckpointCaptured => "checkpoint.captured",
    CheckpointRollbackRequested => "checkpoint.rollback-requested",
    ContextHandoffUpdated => "context-handoff.updated",
    ContextTransferCreated => "context-transfer.created",
    ContextTransferUpdated => "context-transfer.updated",
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::orchestration_v2::KNOWN_ORCHESTRATION_V2_EVENT_TYPES;

    /// The tag table above must stay in step with the contracts' list.
    #[test]
    fn tags_match_the_contract_list() {
        let ours = [
            "thread.created",
            "run.created",
            "turn-item.updated",
            "context-transfer.updated",
        ];
        for tag in ours {
            assert!(KNOWN_ORCHESTRATION_V2_EVENT_TYPES.contains(&tag));
        }
    }
}
