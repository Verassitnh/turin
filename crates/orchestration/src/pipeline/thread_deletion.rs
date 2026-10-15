//! Port of upstream/apps/server/src/orchestration-v2/ThreadDeletion.ts
//!
//! Plans the same durable cleanup for direct thread deletion and project
//! removal: cancel the thread's active work, dispose delegated completion
//! cohorts, detach live provider sessions, and queue terminal / preview /
//! attachment cleanup effects.
//!
//! Events are built as JSON and decoded into `OrchestrationV2DomainEvent`
//! (the envelope fields differ per event; the payloads are the typed
//! entities).

use super::effect_outbox::{EffectRequest, PendingEffect};
use serde_json::{Value, json};
use std::collections::HashSet;
use turin_contracts::orchestration_v2::{
    OrchestrationV2DelegatedCompletionCohort, OrchestrationV2DelegatedCompletionCohortDisposition,
    OrchestrationV2DelegatedCompletionTaskDelivery,
    OrchestrationV2DelegatedCompletionTaskDeliveryState, OrchestrationV2DomainEvent,
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderSessionStatus,
    OrchestrationV2ResponseCapability, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
    OrchestrationV2RuntimeRequestStatus, OrchestrationV2SubagentOrigin,
    OrchestrationV2ThreadProjection,
};
use turin_contracts::{CommandId, PositiveInt, ThreadId, UtcDateTime};
use turin_provider_core::id_allocator::IdAllocator;

#[derive(Debug, Clone)]
pub struct ThreadDeletionPlan {
    pub events: Vec<OrchestrationV2DomainEvent>,
    pub effects: Vec<PendingEffect>,
}

#[derive(Debug, thiserror::Error)]
#[error("Could not build the deletion event {kind}: {source}")]
pub struct ThreadDeletionError {
    kind: &'static str,
    #[source]
    source: serde_json::Error,
}

struct Emitter<'a> {
    ids: &'a IdAllocator,
    thread_id: &'a ThreadId,
    command_id: &'a CommandId,
    now: String,
    events: Vec<OrchestrationV2DomainEvent>,
}

impl Emitter<'_> {
    /// `envelope` carries the per-event optional fields (`runId`, `nodeId`,
    /// `driver`, `providerInstanceId`).
    fn emit(
        &mut self,
        kind: &'static str,
        envelope: Value,
        payload: Value,
    ) -> Result<(), ThreadDeletionError> {
        let id = self
            .ids
            .event(Some(self.thread_id), Some(self.command_id), None);
        let mut event = json!({
            "type": kind,
            "id": id,
            "threadId": self.thread_id,
            "occurredAt": self.now,
            "payload": payload,
        });
        if let (Some(target), Some(extra)) = (event.as_object_mut(), envelope.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        let event =
            serde_json::from_value(event).map_err(|source| ThreadDeletionError { kind, source })?;
        self.events.push(event);
        Ok(())
    }
}

