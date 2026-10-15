//! Ports of upstream `ThreadManagementService.test.ts`.

use super::*;
use crate::pipeline::hooks_bridge::NoHooks;
use crate::pipeline::test_support::{
    MockOrchestrator, NoSessions, StaticProjects, StaticSettings, projection, run, run_json, stored,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::UtcDateTime;
use turin_contracts::orchestration_v2::OrchestrationV2StoredEvent;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
use turin_persistence::{Db, ReceiptStatus};

use crate::pipeline::command_receipt_store::CommandReceipt;
use crate::pipeline::seams::SessionTaskStopper;

fn server_command(value: Value) -> OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn client_command(value: Value) -> OrchestrationV2Command {
    serde_json::from_value(value).expect("command decodes")
}

fn thread_create(thread_id: &str) -> Value {
    json!({
        "type": "thread.create", "createdBy": "agent", "creationSource": "mcp",
        "commandId": "command:thread-management:create", "threadId": thread_id,
        "projectId": "project:thread-management", "title": "Thread management",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5-codex"},
        "runtimeMode": "full-access", "interactionMode": "default",
        "branch": null, "worktreePath": null,
    })
}

#[test]
fn stamps_authoritative_provenance_on_commands_that_create_threads_or_messages() {
    let stamped = with_creation_provenance(
        client_command(thread_create("thread:thread-management:create")),
        Provenance {
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Web,
        },
    );
    let value = serde_json::to_value(stamped).unwrap();
    assert_eq!(value["createdBy"], "user");
    assert_eq!(value["creationSource"], "web");
}

#[test]
fn leaves_commands_that_do_not_create_durable_authored_content_unchanged() {
    let command = client_command(json!({
        "type": "run.interrupt", "commandId": "command:thread-management:interrupt",
        "threadId": "thread:thread-management:interrupt",
        "runId": "run:thread-management:interrupt",
    }));
    let stamped = with_creation_provenance(
        command.clone(),
        Provenance {
            created_by: OrchestrationV2Actor::User,
            creation_source: OrchestrationV2CreationSource::Web,
        },
    );
    assert_eq!(stamped, command);
}

#[test]
fn identifies_every_existing_thread_that_must_be_hydrated_before_dispatch() {
    let ids = |value: Value| -> Vec<String> {
        existing_thread_ids_for_command(&server_command(value))
            .into_iter()
            .map(|id| id.to_string())
            .collect()
    };
    let source = "thread:thread-management:source";
    let target = "thread:thread-management:target";
    let parent = "thread:thread-management:parent";
    assert!(ids(thread_create(target)).is_empty());
    assert_eq!(
        ids(json!({"type": "thread.archive", "commandId": "c", "threadId": target})),
        [target]
    );
    // Read-state commands skip transcript hydration entirely: they fire on
    // every activity bump while a thread is open and never touch messages.
    assert!(
        ids(json!({"type": "thread.visit", "commandId": "c", "threadId": target, "visitedAt": "2026-07-30T00:00:00.000Z"}))
            .is_empty()
    );
    assert!(
        ids(json!({"type": "thread.mark-unread", "commandId": "c", "threadId": target})).is_empty()
    );
    assert_eq!(
        ids(json!({
            "type": "thread.fork", "createdBy": "user", "creationSource": "web", "commandId": "c",
            "sourceThreadId": source, "targetThreadId": target,
            "sourcePoint": {"type": "run", "runId": "run:thread-management:source"},
        })),
        [source]
    );
    assert_eq!(
        ids(json!({
            "type": "thread.merge_back", "createdBy": "user", "creationSource": "web", "commandId": "c",
            "sourceThreadId": source, "targetThreadId": target,
            "sourcePoint": {"type": "run", "runId": "run:thread-management:source"},
        })),
        [source, target]
    );
    assert_eq!(
        ids(json!({
            "type": "delegated_task.wake-policy", "commandId": "c", "parentThreadId": parent,
            "taskId": "node:thread-management:delegated", "completionWake": "always",
        })),
        [parent]
    );
    assert_eq!(
        ids(json!({
            "type": "thread.created.record", "commandId": "c", "parentThreadId": parent,
            "parentRunId": "run:p", "parentNodeId": "node:p", "targetThreadId": target,
            "targetRunId": null,
        })),
        [parent, target]
    );
    // The same thread on both sides is hydrated once.
    assert_eq!(
        ids(json!({
            "type": "thread.merge_back", "createdBy": "user", "creationSource": "web", "commandId": "c",
            "sourceThreadId": source, "targetThreadId": source,
            "sourcePoint": {"type": "run", "runId": "run:x"},
        })),
        [source]
    );
}

#[test]
fn derives_thread_management_messages_from_structural_error_attributes() {
    let project_id = ProjectId::new("project:thread-management:errors").unwrap();
    let thread_id = ThreadId::new("thread:thread-management:errors").unwrap();
    let run_id = RunId::new("run:thread-management:errors").unwrap();
    let message_id = MessageId::new("message:thread-management:errors").unwrap();
    assert_eq!(
        ThreadManagementError::ThreadNotFound {
            project_id: project_id.clone(),
            thread_id: thread_id.clone()
        }
        .to_string(),
        format!("Thread {thread_id} was not found in project {project_id}.")
    );
    assert_eq!(
        ThreadManagementError::RunNotFound {
            thread_id: thread_id.clone(),
            run_id: run_id.clone()
        }
        .to_string(),
        format!("Run {run_id} does not belong to thread {thread_id}.")
    );
    assert_eq!(
        ThreadManagementError::ThreadArchived {
            thread_id: thread_id.clone()
        }
        .to_string(),
        format!("Thread {thread_id} is archived and cannot receive messages.")
    );
    assert_eq!(
        ThreadManagementError::NoSteerableRun {
            thread_id: thread_id.clone(),
            mode: SendMode::Restart
        }
        .to_string(),
        format!("Thread {thread_id} has no running turn that can be restarted.")
    );
    assert_eq!(
        ThreadManagementError::NoSteerableRun {
            thread_id: thread_id.clone(),
            mode: SendMode::Steer
        }
        .to_string(),
        format!("Thread {thread_id} has no running turn that can be steered.")
    );
    assert_eq!(
        ThreadManagementError::ThreadNotInterruptible {
            thread_id: thread_id.clone(),
            run_id: run_id.clone()
        }
        .to_string(),
        format!("Run {run_id} is not currently interruptible.")
    );
    // The infrastructure detail stays on `source`, out of the message.
    let list_failure = ThreadManagementError::ProjectThreadsList {
        project_id: project_id.clone(),
        cause: "private sqlite detail".into(),
    };
    assert_eq!(
        list_failure.to_string(),
        format!("Unable to list threads in project {project_id}.")
    );
    assert!(!list_failure.to_string().contains("private sqlite detail"));
    assert_eq!(
        ThreadManagementError::DurableRunProjection {
            thread_id: thread_id.clone(),
            message_id: message_id.clone()
        }
        .to_string(),
        format!(
            "Message {message_id} was accepted on thread {thread_id} without a durable run projection."
        )
    );
}

struct Harness {
    service: Arc<ThreadManagementService>,
    orchestrator: Arc<MockOrchestrator>,
    receipts: CommandReceiptStore,
}

async fn harness(
    orchestrator: MockOrchestrator,
    settings: Arc<StaticSettings>,
    sessions: Arc<dyn ProviderSessionLookup>,
) -> Harness {
    harness_with_projects(
        orchestrator,
        settings,
        sessions,
        StaticProjects(Default::default()),
    )
    .await
}

async fn harness_with_projects(
    orchestrator: MockOrchestrator,
    settings: Arc<StaticSettings>,
    sessions: Arc<dyn ProviderSessionLookup>,
    projects: StaticProjects,
) -> Harness {
    let orchestrator = Arc::new(orchestrator);
    let receipts = CommandReceiptStore::new(Db::open_in_memory().await.unwrap());
    let service = Arc::new(
        ThreadManagementService::new(
            ThreadCommandExecutor::new(),
            orchestrator.clone(),
            sessions,
            settings,
            receipts.clone(),
            Arc::new(projects),
            Arc::new(NoHooks),
        )
        .with_stop_task_timeout(Duration::from_millis(60)),
    );
    Harness {
        service,
        orchestrator,
        receipts,
    }
}

fn settings_with_disabled_override(
    project: &str,
    instance: &str,
    machine: bool,
    project_enabled: Option<bool>,
) -> Arc<StaticSettings> {
    let mut value = json!({
        "providerInstances": {instance: {"driver": "codex", "enabled": machine}},
    });
    if let Some(enabled) = project_enabled {
        value["projectSettingsOverrides"] =
            json!({project: {"providerInstances": {instance: {"enabled": enabled}}}});
    }
    StaticSettings::with(value)
}

#[tokio::test]
async fn refuses_a_new_turn_when_the_project_effectively_disables_the_instance() {
    let project = "project:thread-management:gate-disabled";
    let h = harness(
        MockOrchestrator::default(),
        settings_with_disabled_override(project, "codex_work", true, Some(false)),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .assert_provider_instance_enabled(
            &ProjectId::new(project).unwrap(),
            &CommandId::new("command:thread-management:gate-disabled").unwrap(),
            &ProviderInstanceId::new("codex_work").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::ProviderInstanceDisabled { .. }
    ));
    assert_eq!(
        error.to_string(),
        format!("Provider instance \"codex_work\" is disabled for project {project}.")
    );
}

#[tokio::test]
async fn allows_a_new_turn_when_the_project_enables_an_instance_the_machine_disabled() {
    let project = "project:thread-management:gate-enabled";
    let h = harness(
        MockOrchestrator::default(),
        settings_with_disabled_override(project, "codex_work", false, Some(true)),
        Arc::new(NoSessions),
    )
    .await;
    h.service
        .assert_provider_instance_enabled(
            &ProjectId::new(project).unwrap(),
            &CommandId::new("command:thread-management:gate-enabled").unwrap(),
            &ProviderInstanceId::new("codex_work").unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn inherits_the_machine_value_when_the_project_has_no_override() {
    let project = "project:thread-management:gate-inherit";
    let h = harness(
        MockOrchestrator::default(),
        settings_with_disabled_override(project, "codex_work", false, None),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .assert_provider_instance_enabled(
            &ProjectId::new(project).unwrap(),
            &CommandId::new("command:thread-management:gate-inherit").unwrap(),
            &ProviderInstanceId::new("codex_work").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::ProviderInstanceDisabled { .. }
    ));
    assert!(
        !h.service
            .is_provider_instance_enabled_for_project(
                &ProjectId::new(project).unwrap(),
                &ProviderInstanceId::new("codex_work").unwrap()
            )
            .await
    );
}

fn chokepoint_orchestrator(project: &str) -> MockOrchestrator {
    let project = project.to_owned();
    MockOrchestrator::default().with_records(move |id| {
        let mut records = projection(id.as_str(), &project, json!({}));
        records.thread.model_selection.instance_id = ProviderInstanceId::new("codex_work").unwrap();
        Ok(records)
    })
}

fn message_dispatch(
    command_id: &str,
    thread_id: &str,
    message_id: &str,
) -> OrchestrationV2ServerCommand {
    server_command(json!({
        "type": "message.dispatch", "commandId": command_id, "threadId": thread_id,
        "messageId": message_id, "text": "Background task completed.", "attachments": [],
        "dispatchMode": {"type": "queue_after_active"}, "createdBy": "agent",
        "creationSource": "server",
    }))
}

#[tokio::test]
async fn the_dispatch_chokepoint_refuses_a_continuation_on_a_project_disabled_instance_before_the_orchestrator()
 {
    let project = "project:thread-management:chokepoint";
    let h = harness(
        chokepoint_orchestrator(project),
        settings_with_disabled_override(project, "codex_work", true, Some(false)),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .dispatch(message_dispatch(
            "command:thread-management:chokepoint-disabled",
            "thread:thread-management:chokepoint",
            "message:thread-management:chokepoint-disabled",
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ThreadManagementError::ProviderInstanceDisabled { project_id, instance_id }
            if project_id.as_str() == project && instance_id.as_str() == "codex_work")
    );
    assert!(error.was_not_accepted());
    assert!(h.orchestrator.dispatched_types().is_empty());
}

#[tokio::test]
async fn a_replay_of_an_already_accepted_command_id_dispatches_despite_the_instance_now_being_disabled()
 {
    let project = "project:thread-management:chokepoint";
    let h = harness(
        chokepoint_orchestrator(project),
        settings_with_disabled_override(project, "codex_work", true, Some(false)),
        Arc::new(NoSessions),
    )
    .await;
    let command_id = CommandId::new("command:thread-management:chokepoint-replay").unwrap();
    h.receipts
        .insert_if_absent(CommandReceipt {
            command_id: command_id.clone(),
            thread_id: ThreadId::new("thread:thread-management:chokepoint").unwrap(),
            command_type: "message.dispatch".into(),
            accepted_at: UtcDateTime::now(),
            result_sequence: 1,
            status: ReceiptStatus::Accepted,
            error: None,
        })
        .await
        .unwrap();
    h.service
        .dispatch(message_dispatch(
            command_id.as_str(),
            "thread:thread-management:chokepoint",
            "message:thread-management:chokepoint-replay",
        ))
        .await
        .unwrap();
    assert_eq!(h.orchestrator.dispatched_types(), ["message.dispatch"]);
}

#[tokio::test]
async fn classifies_projection_infrastructure_failures_separately_from_a_missing_thread() {
    let project_id = ProjectId::new("project:thread-management:projection-failure").unwrap();
    let thread_id = ThreadId::new("thread:thread-management:projection-failure").unwrap();
    let orchestrator = MockOrchestrator::default();
    *orchestrator.projection_fn.lock() = Box::new(|id| {
        Err(OrchestratorError::projection(
            id.clone(),
            "sqlite read failed",
        ))
    });
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .get_project_thread(&project_id, &thread_id)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::ProjectionLoad { .. }
    ));
    assert_eq!(
        error.to_string(),
        format!("Unable to load thread {thread_id} in project {project_id}.")
    );
}

#[tokio::test]
async fn uses_thread_not_found_only_after_a_projection_loads_outside_the_project() {
    let orchestrator = MockOrchestrator::default();
    *orchestrator.projection_fn.lock() = Box::new(|id| {
        Ok(projection(
            id.as_str(),
            "project:thread-management:other",
            json!({}),
        ))
    });
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .get_project_thread(
            &ProjectId::new("project:thread-management:requested").unwrap(),
            &ThreadId::new("thread:thread-management:wrong-project").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::ThreadNotFound { .. }
    ));
    assert!(std::error::Error::source(&error).is_none());
}

const WAIT_PROJECT: &str = "project:thread-management:wait";
const WAIT_THREAD: &str = "thread:thread-management:wait";
const WAIT_RUN: &str = "run:thread-management:wait";

fn wait_input(timeout_ms: u64) -> WaitInput {
    WaitInput {
        project_id: ProjectId::new(WAIT_PROJECT).unwrap(),
        thread_id: ThreadId::new(WAIT_THREAD).unwrap(),
        run_id: Some(RunId::new(WAIT_RUN).unwrap()),
        timeout_ms,
    }
}

fn wait_projection(runs: Vec<Value>) -> ThreadRecords {
    projection(WAIT_THREAD, WAIT_PROJECT, json!({ "runs": runs }))
}

fn run_updated_event(sequence: u64, payload: Value) -> OrchestrationV2StoredEvent {
    stored(
        sequence,
        json!({
            "type": "run.updated", "id": format!("event:wait:{sequence}"), "threadId": WAIT_THREAD,
            "occurredAt": "2026-06-21T00:00:00.000Z", "payload": payload,
        }),
    )
}

async fn wait_timeout_scenario(final_status: &str) -> Result<WaitResult, ThreadManagementError> {
    let status = final_status.to_owned();
    let reads = Arc::new(Mutex::new(0));
    let orchestrator = MockOrchestrator::default().with_records(move |_| {
        let mut count = reads.lock();
        *count += 1;
        let current = if *count == 1 {
            "running"
        } else {
            status.as_str()
        };
        Ok(wait_projection(if current == "missing" {
            vec![]
        } else {
            vec![run_json(WAIT_RUN, WAIT_THREAD, 1, current)]
        }))
    });
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    h.service.wait_for_thread(wait_input(1)).await
}

#[tokio::test]
async fn wait_for_thread_timeout_final_read_reports_a_run_that_finished_during_the_timeout() {
    for status in [
        "completed",
        "failed",
        "cancelled",
        "interrupted",
        "rolled_back",
    ] {
        let result = wait_timeout_scenario(status).await.unwrap();
        assert!(!result.timed_out, "{status}");
        assert_eq!(result.run.unwrap().id.as_str(), WAIT_RUN);
    }
    let still_running = wait_timeout_scenario("running").await.unwrap();
    assert!(still_running.timed_out);
    assert_eq!(
        still_running.run.unwrap().status,
        OrchestrationV2RunStatus::Running
    );
}

#[tokio::test]
async fn wait_for_thread_timeout_with_a_vanished_run_is_run_not_found() {
    let error = wait_timeout_scenario("missing").await.unwrap_err();
    assert!(
        matches!(error, ThreadManagementError::RunNotFound { ref thread_id, ref run_id }
            if thread_id.as_str() == WAIT_THREAD && run_id.as_str() == WAIT_RUN)
    );
}

#[tokio::test]
async fn wait_for_thread_reads_the_run_again_only_when_the_run_updates() {
    let status = Arc::new(Mutex::new("running".to_owned()));
    let for_records = status.clone();
    let orchestrator = MockOrchestrator::default().with_records(move |_| {
        Ok(wait_projection(vec![run_json(
            WAIT_RUN,
            WAIT_THREAD,
            1,
            &for_records.lock(),
        )]))
    });
    let events = orchestrator.open_stream("run.updated");
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let waiter = {
        let service = h.service.clone();
        tokio::spawn(async move { service.wait_for_thread(wait_input(60 * 60 * 1_000)).await })
    };
    h.orchestrator.run_stream_requested.notified().await;
    *status.lock() = "completed".into();
    events
        .send(run_updated_event(
            1,
            run_json("run:other", WAIT_THREAD, 2, "running"),
        ))
        .unwrap();
    events
        .send(run_updated_event(
            2,
            run_json(WAIT_RUN, WAIT_THREAD, 1, "completed"),
        ))
        .unwrap();
    let result = waiter.await.unwrap().unwrap();
    assert!(!result.timed_out);
    assert_eq!(
        result.run.unwrap().status,
        OrchestrationV2RunStatus::Completed
    );
    // The first read plus one for this run's update. The other run caused none.
    assert_eq!(*h.orchestrator.read_count.lock(), 2);
}

#[tokio::test]
async fn wait_for_thread_follows_a_restart_continuation_instead_of_the_cut_run() {
    let continuation_id = "run:thread-management:wait:2";
    let runs = Arc::new(Mutex::new(vec![run_json(
        WAIT_RUN,
        WAIT_THREAD,
        1,
        "cancelled",
    )]));
    let pending = Arc::new(Mutex::new(true));
    let for_records = runs.clone();
    let for_pending = pending.clone();
    let orchestrator = MockOrchestrator::default()
        .with_records(move |_| Ok(wait_projection(for_records.lock().clone())));
    *orchestrator.restart_pending.lock() =
        Box::new(move |run| *for_pending.lock() && run.id.as_str() == WAIT_RUN);
    let events = orchestrator.open_stream("run.updated");
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let waiter = {
        let service = h.service.clone();
        tokio::spawn(async move { service.wait_for_thread(wait_input(60 * 60 * 1_000)).await })
    };
    h.orchestrator.run_stream_requested.notified().await;
    let continuation = |status: &str| {
        let mut run = run_json(continuation_id, WAIT_THREAD, 2, status);
        run["restartContinuationOfRunId"] = json!(WAIT_RUN);
        run
    };
    runs.lock().push(continuation("running"));
    *pending.lock() = false;
    events
        .send(run_updated_event(1, continuation("running")))
        .unwrap();
    runs.lock()[1] = continuation("completed");
    events
        .send(run_updated_event(2, continuation("completed")))
        .unwrap();
    let result = waiter.await.unwrap().unwrap();
    assert!(!result.timed_out);
    let run = result.run.unwrap();
    assert_eq!(run.id.as_str(), continuation_id);
    assert_eq!(run.status, OrchestrationV2RunStatus::Completed);
}

#[tokio::test]
async fn wait_for_thread_returns_the_cut_run_once_its_continuation_is_declined() {
    let pending = Arc::new(Mutex::new(true));
    let for_pending = pending.clone();
    let orchestrator = MockOrchestrator::default().with_records(|_| {
        Ok(wait_projection(vec![run_json(
            WAIT_RUN,
            WAIT_THREAD,
            1,
            "cancelled",
        )]))
    });
    *orchestrator.restart_pending.lock() = Box::new(move |_| *for_pending.lock());
    // Declining records no thread event.
    let _events = orchestrator.open_stream("run.updated");
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let waiter = {
        let service = h.service.clone();
        tokio::spawn(async move { service.wait_for_thread(wait_input(60 * 60 * 1_000)).await })
    };
    h.orchestrator.run_stream_requested.notified().await;
    *pending.lock() = false;
    h.orchestrator.restart_declined.notify_one();
    let result = waiter.await.unwrap().unwrap();
    assert!(!result.timed_out);
    assert_eq!(
        result.run.unwrap().status,
        OrchestrationV2RunStatus::Cancelled
    );
}

#[tokio::test]
async fn settle_after_run_settles_only_when_the_run_completed() {
    for (status, settles) in [
        ("completed", true),
        ("failed", false),
        ("interrupted", false),
    ] {
        let orchestrator = MockOrchestrator::default().with_records(move |_| {
            Ok(wait_projection(vec![run_json(
                WAIT_RUN,
                WAIT_THREAD,
                1,
                status,
            )]))
        });
        let h = harness(
            orchestrator,
            StaticSettings::default_settings(),
            Arc::new(NoSessions),
        )
        .await;
        h.service
            .settle_after_run(
                &ProjectId::new(WAIT_PROJECT).unwrap(),
                &ThreadId::new(WAIT_THREAD).unwrap(),
                &RunId::new(WAIT_RUN).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            h.orchestrator.dispatched_types(),
            if settles {
                vec!["thread.settle"]
            } else {
                vec![]
            },
            "{status}"
        );
    }
}

// ---- stopWorkflow -------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum StopBehavior {
    Succeeds,
    Fails,
    Hangs,
    Unsupported,
}

