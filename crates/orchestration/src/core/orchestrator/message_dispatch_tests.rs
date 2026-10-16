//! Start-path tests for `message.dispatch` (`message_dispatch.rs`): a message
//! starts a run, a deferred one waits for its workspace, a model change is
//! applied first, and a message behind a starting run queues. Upstream's
//! `Orchestrator.*.test.ts` drive the same paths through the Effect layer.
//! The fakes stand in for the checkpoint, adapter and project-root seams that
//! T06-git and the T13e turn port wire in.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use turin_contracts::{
    orchestration_v2::{
        OrchestrationV2CheckpointScope, OrchestrationV2CheckpointScopeKind,
        OrchestrationV2ProviderCapabilities, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
    },
    provider_instance::ProviderInstanceId,
};
use turin_persistence::{Db, OrchestrationEventStore};
use turin_provider_core::{
    id_allocator::IdAllocator,
    provider_adapter::{
        ProviderAdapterV2, ProviderAdapterV2Error, ProviderAdapterV2OpenSessionInput,
    },
};

use super::Orchestrator;
use super::services::{
    CheckpointScopes, DispatchSeams, NoRestartRecovery, PrepareRootRunScope, ProjectRoots,
};
use super::tests::{command_id, create_command, message_command, thread};
use crate::core::provider_session_manager::ProviderAdapterLookup;
use crate::pipeline::command_receipt_store::CommandReceiptStore;
use crate::pipeline::effect_outbox::{EffectOutbox, EffectRequest};
use crate::pipeline::event_sink::EventSink;
use crate::pipeline::orchestrator_port::{BoxError, OrchestratorPort};
use crate::projection::store::ProjectionStore;

/// The checkpoint seam with no git behind it: a root scope under the run.
#[derive(Debug, Default)]
pub(super) struct FakeCheckpoints;

#[async_trait]
impl CheckpointScopes for FakeCheckpoints {
    async fn prepare_root_run_scope(
        &self,
        input: PrepareRootRunScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        Ok(OrchestrationV2CheckpointScope {
            id: IdAllocator.checkpoint_scope(&input.thread_id, &format!("root:{}", input.run_id)),
            thread_id: input.thread_id,
            run_id: Some(input.run_id),
            node_id: input.root_node_id,
            parent_scope_id: None,
            provider_thread_id: Some(input.provider_thread_id),
            kind: OrchestrationV2CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: serde_json::from_value(json!(input.cwd))?,
            created_at: input.created_at,
        })
    }

    async fn ensure_scope(
        &self,
        scope: OrchestrationV2CheckpointScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        Ok(scope)
    }
}

/// The project-root read: every project lives at one fixed directory.
#[derive(Debug, Default)]
pub(super) struct FakeProjectRoots;

#[async_trait]
impl ProjectRoots for FakeProjectRoots {
    async fn workspace_root(
        &self,
        _project_id: &turin_contracts::ProjectId,
    ) -> Result<Option<String>, BoxError> {
        Ok(Some("/turin-fixture/project".to_owned()))
    }
}

/// One Codex-like adapter with the full capability set. Sessions and turns
/// are not opened here, so the session and turn methods refuse.
#[derive(Debug)]
pub(super) struct FakeAdapter {
    pub(super) instance: ProviderInstanceId,
    pub(super) capabilities: OrchestrationV2ProviderCapabilities,
    pub(super) driver: turin_contracts::provider_instance::ProviderDriverKind,
    /// What `plan_selection_transition` answers; `None` fails it.
    pub(super) transition:
        Option<turin_provider_core::selection_transition::ProviderSelectionTransitionPlan>,
}

#[async_trait]
impl ProviderAdapterV2 for FakeAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance
    }

    fn driver(&self) -> &turin_contracts::provider_instance::ProviderDriverKind {
        &self.driver
    }

    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(self.capabilities)
    }

    async fn plan_selection_transition(
        &self,
        _input: turin_provider_core::selection_transition::ProviderSelectionTransitionInput,
    ) -> Result<
        turin_provider_core::selection_transition::ProviderSelectionTransitionPlan,
        ProviderAdapterV2Error,
    > {
        if let Some(plan) = &self.transition {
            return Ok(plan.clone());
        }
        Err(ProviderAdapterV2Error::new(
            turin_provider_core::provider_adapter::ProviderAdapterErrorKind::Capabilities {
                driver: self.driver.clone(),
            },
        ))
    }

    async fn open_session(
        &self,
        _input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<
        Arc<dyn turin_provider_core::provider_adapter::ProviderSessionRuntime>,
        ProviderAdapterV2Error,
    > {
        Err(ProviderAdapterV2Error::new(
            turin_provider_core::provider_adapter::ProviderAdapterErrorKind::Capabilities {
                driver: self.driver.clone(),
            },
        ))
    }
}

/// Resolves the one fake instance and nothing else.
pub(super) struct FakeAdapters {
    pub(super) adapter: Arc<dyn ProviderAdapterV2>,
    /// A second provider, for switches and handoffs.
    pub(super) other: Option<Arc<dyn ProviderAdapterV2>>,
}

impl ProviderAdapterLookup for FakeAdapters {
    fn adapter(&self, instance_id: &ProviderInstanceId) -> Option<Arc<dyn ProviderAdapterV2>> {
        std::iter::once(&self.adapter)
            .chain(self.other.iter())
            .find(|adapter| adapter.instance_id() == instance_id)
            .cloned()
    }
}

