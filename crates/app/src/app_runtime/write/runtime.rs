//! The write-side composition root. Builds, over one [`Stores`]:
//!
//! ```text
//! Providers (host, drivers, instance registry, status registry)
//! ProviderSessionManager ◀─ EventSessionSink ◀─ ProviderEventIngestor ─▶ EventSink
//! RunExecutionService ─▶ ProviderTurnStartService ─┐
//! ProviderTurnControlService, RuntimeRequestService │
//! RunFinalizationService (checkpoint.capture)       ├▶ TurinEffectExecutor ◀─ EffectWorker (daemon)
//! Orchestrator (decider, receipts) ─▶ ThreadManagementService ◀── commands from the UI
//! ```
//!
//! Commands commit through the orchestrator; the sink folds them into the
//! projections the read path serves, so a write shows up in the open feeds.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use futures::StreamExt as _;
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2ServerCommand};
use turin_contracts::settings::ServerSettings;
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::process::ProcessRunner;
use turin_orchestration::core::context_handoff_service::ContextHandoffService;
use turin_orchestration::core::orchestrator::Orchestrator;
use turin_orchestration::core::orchestrator::services::{
    DispatchSeams, ReconcileTrigger, RestartRecovery as _,
};
use turin_orchestration::core::provider_event_ingestor::ProviderEventIngestor;
use turin_orchestration::core::provider_runtime_recovery_service::ProviderRuntimeRecoveryService;
use turin_orchestration::core::provider_session_manager::{
    ProviderSessionManager, ProviderSessionManagerOptions,
};
use turin_orchestration::core::provider_switch_service::ProviderSwitchService;
use turin_orchestration::core::provider_turn_control_service::ProviderTurnControlService;
use turin_orchestration::core::provider_turn_start_service::{
    LocalTurnStartWorkspace, NoPromptCommands, ProjectRootsRuntimePolicy, ProviderTurnStartService,
};
use turin_orchestration::core::restart_continuation_service::{
    LateRestartThreads, NoDelegatedTaskRecovery, RestartContinuationService,
};
use turin_orchestration::core::run_execution_service::{NoAfterTurnRefresh, RunExecutionService};
use turin_orchestration::core::run_finalization_service::RunFinalizationService;
use turin_orchestration::core::runtime_request_service::RuntimeRequestService;
use turin_orchestration::core::session_sink::EventSessionSink;
use turin_orchestration::pipeline::command_receipt_store::CommandReceiptStore;
use turin_orchestration::pipeline::effect_worker::{
    EffectDaemon, EffectDaemonOptions, EffectWorker, EffectWorkerOptions,
};
use turin_orchestration::pipeline::event_sink::{EventSink, StreamOptions};
use turin_orchestration::pipeline::hooks_bridge::NoHooks;
use turin_orchestration::pipeline::orchestrator_port::{DispatchResult, OrchestratorPort};
use turin_orchestration::pipeline::thread_command_executor::ThreadCommandExecutor;
use turin_orchestration::pipeline::thread_management_service::{
    ThreadManagementError, ThreadManagementService,
};
use turin_orchestration::reactors::checkpoint_capture_service::CheckpointCaptureService;
use turin_orchestration::reactors::checkpoint_git::TurinCheckpointGit;
use turin_orchestration::reactors::checkpoint_service::CheckpointService;
use turin_provider_core::provider_adapter_registry::ProviderAdapterRegistry;

use super::adapters::{
    HostSettings, ManagerSessions, NoWorkspaceRefresh, ServiceCheckpointScopes, StoredProjects,
};
use super::effects::TurinEffectExecutor;
use super::providers::Providers;
use crate::app_runtime::Stores;

pub struct WriteRuntime {
    pub stores: Arc<Stores>,
    pub providers: Providers,
    pub orchestrator: Arc<Orchestrator>,
    pub threads: Arc<ThreadManagementService>,
    pub sessions: Arc<ProviderSessionManager>,
    effects: EffectDaemon,
    recovery: Arc<ProviderRuntimeRecoveryService>,
    _terminal_runs: AbortOnDropHandle<()>,
}

