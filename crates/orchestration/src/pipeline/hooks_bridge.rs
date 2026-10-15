//! Turin-only (no upstream counterpart): wires `turin-hooks` into the write
//! pipeline. Replaces upstream's `ThreadLaunchService` (launch into a new
//! worktree) per PORTING.md "Hooks (new)".
//!
//! * `thread.created` runs **before** the `thread.create` command commits, in
//!   [`super::thread_management_service::ThreadManagementService::dispatch`]:
//!   a blocking hook may print `{"worktreePath","branch"}`, which
//!   `DispatchSummary::thread_workspace` returns, and the command is
//!   dispatched with that workspace bound. A command id that already has a
//!   receipt does not run hooks again.
//! * `turn.started`, `turn.completed`, `turn.failed`, `approval.requested`
//!   and `pr.linked` are derived from committed events by
//!   [`HookEventBridge`], which follows `OrchestratorPort::stream_domain_events`.
//! * `pr.checks.completed` and `git.committed` have no orchestration event.
//!   The PR watch reactor (T07/T13c) and the git workflow service call
//!   [`OrchestrationHooks::dispatch`] with the matching [`HookEvent`] themselves.

use super::orchestrator_port::OrchestratorPort;
use super::seams::ProjectDirectory;
use super::task::OwnedTask;
use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use turin_contracts::orchestration_v2::{
    OrchestrationV2DomainEvent, OrchestrationV2RunStatus, OrchestrationV2RuntimeRequestKind,
    OrchestrationV2RuntimeRequestStatus,
};
use turin_contracts::{RunId, RuntimeRequestId, ThreadId};
use turin_hooks::{DispatchSummary, HookDispatchContext, HookEvent, HookRunner};

/// What the pipeline needs from the hook system.
#[async_trait]
pub trait OrchestrationHooks: Send + Sync {
    async fn dispatch(&self, event: HookEvent, context: &HookDispatchContext) -> DispatchSummary;
}

/// No hooks configured.
pub struct NoHooks;

#[async_trait]
impl OrchestrationHooks for NoHooks {
    async fn dispatch(&self, _: HookEvent, _: &HookDispatchContext) -> DispatchSummary {
        DispatchSummary::default()
    }
}

/// One [`HookRunner`] per project root (each reads `<root>/.turin/hooks.toml`
/// plus the global file), created on first use.
#[derive(Default)]
pub struct ProjectHookRunners {
    runners: Mutex<HashMap<PathBuf, HookRunner>>,
}

impl ProjectHookRunners {
    pub fn new() -> Self {
        Self::default()
    }

    fn runner_for(&self, project_root: &std::path::Path) -> HookRunner {
        self.runners
            .lock()
            .entry(project_root.to_path_buf())
            .or_insert_with(|| HookRunner::for_project(project_root))
            .clone()
    }

    /// Stops every runner's watcher and background hook runs.
    pub async fn shutdown(&self) {
        let runners: Vec<HookRunner> = self.runners.lock().drain().map(|(_, r)| r).collect();
        for runner in runners {
            runner.shutdown().await;
        }
    }
}

#[async_trait]
impl OrchestrationHooks for ProjectHookRunners {
    async fn dispatch(&self, event: HookEvent, context: &HookDispatchContext) -> DispatchSummary {
        self.runner_for(&context.project_root)
            .dispatch(event, context)
            .await
    }
}

/// Maps a committed event to the hook it triggers, given what the bridge
/// remembers about earlier events. Pure so it is testable without a runner.
#[derive(Default)]
pub struct HookEventDeriver {
    run_status: HashMap<RunId, OrchestrationV2RunStatus>,
    announced_requests: HashSet<RuntimeRequestId>,
}

impl HookEventDeriver {
    pub fn derive(&mut self, event: &OrchestrationV2DomainEvent) -> Option<(ThreadId, HookEvent)> {
        match event {
            OrchestrationV2DomainEvent::RunCreated {
                thread_id, payload, ..
            }
            | OrchestrationV2DomainEvent::RunUpdated {
                thread_id, payload, ..
            } => {
                let previous = self.run_status.insert(payload.id.clone(), payload.status);
                if previous == Some(payload.status) {
                    return None;
                }
                let body = json!({
                    "threadId": thread_id,
                    "runId": payload.id,
                    "ordinal": payload.ordinal,
                    "status": payload.status,
                });
                let event = match payload.status {
                    OrchestrationV2RunStatus::Running => HookEvent::turn_started(body),
                    OrchestrationV2RunStatus::Completed => HookEvent::turn_completed(body),
                    OrchestrationV2RunStatus::Failed => HookEvent::turn_failed(body),
                    _ => return None,
                };
                Some((thread_id.clone(), event))
            }
            OrchestrationV2DomainEvent::RuntimeRequestUpdated {
                thread_id, payload, ..
            } => {
                if payload.status != OrchestrationV2RuntimeRequestStatus::Pending
                    || matches!(
                        payload.kind,
                        OrchestrationV2RuntimeRequestKind::UserInput
                            | OrchestrationV2RuntimeRequestKind::AuthRefresh
                    )
                    || !self.announced_requests.insert(payload.id.clone())
                {
                    return None;
                }
                Some((
                    thread_id.clone(),
                    HookEvent::approval_requested(json!({
                        "threadId": thread_id,
                        "requestId": payload.id,
                        "kind": payload.kind,
                    })),
                ))
            }
            OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated {
                thread_id, payload, ..
            } => Some((
                thread_id.clone(),
                HookEvent::pr_linked(json!({
                    "threadId": thread_id,
                    "pullRequest": serde_json::to_value(payload).unwrap_or(Value::Null),
                })),
            )),
            _ => None,
        }
    }
}

