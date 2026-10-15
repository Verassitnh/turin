//! The client's thread read model: upstream's `OrchestrationV2ThreadProjection`
//! with every entity and every collection behind an `Arc`.
//!
//! Upstream's reducers rely on structural sharing (`next.runs === prev.runs`,
//! `next.visibleTurnItems[0] === prev.visibleTurnItems[0]`) so selectors can
//! memoize on identity and the UI re-renders only changed rows. Here a
//! projection is a bag of `Arc<Vec<Arc<T>>>`: cloning it is a handful of
//! reference-count bumps, an event replaces only the collection it touches
//! (`Arc::make_mut` copies at most that one vector of pointers), and "same
//! object" is `Arc::ptr_eq`.

use std::sync::Arc;

use crate::state::contracts::{
    NonNegativeInt, OrchestrationV2AppThread, OrchestrationV2Checkpoint,
    OrchestrationV2CheckpointScope, OrchestrationV2ContextHandoff, OrchestrationV2ContextTransfer,
    OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode, OrchestrationV2PlanArtifact,
    OrchestrationV2ProjectedTurnItem, OrchestrationV2ProjectedTurnItemVisibility,
    OrchestrationV2ProviderSession, OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
    OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RuntimeRequest,
    OrchestrationV2Subagent, OrchestrationV2ThreadProjection, OrchestrationV2TurnItem, ThreadId,
    TurnItemId, UtcDateTime,
};

pub type Rows<T> = Arc<Vec<Arc<T>>>;

/// `OrchestrationV2ProjectedTurnItem` with its item shared with `turn_items`.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleTurnItem {
    pub position: NonNegativeInt,
    pub visibility: OrchestrationV2ProjectedTurnItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: Arc<OrchestrationV2TurnItem>,
}

pub type VisibleTurnItemRow = Arc<VisibleTurnItem>;

impl From<OrchestrationV2ProjectedTurnItem> for VisibleTurnItem {
    fn from(row: OrchestrationV2ProjectedTurnItem) -> Self {
        Self {
            position: row.position,
            visibility: row.visibility,
            source_thread_id: row.source_thread_id,
            source_item_id: row.source_item_id,
            item: Arc::new(row.item),
        }
    }
}

impl From<&VisibleTurnItem> for OrchestrationV2ProjectedTurnItem {
    fn from(row: &VisibleTurnItem) -> Self {
        Self {
            position: row.position,
            visibility: row.visibility,
            source_thread_id: row.source_thread_id.clone(),
            source_item_id: row.source_item_id.clone(),
            item: (*row.item).clone(),
        }
    }
}

/// `OrchestrationV2ThreadProjection`.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadProjection {
    pub thread: Arc<OrchestrationV2AppThread>,
    pub runs: Rows<OrchestrationV2Run>,
    pub attempts: Rows<OrchestrationV2RunAttempt>,
    pub nodes: Rows<OrchestrationV2ExecutionNode>,
    pub subagents: Rows<OrchestrationV2Subagent>,
    pub provider_sessions: Rows<OrchestrationV2ProviderSession>,
    pub provider_threads: Rows<OrchestrationV2ProviderThread>,
    pub provider_turns: Rows<OrchestrationV2ProviderTurn>,
    pub runtime_requests: Rows<OrchestrationV2RuntimeRequest>,
    pub messages: Rows<OrchestrationV2ConversationMessage>,
    pub plans: Rows<OrchestrationV2PlanArtifact>,
    pub turn_items: Rows<OrchestrationV2TurnItem>,
    pub checkpoint_scopes: Rows<OrchestrationV2CheckpointScope>,
    pub checkpoints: Rows<OrchestrationV2Checkpoint>,
    pub context_handoffs: Rows<OrchestrationV2ContextHandoff>,
    pub context_transfers: Rows<OrchestrationV2ContextTransfer>,
    pub visible_turn_items: Arc<Vec<VisibleTurnItemRow>>,
    pub updated_at: UtcDateTime,
}

fn rows<T>(items: Vec<T>) -> Rows<T> {
    Arc::new(items.into_iter().map(Arc::new).collect())
}

impl From<OrchestrationV2ThreadProjection> for ThreadProjection {
    fn from(projection: OrchestrationV2ThreadProjection) -> Self {
        Self {
            thread: Arc::new(projection.thread),
            runs: rows(projection.runs),
            attempts: rows(projection.attempts),
            nodes: rows(projection.nodes),
            subagents: rows(projection.subagents),
            provider_sessions: rows(projection.provider_sessions),
            provider_threads: rows(projection.provider_threads),
            provider_turns: rows(projection.provider_turns),
            runtime_requests: rows(projection.runtime_requests),
            messages: rows(projection.messages),
            plans: rows(projection.plans),
            turn_items: rows(projection.turn_items),
            checkpoint_scopes: rows(projection.checkpoint_scopes),
            checkpoints: rows(projection.checkpoints),
            context_handoffs: rows(projection.context_handoffs),
            context_transfers: rows(projection.context_transfers),
            visible_turn_items: Arc::new(
                projection
                    .visible_turn_items
                    .into_iter()
                    .map(|row| Arc::new(VisibleTurnItem::from(row)))
                    .collect(),
            ),
            updated_at: projection.updated_at,
        }
    }
}

fn unrows<T: Clone>(rows: &Rows<T>) -> Vec<T> {
    rows.iter().map(|row| (**row).clone()).collect()
}

impl ThreadProjection {
    /// The contract value (persistence, tests). Deep-copies.
    pub fn to_contract(&self) -> OrchestrationV2ThreadProjection {
        OrchestrationV2ThreadProjection {
            thread: (*self.thread).clone(),
            runs: unrows(&self.runs),
            attempts: unrows(&self.attempts),
            nodes: unrows(&self.nodes),
            subagents: unrows(&self.subagents),
            provider_sessions: unrows(&self.provider_sessions),
            provider_threads: unrows(&self.provider_threads),
            provider_turns: unrows(&self.provider_turns),
            runtime_requests: unrows(&self.runtime_requests),
            messages: unrows(&self.messages),
            plans: unrows(&self.plans),
            turn_items: unrows(&self.turn_items),
            checkpoint_scopes: unrows(&self.checkpoint_scopes),
            checkpoints: unrows(&self.checkpoints),
            context_handoffs: unrows(&self.context_handoffs),
            context_transfers: unrows(&self.context_transfers),
            visible_turn_items: self
                .visible_turn_items
                .iter()
                .map(|row| OrchestrationV2ProjectedTurnItem::from(&**row))
                .collect(),
            updated_at: self.updated_at,
        }
    }
}
