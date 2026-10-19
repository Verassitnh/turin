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
//!
//! Port of upstream/apps/server/src/serverRuntimeStartup.ts (startup order only).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use futures::StreamExt as _;
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::orchestration_v2::{OrchestrationV2Command, OrchestrationV2DomainEvent};
use turin_contracts::settings::ServerSettings;
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::process::ProcessRunner;
use turin_orchestration::core::context_handoff_service::ContextHandoffService;
use turin_orchestration::core::orchestrator::Orchestrator;
use turin_orchestration::core::orchestrator::delegated_task::LateDelegatedTaskRecovery;
use turin_orchestration::core::orchestrator::services::{
    DispatchSeams, ReconcileTrigger, RestartRecovery as _,
};
use turin_orchestration::core::provider_continuation_service::ProviderContinuationService;
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
    LateRestartThreads, RestartContinuationService,
};
use turin_orchestration::core::run_execution_service::{NoAfterTurnRefresh, RunExecutionService};
use turin_orchestration::core::run_finalization_service::RunFinalizationService;
use turin_orchestration::core::runtime_request_service::RuntimeRequestService;
use turin_orchestration::core::session_sink::EventSessionSink;
use turin_orchestration::pipeline::attachment_claims::AttachmentClaims;
use turin_orchestration::pipeline::command_receipt_store::CommandReceiptStore;
use turin_orchestration::pipeline::effect_worker::{
    EffectDaemon, EffectDaemonOptions, EffectWorker, EffectWorkerOptions,
};
use turin_orchestration::pipeline::event_sink::{EventSink, StreamOptions};
use turin_orchestration::pipeline::hooks_bridge::NoHooks;
use turin_orchestration::pipeline::orchestrator_port::{DispatchResult, OrchestratorPort};
use turin_orchestration::pipeline::thread_command_executor::ThreadCommandExecutor;
use turin_orchestration::pipeline::thread_management_service::ThreadManagementService;
use turin_orchestration::pipeline::thread_message_intake::{IntakeError, ThreadMessageIntake};
use turin_orchestration::reactors::checkpoint_capture_service::CheckpointCaptureService;
use turin_orchestration::reactors::checkpoint_git::TurinCheckpointGit;
use turin_orchestration::reactors::checkpoint_service::CheckpointService;
use turin_orchestration::reactors::workspace_claims::ProjectionWorkspaceClaims;
use turin_provider_core::id_allocator::IdAllocator;
use turin_provider_core::provider_adapter_registry::ProviderAdapterRegistry;
use turin_provider_core::provider_continuation_requests::ProviderContinuationRequests;

use super::adapters::{
    HostSettings, IntakeThreads, ManagerSessions, NoWorkspaceRefresh, ServiceCheckpointScopes,
    StoredProjects, ThreadsForContinuations,
};
use super::effects::TurinEffectExecutor;
use super::providers::Providers;
use crate::app_runtime::Stores;

