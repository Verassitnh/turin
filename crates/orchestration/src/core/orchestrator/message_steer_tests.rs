//! Steering into a running run (`message_steer.rs`, upstream
//! `dispatchSteerIntoRun`). Upstream drives these paths through
//! `SteeringCompletion.integration.test.ts` and `SelectionRestart.integration.test.ts`
//! with real provider fixtures; here a seeded running run and a fake live
//! session stand in, so the tests read the events and effects the decision
//! writes.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::{
    ProviderSessionId,
    orchestration_v2::{
        OrchestrationV2ProviderCapabilities, OrchestrationV2ProviderSession,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
        OrchestrationV2UserMessageInputIntent,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_persistence::{Db, OrchestrationEventStore};
use turin_provider_core::{
    provider_adapter::{
        ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2ReadThreadSnapshotInput,
        ProviderAdapterV2ResumeThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
        ProviderAdapterV2SteerInput, ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderEventStream, ProviderSessionRuntime,
    },
    selection_transition::ProviderSelectionTransitionPlan,
};

use super::Orchestrator;
use super::message_dispatch_tests::{FakeAdapter, FakeAdapters, FakeCheckpoints, FakeProjectRoots};
use super::services::{DispatchSeams, NoRestartRecovery};
use super::tests::{command_id, create_command, message_command, thread};
use crate::core::runtime_request_service::LiveSessions;
use crate::pipeline::command_receipt_store::CommandReceiptStore;
use crate::pipeline::effect_outbox::{EffectOutbox, EffectRequest, SessionTransition};
use crate::pipeline::event_sink::EventSink;
use crate::pipeline::orchestrator_port::{BoxError, OrchestratorError, OrchestratorPort};
use crate::projection::store::ProjectionStore;
use crate::projection::test_support::{apply_all, event};

const T0: &str = "2026-01-01T00:00:00.000Z";
pub(super) const T1: &str = "2026-01-01T00:01:00.000Z";

/// A live session that answers nothing: steering only reads its record.
struct FakeRuntime {
    session: OrchestrationV2ProviderSession,
    session_id: ProviderSessionId,
}

fn unused() -> ! {
    unreachable!("a steer decision only reads the session's capabilities")
}

#[async_trait]
impl ProviderSessionRuntime for FakeRuntime {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.session.provider_instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.session.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }
    fn events(&self) -> ProviderEventStream {
        Box::pin(futures::stream::empty())
    }
    async fn ensure_thread(
        &self,
        _input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        unused()
    }
    async fn resume_thread(
        &self,
        _input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        unused()
    }
    async fn start_turn(
        &self,
        _input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn steer_turn(
        &self,
        _input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn interrupt_turn(
        &self,
        _input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn respond_to_runtime_request(
        &self,
        _input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn read_thread_snapshot(
        &self,
        _input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        unused()
    }
    async fn rollback_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        unused()
    }
    async fn fork_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2ForkThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        unused()
    }
}

/// Serves the one runtime, once a test installs it, under any session id.
#[derive(Default)]
pub(super) struct FakeSessions {
    pub(super) runtime: Mutex<Option<Arc<dyn ProviderSessionRuntime>>>,
}

#[async_trait]
impl LiveSessions for FakeSessions {
    async fn get_session(
        &self,
        _provider_session_id: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn ProviderSessionRuntime>>, BoxError> {
        Ok(self.runtime.lock().clone())
    }
}

pub(super) struct Steer {
    pub(super) orchestrator: Orchestrator,
    pub(super) outbox: EffectOutbox,
    pub(super) projection: Arc<ProjectionStore>,
    pub(super) sessions: Arc<FakeSessions>,
    pub(super) capabilities: OrchestrationV2ProviderCapabilities,
}

/// Active steering is offered when `active_steering`; a restart is always
/// possible. `transition` is what the adapter answers for a selection change.
pub(super) async fn harness(
    active_steering: bool,
    transition: Option<ProviderSelectionTransitionPlan>,
) -> Steer {
    harness_with(active_steering, true, transition).await
}

/// `restart_steering` is whether the provider can steer by interrupt and
/// restart.
pub(super) async fn harness_with(
    active_steering: bool,
    restart_steering: bool,
    transition: Option<ProviderSelectionTransitionPlan>,
) -> Steer {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        projection.clone(),
    );
    let mut capabilities_json =
        serde_json::to_value(crate::pipeline::test_support::default_capabilities())
            .expect("capabilities encode");
    capabilities_json["turns"]["supportsActiveSteering"] = json!(active_steering);
    capabilities_json["turns"]["supportsSteeringByInterruptRestart"] = json!(restart_steering);
    let capabilities: OrchestrationV2ProviderCapabilities =
        serde_json::from_value(capabilities_json).expect("capabilities decode");
    let adapter: Arc<dyn ProviderAdapterV2> = Arc::new(FakeAdapter {
        instance: serde_json::from_value(json!("codex")).expect("instance fixture"),
        capabilities,
        driver: serde_json::from_value(json!("codex")).expect("driver fixture"),
        transition,
    });
    let sessions = Arc::new(FakeSessions::default());
    let orchestrator = Orchestrator::new(
        projection.clone(),
        sink,
        CommandReceiptStore::new(db.clone()),
        Arc::new(NoRestartRecovery),
    )
    .with_dispatch_seams(DispatchSeams {
        checkpoints: Arc::new(FakeCheckpoints),
        adapters: Arc::new(FakeAdapters { adapter }),
        project_roots: Arc::new(FakeProjectRoots),
        sessions: sessions.clone(),
        ..DispatchSeams::default()
    });
    Steer {
        orchestrator,
        outbox: EffectOutbox::new(db),
        projection,
        sessions,
        capabilities,
    }
}

impl Steer {
    /// A thread whose first message started a run, then the provider started
    /// its turn: the run is `running` with a `running` provider turn, and the
    /// provider thread's session is live. Returns the run id.
    pub(super) async fn running_run(&self, thread_id: &str) -> String {
        self.orchestrator
            .dispatch(create_command(
                &format!("command:create:{thread_id}"),
                thread_id,
                "Thread",
            ))
            .await
            .expect("create commits");
        self.orchestrator
            .dispatch(message_command(
                &format!("command:first:{thread_id}"),
                thread_id,
                json!({}),
            ))
            .await
            .expect("the first message starts a run");
        let projection = self
            .orchestrator
            .get_thread_projection(&thread(thread_id))
            .await
            .expect("projection reads");
        let run = &projection.runs[0];
        let session_id = projection.provider_threads[0]
            .provider_session_id
            .clone()
            .expect("the provider thread has a session");
        let mut running = serde_json::to_value(run).expect("run encodes");
        running["status"] = json!("running");
        running["startedAt"] = json!(T1);
        let attempt_id = run.active_attempt_id.clone().expect("attempt");
        let mut attempt = serde_json::to_value(&projection.attempts[0]).expect("attempt encodes");
        attempt["status"] = json!("running");
        attempt["startedAt"] = json!(T1);
        let turn = json!({
            "id": format!("provider-turn:{thread_id}:1"),
            "providerThreadId": run.provider_thread_id,
            "nodeId": run.root_node_id,
            "runAttemptId": attempt_id,
            "nativeTurnRef": null,
            "ordinal": 1,
            "status": "running",
            "startedAt": T1,
            "completedAt": null,
        });
        apply_all(
            &self.projection,
            vec![
                event("run.updated", thread_id, T1, running),
                event("run-attempt.updated", thread_id, T1, attempt),
                event("provider-turn.updated", thread_id, T1, turn),
            ],
        )
        .await;
        let session: OrchestrationV2ProviderSession = serde_json::from_value(json!({
            "id": session_id,
            "driver": "codex",
            "providerInstanceId": "codex",
            "status": "running",
            "cwd": "/turin-fixture/project",
            "model": "gpt-test",
            "capabilities": self.capabilities,
            "createdAt": T0,
            "updatedAt": T0,
            "lastError": null,
        }))
        .expect("session fixture");
        *self.sessions.runtime.lock() = Some(Arc::new(FakeRuntime {
            session,
            session_id,
        }));
        run.id.to_string()
    }

    pub(super) async fn steer(
        &self,
        command: &str,
        thread_id: &str,
        mode: &str,
        run_id: &str,
        extra: Value,
    ) -> Result<(), OrchestratorError> {
        let mut body =
            json!({"dispatchMode": {"type": mode, "targetRunId": run_id}, "text": "steer"});
        body.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        self.orchestrator
            .dispatch(message_command(command, thread_id, body))
            .await
            .map(|_| ())
    }

    pub(super) async fn effects(&self, command: &str) -> Vec<EffectRequest> {
        self.outbox
            .list_by_command_id(command_id(command))
            .await
            .expect("effects read")
            .into_iter()
            .map(|effect| effect.request)
            .collect()
    }
}

#[tokio::test]
async fn an_active_steer_joins_the_running_turn() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:steer").await;
    h.steer(
        "command:steer",
        "thread:steer",
        "steer_active",
        &run_id,
        json!({}),
    )
    .await
    .expect("the steer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:steer"))
        .await
        .expect("projection reads");
    // The run keeps its attempt: no restart.
    assert_eq!(projection.runs.len(), 1);
    assert_eq!(projection.runs[0].status, OrchestrationV2RunStatus::Running);
    assert_eq!(projection.attempts.len(), 1);
    let steer_item = projection.turn_items.iter().find_map(|item| match item {
        OrchestrationV2TurnItem::UserMessage {
            input_intent,
            text,
            provider_turn_id,
            ..
        } if text == "steer" => Some((*input_intent, provider_turn_id.clone())),
        _ => None,
    });
    let (intent, turn_id) = steer_item.expect("the steer is a user turn item");
    assert_eq!(intent, OrchestrationV2UserMessageInputIntent::Steer);
    assert!(
        turn_id.is_some(),
        "an active steer lands in the running turn"
    );

    let effects = h.effects("command:steer").await;
    let [EffectRequest::ProviderTurnSteer { message_id, .. }] = effects.as_slice() else {
        panic!("one steer effect: {effects:?}");
    };
    assert_eq!(message_id.as_str(), "message:command:steer");
}

