//! What the composer shows about a live thread beyond its drafts: the
//! context-window meter (`activeContextWindow`) and the plan follow-up prompt
//! (`activeProposedPlan` / `showPlanFollowUpPrompt`) in upstream's ChatView.

use turin_contracts::orchestration_v2::{
    OrchestrationV2PlanArtifact, OrchestrationV2PlanArtifactStatus, OrchestrationV2TurnItem,
};
use turin_shared::proposed_plan_text::proposed_plan_title;

use super::context_window::{
    OrchestrationV2ProviderTurnTokenUsageHolder, derive_latest_context_window_snapshot,
    latest_provider_turn_token_usage,
};
use super::timeline::live_projection::active_run;
use crate::composer::view_model::ContextWindowSnapshot;
use crate::state::thread_projection::ThreadProjection;

/// The newest context usage the thread reports, as the meter shows it.
pub fn context_window(projection: &ThreadProjection) -> Option<ContextWindowSnapshot> {
    let live_usage =
        latest_provider_turn_token_usage(projection.provider_turns.iter().map(|turn| {
            OrchestrationV2ProviderTurnTokenUsageHolder {
                token_usage: turn.token_usage.as_ref(),
            }
        }));
    // Only compactions can supply usage when no provider report exists.
    let compactions: Vec<OrchestrationV2TurnItem> = projection
        .visible_turn_items
        .iter()
        .filter(|row| matches!(*row.item, OrchestrationV2TurnItem::Compaction { .. }))
        .map(|row| (*row.item).clone())
        .collect();
    let active_provider_thread = projection
        .thread
        .active_provider_thread_id
        .as_ref()
        .and_then(|id| projection.provider_threads.iter().find(|t| &t.id == id));
    let snapshot = derive_latest_context_window_snapshot(
        &compactions,
        live_usage,
        active_provider_thread.map(|thread| &**thread),
    )?;
    Some(ContextWindowSnapshot {
        used_tokens: snapshot.used_tokens,
        max_tokens: snapshot.max_tokens,
        total_processed_tokens: snapshot.total_processed_tokens,
        compacts_automatically: snapshot.compacts_automatically.unwrap_or(false),
        auto_compact_threshold: snapshot.auto_compact_threshold,
        cost: snapshot.cost,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestProposedPlan {
    pub plan_id: String,
    pub markdown: String,
    pub status: OrchestrationV2PlanArtifactStatus,
}

/// `findLatestProposedPlan`: the newest proposed plan of the latest run, else
/// of the thread.
pub fn latest_proposed_plan(
    projection: &ThreadProjection,
    latest_run_id: Option<&str>,
) -> Option<LatestProposedPlan> {
    let plans: Vec<_> = projection
        .plans
        .iter()
        .filter_map(|plan| match &**plan {
            OrchestrationV2PlanArtifact::ProposedPlan {
                id,
                run_id,
                status,
                markdown,
                ..
            } => Some((id, run_id.as_ref(), *status, markdown)),
            OrchestrationV2PlanArtifact::TodoList { .. } => None,
        })
        .collect();
    let of_latest_run: Vec<_> = plans
        .iter()
        .filter(|(_, run_id, ..)| {
            latest_run_id.is_some() && run_id.map(|r| r.as_str()) == latest_run_id
        })
        .copied()
        .collect();
    let candidates = if of_latest_run.is_empty() {
        plans
    } else {
        of_latest_run
    };
    // `planItemTime`: the plan item's last update, else the thread's.
    let plan_time = |plan_id: &str| {
        projection
            .turn_items
            .iter()
            .rev()
            .find_map(|item| match &**item {
                OrchestrationV2TurnItem::ProposedPlan {
                    plan_id: item_plan,
                    updated_at,
                    ..
                } if item_plan.as_str() == plan_id => Some(updated_at.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| projection.updated_at.to_string())
    };
    candidates
        .into_iter()
        .max_by(|left, right| {
            plan_time(left.0.as_str())
                .cmp(&plan_time(right.0.as_str()))
                .then_with(|| left.0.as_str().cmp(right.0.as_str()))
        })
        .map(|(id, _, status, markdown)| LatestProposedPlan {
            plan_id: id.to_string(),
            markdown: markdown.clone(),
            status,
        })
}

/// A plan awaiting the user's decision: Implement it or refine it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFollowUp {
    pub plan_id: String,
    pub markdown: String,
    pub title: Option<String>,
}

/// `shouldShowPlanFollowUpPrompt` over the thread (composer attachments are
/// checked by the composer, which owns them).
pub fn plan_follow_up(
    projection: &ThreadProjection,
    plan_mode: bool,
    pending_user_input_count: usize,
) -> Option<PlanFollowUp> {
    // `isLatestRunSettled`: nothing is executing and a run exists.
    let latest_run = projection.runs.iter().max_by_key(|run| run.ordinal)?;
    if active_run(&projection.runs).is_some() {
        return None;
    }
    let plan = latest_proposed_plan(projection, Some(latest_run.id.as_str()))?;
    let shown = crate::components::chat_view_logic::should_show_plan_follow_up_prompt(
        crate::components::chat_view_logic::PlanFollowUpInput {
            pending_user_input_count,
            interaction_mode: if plan_mode {
                turin_contracts::provider_policy::ProviderInteractionMode::Plan
            } else {
                turin_contracts::provider_policy::ProviderInteractionMode::Default
            },
            latest_turn_settled: true,
            has_actionable_proposed_plan: plan.status == OrchestrationV2PlanArtifactStatus::Active,
            has_composer_attachments: false,
        },
    );
    shown.then(|| PlanFollowUp {
        title: proposed_plan_title(&plan.markdown),
        plan_id: plan.plan_id,
        markdown: plan.markdown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_projection, v2_run};
    use serde_json::json;
    use std::sync::Arc;

    fn plan(id: &str, run: &str, status: &str, markdown: &str) -> Arc<OrchestrationV2PlanArtifact> {
        Arc::new(from_json(json!({
            "kind": "proposed_plan", "id": id, "threadId": "thread-v2", "runId": run,
            "nodeId": "node-1", "status": status, "markdown": markdown,
        })))
    }

    fn projection(status: &str, plans: Vec<Arc<OrchestrationV2PlanArtifact>>) -> ThreadProjection {
        let mut contract = v2_projection();
        contract.runs = vec![v2_run("run-1", 1, status)];
        let mut projection = ThreadProjection::from(contract);
        projection.plans = Arc::new(plans);
        projection
    }

    #[test]
    fn a_settled_plan_turn_offers_implement_and_refine() {
        let projection = projection(
            "completed",
            vec![plan("plan-1", "run-1", "active", "# Ship it\n\n- step 1")],
        );
        let follow_up = plan_follow_up(&projection, true, 0).unwrap();
        assert_eq!(follow_up.plan_id, "plan-1");
        assert_eq!(follow_up.title.as_deref(), Some("Ship it"));
    }

    #[test]
    fn no_follow_up_while_running_or_in_build_mode_or_with_a_question_open() {
        let plans = vec![plan("plan-1", "run-1", "active", "# Ship it")];
        assert!(plan_follow_up(&projection("running", plans.clone()), true, 0).is_none());
        let settled = projection("completed", plans);
        assert!(plan_follow_up(&settled, false, 0).is_none());
        assert!(plan_follow_up(&settled, true, 1).is_none());
    }

    #[test]
    fn a_completed_or_superseded_plan_is_not_actionable() {
        for status in ["completed", "superseded", "draft"] {
            let projection = projection(
                "completed",
                vec![plan("plan-1", "run-1", status, "# Ship it")],
            );
            assert!(plan_follow_up(&projection, true, 0).is_none(), "{status}");
        }
    }

    #[test]
    fn the_plan_of_the_latest_run_wins() {
        let projection = projection(
            "completed",
            vec![
                plan("plan-a", "run-0", "active", "# Old"),
                plan("plan-b", "run-1", "active", "# New"),
            ],
        );
        assert_eq!(
            latest_proposed_plan(&projection, Some("run-1"))
                .unwrap()
                .plan_id,
            "plan-b"
        );
    }

    #[test]
    fn the_providers_latest_usage_report_drives_the_meter() {
        let mut projection = projection("completed", Vec::new());
        let turn = |id: &str, used: u64, at: &str| {
            Arc::new(from_json(json!({
                "id": id, "providerThreadId": "provider-thread:1", "nodeId": "node-1",
                "runAttemptId": null, "nativeTurnRef": null, "ordinal": 1,
                "status": "completed", "startedAt": null, "completedAt": null,
                "tokenUsage": {"usedTokens": used, "maxTokens": 200000, "updatedAt": at},
            })))
        };
        projection.provider_turns = Arc::new(vec![
            turn("turn-new", 50_000, "2026-06-20T00:02:00.000Z"),
            turn("turn-old", 10_000, "2026-06-20T00:01:00.000Z"),
        ]);
        let meter = context_window(&projection).unwrap();
        assert_eq!(meter.used_tokens, 50_000.);
        assert_eq!(meter.max_tokens, Some(200_000.));
        assert_eq!(meter.used_percentage(), Some(25.));
    }

    #[test]
    fn a_thread_without_usage_has_no_meter() {
        let projection = projection("completed", Vec::new());
        assert!(context_window(&projection).is_none());
    }
}
