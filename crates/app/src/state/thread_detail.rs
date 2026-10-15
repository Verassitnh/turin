//! Port of upstream/packages/client-runtime/src/state/threadDetail.ts
//!
//! The per-thread atom family becomes [`ThreadDetailView`]: every selector the
//! thread view and composer read for one thread, recomputed only when the
//! projection collections it depends on changed identity (upstream's
//! `previous !== projection.x` checks).

use std::sync::Arc;

use crate::state::contracts::{OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItem};
use crate::state::thread_checkpoints::{
    ThreadCheckpointSummary, derive_thread_checkpoint_summaries,
};
use crate::state::thread_execution::{
    ProviderSubagentStatus, ThreadRunDetail, ThreadRuntimeDetail, derive_latest_thread_run,
    derive_provider_subagent_status, derive_runless_work_started_at, derive_thread_activity_run,
    derive_thread_runtime,
};
use crate::state::thread_projection::{Rows, ThreadProjection, VisibleTurnItemRow};
use crate::state::thread_requests::{
    PendingThreadRequests, QuestionHistoryProjector, derive_pending_thread_requests,
};
use crate::state::thread_subagents::{ThreadTurnSubagents, derive_thread_turn_subagents};
use crate::state::thread_workflows::{
    ThreadQueueWorkflowState, derive_thread_queue_workflow_state, get_user_queued_thread_runs,
};

/// Everything derived from one thread projection.
#[derive(Clone, Debug)]
pub struct ThreadDetailView {
    pub projection: Arc<ThreadProjection>,
    /// `visibleTurnItemsAtom`: rows with restored question answers.
    pub visible_turn_items: Arc<Vec<VisibleTurnItemRow>>,
    pub worktree_path: Option<String>,
    pub pending_requests: Arc<PendingThreadRequests>,
    pub queue_workflow: Arc<ThreadQueueWorkflowState>,
    pub queued_count: usize,
    pub turn_subagents: Option<Arc<ThreadTurnSubagents>>,
    pub latest_run: Option<Arc<ThreadRunDetail>>,
    pub activity_run: Option<Arc<ThreadRunDetail>>,
    pub runtime: Option<Arc<ThreadRuntimeDetail>>,
    pub runless_work_started_at: Option<String>,
    pub provider_subagent_status: Option<ProviderSubagentStatus>,
    pub checkpoints: Arc<Vec<ThreadCheckpointSummary>>,
}

/// The turn items a pending request resolves display data from, in order.
struct PendingInputs {
    runtime_requests: Vec<Arc<crate::state::contracts::OrchestrationV2RuntimeRequest>>,
    turn_items: Vec<Arc<OrchestrationV2TurnItem>>,
}

fn same_arcs<T>(left: &[Arc<T>], right: &[Arc<T>]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| Arc::ptr_eq(a, b))
}

fn same_rows<T>(left: &Rows<T>, right: &Rows<T>) -> bool {
    Arc::ptr_eq(left, right)
}

/// Memoizes [`ThreadDetailView`]s for one thread across projection changes.
#[derive(Default)]
pub struct ThreadDetailSelectors {
    question_history: QuestionHistoryProjector,
    previous: Option<ThreadDetailView>,
    pending_inputs: Option<PendingInputs>,
}

impl ThreadDetailSelectors {
    pub fn new() -> Self {
        Self::default()
    }