#[tokio::test]
async fn a_restart_supersedes_the_attempt_and_queues_the_restart_effect() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:restart").await;
    h.steer(
        "command:restart",
        "thread:restart",
        "restart_active",
        &run_id,
        json!({}),
    )
    .await
    .expect("the restart commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:restart"))
        .await
        .expect("projection reads");
    let run = &projection.runs[0];
    assert_eq!(run.status, OrchestrationV2RunStatus::Starting);
    assert_eq!(run.user_message_id.as_str(), "message:command:restart");
    let [old, new] = projection.attempts.as_slice() else {
        panic!("two attempts: {:?}", projection.attempts);
    };
    assert_eq!(old.status, OrchestrationV2RunAttemptStatus::Superseded);
    assert_eq!(new.attempt_ordinal.get(), 2);
    assert_eq!(new.status, OrchestrationV2RunAttemptStatus::Pending);
    assert_eq!(Some(&new.id), run.active_attempt_id.as_ref());
    assert_eq!(Some(&new.root_node_id), run.root_node_id.as_ref());
    // The steering message belongs to the new root node.
    let message = projection
        .messages
        .iter()
        .find(|message| message.text == "steer")
        .expect("steering message");
    assert_eq!(message.node_id.as_ref(), Some(&new.root_node_id));

    let effects = h.effects("command:restart").await;
    let [
        EffectRequest::ProviderTurnRestart {
            interrupted_attempt_id,
            run_id: effect_run,
            session_transition,
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("one restart effect: {effects:?}");
    };
    assert_eq!(interrupted_attempt_id, &old.id);
    assert_eq!(effect_run, &run.id);
    assert!(session_transition.is_none());
}