struct FakeStopper {
    behavior: StopBehavior,
    calls: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl SessionTaskStopper for FakeStopper {
    async fn stop_task(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2StopTaskInput,
    ) -> Result<bool, crate::pipeline::orchestrator_port::BoxError> {
        match self.behavior {
            StopBehavior::Unsupported => Ok(false),
            StopBehavior::Fails => Err("private provider detail".into()),
            StopBehavior::Hangs => std::future::pending().await,
            StopBehavior::Succeeds => {
                self.calls.lock().push((
                    input.task_id,
                    serde_json::to_value(&input.provider_thread).unwrap(),
                ));
                Ok(true)
            }
        }
    }
}

struct OneSession(Option<Arc<FakeStopper>>);

#[async_trait]
impl ProviderSessionLookup for OneSession {
    async fn get(
        &self,
        id: &turin_contracts::ProviderSessionId,
    ) -> Result<Option<Arc<dyn SessionTaskStopper>>, crate::pipeline::orchestrator_port::BoxError>
    {
        assert_eq!(id.as_str(), "provider-session:workflow-stop");
        Ok(self.0.clone().map(|s| s as Arc<dyn SessionTaskStopper>))
    }
}

fn workflow_records(
    state: &str,
    modes: &Arc<Mutex<(RuntimeMode, ProviderInteractionMode)>>,
) -> ThreadRecords {
    let thread_id = "thread:workflow-stop";
    let (runtime_mode, interaction_mode) = *modes.lock();
    let mut task = json!({
        "id": "node:workflow-stop", "threadId": thread_id, "runId": "run:workflow-stop",
        "parentNodeId": "node:workflow-parent", "origin": "provider_native", "createdBy": "agent",
        "driver": if state == "unsupported" { "codex" } else { "claudeAgent" },
        "providerInstanceId": "claudeAgent", "providerThreadId": null, "childThreadId": null,
        "nativeTaskRef": if state == "missing-native-id" { Value::Null } else {
            json!({"driver": "claudeAgent", "nativeId": "native-workflow-id", "strength": "strong"}) },
        "workflow": {"phases": [], "agents": []}, "prompt": "Run the workflow", "title": null,
        "model": null, "status": if state == "completed" { "completed" } else { "running" },
        "result": null, "startedAt": "2026-06-21T00:00:00.000Z", "completedAt": null,
        "updatedAt": "2026-06-21T00:00:00.000Z",
    });
    if state == "missing" {
        task = Value::Null;
    }
    let mut run = run_json("run:workflow-stop", thread_id, 1, "running");
    run["providerThreadId"] = json!("provider-thread:workflow-stop");
    let provider_thread = json!({
        "id": "provider-thread:workflow-stop", "driver": "claudeAgent",
        "providerInstanceId": "claudeAgent", "providerSessionId": "provider-session:workflow-stop",
        "appThreadId": if state == "wrong-thread" { "other-thread" } else { thread_id },
        "ownerNodeId": null,
        "nativeThreadRef": {"driver": "claudeAgent", "nativeId": "native-thread", "strength": "strong"},
        "nativeConversationHeadRef": null, "status": "idle", "firstRunOrdinal": 1,
        "lastRunOrdinal": 1, "handoffIds": [], "forkedFrom": null, "pendingBackgroundTasks": [],
        "contextUsage": null, "nativeMetadata": null,
        "createdAt": "2026-06-21T00:00:00.000Z", "updatedAt": "2026-06-21T00:00:00.000Z",
    });
    let mut records = projection(
        thread_id,
        "project:workflow-stop",
        json!({
            "subagents": if task.is_null() { json!([]) } else { json!([task]) },
            "runs": [run], "providerThreads": [provider_thread],
        }),
    );
    records.thread.runtime_mode = runtime_mode;
    records.thread.interaction_mode = interaction_mode;
    records
}

async fn stop_workflow_state(
    state: &str,
) -> (
    Result<(), OrchestrationV2StopWorkflowError>,
    Vec<(String, Value)>,
    Option<DispatchModeRefusal>,
) {
    let modes = Arc::new(Mutex::new((
        RuntimeMode::ApprovalRequired,
        ProviderInteractionMode::Plan,
    )));
    let for_records = modes.clone();
    let state_for_records = state.to_owned();
    let orchestrator = MockOrchestrator::default()
        .with_records(move |_| Ok(workflow_records(&state_for_records, &for_records)));
    let stopper = match state {
        "no-session" => None,
        "unsupported-runtime" => Some(Arc::new(FakeStopper {
            behavior: StopBehavior::Unsupported,
            calls: Mutex::new(vec![]),
        })),
        "provider-failed" => Some(Arc::new(FakeStopper {
            behavior: StopBehavior::Fails,
            calls: Mutex::new(vec![]),
        })),
        "provider-timeout" => Some(Arc::new(FakeStopper {
            behavior: StopBehavior::Hangs,
            calls: Mutex::new(vec![]),
        })),
        _ => Some(Arc::new(FakeStopper {
            behavior: StopBehavior::Succeeds,
            calls: Mutex::new(vec![]),
        })),
    };
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(OneSession(stopper.clone())),
    )
    .await;
    let limited = matches!(
        state,
        "within-limit" | "runtime-raised" | "interaction-raised"
    );
    let (limit, slot) = DispatchModeLimit::with_refusal_slot(DispatchModes {
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: ProviderInteractionMode::Plan,
    });
    let input = OrchestrationV2StopWorkflowInput {
        thread_id: ThreadId::new("thread:workflow-stop").unwrap(),
        subagent_id: turin_contracts::NodeId::new("node:workflow-stop").unwrap(),
    };
    let result = if state == "runtime-raised" || state == "interaction-raised" {
        // The thread's user raises its modes after the stop is requested but
        // before it holds the lock.
        let executor = h.service.executor().clone();
        let thread_id = input.thread_id.clone();
        let pending = executor
            .with_lock(&thread_id, || async {
                let service = h.service.clone();
                let input = input.clone();
                let limit = limit.clone();
                let handle =
                    tokio::spawn(async move { service.stop_workflow(input, Some(&limit)).await });
                tokio::task::yield_now().await;
                *modes.lock() = if state == "runtime-raised" {
                    (RuntimeMode::FullAccess, ProviderInteractionMode::Plan)
                } else {
                    (
                        RuntimeMode::ApprovalRequired,
                        ProviderInteractionMode::Default,
                    )
                };
                (handle,)
            })
            .await;
        pending.0.await.unwrap()
    } else {
        let result = h
            .service
            .stop_workflow(input.clone(), limited.then_some(&limit))
            .await;
        if state == "provider-timeout" {
            // The lock must be free again.
            h.service
                .executor()
                .with_lock(&input.thread_id, || async {})
                .await;
        }
        result
    };
    let calls = stopper.map(|s| s.calls.lock().clone()).unwrap_or_default();
    let refusal = slot.lock().clone();
    (result, calls, refusal)
}

