//! Accessors over `OrchestrationV2DomainEvent`'s base fields (every variant
//! carries `id`, `threadId` and `occurredAt`).

use crate::state::contracts::{EventId, OrchestrationV2DomainEvent, ThreadId, UtcDateTime};

macro_rules! domain_event_dispatch {
    ($self:expr, $field:ident) => {
        match $self {
            OrchestrationV2DomainEvent::ThreadCreated { $field, .. }
            | OrchestrationV2DomainEvent::ThreadArchived { $field, .. }
            | OrchestrationV2DomainEvent::ThreadUnarchived { $field, .. }
            | OrchestrationV2DomainEvent::ThreadDeleted { $field, .. }
            | OrchestrationV2DomainEvent::ThreadSettled { $field, .. }
            | OrchestrationV2DomainEvent::ThreadUnsettled { $field, .. }
            | OrchestrationV2DomainEvent::ThreadSnoozed { $field, .. }
            | OrchestrationV2DomainEvent::ThreadUnsnoozed { $field, .. }
            | OrchestrationV2DomainEvent::ThreadPinned { $field, .. }
            | OrchestrationV2DomainEvent::ThreadAutoSettleSet { $field, .. }
            | OrchestrationV2DomainEvent::ThreadUnpinned { $field, .. }
            | OrchestrationV2DomainEvent::ThreadPinReordered { $field, .. }
            | OrchestrationV2DomainEvent::ThreadActiveReordered { $field, .. }
            | OrchestrationV2DomainEvent::ThreadVisited { $field, .. }
            | OrchestrationV2DomainEvent::ThreadMarkedUnread { $field, .. }
            | OrchestrationV2DomainEvent::ThreadMetadataUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ThreadPullRequestSynced { $field, .. }
            | OrchestrationV2DomainEvent::ThreadRuntimeModeUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ThreadInteractionModeUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ThreadModelSelectionUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ThreadProviderSwitched { $field, .. }
            | OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated { $field, .. }
            | OrchestrationV2DomainEvent::RunCreated { $field, .. }
            | OrchestrationV2DomainEvent::RunUpdated { $field, .. }
            | OrchestrationV2DomainEvent::RunBackgroundWorkCancelled { $field, .. }
            | OrchestrationV2DomainEvent::RunAttemptCreated { $field, .. }
            | OrchestrationV2DomainEvent::RunAttemptUpdated { $field, .. }
            | OrchestrationV2DomainEvent::NodeUpdated { $field, .. }
            | OrchestrationV2DomainEvent::SubagentUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ProviderSessionAttached { $field, .. }
            | OrchestrationV2DomainEvent::ProviderSessionUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ProviderSessionDetached { $field, .. }
            | OrchestrationV2DomainEvent::ProviderThreadUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ProviderTurnUpdated { $field, .. }
            | OrchestrationV2DomainEvent::RuntimeRequestUpdated { $field, .. }
            | OrchestrationV2DomainEvent::MessageUpdated { $field, .. }
            | OrchestrationV2DomainEvent::TurnItemUpdated { $field, .. }
            | OrchestrationV2DomainEvent::PlanUpdated { $field, .. }
            | OrchestrationV2DomainEvent::CheckpointScopeCreated { $field, .. }
            | OrchestrationV2DomainEvent::CheckpointCaptured { $field, .. }
            | OrchestrationV2DomainEvent::CheckpointRollbackRequested { $field, .. }
            | OrchestrationV2DomainEvent::ContextHandoffUpdated { $field, .. }
            | OrchestrationV2DomainEvent::ContextTransferCreated { $field, .. }
            | OrchestrationV2DomainEvent::ContextTransferUpdated { $field, .. } => $field,
        }
    };
}

pub trait DomainEventExt {
    fn id(&self) -> &EventId;
    fn thread_id(&self) -> &ThreadId;
    fn occurred_at(&self) -> &UtcDateTime;
}

impl DomainEventExt for OrchestrationV2DomainEvent {
    fn id(&self) -> &EventId {
        domain_event_dispatch!(self, id)
    }

    fn thread_id(&self) -> &ThreadId {
        domain_event_dispatch!(self, thread_id)
    }

    fn occurred_at(&self) -> &UtcDateTime {
        domain_event_dispatch!(self, occurred_at)
    }
}