#[tokio::test]
async fn a_provider_without_active_steering_restarts_instead() {
    let h = harness(false, None).await;
    let run_id = h.running_run("thread:nosteer").await;
    h.steer(
        "command:steer",
        "thread:nosteer",
        "steer_active",
        &run_id,
        json!({}),
    )
    .await
    .expect("the steer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:nosteer"))
        .await
        .expect("projection reads");
    assert_eq!(projection.attempts.len(), 2);
    let effects = h.effects("command:steer").await;
    assert!(matches!(
        effects.as_slice(),
        [EffectRequest::ProviderTurnRestart { .. }]
    ));
}

#[tokio::test]
async fn a_selection_for_the_next_turn_is_saved_without_restarting() {
    // A provider that can restart for a new selection would; this one can only
    // steer, so the selection waits for the next turn.
    let h = harness_with(
        true,
        false,
        Some(ProviderSelectionTransitionPlan::ApplyOnNextTurn),
    )
    .await;
    let run_id = h.running_run("thread:next").await;
    h.steer(
        "command:next",
        "thread:next",
        "steer_active",
        &run_id,
        json!({"modelSelection": {"instanceId": "codex", "model": "gpt-next"}}),
    )
    .await
    .expect("the steer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:next"))
        .await
        .expect("projection reads");
    // The running run keeps its model; the thread remembers the new one.
    let thread_model = serde_json::to_value(&projection.thread.model_selection).unwrap();
    assert_eq!(thread_model["model"], "gpt-next");
    let run_model = serde_json::to_value(&projection.runs[0].model_selection).unwrap();
    assert_ne!(run_model["model"], "gpt-next");
    assert_eq!(projection.attempts.len(), 1);
    assert!(matches!(
        h.effects("command:next").await.as_slice(),
        [EffectRequest::ProviderTurnSteer { .. }]
    ));
}

#[tokio::test]
async fn a_selection_change_restarts_a_provider_that_can_restart() {
    let h = harness(true, Some(ProviderSelectionTransitionPlan::ApplyOnNextTurn)).await;
    let run_id = h.running_run("thread:eager").await;
    h.steer(
        "command:eager",
        "thread:eager",
        "steer_active",
        &run_id,
        json!({"modelSelection": {"instanceId": "codex", "model": "gpt-eager"}}),
    )
    .await
    .expect("the steer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:eager"))
        .await
        .expect("projection reads");
    let run_model = serde_json::to_value(&projection.runs[0].model_selection).unwrap();
    assert_eq!(run_model["model"], "gpt-eager");
    assert_eq!(projection.attempts.len(), 2);
    assert!(matches!(
        h.effects("command:eager").await.as_slice(),
        [EffectRequest::ProviderTurnRestart {
            session_transition: None,
            ..
        }]
    ));
}