pub struct HookEventBridge {
    _task: OwnedTask,
}

impl HookEventBridge {
    /// Follows committed events until dropped. Hook dispatch (and thread /
    /// project lookups) happen off the event stream's consumer: a blocked or
    /// slow hook cannot back up publishing, because the stream is unbounded.
    pub async fn start(
        orchestrator: Arc<dyn OrchestratorPort>,
        projects: Arc<dyn ProjectDirectory>,
        hooks: Arc<dyn OrchestrationHooks>,
    ) -> Result<Self, super::orchestrator_port::OrchestratorError> {
        let mut events = orchestrator.stream_domain_events().await?;
        let task = OwnedTask::spawn(async move {
            let mut deriver = HookEventDeriver::default();
            while let Some(event) = events.next().await {
                let Some((thread_id, hook_event)) = deriver.derive(&event) else {
                    continue;
                };
                let shell = match orchestrator.get_thread_shell(&thread_id).await {
                    Ok(Some(shell)) => shell,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, %thread_id, "hook: could not read thread");
                        continue;
                    }
                };
                let root = match projects.get(&shell.project_id).await {
                    Ok(Some(project)) => PathBuf::from(project.workspace_root),
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "hook: could not read project");
                        continue;
                    }
                };
                let context = HookDispatchContext {
                    project_id: shell.project_id.clone(),
                    project_root: root,
                    thread_id: Some(thread_id),
                    thread_cwd: shell
                        .worktree_path
                        .as_ref()
                        .map(|path| PathBuf::from(path.as_str())),
                };
                hooks.dispatch(hook_event, &context).await;
            }
        });
        Ok(Self { _task: task })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_hooks::HookEventKind;

    fn run_event(kind: &str, id: &str, status: &str) -> OrchestrationV2DomainEvent {
        serde_json::from_value(json!({
            "type": kind, "id": format!("event:{id}:{status}"), "threadId": "thread:h",
            "runId": id, "occurredAt": "2026-06-21T00:00:00.000Z",
            "payload": {
                "id": id, "threadId": "thread:h", "ordinal": 1, "providerInstanceId": "codex",
                "modelSelection": {"instanceId": "codex", "model": "m"}, "providerThreadId": null,
                "userMessageId": "message:h", "rootNodeId": null, "activeAttemptId": null,
                "status": status, "requestedAt": "2026-06-21T00:00:00.000Z", "startedAt": null,
                "completedAt": null, "checkpointId": null, "contextHandoffId": null,
            },
        }))
        .unwrap()
    }

    fn request_event(id: &str, kind: &str, status: &str) -> OrchestrationV2DomainEvent {
        serde_json::from_value(json!({
            "type": "runtime-request.updated", "id": format!("event:{id}:{status}"),
            "threadId": "thread:h", "nodeId": "node:h", "occurredAt": "2026-06-21T00:00:00.000Z",
            "payload": {
                "id": id, "nodeId": "node:h", "providerTurnId": null, "nativeRequestRef": null,
                "kind": kind, "status": status,
                "responseCapability": {"type": "live", "providerSessionId": "session:h"},
                "createdAt": "2026-06-21T00:00:00.000Z", "resolvedAt": null,
            },
        }))
        .unwrap()
    }

    fn kind_of(result: Option<(ThreadId, HookEvent)>) -> Option<HookEventKind> {
        result.map(|(_, event)| event.kind)
    }

    #[test]
    fn run_transitions_map_to_turn_hooks_once() {
        let mut deriver = HookEventDeriver::default();
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.created", "run:1", "queued"))),
            None
        );
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.updated", "run:1", "running"))),
            Some(HookEventKind::TurnStarted)
        );
        // A repeated update with the same status is not a new turn start.
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.updated", "run:1", "running"))),
            None
        );
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.updated", "run:1", "completed"))),
            Some(HookEventKind::TurnCompleted)
        );
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.updated", "run:2", "failed"))),
            Some(HookEventKind::TurnFailed)
        );
        assert_eq!(
            kind_of(deriver.derive(&run_event("run.updated", "run:3", "interrupted"))),
            None
        );
    }

    #[test]
    fn only_new_pending_approvals_announce() {
        let mut deriver = HookEventDeriver::default();
        assert_eq!(
            kind_of(deriver.derive(&request_event("request:1", "command", "pending"))),
            Some(HookEventKind::ApprovalRequested)
        );
        assert_eq!(
            kind_of(deriver.derive(&request_event("request:1", "command", "pending"))),
            None
        );
        assert_eq!(
            kind_of(deriver.derive(&request_event("request:1", "command", "resolved"))),
            None
        );
        // Questions the agent asks are not approvals.
        assert_eq!(
            kind_of(deriver.derive(&request_event("request:2", "user_input", "pending"))),
            None
        );
    }
}