impl WriteRuntime {
    /// Builds every service and starts the effect worker and the terminal-run
    /// listener. `settings` seeds the provider host (instance configuration,
    /// binary paths); persisting `settings.json` is a later slice.
    pub async fn build(
        stores: Arc<Stores>,
        data_dir: &Path,
        settings: ServerSettings,
    ) -> anyhow::Result<Arc<Self>> {
        let providers = Providers::build(data_dir, settings).await?;
        let instances = providers.instances();
        let sink = Arc::new(stores.sink.clone());
        let projection = Arc::clone(&stores.projections);
        let host_settings = Arc::new(HostSettings(providers.settings.clone()));
        let projects = Arc::new(StoredProjects(Arc::clone(&stores.projects)));

        // Checkpoints: hidden git refs per run. Non-git workspaces skip capture.
        let checkpoints = Arc::new(CheckpointService::new(Arc::new(TurinCheckpointGit::new(
            Arc::new(GitVcsDriver::new(turin_git::vcs_process::VcsProcess::new(
                ProcessRunner,
            ))),
        ))));
        let capture = Arc::new(CheckpointCaptureService::new(
            Arc::clone(&checkpoints),
            Arc::clone(&sink),
            Arc::clone(&projection),
        ));

        // Provider sessions and the event ingestor behind them. The ingestor
        // gets its own lock executor: `Orchestrator` builds a private one, so
        // the two do not exclude each other yet (docs/handoff/T22.md).
        let ingest_threads = Arc::new(ThreadCommandExecutor::new());
        let ingestor = Arc::new(ProviderEventIngestor::new(
            Arc::clone(&sink),
            projection.clone(),
            ingest_threads,
        ));
        let session_sink = Arc::new(EventSessionSink::new(
            Arc::clone(&sink),
            Arc::clone(&projection),
            ingestor.clone(),
        ));
        let sessions = Arc::new(ProviderSessionManager::new(
            instances.clone(),
            session_sink,
            projection.clone(),
            ProviderSessionManagerOptions::default(),
        ));

        // Turn services.
        let run_execution = Arc::new(RunExecutionService::new(
            checkpoints.clone(),
            sink.clone(),
            ingestor.clone(),
            host_settings.clone(),
            Arc::new(NoAfterTurnRefresh),
        ));
        let turn_start = Arc::new(ProviderTurnStartService::new(
            sink.clone(),
            ContextHandoffService::from_environment(),
            projection.clone(),
            sessions.clone(),
            run_execution,
            Arc::new(ProjectRootsRuntimePolicy::new(projects.clone())),
            Arc::new(NoPromptCommands),
            Arc::new(LocalTurnStartWorkspace::new(projects.clone(), None)),
        ));
        let turn_control = Arc::new(ProviderTurnControlService::new(
            projection.clone(),
            sessions.clone(),
        ));
        let runtime_requests = Arc::new(RuntimeRequestService::new(
            projection.clone(),
            sessions.clone(),
        ));
        let finalization = Arc::new(RunFinalizationService::new(
            capture,
            projection.clone(),
            Arc::new(NoWorkspaceRefresh),
        ));

        // Restart continuation: the orchestrator needs the service at
        // construction and the service needs the thread-management service
        // (which holds the orchestrator), so the threads are bound late.
        let late_threads = LateRestartThreads::new();
        let restart = Arc::new(RestartContinuationService::new(
            host_settings.clone(),
            late_threads.clone(),
            projection.clone(),
            stores.outbox.clone(),
            // Delegated tasks (finalizeAppOwnedSubagent) are not ported yet.
            Arc::new(NoDelegatedTaskRecovery),
        ));

        // The decider.
        let switch_planner = Arc::new(ProviderSwitchService::new(Arc::new(
            ProviderAdapterRegistry::from_instances(instances.clone()),
        )));
        let orchestrator = Arc::new(
            Orchestrator::new(
                projection.clone(),
                stores.sink.clone(),
                CommandReceiptStore::new(stores.db.clone()),
                restart.clone(),
            )
            .with_dispatch_seams(DispatchSeams {
                checkpoints: Arc::new(ServiceCheckpointScopes(checkpoints)),
                adapters: instances.clone(),
                project_roots: projects.clone(),
                sessions: sessions.clone(),
                switch_planner,
                provider_instances: host_settings.clone(),
            }),
        );
        let threads = Arc::new(ThreadManagementService::new(
            ThreadCommandExecutor::new(),
            orchestrator.clone() as Arc<dyn OrchestratorPort>,
            Arc::new(ManagerSessions(sessions.clone())),
            host_settings.clone(),
            CommandReceiptStore::new(stores.db.clone()),
            projects,
            Arc::new(NoHooks),
        ));

        late_threads.bind(threads.clone());

        // Startup order (upstream serverRuntimeStartup): reconcile runs that
        // were live when the last process died, settle delegated tasks, then
        // start the effect worker.
        let recovery = Arc::new(ProviderRuntimeRecoveryService::new(
            host_settings.clone(),
            projection.clone(),
            sink.clone(),
        ));
        match recovery.recover().await {
            Ok(summary) => tracing::info!(?summary, "provider runtime recovery"),
            Err(error) => tracing::warn!(%error, "provider runtime recovery failed"),
        }
        restart.recover_delegated_tasks().await;

        // Effects: claimed from the outbox the sink enqueues into.
        let executor = Arc::new(TurinEffectExecutor {
            turn_start,
            turn_control,
            runtime_requests,
            sessions: sessions.clone(),
            finalization,
            threads: threads.clone(),
            restart,
        });
        let worker = EffectWorker::new(
            stores.outbox.clone(),
            executor,
            EffectWorkerOptions::default(),
        );
        let effects = worker.spawn_daemon(EffectDaemonOptions::default());

        let terminal_runs = spawn_terminal_run_listener(&stores.sink, orchestrator.clone())
            .await
            .context("subscribing to run updates")?;
        match orchestrator.resume_queued_runs().await {
            Ok(resumed) if resumed > 0 => tracing::info!(resumed, "resumed queued runs"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to resume queued runs"),
        }

        Ok(Arc::new(Self {
            stores,
            providers,
            orchestrator,
            threads,
            sessions,
            effects,
            recovery,
            _terminal_runs: terminal_runs,
        }))
    }

    /// Shutdown order (upstream): stop the effect worker, let recovery queue
    /// continuations for live runs, close provider sessions, then reconcile.
    /// Failures are warnings, not crashes.
    pub async fn shutdown(&self) {
        self.effects.shutdown();
        if let Err(error) = self.recovery.prepare_for_shutdown().await {
            tracing::warn!(%error, "preparing for shutdown failed");
        }
        self.sessions.shutdown().await;
        match self.recovery.reconcile(ReconcileTrigger::Shutdown).await {
            Ok(summary) => tracing::info!(?summary, "shutdown reconciliation"),
            Err(error) => tracing::warn!(%error, "shutdown reconciliation failed"),
        }
    }

    /// One command through the thread-management chokepoint (provider
    /// instance check, workspace binding), then the orchestrator's receipt
    /// and decider.
    pub async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        self.threads.dispatch(command).await
    }
}