    /// The view for `projection`, reusing every part whose inputs did not change.
    pub fn select(&mut self, projection: &Arc<ThreadProjection>) -> ThreadDetailView {
        let previous = self.previous.take();
        let previous_projection = previous.as_ref().map(|view| &view.projection);
        if let Some(previous) = &previous
            && Arc::ptr_eq(&previous.projection, projection)
        {
            let view = previous.clone();
            self.previous = Some(previous.clone());
            return view;
        }
        let changed = |same: &dyn Fn(&ThreadProjection) -> bool| {
            previous_projection.is_none_or(|previous| !same(previous))
        };

        let visible_turn_items = self
            .question_history
            .project(&projection.visible_turn_items, &projection.runtime_requests);

        let pending_requests = {
            let runtime_requests: Vec<_> = projection
                .runtime_requests
                .iter()
                .filter(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
                .cloned()
                .collect();
            let ids: std::collections::HashSet<_> = runtime_requests
                .iter()
                .map(|request| request.id.clone())
                .collect();
            let turn_items: Vec<_> = if ids.is_empty() {
                Vec::new()
            } else {
                projection
                    .turn_items
                    .iter()
                    .filter(|item| match &***item {
                        OrchestrationV2TurnItem::ApprovalRequest { request_id, .. }
                        | OrchestrationV2TurnItem::UserInputRequest { request_id, .. } => {
                            ids.contains(request_id)
                        }
                        _ => false,
                    })
                    .cloned()
                    .collect()
            };
            // Streaming output on unrelated items keeps the same pending set.
            match (&self.pending_inputs, previous.as_ref()) {
                (Some(inputs), Some(previous))
                    if same_arcs(&inputs.runtime_requests, &runtime_requests)
                        && same_arcs(&inputs.turn_items, &turn_items) =>
                {
                    Arc::clone(&previous.pending_requests)
                }
                _ => {
                    let derived = derive_pending_thread_requests(&runtime_requests, &turn_items);
                    self.pending_inputs = Some(PendingInputs {
                        runtime_requests,
                        turn_items,
                    });
                    Arc::new(derived)
                }
            }
        };

        let queue_inputs_changed = changed(&|previous| {
            Arc::ptr_eq(&previous.thread, &projection.thread)
                && same_rows(&previous.runs, &projection.runs)
                && same_rows(&previous.messages, &projection.messages)
                && same_rows(&previous.provider_threads, &projection.provider_threads)
                && same_rows(&previous.provider_sessions, &projection.provider_sessions)
                && same_rows(&previous.provider_turns, &projection.provider_turns)
        });
        let queue_workflow = match (&previous, queue_inputs_changed) {
            (Some(previous), false) => Arc::clone(&previous.queue_workflow),
            _ => Arc::new(derive_thread_queue_workflow_state(projection)),
        };
        let queued_count = match (
            &previous,
            changed(&|previous| {
                same_rows(&previous.runs, &projection.runs)
                    && same_rows(&previous.messages, &projection.messages)
            }),
        ) {
            (Some(previous), false) => previous.queued_count,
            _ => get_user_queued_thread_runs(&projection.runs, &projection.messages).len(),
        };
        let turn_subagents = match (
            &previous,
            changed(&|previous| {
                same_rows(&previous.runs, &projection.runs)
                    && same_rows(&previous.subagents, &projection.subagents)
            }),
        ) {
            (Some(previous), false) => previous.turn_subagents.clone(),
            _ => {
                derive_thread_turn_subagents(&projection.runs, &projection.subagents).map(Arc::new)
            }
        };

        // Execution state reads many collections and is cheap next to a render.
        let latest_run = derive_latest_thread_run(projection).map(Arc::new);
        let activity_run = derive_thread_activity_run(projection).map(Arc::new);
        let runtime = derive_thread_runtime(projection).map(Arc::new);
        let latest_run = reuse(
            previous.as_ref().and_then(|v| v.latest_run.clone()),
            latest_run,
        );
        let activity_run = reuse(
            previous.as_ref().and_then(|v| v.activity_run.clone()),
            activity_run,
        );
        let runtime = reuse(previous.as_ref().and_then(|v| v.runtime.clone()), runtime);

        let checkpoints = match (
            &previous,
            changed(&|previous| {
                same_rows(&previous.checkpoints, &projection.checkpoints)
                    && same_rows(&previous.messages, &projection.messages)
            }),
        ) {
            (Some(previous), false) => Arc::clone(&previous.checkpoints),
            _ => Arc::new(derive_thread_checkpoint_summaries(projection)),
        };

        let view = ThreadDetailView {
            projection: Arc::clone(projection),
            visible_turn_items,
            worktree_path: projection
                .thread
                .worktree_path
                .as_ref()
                .map(|path| path.as_str().to_owned()),
            pending_requests,
            queue_workflow,
            queued_count,
            turn_subagents,
            latest_run,
            activity_run,
            runtime,
            runless_work_started_at: derive_runless_work_started_at(projection),
            provider_subagent_status: derive_provider_subagent_status(projection),
            checkpoints,
        };
        self.previous = Some(view.clone());
        view
    }
}

fn reuse<T: PartialEq>(previous: Option<Arc<T>>, next: Option<Arc<T>>) -> Option<Arc<T>> {
    match (previous, next) {
        (Some(previous), Some(next)) if *previous == *next => Some(previous),
        (_, next) => next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::orchestration_v2_projection::apply_orchestration_v2_projection_event;
    use crate::state::test_fixtures::{from_json, v2_command_item, v2_projection, v2_run};
    use serde_json::json;

    fn projection() -> Arc<ThreadProjection> {
        let mut contract = v2_projection();
        contract.runs = vec![v2_run("run-1", 1, "running")];
        contract.turn_items = vec![v2_command_item("item-1", "run-1", "x", 1)];
        Arc::new(contract.into())
    }

    fn stream(projection: &Arc<ThreadProjection>, output: &str) -> Arc<ThreadProjection> {
        let item = v2_command_item("item-1", "run-1", output, 1);
        let event = from_json(json!({
            "id": "e", "type": "turn-item.updated", "threadId": "thread-v2",
            "occurredAt": "2026-06-20T00:00:00.000Z", "payload": item,
        }));
        Arc::new(apply_orchestration_v2_projection_event(projection, event, None).unwrap())
    }

    #[test]
    fn derives_the_views_a_running_thread_needs() {
        let mut selectors = ThreadDetailSelectors::new();
        let view = selectors.select(&projection());
        assert_eq!(
            view.runtime.as_ref().unwrap().status,
            crate::state::models::RunStatus::Running
        );
        assert!(view.pending_requests.approvals.is_empty());
        assert_eq!(view.queued_count, 0);
        assert!(view.turn_subagents.is_none());
        assert_eq!(view.worktree_path, None);
    }

    #[test]
    fn streaming_output_keeps_every_unrelated_derivation() {
        let mut selectors = ThreadDetailSelectors::new();
        let first = projection();
        let a = selectors.select(&first);
        let b = selectors.select(&stream(&first, "more output"));
        // Different projection, but nothing the selectors depend on besides turn items changed.
        assert!(!Arc::ptr_eq(&a.projection, &b.projection));
        assert!(Arc::ptr_eq(&a.pending_requests, &b.pending_requests));
        assert!(Arc::ptr_eq(&a.queue_workflow, &b.queue_workflow));
        assert!(Arc::ptr_eq(
            a.runtime.as_ref().unwrap(),
            b.runtime.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&a.checkpoints, &b.checkpoints));
        // Visible rows reflect the stream.
        assert!(!Arc::ptr_eq(&a.visible_turn_items, &b.visible_turn_items));
    }

    #[test]
    fn the_same_projection_returns_the_same_view() {
        let mut selectors = ThreadDetailSelectors::new();
        let projection = projection();
        let a = selectors.select(&projection);
        let b = selectors.select(&projection);
        assert!(Arc::ptr_eq(&a.queue_workflow, &b.queue_workflow));
        assert!(Arc::ptr_eq(&a.visible_turn_items, &b.visible_turn_items));
    }
}