#[tokio::test]
async fn a_selection_that_needs_a_new_session_restarts_with_a_replacement() {
    let h = harness(true, Some(ProviderSelectionTransitionPlan::RestartSession)).await;
    let run_id = h.running_run("thread:session").await;
    h.steer(
        "command:session",
        "thread:session",
        "steer_active",
        &run_id,
        json!({"modelSelection": {"instanceId": "codex", "model": "gpt-big"}}),
    )
    .await
    .expect("the steer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:session"))
        .await
        .expect("projection reads");
    let run_model = serde_json::to_value(&projection.runs[0].model_selection).unwrap();
    assert_eq!(run_model["model"], "gpt-big");
    let provider_thread = &projection.provider_threads[0];
    let effects = h.effects("command:session").await;
    let [
        EffectRequest::ProviderTurnRestart {
            session_transition:
                Some(SessionTransition::Replace {
                    replacement_provider_session_id,
                }),
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("a restart that replaces the session: {effects:?}");
    };
    assert_eq!(
        provider_thread.provider_session_id.as_ref(),
        Some(replacement_provider_session_id),
        "the provider thread binds the replacement session"
    );
}

#[tokio::test]
async fn a_rejected_selection_is_refused() {
    let h = harness(
        true,
        Some(ProviderSelectionTransitionPlan::Reject {
            reason: "No such model in this session.".to_owned(),
        }),
    )
    .await;
    let run_id = h.running_run("thread:reject").await;
    let error = h
        .steer(
            "command:reject",
            "thread:reject",
            "steer_active",
            &run_id,
            json!({"modelSelection": {"instanceId": "codex", "model": "gpt-nope"}}),
        )
        .await
        .expect_err("the adapter rejects the selection");
    assert_eq!(
        std::error::Error::source(&error).map(ToString::to_string),
        Some("No such model in this session.".to_owned())
    );
}

#[tokio::test]
async fn a_steer_needs_a_live_session() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:gone").await;
    *h.sessions.runtime.lock() = None;
    let error = h
        .steer(
            "command:gone",
            "thread:gone",
            "steer_active",
            &run_id,
            json!({}),
        )
        .await
        .expect_err("the session is gone");
    assert!(
        std::error::Error::source(&error)
            .is_some_and(|cause| cause.to_string().ends_with("is not active.")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_run_that_is_not_running_cannot_be_steered() {
    let h = harness(true, None).await;
    h.orchestrator
        .dispatch(create_command(
            "command:create",
            "thread:starting",
            "Thread",
        ))
        .await
        .expect("create commits");
    h.orchestrator
        .dispatch(message_command(
            "command:first",
            "thread:starting",
            json!({}),
        ))
        .await
        .expect("the first message starts a run");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:starting"))
        .await
        .expect("projection reads");
    let error = h
        .steer(
            "command:steer",
            "thread:starting",
            "steer_active",
            projection.runs[0].id.as_str(),
            json!({}),
        )
        .await
        .expect_err("a starting run takes no steer");
    assert!(
        std::error::Error::source(&error).is_some_and(|cause| cause
            .to_string()
            .ends_with("is starting and cannot be steered.")),
        "{error:?}"
    );
}

#[tokio::test]
async fn native_maintenance_and_goal_commands_never_steer() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:slash").await;
    for (text, cause) in [
        (
            "/compact",
            "Context compaction must run as a separate turn. Queue it or wait for the active turn to finish.",
        ),
        (
            "/logout",
            "Signing out must run as a separate turn. Queue it or wait for the active turn to finish.",
        ),
        (
            "/goal ship it",
            "Goal commands must run as a separate turn. Queue it or wait for the active turn to finish.",
        ),
    ] {
        let error = h
            .steer(
                &format!("command:slash:{text}"),
                "thread:slash",
                "steer_active",
                &run_id,
                json!({"text": text}),
            )
            .await
            .expect_err("a separate-turn command is refused");
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some(cause.to_owned()),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_provider_change_without_a_resumable_thread_needs_the_handoff_service() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:switch").await;
    let error = h
        .steer(
            "command:switch",
            "thread:switch",
            "steer_active",
            &run_id,
            json!({"modelSelection": {"instanceId": "claudeAgent", "model": "claude-test"}}),
        )
        .await
        .expect_err("a handoff restart is not ported");
    assert!(
        std::error::Error::source(&error)
            .is_some_and(|cause| cause.to_string().contains("context handoff service")),
        "{error:?}"
    );
}
