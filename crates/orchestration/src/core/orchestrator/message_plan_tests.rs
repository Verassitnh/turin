//! Delegated-completion deliveries and proposed-plan starts through
//! `message.dispatch` (`Orchestrator.ts` 4796-4930 and the `completeSourcePlan`
//! calls). Upstream covers them in the mailbox and plan integration suites with
//! real providers; these run the decider on seeded projections.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2PlanArtifact, OrchestrationV2PlanArtifactStatus, OrchestrationV2RunStatus,
};

use super::message_steer_tests::{Steer, T1, harness, harness_with};
use super::tests::{create_command, message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::OrchestratorPort;
use crate::projection::test_support::{apply_all, event};

const TASK: &str = "node:task:1";
const MESSAGE: &str = "message:delivery:1";

fn cause(error: &crate::pipeline::orchestrator_port::OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// The running run now expects delivery 1 of `TASK` as `MESSAGE`; the task is
/// an app-owned subagent whose completion always wakes the parent.
async fn open_delivery(h: &Steer, thread_id: &str, run_id: &str, wake: &str) {
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let mut run = serde_json::to_value(&projection.runs[0]).expect("run encodes");
    run["delegatedCompletion"] = json!({
        "disposition": "open",
        "nextGeneration": 2,
        "delivery": {"generation": 1, "messageId": MESSAGE, "taskIds": [TASK]},
    });
    let provider_thread = &projection.provider_threads[0];
    let subagent = json!({
        "id": TASK,
        "threadId": thread_id,
        "runId": run_id,
        "parentNodeId": run["rootNodeId"],
        "origin": "app_owned",
        "createdBy": "agent",
        "driver": "codex",
        "providerInstanceId": "codex",
        "providerThreadId": provider_thread.id,
        "childThreadId": null,
        "nativeTaskRef": null,
        "prompt": "do it",
        "title": null,
        "model": null,
        "completionWake": wake,
        "status": "completed",
        "result": "done",
        "startedAt": T1,
        "completedAt": T1,
        "updatedAt": T1,
    });
    apply_all(
        &h.projection,
        vec![
            event("run.updated", thread_id, T1, run),
            event("subagent.updated", thread_id, T1, subagent),
        ],
    )
    .await;
}

fn delivery(run_id: &str, extra: Value) -> Value {
    let mut body = json!({
        "createdBy": "agent",
        "creationSource": "server",
        "messageId": MESSAGE,
        "dispatchMode": {"type": "queue_after_active"},
        "text": "ignored: the wake text replaces it",
        "delegatedCompletion": {"parentRunId": run_id, "generation": 1, "taskIds": [TASK]},
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

#[tokio::test]
async fn a_delivery_queues_behind_the_run_with_its_wake_text() {
    let h = harness_with(false, true, None).await;
    let run_id = h.running_run("thread:wake").await;
    open_delivery(&h, "thread:wake", &run_id, "always").await;
    h.orchestrator
        .dispatch(message_command(
            "command:wake",
            "thread:wake",
            delivery(&run_id, json!({})),
        ))
        .await
        .expect("the delivery commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:wake"))
        .await
        .expect("projection reads");
    let [_, queued] = projection.runs.as_slice() else {
        panic!("a queued run behind the running one: {:?}", projection.runs);
    };
    assert_eq!(queued.status, OrchestrationV2RunStatus::Queued);
    let message = projection
        .messages
        .iter()
        .find(|message| message.id.as_str() == MESSAGE)
        .expect("the delivery message");
    assert_eq!(
        message.text,
        format!(
            "Delegated task {TASK} reached a terminal state. Use task_status with taskId {TASK} to read the result."
        )
    );
    let completion = message.delegated_completion.as_ref().expect("completion");
    assert_eq!(completion.generation.get(), 1);
    assert_eq!(completion.task_ids.len(), 1);
}

#[tokio::test]
async fn a_delivery_steers_the_running_turn_when_the_session_allows_it() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:mailbox").await;
    open_delivery(&h, "thread:mailbox", &run_id, "always").await;
    h.orchestrator
        .dispatch(message_command(
            "command:mailbox",
            "thread:mailbox",
            delivery(&run_id, json!({})),
        ))
        .await
        .expect("the delivery commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:mailbox"))
        .await
        .expect("projection reads");
    // Steered into the running run: no queued run, no new attempt.
    assert_eq!(projection.runs.len(), 1);
    assert_eq!(projection.attempts.len(), 1);
    let message = projection
        .messages
        .iter()
        .find(|message| message.id.as_str() == MESSAGE)
        .expect("the delivery message");
    assert!(message.delegated_completion.is_some());
    assert!(matches!(
        h.effects("command:mailbox").await.as_slice(),
        [EffectRequest::ProviderTurnSteer { .. }]
    ));
}

#[tokio::test]
async fn a_settled_only_task_does_not_steer() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:settled").await;
    open_delivery(&h, "thread:settled", &run_id, "settled_only").await;
    h.orchestrator
        .dispatch(message_command(
            "command:settled",
            "thread:settled",
            delivery(&run_id, json!({})),
        ))
        .await
        .expect("the delivery commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:settled"))
        .await
        .expect("projection reads");
    assert_eq!(projection.runs.len(), 2, "the delivery queues instead");
}