fn reason_of(error: &OrchestrationV2StopWorkflowError) -> OrchestrationV2StopWorkflowReason {
    match error {
        OrchestrationV2StopWorkflowError::OrchestrationV2StopWorkflowError { reason, .. } => {
            *reason
        }
    }
}

#[tokio::test]
async fn stops_a_persisted_running_workflow() {
    for state in ["running", "within-limit"] {
        let (result, calls, _) = stop_workflow_state(state).await;
        result.unwrap_or_else(|e| panic!("{state}: {e}"));
        assert_eq!(calls.len(), 1, "{state}");
        assert_eq!(calls[0].0, "native-workflow-id");
        assert_eq!(calls[0].1["id"], "provider-thread:workflow-stop");
    }
}

#[tokio::test]
async fn refuses_to_stop_a_workflow_that_cannot_be_stopped() {
    use OrchestrationV2StopWorkflowReason as Reason;
    for (state, reason) in [
        ("missing", Reason::NotRunning),
        ("completed", Reason::NotRunning),
        ("unsupported", Reason::Unsupported),
        ("unsupported-runtime", Reason::Unsupported),
        ("missing-native-id", Reason::Unavailable),
        ("wrong-thread", Reason::Unavailable),
        ("no-session", Reason::Unavailable),
        ("provider-failed", Reason::StopFailed),
        ("provider-timeout", Reason::StopFailed),
    ] {
        let (result, calls, _) = stop_workflow_state(state).await;
        let error = result.expect_err(state);
        assert_eq!(reason_of(&error), reason, "{state}");
        assert!(calls.is_empty(), "{state}");
        if state == "provider-timeout" {
            let OrchestrationV2StopWorkflowError::OrchestrationV2StopWorkflowError {
                cause, ..
            } = &error;
            assert_eq!(cause.as_ref().unwrap()["_tag"], "TimeoutError");
        }
        if state == "provider-failed" {
            // The error text is for logs; the wire message stays generic.
            assert_eq!(error.to_string(), "Failed to stop the workflow.");
        }
    }
}