fn with_fields<T: serde::Serialize>(value: &T, fields: Value) -> Value {
    let mut base = serde_json::to_value(value).unwrap_or(Value::Null);
    if let (Some(target), Some(extra)) = (base.as_object_mut(), fields.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    base
}

pub fn plan_thread_deletion(
    command_id: &CommandId,
    thread_id: &ThreadId,
    projection: &OrchestrationV2ThreadProjection,
    attachment_ids: &[String],
    now: UtcDateTime,
    ids: &IdAllocator,
) -> Result<ThreadDeletionPlan, ThreadDeletionError> {
    let now_text = now.to_string();
    let mut out = Emitter {
        ids,
        thread_id,
        command_id,
        now: now_text.clone(),
        events: vec![],
    };
    let mut effects = Vec::new();
    // The projection as the emitted events leave it, so a cohort disposed
    // after its run was cancelled cannot revive that run.
    let mut runs = projection.runs.clone();
    let mut subagents = projection.subagents.clone();

    let deleted_at = projection.thread.deleted_at.unwrap_or(now);
    out.emit(
        "thread.deleted",
        json!({ "providerInstanceId": projection.thread.provider_instance_id }),
        with_fields(
            &projection.thread,
            json!({
                "deletedAt": deleted_at,
                "titleRegeneration": null,
                "updatedAt": now,
            }),
        ),
    )?;

    let is_active = |status: OrchestrationV2RunStatus| {
        matches!(
            status,
            OrchestrationV2RunStatus::Preparing
                | OrchestrationV2RunStatus::Queued
                | OrchestrationV2RunStatus::Starting
                | OrchestrationV2RunStatus::Running
                | OrchestrationV2RunStatus::Waiting
        )
    };
    let active_runs: Vec<_> = projection
        .runs
        .iter()
        .filter(|run| is_active(run.status))
        .cloned()
        .collect();
    for run in &active_runs {
        let mut cancelled = run.clone();
        cancelled.status = OrchestrationV2RunStatus::Cancelled;
        cancelled.queue_position = Some(None);
        cancelled.completed_at = Some(now);
        out.emit(
            "run.updated",
            json!({ "runId": run.id, "providerInstanceId": run.provider_instance_id }),
            serde_json::to_value(&cancelled).unwrap_or(Value::Null),
        )?;
        if let Some(slot) = runs.iter_mut().find(|candidate| candidate.id == run.id) {
            *slot = cancelled;
        }
    }
    let active_by_id =
        |run_id: &turin_contracts::RunId| active_runs.iter().find(|r| &r.id == run_id);

    for attempt in &projection.attempts {
        let Some(run) = active_by_id(&attempt.run_id) else {
            continue;
        };
        if !matches!(
            attempt.status,
            OrchestrationV2RunAttemptStatus::Pending | OrchestrationV2RunAttemptStatus::Running
        ) {
            continue;
        }
        out.emit(
            "run-attempt.updated",
            json!({
                "runId": attempt.run_id,
                "nodeId": attempt.root_node_id,
                "providerInstanceId": run.provider_instance_id,
            }),
            with_fields(
                attempt,
                json!({ "status": "cancelled", "completedAt": now }),
            ),
        )?;
    }
    for node in &projection.nodes {
        let Some(run) = node.run_id.as_ref().and_then(active_by_id) else {
            continue;
        };
        if !matches!(
            node.status,
            OrchestrationV2ExecutionNodeStatus::Pending
                | OrchestrationV2ExecutionNodeStatus::Running
                | OrchestrationV2ExecutionNodeStatus::Waiting
        ) {
            continue;
        }
        out.emit(
            "node.updated",
            json!({
                "runId": run.id,
                "nodeId": node.id,
                "providerInstanceId": run.provider_instance_id,
            }),
            with_fields(node, json!({ "status": "cancelled", "completedAt": now })),
        )?;
    }
    for request in projection
        .runtime_requests
        .iter()
        .filter(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
    {
        out.emit(
            "runtime-request.updated",
            json!({ "nodeId": request.node_id }),
            with_fields(
                request,
                json!({
                    "status": "cancelled",
                    "responseCapability": serde_json::to_value(
                        OrchestrationV2ResponseCapability::NotResumable {
                            reason: "The thread was deleted.".to_owned(),
                        }
                    ).unwrap_or(Value::Null),
                    "resolvedAt": now,
                }),
            ),
        )?;
    }

    // Cohorts: runs that carry delegated completion, plus the runs of
    // app-owned tasks.
    let mut cohort_run_ids = Vec::new();
    for run in runs.iter().filter(|run| run.delegated_completion.is_some()) {
        cohort_run_ids.push(run.id.clone());
    }
    for task in subagents
        .iter()
        .filter(|task| task.origin == OrchestrationV2SubagentOrigin::AppOwned)
    {
        if let Some(run_id) = &task.run_id {
            cohort_run_ids.push(run_id.clone());
        }
    }
    let mut seen = HashSet::new();
    cohort_run_ids.retain(|id| seen.insert(id.clone()));
    for parent_run_id in cohort_run_ids {
        let Some(parent_run) = runs.iter().find(|run| run.id == parent_run_id).cloned() else {
            continue;
        };
        let next_generation = parent_run
            .delegated_completion
            .as_ref()
            .map_or(PositiveInt::new(1).expect("one"), |cohort| {
                cohort.next_generation
            });
        let mut disposed = parent_run.clone();
        disposed.delegated_completion = Some(OrchestrationV2DelegatedCompletionCohort {
            disposition: OrchestrationV2DelegatedCompletionCohortDisposition::Disposed,
            next_generation,
            delivery: None,
        });
        let mut envelope = json!({
            "runId": parent_run.id,
            "providerInstanceId": parent_run.provider_instance_id,
        });
        if let Some(root) = &parent_run.root_node_id {
            envelope["nodeId"] = json!(root);
        }
        out.emit(
            "run.updated",
            envelope,
            serde_json::to_value(&disposed).unwrap_or(Value::Null),
        )?;
        if let Some(slot) = runs.iter_mut().find(|run| run.id == parent_run.id) {
            *slot = disposed;
        }
        let tasks: Vec<_> = subagents
            .iter()
            .filter(|task| {
                task.origin == OrchestrationV2SubagentOrigin::AppOwned
                    && task.run_id.as_ref() == Some(&parent_run_id)
            })
            .cloned()
            .collect();
        for task in tasks {
            if task.completion_delivery.as_ref().is_some_and(|delivery| {
                matches!(
                    delivery.state,
                    OrchestrationV2DelegatedCompletionTaskDeliveryState::Acknowledged
                        | OrchestrationV2DelegatedCompletionTaskDeliveryState::Delivered
                        | OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed
                )
            }) {
                continue;
            }
            let mut updated = task.clone();
            updated.completion_delivery = Some(OrchestrationV2DelegatedCompletionTaskDelivery {
                state: OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed,
                observed_by_run_id: None,
            });
            updated.updated_at = now;
            let mut envelope = json!({
                "nodeId": task.id,
                "driver": task.driver,
                "providerInstanceId": task.provider_instance_id,
            });
            if let Some(run_id) = &task.run_id {
                envelope["runId"] = json!(run_id);
            }
            out.emit(
                "subagent.updated",
                envelope,
                serde_json::to_value(&updated).unwrap_or(Value::Null),
            )?;
            if let Some(slot) = subagents
                .iter_mut()
                .find(|candidate| candidate.id == task.id)
            {
                *slot = updated;
            }
        }
    }

    for session in &projection.provider_sessions {
        if matches!(
            session.status,
            OrchestrationV2ProviderSessionStatus::Stopped
                | OrchestrationV2ProviderSessionStatus::Error
        ) {
            continue;
        }
        out.emit(
            "provider-session.detached",
            json!({
                "driver": session.driver,
                "providerInstanceId": session.provider_instance_id,
            }),
            json!({
                "providerSessionId": session.id,
                "detachedAt": now,
                "reason": "Thread deleted.",
            }),
        )?;
        effects.push(PendingEffect {
            id: format!("effect:{command_id}:provider-session.detach:{}", session.id),
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
            request: EffectRequest::ProviderSessionDetach {
                provider_session_id: session.id.clone(),
                detail: Some("Thread deleted.".to_owned()),
                revoke_mcp_credential: Some(true),
            },
            available_at: None,
        });
    }
    let cleanup = |suffix: &str, request: EffectRequest| PendingEffect {
        id: format!("effect:{command_id}:{suffix}"),
        command_id: command_id.clone(),
        thread_id: thread_id.clone(),
        request,
        available_at: None,
    };
    effects.push(cleanup(
        "terminal.cleanup",
        EffectRequest::TerminalCleanup {},
    ));
    effects.push(cleanup("preview.cleanup", EffectRequest::PreviewCleanup {}));
    let mut seen = HashSet::new();
    let attachment_ids: Vec<String> = attachment_ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    if !attachment_ids.is_empty() {
        effects.push(cleanup(
            "attachment.cleanup",
            EffectRequest::AttachmentCleanup { attachment_ids },
        ));
    }
    Ok(ThreadDeletionPlan {
        events: out.events,
        effects,
    })
}

#[cfg(test)]
mod tests;