#[tokio::test]
async fn a_stale_or_misplaced_delivery_is_refused() {
    let h = harness(false, None).await;
    let run_id = h.running_run("thread:stale").await;
    open_delivery(&h, "thread:stale", &run_id, "always").await;

    let wrong_generation = h
        .orchestrator
        .dispatch(message_command(
            "command:generation",
            "thread:stale",
            delivery(&run_id,
                json!({"delegatedCompletion": {"parentRunId": run_id, "generation": 2, "taskIds": [TASK]}}),
            ),
        ))
        .await
        .expect_err("the cohort expects generation 1");
    assert_eq!(
        cause(&wrong_generation),
        "Delegated completion delivery is no longer dispatchable."
    );

    let wrong_message = h
        .orchestrator
        .dispatch(message_command(
            "command:message",
            "thread:stale",
            delivery(&run_id, json!({"messageId": "message:other"})),
        ))
        .await
        .expect_err("the delivery names its message");
    assert_eq!(
        cause(&wrong_message),
        "Delegated completion delivery is no longer dispatchable."
    );

    let from_user = h
        .orchestrator
        .dispatch(message_command(
            "command:user",
            "thread:stale",
            delivery(
                &run_id,
                json!({"createdBy": "user", "creationSource": "web"}),
            ),
        ))
        .await
        .expect_err("only the server delivers");
    assert_eq!(
        cause(&from_user),
        "Delegated completion delivery must be a server-created queued message."
    );
}

/// A proposed plan on `thread_id` in the given status.
async fn seed_plan(h: &Steer, thread_id: &str, status: &str) {
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let node = projection
        .nodes
        .first()
        .map_or_else(|| "node:plan".to_owned(), |node| node.id.to_string());
    let plan = json!({
        "kind": "proposed_plan",
        "id": "plan:1",
        "threadId": thread_id,
        "runId": null,
        "nodeId": node,
        "status": status,
        "markdown": "# Plan",
    });
    apply_all(
        &h.projection,
        vec![event("plan.updated", thread_id, T1, plan)],
    )
    .await;
}

#[tokio::test]
async fn a_message_from_an_active_plan_starts_the_run_and_completes_the_plan() {
    let h = harness(true, None).await;
    h.orchestrator
        .dispatch(create_command("command:create", "thread:plan", "Plan"))
        .await
        .expect("create commits");
    seed_plan(&h, "thread:plan", "active").await;
    h.orchestrator
        .dispatch(message_command(
            "command:from-plan",
            "thread:plan",
            json!({"sourcePlanRef": {"threadId": "thread:plan", "planId": "plan:1"}}),
        ))
        .await
        .expect("the plan starts a run");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:plan"))
        .await
        .expect("projection reads");
    let [run] = projection.runs.as_slice() else {
        panic!("one run: {:?}", projection.runs);
    };
    let reference = run
        .source_plan_ref
        .as_ref()
        .expect("the run names its plan");
    assert_eq!(reference.plan_id.as_str(), "plan:1");
    let plan = h
        .projection
        .get_plan(&thread("thread:plan"), "plan:1")
        .await
        .expect("plan reads")
        .expect("plan exists");
    assert!(matches!(
        plan,
        OrchestrationV2PlanArtifact::ProposedPlan {
            status: OrchestrationV2PlanArtifactStatus::Completed,
            ..
        }
    ));
}

#[tokio::test]
async fn a_missing_or_inactive_plan_is_refused() {
    let h = harness(true, None).await;
    h.orchestrator
        .dispatch(create_command("command:create", "thread:noplan", "Plan"))
        .await
        .expect("create commits");
    let missing = h
        .orchestrator
        .dispatch(message_command(
            "command:missing",
            "thread:noplan",
            json!({"sourcePlanRef": {"threadId": "thread:noplan", "planId": "plan:none"}}),
        ))
        .await
        .expect_err("no such plan");
    assert_eq!(
        cause(&missing),
        "Proposed plan plan:none does not exist on thread thread:noplan."
    );

    seed_plan(&h, "thread:noplan", "superseded").await;
    let inactive = h
        .orchestrator
        .dispatch(message_command(
            "command:inactive",
            "thread:noplan",
            json!({"sourcePlanRef": {"threadId": "thread:noplan", "planId": "plan:1"}}),
        ))
        .await
        .expect_err("the plan was superseded");
    assert_eq!(cause(&inactive), "Proposed plan plan:1 is not active.");
}