#[tokio::test]
async fn refuses_a_limited_stop_on_a_thread_its_user_raised_and_records_the_refusal() {
    for (state, mode, expected) in [
        (
            "runtime-raised",
            crate::pipeline::orchestrator_port::ModeKind::Runtime,
            (RuntimeMode::FullAccess, ProviderInteractionMode::Plan),
        ),
        (
            "interaction-raised",
            crate::pipeline::orchestrator_port::ModeKind::Interaction,
            (
                RuntimeMode::ApprovalRequired,
                ProviderInteractionMode::Default,
            ),
        ),
    ] {
        let (result, calls, refusal) = stop_workflow_state(state).await;
        assert_eq!(
            reason_of(&result.unwrap_err()),
            OrchestrationV2StopWorkflowReason::Unavailable
        );
        assert!(calls.is_empty());
        assert_eq!(
            refusal,
            Some(DispatchModeRefusal {
                thread_id: ThreadId::new("thread:workflow-stop").unwrap(),
                runtime_mode: expected.0,
                interaction_mode: expected.1,
                mode,
            }),
            "{state}"
        );
    }
}

#[tokio::test]
async fn thread_created_hooks_bind_the_workspace_a_blocking_hook_reported() {
    use crate::pipeline::hooks_bridge::OrchestrationHooks;
    use turin_hooks::{DispatchSummary, HookDispatchContext, HookEvent, HookEventKind, HookReturn};

    struct ReportsWorktree(Mutex<Vec<HookEventKind>>);
    #[async_trait]
    impl OrchestrationHooks for ReportsWorktree {
        async fn dispatch(&self, event: HookEvent, _: &HookDispatchContext) -> DispatchSummary {
            self.0.lock().push(event.kind);
            DispatchSummary {
                thread_workspace: Some(HookReturn {
                    worktree_path: Some("/work/tree".into()),
                    branch: Some("feature/x".into()),
                }),
                ..DispatchSummary::default()
            }
        }
    }

    let hooks = Arc::new(ReportsWorktree(Mutex::new(vec![])));
    let orchestrator = Arc::new(MockOrchestrator::default());
    let receipts = CommandReceiptStore::new(Db::open_in_memory().await.unwrap());
    let projects = StaticProjects(
        [(
            ProjectId::new("project:thread-management").unwrap(),
            "/repo".to_owned(),
        )]
        .into(),
    );
    let service = ThreadManagementService::new(
        ThreadCommandExecutor::new(),
        orchestrator.clone(),
        Arc::new(NoSessions),
        StaticSettings::default_settings(),
        receipts.clone(),
        Arc::new(projects),
        hooks.clone(),
    );
    service
        .dispatch(server_command(thread_create("thread:hooked")))
        .await
        .unwrap();
    assert_eq!(hooks.0.lock().as_slice(), [HookEventKind::ThreadCreated]);
    let dispatched = serde_json::to_value(&orchestrator.dispatched.lock()[0]).unwrap();
    assert_eq!(dispatched["worktreePath"], "/work/tree");
    assert_eq!(dispatched["branch"], "feature/x");

    // A retry of an accepted command id runs no hooks and binds nothing new.
    receipts
        .insert_if_absent(CommandReceipt {
            command_id: CommandId::new("command:thread-management:create").unwrap(),
            thread_id: ThreadId::new("thread:hooked").unwrap(),
            command_type: "thread.create".into(),
            accepted_at: UtcDateTime::now(),
            result_sequence: 1,
            status: ReceiptStatus::Accepted,
            error: None,
        })
        .await
        .unwrap();
    service
        .dispatch(server_command(thread_create("thread:hooked")))
        .await
        .unwrap();
    assert_eq!(hooks.0.lock().len(), 1);
    assert!(
        serde_json::to_value(&orchestrator.dispatched.lock()[1]).unwrap()["worktreePath"].is_null()
    );
}

