//! Port of upstream/apps/server/src/orchestration-v2/ThreadForkService.ts

use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2AppThreadLineage,
    OrchestrationV2ContextSourcePoint, OrchestrationV2ContextTransfer,
    OrchestrationV2ContextTransferStatus, OrchestrationV2ContextTransferType,
    OrchestrationV2CreationSource, OrchestrationV2ForkSource, OrchestrationV2LineageRelationship,
    OrchestrationV2NativeRefStrength, OrchestrationV2ProviderThread, OrchestrationV2Run,
    OrchestrationV2RunStatus,
};
use turin_contracts::{ContextTransferId, ThreadId, UtcDateTime};

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadForkPlan {
    pub target_thread: OrchestrationV2AppThread,
    pub transfer: OrchestrationV2ContextTransfer,
}

/// `ThreadForkPlanError`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Failed to plan a fork of {source_thread_id} into {target_thread_id}: {cause}")]
pub struct ThreadForkPlanError {
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub cause: String,
}

/// Fork copies a provider-finished conversation. Usage-limited and other
/// failed, interrupted, or cancelled turns still have a native thread (or a
/// portable transcript) even though the run did not complete successfully.
/// `waiting` is provider-finished with checkpoint capture still pending.
/// In-progress and rolled-back runs are not forkable.
pub fn is_forkable_source_run_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Waiting
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::Cancelled
    )
}

pub fn forkable_source_run_status_error(run: &OrchestrationV2Run) -> String {
    let status = serde_json::to_value(run.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!(
        "Fork source run {} is {status}; in-progress and rolled-back runs cannot be forked.",
        run.id
    )
}

pub struct ForkPlanInput<'a> {
    pub source_thread: &'a OrchestrationV2AppThread,
    pub source_run: &'a OrchestrationV2Run,
    pub source_provider_thread: Option<&'a OrchestrationV2ProviderThread>,
    pub canonical_source_point: OrchestrationV2ContextSourcePoint,
    pub transfer_id: ContextTransferId,
    pub target_thread_id: ThreadId,
    pub title: Option<String>,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    pub created_at: UtcDateTime,
}

