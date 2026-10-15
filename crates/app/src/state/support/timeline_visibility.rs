//! Port of upstream/packages/shared/src/orchestrationV2Timeline.ts
//!
//! Upstream's `Pick<…>` inputs become the contract types themselves.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};

use crate::state::contracts::{
    NodeId, OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RunAttemptStatus,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2UserMessageInputIntent,
    RunId,
};

use super::turn_item::{TurnItemExt, TurnItemType};

fn input_intent(item: &OrchestrationV2TurnItem) -> Option<OrchestrationV2UserMessageInputIntent> {
    match item {
        OrchestrationV2TurnItem::UserMessage { input_intent, .. } => Some(*input_intent),
        _ => None,
    }
}

pub fn is_orchestration_v2_superseded_interrupt<I: Borrow<OrchestrationV2TurnItem>>(
    item: &OrchestrationV2TurnItem,
    attempts: &[impl Borrow<OrchestrationV2RunAttempt>],
    items: &[I],
) -> bool {
    let (TurnItemType::RunInterruptResult, Some(run_id), Some(node_id)) =
        (item.item_type(), item.run_id(), item.node_id())
    else {
        return false;
    };

    let is_superseded = attempts.iter().any(|attempt| {
        let attempt = attempt.borrow();
        attempt.run_id == *run_id
            && attempt.root_node_id == *node_id
            && attempt.status == OrchestrationV2RunAttemptStatus::Superseded
    });
    if !is_superseded {
        return false;
    }

    // Paired stop-then-steer results have a matching request on the same run and
    // must stay visible. Legacy plain-steer results have no request and stay hidden.
    let has_matching_request = items.iter().any(|candidate| {
        let candidate = candidate.borrow();
        candidate.item_type() == TurnItemType::RunInterruptRequest
            && candidate.run_id() == Some(run_id)
    });
    !has_matching_request
}

pub fn is_orchestration_v2_turn_item_visible<I: Borrow<OrchestrationV2TurnItem>>(
    item: &OrchestrationV2TurnItem,
    runs: &[impl Borrow<OrchestrationV2Run>],
    attempts: &[impl Borrow<OrchestrationV2RunAttempt>],
    items: &[I],
) -> bool {
    if let Some(run_id) = item.run_id()
        && runs.iter().any(|run| {
            let run = run.borrow();
            run.id == *run_id && run.status == OrchestrationV2RunStatus::RolledBack
        })
    {
        return false;
    }

    // A queued message is composer state, not conversation history. When its run
    // is cancelled the input never reached the provider, so it must not surface
    // as a transcript row.
    if item.item_type() == TurnItemType::UserMessage
        && input_intent(item) == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
        && let Some(run_id) = item.run_id()
        && runs.iter().any(|run| {
            let run = run.borrow();
            run.id == *run_id && run.status == OrchestrationV2RunStatus::Cancelled
        })
    {
        return false;
    }

    !is_orchestration_v2_superseded_interrupt(item, attempts, items)
}

/// Index once when reconciling a whole timeline after run/attempt state changes.
pub struct TurnItemVisibility {
    statuses: HashMap<RunId, OrchestrationV2RunStatus>,
    superseded_roots: HashMap<RunId, HashSet<NodeId>>,
    interrupt_runs: HashSet<Option<RunId>>,
}

pub fn create_orchestration_v2_turn_item_visibility<I: Borrow<OrchestrationV2TurnItem>>(
    runs: &[impl Borrow<OrchestrationV2Run>],
    attempts: &[impl Borrow<OrchestrationV2RunAttempt>],
    items: &[I],
) -> TurnItemVisibility {
    let statuses = runs
        .iter()
        .map(|run| {
            let run = run.borrow();
            (run.id.clone(), run.status)
        })
        .collect();
    let mut superseded_roots: HashMap<RunId, HashSet<NodeId>> = HashMap::new();
    for attempt in attempts {
        let attempt = attempt.borrow();
        if attempt.status != OrchestrationV2RunAttemptStatus::Superseded {
            continue;
        }
        superseded_roots
            .entry(attempt.run_id.clone())
            .or_default()
            .insert(attempt.root_node_id.clone());
    }
    let interrupt_runs = items
        .iter()
        .map(Borrow::borrow)
        .filter(|item| item.item_type() == TurnItemType::RunInterruptRequest)
        .map(|item| item.run_id().cloned())
        .collect();
    TurnItemVisibility {
        statuses,
        superseded_roots,
        interrupt_runs,
    }
}

impl TurnItemVisibility {
    pub fn is_visible(&self, item: &OrchestrationV2TurnItem) -> bool {
        let status = item.run_id().and_then(|run_id| self.statuses.get(run_id));
        if status == Some(&OrchestrationV2RunStatus::RolledBack) {
            return false;
        }
        if status == Some(&OrchestrationV2RunStatus::Cancelled)
            && item.item_type() == TurnItemType::UserMessage
            && input_intent(item) == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
        {
            return false;
        }
        let superseded_interrupt = item.item_type() == TurnItemType::RunInterruptResult
            && match (item.run_id(), item.node_id()) {
                (Some(run_id), Some(node_id)) => {
                    self.superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                        && !self.interrupt_runs.contains(&Some(run_id.clone()))
                }
                _ => false,
            };
        !superseded_interrupt
    }
}