#[tokio::test]
async fn send_to_thread_picks_the_dispatch_mode_and_requires_a_durable_run() {
    let thread = "thread:send";
    let project = "project:send";
    let steerable = |with_turn: bool| {
        let mut run = run_json("run:send:1", thread, 1, "running");
        run["activeAttemptId"] = json!("attempt:send");
        let turns = if with_turn {
            json!([{
                "id": "turn:send", "runAttemptId": "attempt:send", "status": "running",
            }])
        } else {
            json!([])
        };
        (run, turns)
    };
    let _ = steerable;
    // No running turn: steer/restart are refused, auto starts immediately.
    let orchestrator = MockOrchestrator::default()
        .with_records(move |_| Ok(projection(thread, project, json!({}))));
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let input = |mode| SendInput {
        project_id: ProjectId::new(project).unwrap(),
        command_id: CommandId::new("command:send").unwrap(),
        thread_id: ThreadId::new(thread).unwrap(),
        message_id: MessageId::new("message:send").unwrap(),
        scheduled_task_id: None,
        sender_thread_id: None,
        text: "hello".into(),
        attachments: vec![],
        model_selection: None,
        mode,
        created_by: OrchestrationV2Actor::User,
        creation_source: OrchestrationV2CreationSource::Web,
    };
    for mode in [SendMode::Steer, SendMode::Restart] {
        let error = h.service.send_to_thread(input(mode)).await.unwrap_err();
        assert!(matches!(
            error,
            ThreadManagementError::NoSteerableRun { .. }
        ));
    }
    // Accepted but never projected: the durable-run guard fires.
    let error = h
        .service
        .send_to_thread(input(SendMode::Auto))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::DurableRunProjection { .. }
    ));
    let dispatched = serde_json::to_value(&h.orchestrator.dispatched.lock()[0]).unwrap();
    assert_eq!(dispatched["dispatchMode"]["type"], "start_immediately");

    // An archived thread takes no messages.
    let archived = MockOrchestrator::default().with_records(move |_| {
        let mut records = projection(thread, project, json!({}));
        records.thread.archived_at = Some(UtcDateTime::now());
        Ok(records)
    });
    let h = harness(
        archived,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    assert!(matches!(
        h.service
            .send_to_thread(input(SendMode::Queue))
            .await
            .unwrap_err(),
        ThreadManagementError::ThreadArchived { .. }
    ));
}