pub struct WriteRuntime {
    pub stores: Arc<Stores>,
    pub providers: Providers,
    pub orchestrator: Arc<Orchestrator>,
    pub threads: Arc<ThreadManagementService>,
    /// Claims staged uploads before a message reaches the orchestrator.
    intake: ThreadMessageIntake,
    /// Where uploads are staged and claimed (`<data dir>/userdata/attachments`).
    pub attachments_dir: std::path::PathBuf,
    /// `@` path completion over a workspace.
    pub workspace_entries: Arc<turin_git::workspace::workspace_entries::WorkspaceEntries>,
    /// The Scratch project ("No project") and its per-thread folders.
    pub managed_folders: Arc<turin_projects::managed_project_folders::ManagedProjectFolders>,
    pub sessions: Arc<ProviderSessionManager>,
    pub provider_auth: Arc<turin_providers::provider_auth_service::ProviderAuthService>,
    pub provider_installation: Arc<turin_providers::provider_installation::ProviderInstallation>,
    pub provider_maintenance:
        Arc<turin_providers::provider_maintenance_runner::ProviderMaintenanceRunner>,
    pub server_settings: Arc<turin_runtime::server_settings::ServerSettingsService>,
    pub keybindings: Arc<turin_runtime::keybindings::Keybindings>,
    pub source_control_host: Arc<turin_git::server_source_control_host::ServerSourceControlHost>,
    pub source_control:
        Arc<turin_source_control::source_control::provider_registry::SourceControlProviderRegistry>,
    pub source_control_discovery:
        Arc<turin_source_control::source_control::discovery::SourceControlDiscovery>,
    pub git: Arc<turin_git::git_vcs_driver_core::GitVcsDriverCore>,
    /// Reads checkpoint refs for the diff panel.
    pub checkpoint_store: Arc<turin_git::checkpointing::checkpoint_store::CheckpointStore>,
    pub git_manager: Arc<turin_git::git_manager::GitManager>,
    pub pull_requests: Arc<turin_source_control::pull_request::service::PullRequestService>,
    pub review_guides: Arc<
        turin_source_control::pull_request::review_guide_service::PullRequestReviewGuideService,
    >,
    pub usage: Arc<turin_usage::usage_service::UsageService>,
    pub storage_cleanup: Arc<turin_runtime::storage_cleanup::StorageCleanup>,
    pub config: turin_runtime::config::ServerConfig,
    _cleanup_schedule: AbortOnDropHandle<()>,
    pub(crate) usage_readers_available: bool,
    pub usage_history_drivers: Vec<String>,
    settings_sync: Arc<tokio::sync::Mutex<()>>,
    _settings_watcher: Option<turin_runtime::server_settings::service::SettingsWatcher>,
    _keybindings_watcher: Option<turin_runtime::keybindings::KeybindingsWatcher>,
    _settings_feed: AbortOnDropHandle<()>,
    effects: EffectDaemon,
    _continuations: ProviderContinuationService,
    recovery: Arc<ProviderRuntimeRecoveryService>,
    _terminal_runs: AbortOnDropHandle<()>,
}

