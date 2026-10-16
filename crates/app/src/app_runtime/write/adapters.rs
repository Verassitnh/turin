//! Production impls of the narrow seams T13c/T13e define and nobody had
//! implemented yet: server settings, the project directory and roots, the
//! checkpoint scopes and the session lookup. Each is a few lines over a store
//! the composition root already owns.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::ServerSettings;
use turin_contracts::{MessageId, ProjectId, ProviderSessionId, RunId, ThreadId};
use turin_orchestration::core::orchestrator::services::{
    CheckpointScopes, PrepareRootRunScope, ProjectRoots, ProviderInstanceSettings,
};
use turin_orchestration::core::provider_continuation_service::ContinuationThreads;
use turin_orchestration::core::provider_session_manager::ProviderSessionManager;
use turin_orchestration::core::run_finalization_service::{
    RunFinalizationObserver, RunFinalizationRefreshError,
};
use turin_orchestration::pipeline::orchestrator_port::BoxError;
use turin_orchestration::pipeline::seams::{
    ProjectDirectory, ProjectSummary, ProviderSessionLookup, RuntimeTaskStopper,
    ServerSettingsSource, SessionTaskStopper, instance_enabled_for_project,
};
use turin_orchestration::pipeline::thread_management_service::{
    ThreadManagementError, ThreadManagementService,
};
use turin_orchestration::projection::project_store::ProjectStore;
use turin_orchestration::reactors::checkpoint_service::{self, CheckpointService};
use turin_provider_core::provider_host_live::DesktopSettingsHandle;

use turin_contracts::orchestration_v2::OrchestrationV2CheckpointScope;

/// `ServerSettingsService.getSettings`, over the provider host's live settings
/// so a settings write reaches the instance registry and the pipeline alike.
pub struct HostSettings(pub DesktopSettingsHandle);

#[async_trait]
impl ServerSettingsSource for HostSettings {
    async fn get_settings(&self) -> Result<ServerSettings, BoxError> {
        Ok(self.0.settings().await)
    }
}

/// `ProjectStore.get`, for the pipeline's directory and roots seams.
pub struct StoredProjects(pub Arc<ProjectStore>);

#[async_trait]
impl ProjectDirectory for StoredProjects {
    async fn get(&self, project_id: &ProjectId) -> Result<Option<ProjectSummary>, BoxError> {
        let row = self.0.get(project_id.clone(), false).await?;
        Ok(row.map(|row| ProjectSummary {
            project_id: row.project_id,
            workspace_root: row.workspace_root,
        }))
    }
}

#[async_trait]
impl ProjectRoots for StoredProjects {
    async fn workspace_root(&self, project_id: &ProjectId) -> Result<Option<String>, BoxError> {
        Ok(self
            .0
            .get(project_id.clone(), false)
            .await?
            .map(|row| row.workspace_root))
    }
}

/// `CheckpointService.prepareRootRunScope` / `ensureScope`. Both are pure; git
/// work happens at baseline capture and finalization.
pub struct ServiceCheckpointScopes(pub Arc<CheckpointService>);

#[async_trait]
impl CheckpointScopes for ServiceCheckpointScopes {
    async fn prepare_root_run_scope(
        &self,
        input: PrepareRootRunScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        self.0
            .prepare_root_run_scope(checkpoint_service::PrepareRootRunScope {
                thread_id: input.thread_id,
                run_id: input.run_id,
                root_node_id: input.root_node_id,
                provider_thread_id: input.provider_thread_id,
                cwd: input.cwd,
                created_at: input.created_at,
            })
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn ensure_scope(
        &self,
        scope: OrchestrationV2CheckpointScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        self.0
            .ensure_scope(scope)
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `ProviderSessionManagerV2.get`, for `stopTask` on a live session.
pub struct ManagerSessions(pub Arc<ProviderSessionManager>);

#[async_trait]
impl ProviderSessionLookup for ManagerSessions {
    async fn get(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn SessionTaskStopper>>, BoxError> {
        Ok(self
            .0
            .get(provider_session_id)
            .map(|runtime| Arc::new(RuntimeTaskStopper(runtime)) as Arc<dyn SessionTaskStopper>))
    }
}

#[async_trait]
impl ProviderInstanceSettings for HostSettings {
    async fn disabled_for_project(
        &self,
        project_id: &ProjectId,
        instance_id: &ProviderInstanceId,
    ) -> Result<bool, BoxError> {
        let settings = self.0.settings().await;
        Ok(!instance_enabled_for_project(
            &settings,
            project_id,
            instance_id,
        ))
    }
}

/// The workspace refresh after a checkpoint (file tree and VCS status for the
/// UI). The app does not keep a workspace index yet, so there is nothing to
/// refresh.
pub struct NoWorkspaceRefresh;

#[async_trait]
impl RunFinalizationObserver for NoWorkspaceRefresh {
    async fn refresh(
        &self,
        _cwd: &str,
        _thread_id: &ThreadId,
        _run_id: &RunId,
    ) -> Result<(), RunFinalizationRefreshError> {
        Ok(())
    }
}

/// `ContinuationThreads` for the thread-management service: `Arc<ThreadManagementCore>`
/// is private to it, so this delegates through the service's deref.
pub struct ThreadsForContinuations(pub Arc<ThreadManagementService>);

#[async_trait]
impl ContinuationThreads for ThreadsForContinuations {
    async fn records(
        &self,
        thread_id: &ThreadId,
        message_ids: Vec<MessageId>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
        ContinuationThreads::records(&**self.0, thread_id, message_ids).await
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<(), ThreadManagementError> {
        ContinuationThreads::dispatch(&**self.0, command).await
    }
}