#[tokio::test]
async fn interrupt_thread_reports_each_outcome() {
    let thread = "thread:interrupt";
    let project = "project:interrupt";
    let runs = Arc::new(Mutex::new(vec![run_json(
        "run:i:1",
        thread,
        1,
        "completed",
    )]));
    let for_records = runs.clone();
    let orchestrator = MockOrchestrator::default().with_records(move |_| {
        Ok(projection(
            thread,
            project,
            json!({ "runs": for_records.lock().clone() }),
        ))
    });
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let input = |run_id: Option<&str>| InterruptInput {
        project_id: ProjectId::new(project).unwrap(),
        command_id: CommandId::new("command:interrupt").unwrap(),
        thread_id: ThreadId::new(thread).unwrap(),
        run_id: run_id.map(|id| RunId::new(id).unwrap()),
        reason: None,
    };
    assert!(matches!(
        h.service.interrupt_thread(input(None)).await.unwrap(),
        InterruptResult::NoActiveRun
    ));
    assert!(matches!(
        h.service
            .interrupt_thread(input(Some("run:i:1")))
            .await
            .unwrap(),
        InterruptResult::AlreadyTerminal { .. }
    ));
    assert!(matches!(
        h.service
            .interrupt_thread(input(Some("run:missing")))
            .await
            .unwrap_err(),
        ThreadManagementError::RunNotFound { .. }
    ));
    runs.lock().push(run_json("run:i:2", thread, 2, "running"));
    assert!(matches!(
        h.service
            .interrupt_thread(input(Some("run:i:1")))
            .await
            .unwrap(),
        InterruptResult::AlreadyTerminal { .. }
    ));
    // The active run is run 2; asking to interrupt a different live run is refused.
    runs.lock().push(run_json("run:i:3", thread, 1, "waiting"));
    assert!(matches!(
        h.service
            .interrupt_thread(input(Some("run:i:3")))
            .await
            .unwrap_err(),
        ThreadManagementError::ThreadNotInterruptible { .. }
    ));
    let result = h.service.interrupt_thread(input(None)).await.unwrap();
    let InterruptResult::InterruptRequested { run, .. } = result else {
        panic!("expected an interrupt");
    };
    assert_eq!(run.id.as_str(), "run:i:2");
    assert_eq!(h.orchestrator.dispatched_types(), ["run.interrupt"]);
    let _ = run;
}