/// An orchestrator with the seams a start needs, over an in-memory database.
/// The effect outbox is returned too, to read the effects a command queued.
async fn harness() -> (Orchestrator, EffectOutbox) {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let projection = Arc::new(ProjectionStore::new(db.clone()));
    let sink = EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        projection.clone(),
    );
    let adapter: Arc<dyn ProviderAdapterV2> = Arc::new(FakeAdapter {
        instance: serde_json::from_value(json!("codex")).expect("instance fixture"),
        capabilities: crate::pipeline::test_support::default_capabilities(),
        driver: serde_json::from_value(json!("codex")).expect("driver fixture"),
        transition: None,
    });
    let orchestrator = Orchestrator::new(
        projection,
        sink,
        CommandReceiptStore::new(db.clone()),
        Arc::new(NoRestartRecovery),
    )
    .with_dispatch_seams(DispatchSeams {
        checkpoints: Arc::new(FakeCheckpoints),
        adapters: Arc::new(FakeAdapters {
            adapter,
            other: None,
        }),
        project_roots: Arc::new(FakeProjectRoots),
        ..DispatchSeams::default()
    });
    (orchestrator, EffectOutbox::new(db))
}

#[tokio::test]
async fn a_message_starts_a_run_and_queues_its_provider_turn_effect() {
    let (orchestrator, outbox) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:start", "Thread"))
        .await
        .expect("create commits");
    orchestrator
        .dispatch(message_command("command:start", "thread:start", json!({})))
        .await
        .expect("the start commits");

    let projection = orchestrator
        .get_thread_projection(&thread("thread:start"))
        .await
        .expect("projection reads");
    let [run] = projection.runs.as_slice() else {
        panic!("one run starts: {:?}", projection.runs);
    };
    assert_eq!(run.status, OrchestrationV2RunStatus::Starting);
    assert_eq!(run.ordinal.get(), 1);
    assert!(run.workspace_preparation.is_none());
    assert_eq!(projection.provider_threads.len(), 1);
    assert!(projection.provider_threads[0].provider_session_id.is_some());
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::UserMessage { text, .. } if text == "hello"
    )));

    let effects = outbox
        .list_by_command_id(command_id("command:start"))
        .await
        .expect("effects read");
    let [effect] = effects.as_slice() else {
        panic!("one provider turn effect: {effects:?}");
    };
    assert!(matches!(
        &effect.request,
        EffectRequest::ProviderTurnStart { run_id } if run_id == &run.id
    ));
}

#[tokio::test]
async fn a_deferred_start_waits_in_preparing_with_no_turn_effect() {
    let (orchestrator, outbox) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:defer", "Thread"))
        .await
        .expect("create commits");
    orchestrator
        .dispatch(message_command(
            "command:defer",
            "thread:defer",
            json!({ "dispatchMode": { "type": "defer_start" } }),
        ))
        .await
        .expect("the deferred start commits");

    let projection = orchestrator
        .get_thread_projection(&thread("thread:defer"))
        .await
        .expect("projection reads");
    let [run] = projection.runs.as_slice() else {
        panic!("one run is held: {:?}", projection.runs);
    };
    assert_eq!(run.status, OrchestrationV2RunStatus::Preparing);
    // The preparation item shows progress until the workspace is ready.
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::CommandExecution { title, .. }
            if title.as_deref() == Some("Preparing workspace")
    )));
    let effects = outbox
        .list_by_command_id(command_id("command:defer"))
        .await
        .expect("effects read");
    assert!(
        effects.is_empty(),
        "no turn starts before preparation: {effects:?}"
    );
}

#[tokio::test]
async fn a_model_change_on_start_updates_the_thread_before_the_run() {
    let (orchestrator, _) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:model", "Thread"))
        .await
        .expect("create commits");
    orchestrator
        .dispatch(message_command(
            "command:model",
            "thread:model",
            json!({ "modelSelection": { "instanceId": "codex", "model": "gpt-other" } }),
        ))
        .await
        .expect("the start commits");

    let projection = orchestrator
        .get_thread_projection(&thread("thread:model"))
        .await
        .expect("projection reads");
    let model = serde_json::to_value(&projection.thread.model_selection).expect("model encodes");
    assert_eq!(model["model"], "gpt-other");
    let [run] = projection.runs.as_slice() else {
        panic!("one run starts: {:?}", projection.runs);
    };
    assert_eq!(run.model_selection, projection.thread.model_selection);
}

#[tokio::test]
async fn a_message_behind_a_starting_run_queues_without_a_turn_effect() {
    let (orchestrator, outbox) = harness().await;
    orchestrator
        .dispatch(create_command("command:create", "thread:queue", "Thread"))
        .await
        .expect("create commits");
    orchestrator
        .dispatch(message_command("command:first", "thread:queue", json!({})))
        .await
        .expect("the first start commits");
    orchestrator
        .dispatch(message_command(
            "command:second",
            "thread:queue",
            json!({ "text": "second", "messageId": "message:second" }),
        ))
        .await
        .expect("the second message queues");

    let projection = orchestrator
        .get_thread_projection(&thread("thread:queue"))
        .await
        .expect("projection reads");
    let [first, queued] = projection.runs.as_slice() else {
        panic!("two runs: {:?}", projection.runs);
    };
    assert_eq!(first.status, OrchestrationV2RunStatus::Starting);
    assert_eq!(queued.status, OrchestrationV2RunStatus::Queued);
    assert_eq!(queued.ordinal.get(), 2);
    assert_eq!(queued.provider_thread_id, first.provider_thread_id);
    let effects = outbox
        .list_by_command_id(command_id("command:second"))
        .await
        .expect("effects read");
    assert!(
        effects.is_empty(),
        "a queued run waits for its turn: {effects:?}"
    );
}