/// Upstream `handleTerminalRun` listener: every terminal `run.updated` (except
/// runtime reconciliation) may unblock the thread's next queued run. History
/// before this boot is not replayed.
async fn spawn_terminal_run_listener(
    sink: &EventSink,
    orchestrator: Arc<Orchestrator>,
) -> anyhow::Result<AbortOnDropHandle<()>> {
    let after = sink.latest_sequence(None).await?;
    let mut stream = sink
        .stream(StreamOptions {
            after_sequence: Some(after),
            event_type: Some("run.updated".to_owned()),
            ..StreamOptions::default()
        })
        .await?;
    Ok(AbortOnDropHandle::new(tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            let Ok(stored) = item else { break };
            if stored
                .command_id
                .as_ref()
                .is_some_and(|id| id.as_str().starts_with("command:runtime-reconcile:"))
            {
                continue;
            }
            if let OrchestrationV2DomainEvent::RunUpdated { payload, .. } = &stored.event
                && is_terminal(payload.status)
            {
                orchestrator.handle_terminal_run(payload).await;
            }
        }
    })))
}

fn is_terminal(status: turin_contracts::orchestration_v2::OrchestrationV2RunStatus) -> bool {
    turin_orchestration::pipeline::thread_management_service::is_terminal_run_status(status)
}