#[tokio::test]
async fn stop_delegated_tasks_stops_children_depth_first_and_reports_the_first_failure() {
    let subagent = |id: &str, child: &str| {
        json!({
            "id": id, "threadId": "thread:root", "runId": null, "parentNodeId": "node:p",
            "origin": "app_owned", "createdBy": "agent", "driver": "codex",
            "providerInstanceId": "codex", "providerThreadId": null, "childThreadId": child,
            "nativeTaskRef": null, "prompt": "p", "title": null, "model": null,
            "status": "running", "result": null, "startedAt": null, "completedAt": null,
            "updatedAt": "2026-06-21T00:00:00.000Z",
        })
    };
    let orchestrator = MockOrchestrator::default().with_records(move |id| {
        Ok(match id.as_str() {
            "thread:root" => projection(
                "thread:root",
                "project:s",
                json!({
                    "subagents": [subagent("node:a", "thread:a"), subagent("node:b", "thread:b")]
                }),
            ),
            "thread:a" => projection(
                "thread:a",
                "project:s",
                json!({
                    "subagents": [subagent("node:a1", "thread:a1")]
                }),
            ),
            other => projection(other, "project:s", json!({})),
        })
    });
    *orchestrator.dispatch_fn.lock() = Box::new(|command| {
        let value = serde_json::to_value(&command).unwrap();
        if value["threadId"] == "thread:a" {
            return Err(OrchestratorError::CommandRejected {
                command_id: CommandId::new("x").unwrap(),
                command_type: "thread.stop".into(),
                cause: "cannot stop".into(),
            });
        }
        Ok(DispatchResult {
            sequence: 1,
            stored_events: vec![],
        })
    });
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let error = h
        .service
        .stop_delegated_tasks(StopDelegatedTasksInput {
            thread_id: ThreadId::new("thread:root").unwrap(),
            command_id: CommandId::new("command:stop").unwrap(),
            reason: Some("stopped".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ThreadManagementError::Orchestrator(OrchestratorError::CommandRejected { .. })
    ));
    let stopped: Vec<(String, String)> = h
        .orchestrator
        .dispatched
        .lock()
        .iter()
        .map(|c| {
            let v = serde_json::to_value(c).unwrap();
            (
                v["commandId"].as_str().unwrap().to_owned(),
                v["threadId"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    // `thread:a` failed, so its own child is skipped; `thread:b` is still tried.
    assert_eq!(
        stopped,
        [
            (
                "command:stop:stop:thread:a".to_owned(),
                "thread:a".to_owned()
            ),
            (
                "command:stop:stop:thread:b".to_owned(),
                "thread:b".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn list_project_threads_hides_subagents_and_sorts_newest_first() {
    let shell = |id: &str, updated: &str, relationship: Value| {
        let mut shell = json!({
            "createdBy": "user", "creationSource": "web", "id": id, "projectId": "project:list",
            "title": id, "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "m"},
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
            "worktreePath": null, "activeProviderThreadId": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": relationship, "rootThreadId": id},
            "forkedFrom": null, "createdAt": "2026-06-21T00:00:00.000Z", "updatedAt": updated,
            "archivedAt": null, "settledOverride": null, "settledAt": null, "lastVisitedAt": null,
            "deletedAt": null, "latestRunId": null, "activeRunId": null, "status": "idle",
            "latestUserMessageAt": null, "pendingRuntimeRequest": null,
        });
        shell["pendingBackgroundTasks"] = json!([]);
        shell["hasActionableProposedPlan"] = json!(false);
        shell["providerInstanceHistory"] = json!([]);
        shell["latestVisibleMessage"] = Value::Null;
        shell["itemCount"] = json!(0);
        shell["visibleItemCount"] = json!(0);
        shell["messageCount"] = json!(0);
        shell
    };
    let snapshot = json!({
        "schemaVersion": 1, "snapshotSequence": 1, "archivedThreads": [], "projects": [],
        "threads": [
            shell("thread:old", "2026-06-21T00:00:01.000Z", Value::Null),
            shell("thread:new", "2026-06-21T00:00:05.000Z", Value::Null),
            shell("thread:sub", "2026-06-21T00:00:09.000Z", json!("subagent")),
        ],
    });
    let snapshot = serde_json::from_value(snapshot).expect("shell snapshot decodes");
    let orchestrator = MockOrchestrator::default();
    *orchestrator.shell_snapshot.lock() = Some(snapshot);
    let h = harness(
        orchestrator,
        StaticSettings::default_settings(),
        Arc::new(NoSessions),
    )
    .await;
    let project = ProjectId::new("project:list").unwrap();
    let ids = |threads: Vec<OrchestrationV2ThreadShell>| -> Vec<String> {
        threads.into_iter().map(|t| t.id.to_string()).collect()
    };
    assert_eq!(
        ids(h
            .service
            .list_project_threads(&project, false)
            .await
            .unwrap()),
        ["thread:new", "thread:old"]
    );
    assert_eq!(
        ids(h
            .service
            .list_project_threads(&project, true)
            .await
            .unwrap()),
        ["thread:sub", "thread:new", "thread:old"]
    );
}

#[test]
fn run_helpers_pick_the_expected_runs() {
    let runs = vec![
        run("run:1", "thread:r", 1, "completed"),
        run("run:2", "thread:r", 2, "running"),
        run("run:3", "thread:r", 3, "failed"),
    ];
    assert_eq!(latest_run(&runs).unwrap().id.as_str(), "run:3");
    assert_eq!(latest_active_run(&runs).unwrap().id.as_str(), "run:2");
    assert!(is_terminal_run_status(OrchestrationV2RunStatus::RolledBack));
    assert!(!is_terminal_run_status(OrchestrationV2RunStatus::Queued));
    assert!(!is_active_run(&run("run:q", "thread:r", 4, "queued")));
    let mut chained = runs.clone();
    let mut next = run("run:4", "thread:r", 4, "running");
    next.restart_continuation_of_run_id = Some(RunId::new("run:3").unwrap());
    chained.push(next);
    assert_eq!(
        latest_restart_continuation(&chained, &chained[2])
            .id
            .as_str(),
        "run:4"
    );
}