pub fn plan(input: ForkPlanInput<'_>) -> Result<ThreadForkPlan, ThreadForkPlanError> {
    let source = input.source_thread;
    if !is_forkable_source_run_status(input.source_run.status) {
        return Err(ThreadForkPlanError {
            source_thread_id: source.id.clone(),
            target_thread_id: input.target_thread_id,
            cause: forkable_source_run_status_error(input.source_run),
        });
    }
    let title = input
        .title
        .and_then(|title| turin_contracts::TrimmedNonEmptyString::new(title).ok())
        .or_else(|| {
            turin_contracts::TrimmedNonEmptyString::new(format!("{} fork", source.title)).ok()
        })
        .unwrap_or_else(|| source.title.clone());
    let target_thread = OrchestrationV2AppThread {
        created_by: input.created_by,
        creation_source: input.creation_source,
        id: input.target_thread_id.clone(),
        title,
        active_provider_thread_id: None,
        lineage: OrchestrationV2AppThreadLineage {
            parent_thread_id: Some(source.id.clone()),
            relationship_to_parent: Some(OrchestrationV2LineageRelationship::Fork),
            root_thread_id: source.lineage.root_thread_id.clone(),
        },
        forked_from: Some(OrchestrationV2ForkSource::Run {
            thread_id: source.id.clone(),
            run_id: input.source_run.id.clone(),
        }),
        created_at: input.created_at,
        updated_at: input.created_at,
        archived_at: None,
        settled_override: None,
        settled_at: None,
        // A fork starts awake even when its source is snoozed.
        snoozed_until: Some(None),
        snoozed_at: Some(None),
        last_visited_at: None,
        deleted_at: None,
        ..source.clone()
    };
    let strong = input
        .source_provider_thread
        .and_then(|thread| thread.native_thread_ref.as_ref())
        .is_some_and(|native| native.strength == OrchestrationV2NativeRefStrength::Strong);
    let transfer = OrchestrationV2ContextTransfer {
        id: input.transfer_id,
        r#type: OrchestrationV2ContextTransferType::Fork,
        source_thread_id: source.id.clone(),
        target_thread_id: input.target_thread_id,
        source_point: input.canonical_source_point,
        base_point: None,
        source_provider_instance_id: Some(input.source_run.provider_instance_id.clone()),
        target_provider_instance_id: None,
        target_run_id: None,
        status: OrchestrationV2ContextTransferStatus::Pending,
        resolution: None,
        created_by: input.created_by,
        error: (!strong).then(|| {
            "Source provider thread does not expose a strong native thread ref.".to_owned()
        }),
        created_at: input.created_at,
        updated_at: input.created_at,
        consumed_at: None,
    };
    Ok(ThreadForkPlan {
        target_thread,
        transfer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::{projection, run};
    use serde_json::json;

    const SOURCE: &str = "thread:fork-snoozed-source";
    const TARGET: &str = "thread:fork-awake-target";

    fn source_thread() -> OrchestrationV2AppThread {
        let mut thread = projection(SOURCE, "project:fork-snooze", json!({})).thread;
        thread.title = turin_contracts::TrimmedNonEmptyString::new("Snoozed source").unwrap();
        thread.interaction_mode = turin_contracts::provider_policy::ProviderInteractionMode::Plan;
        thread.branch =
            Some(turin_contracts::TrimmedNonEmptyString::new("feature/source").unwrap());
        thread.worktree_path =
            Some(turin_contracts::TrimmedNonEmptyString::new("/tmp/source-worktree").unwrap());
        thread.active_provider_thread_id = Some(
            turin_contracts::ProviderThreadId::new("provider-thread:fork-snoozed-source").unwrap(),
        );
        thread.snoozed_until = Some(Some("2026-07-25T09:00:00.000Z".parse().unwrap()));
        thread.snoozed_at = Some(Some("2026-07-24T09:05:00.000Z".parse().unwrap()));
        thread
    }

    fn plan_fork(status: &str) -> Result<ThreadForkPlan, ThreadForkPlanError> {
        let source = source_thread();
        let source_run = run("run:fork-snoozed-source", SOURCE, 1, status);
        plan(ForkPlanInput {
            source_thread: &source,
            source_run: &source_run,
            source_provider_thread: None,
            canonical_source_point: OrchestrationV2ContextSourcePoint {
                thread_id: ThreadId::new(SOURCE).unwrap(),
                run_id: Some(source_run.id.clone()),
                ..serde_json::from_value(json!({"threadId": SOURCE})).unwrap()
            },
            transfer_id: ContextTransferId::new("context-transfer:fork-snoozed-source").unwrap(),
            target_thread_id: ThreadId::new(TARGET).unwrap(),
            title: Some("Awake fork".into()),
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Mobile,
            created_at: "2026-07-24T09:10:00.000Z".parse().unwrap(),
        })
    }

    #[test]
    fn treats_usage_limited_and_other_provider_finished_runs_as_forkable() {
        use OrchestrationV2RunStatus::*;
        for status in [Completed, Waiting, Failed, Interrupted, Cancelled] {
            assert!(is_forkable_source_run_status(status), "{status:?}");
        }
        for status in [Running, Starting, Queued, Preparing, RolledBack] {
            assert!(!is_forkable_source_run_status(status), "{status:?}");
        }
    }

    #[test]
    fn keeps_a_fork_awake_when_its_source_thread_is_snoozed() {
        let source = source_thread();
        let result = plan_fork("completed").unwrap();
        let target = &result.target_thread;
        assert_eq!(target.snoozed_until, Some(None));
        assert_eq!(target.snoozed_at, Some(None));
        assert_eq!(target.project_id, source.project_id);
        assert_eq!(target.provider_instance_id, source.provider_instance_id);
        assert_eq!(target.model_selection, source.model_selection);
        assert_eq!(target.runtime_mode, source.runtime_mode);
        assert_eq!(target.interaction_mode, source.interaction_mode);
        assert_eq!(target.branch, source.branch);
        assert_eq!(target.worktree_path, source.worktree_path);
        assert_eq!(target.active_provider_thread_id, None);
        assert_eq!(
            serde_json::to_value(&target.lineage).unwrap(),
            json!({"parentThreadId": SOURCE, "relationshipToParent": "fork", "rootThreadId": SOURCE})
        );
        assert_eq!(
            serde_json::to_value(&target.forked_from).unwrap(),
            json!({"type": "run", "threadId": SOURCE, "runId": "run:fork-snoozed-source"})
        );
        assert_eq!(target.title.as_str(), "Awake fork");
        // No strong native ref was supplied, so the transfer records why.
        assert_eq!(
            result.transfer.error.as_deref(),
            Some("Source provider thread does not expose a strong native thread ref.")
        );
        assert_eq!(
            result.transfer.status,
            OrchestrationV2ContextTransferStatus::Pending
        );
    }

    #[test]
    fn forks_from_a_usage_limited_failed_run() {
        let result = plan_fork("failed").unwrap();
        assert!(matches!(
            result.target_thread.forked_from,
            Some(OrchestrationV2ForkSource::Run { .. })
        ));
    }

    #[test]
    fn rejects_in_progress_and_rolled_back_fork_sources() {
        for status in ["running", "rolled_back"] {
            let error = plan_fork(status).unwrap_err();
            assert_eq!(error.source_thread_id.as_str(), SOURCE);
            assert_eq!(error.target_thread_id.as_str(), TARGET);
            assert_eq!(
                error.cause,
                forkable_source_run_status_error(&run(
                    "run:fork-snoozed-source",
                    SOURCE,
                    1,
                    status
                ))
            );
        }
    }

    #[test]
    fn an_untitled_fork_is_named_after_its_source() {
        let source = source_thread();
        let source_run = run("run:r", SOURCE, 1, "completed");
        let result = plan(ForkPlanInput {
            source_thread: &source,
            source_run: &source_run,
            source_provider_thread: None,
            canonical_source_point: serde_json::from_value(json!({"threadId": SOURCE})).unwrap(),
            transfer_id: ContextTransferId::new("t").unwrap(),
            target_thread_id: ThreadId::new(TARGET).unwrap(),
            title: None,
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Web,
            created_at: UtcDateTime::now(),
        })
        .unwrap();
        assert_eq!(result.target_thread.title.as_str(), "Snoozed source fork");
    }
}