impl WriteRuntime {
    /// Builds every service and starts the effect worker and the terminal-run
    /// listener. `settings` seeds new data directories; subsequent settings
    /// reads and patches use the persistent service and its file watcher.
    pub async fn build(
        stores: Arc<Stores>,
        data_dir: &Path,
        settings: ServerSettings,
    ) -> anyhow::Result<Arc<Self>> {
        let (server_settings, settings_watcher) =
            super::settings::open_settings(data_dir, stores.db.clone(), &settings).await?;
        let mut settings_changes = server_settings.subscribe();
        let settings = super::settings::effective_settings(server_settings.get_settings().await?);
        let mut providers = Providers::build(data_dir, settings).await?;
        let keybindings =
            turin_runtime::keybindings::Keybindings::new(data_dir.join("keybindings.json"));
        let keybindings_watcher = keybindings.start().await.context("starting keybindings")?;
        let mut config = turin_runtime::config::ServerConfig::new_desktop(
            std::env::current_dir().context("reading runtime working directory")?,
            data_dir.to_path_buf(),
            None,
        )
        .await
        .context("preparing runtime storage directories")?;
        config.paths.db_path = crate::app_runtime::data_dir::database_path(data_dir);
        config.paths.settings_path = server_settings.settings_path().to_path_buf();
        config.paths.keybindings_config_path = keybindings.config_path().to_path_buf();
        let storage_cleanup = Arc::new(turin_runtime::storage_cleanup::StorageCleanup::files_only(
            config.clone(),
            turin_runtime::storage_cleanup::CleanupHost::new(
                providers.settings.settings().await,
                Vec::new(),
                Vec::new(),
            ),
            "Managed worktree cleanup unavailable: live workspace lease and provider-session snapshot adapters are not connected.",
        ));
        let cleanup_schedule = {
            let cleanup = storage_cleanup.clone();
            AbortOnDropHandle::new(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    cleanup.run_automatic().await;
                }
            }))
        };
        let settings_sync = Arc::new(tokio::sync::Mutex::new(()));
        let settings_feed = {
            let service = server_settings.clone();
            let desktop = providers.settings.clone();
            let sync = settings_sync.clone();
            let cleanup = storage_cleanup.clone();
            AbortOnDropHandle::new(tokio::spawn(async move {
                while settings_changes.recv().await.is_some() {
                    let _guard = sync.lock().await;
                    match service.get_settings().await {
                        Ok(settings) => {
                            cleanup.note_settings(settings.clone());
                            desktop
                                .update_settings(super::settings::effective_settings(settings))
                                .await
                        }
                        Err(error) => {
                            tracing::warn!(%error, "cannot apply settings to provider host")
                        }
                    }
                }
            }))
        };
        let source_control = super::source_control::SourceControlServices::build(
            &stores,
            data_dir,
            providers.settings.clone(),
            providers.registry.clone(),
            providers.instances(),
        )
        .await;
        let usage_history_drivers = providers
            .usage_readers
            .iter()
            .map(|reader| reader.driver_kind.clone())
            .collect();
        let usage_readers_available = !providers.usage_readers.is_empty();
        let usage = Arc::new(turin_usage::usage_service::UsageService::new(
            turin_usage::usage_service::UsageServiceConfig {
                state_dir: data_dir.join("userdata"),
                readers: std::mem::take(&mut providers.usage_readers),
                host_env: std::env::vars().collect(),
                home_dir: std::env::var("HOME").unwrap_or_default(),
                rates_url: turin_usage::usage_service::LITELLM_RATES_URL.to_owned(),
                clock: turin_usage::usage_service::system_clock(),
            },
        ));
        let instances = providers.instances();
        let sink = Arc::new(stores.sink.clone());
        let projection = Arc::clone(&stores.projections);
        let host_settings = Arc::new(HostSettings(providers.settings.clone()));
        let projects = Arc::new(StoredProjects(Arc::clone(&stores.projects)));

        // Checkpoints: hidden git refs per run. Non-git workspaces skip capture.
        let checkpoint_driver = Arc::new(GitVcsDriver::new(
            turin_git::vcs_process::VcsProcess::new(ProcessRunner),
        ));
        let checkpoint_store = Arc::new(
            turin_git::checkpointing::checkpoint_store::CheckpointStore::new(
                checkpoint_driver.clone(),
            ),
        );
        let checkpoint_service = Arc::new(CheckpointService::new(Arc::new(
            TurinCheckpointGit::new(checkpoint_driver),
        )));
        let capture = Arc::new(CheckpointCaptureService::new(
            Arc::clone(&checkpoint_service),
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

        let provider_auth = Arc::new(
            turin_providers::provider_auth_service::ProviderAuthService::new(
                instances.clone(),
                projection.clone(),
                sessions.clone(),
            )
            .with_callback_mode(turin_provider_core::auth::ProviderAuthCallbackMode::Server),
        );

        // Turn services.
        let run_execution = Arc::new(RunExecutionService::new(
            checkpoint_service.clone(),
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
        let late_delegated = LateDelegatedTaskRecovery::new();
        let restart = Arc::new(RestartContinuationService::new(
            host_settings.clone(),
            late_threads.clone(),
            projection.clone(),
            stores.outbox.clone(),
            late_delegated.clone(),
        ));

        let continuation_requests = Arc::new(ProviderContinuationRequests::queue());

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
                checkpoints: Arc::new(ServiceCheckpointScopes(checkpoint_service.clone())),
                adapters: instances.clone(),
                project_roots: projects.clone(),
                sessions: sessions.clone(),
                switch_planner,
                provider_instances: host_settings.clone(),
                // `checkpoint.rollback` file restore: without these it fails closed.
                workspace_claims: Arc::new(ProjectionWorkspaceClaims::new(
                    projection.clone(),
                    Arc::clone(&stores.projects),
                )),
                // Wake offers go to the continuation service started below.
                continuation_requests: continuation_requests.clone(),
            }),
        );
        let attachments_dir = std::path::PathBuf::from(&providers.host.paths.attachments_dir);
        let thread_executor = ThreadCommandExecutor::new();
        let threads = Arc::new(ThreadManagementService::new(
            thread_executor.clone(),
            orchestrator.clone() as Arc<dyn OrchestratorPort>,
            Arc::new(ManagerSessions(sessions.clone())),
            host_settings.clone(),
            CommandReceiptStore::new(stores.db.clone()),
            projects.clone(),
            Arc::new(NoHooks),
        ));

        let intake = ThreadMessageIntake::new(
            AttachmentClaims::new(attachments_dir.clone()),
            Arc::new(IntakeThreads(threads.clone())),
        );
        late_threads.bind(threads.clone());
        let continuations = ProviderContinuationService::start(
            IdAllocator,
            continuation_requests,
            Arc::new(ThreadsForContinuations(threads.clone())),
        );
        late_delegated.bind(orchestrator.clone());

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
            projects: projects.clone(),
            settings: host_settings.clone(),
            text_generation: Arc::new(super::text_generation::build(
                &providers.settings.settings().await,
                instances.clone(),
            )),
            rollback: Arc::new(super::rollback::build(super::rollback::RollbackSources {
                orchestrator: orchestrator.clone(),
                projections: Arc::clone(&projection),
                projects: Arc::clone(&stores.projects),
                stored_projects: projects.clone(),
                checkpoints: checkpoint_service.clone(),
                sink: sink.clone(),
                sessions: sessions.clone(),
                thread_commands: thread_executor,
            })),
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

        let managed_folders = super::managed_folders::build(Arc::clone(&stores), data_dir);
        let created_pr_linker = source_control.created_pr_linker.clone();
        let runtime = Arc::new(Self {
            storage_cleanup,
            config,
            _cleanup_schedule: cleanup_schedule,
            provider_auth,
            provider_installation: providers.installation.clone(),
            provider_maintenance: providers.maintenance.clone(),
            server_settings,
            keybindings,
            source_control_host: source_control.host,
            source_control: source_control.providers,
            source_control_discovery: source_control.discovery,
            git: source_control.git,
            git_manager: source_control.git_manager,
            pull_requests: source_control.pull_requests,
            review_guides: source_control.review_guides,
            usage,
            usage_readers_available,
            usage_history_drivers,
            settings_sync,
            _settings_watcher: settings_watcher,
            _keybindings_watcher: keybindings_watcher,
            _settings_feed: settings_feed,
            stores,
            providers,
            orchestrator,
            threads,
            intake,
            attachments_dir,
            workspace_entries: super::path_search::workspace_entries(),
            managed_folders,
            checkpoint_store,
            sessions,
            effects,
            _continuations: continuations,
            recovery,
            _terminal_runs: terminal_runs,
        });
        created_pr_linker.bind(&runtime);
        Ok(runtime)
    }

    /// Publish the latest persisted settings. Read under the same bridge gate as
    /// file reloads so an older notification cannot replace a newer patch.
    #[allow(clippy::result_large_err)]
    pub async fn sync_provider_settings(
        &self,
    ) -> Result<ServerSettings, turin_contracts::settings::ServerSettingsError> {
        let _guard = self.settings_sync.lock().await;
        let settings =
            super::settings::effective_settings(self.server_settings.get_settings().await?);
        self.storage_cleanup.note_settings(settings.clone());
        self.providers
            .settings
            .update_settings(settings.clone())
            .await;
        self.providers.instances().reconcile(
            turin_provider_core::provider_instance_registry_hydration::derive_provider_instance_config_map(&settings, &self.providers.defaults),
        ).await;
        Ok(settings)
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

    /// One client command through the message intake (attachment claims),
    /// the thread-management chokepoint (provider instance check, workspace
    /// binding), then the orchestrator's receipt and decider.
    pub async fn dispatch(
        &self,
        command: OrchestrationV2Command,
    ) -> Result<DispatchResult, IntakeError> {
        self.intake.dispatch_command(command).await
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
