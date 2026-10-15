//! Narrow traits for the collaborators of the pipeline services that live
//! outside this crate (server settings, the project store, the provider
//! session manager, text generation, hooks). Each names exactly what the
//! pipeline reads, so the owners can implement them without this crate
//! depending on theirs.

use super::orchestrator_port::BoxError;
use async_trait::async_trait;
use std::sync::Arc;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::ServerSettings;
use turin_contracts::{ProjectId, ProviderSessionId};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2StopTaskInput, ProviderSessionRuntime,
};

/// `ServerSettingsService.getSettings`.
#[async_trait]
pub trait ServerSettingsSource: Send + Sync {
    async fn get_settings(&self) -> Result<ServerSettings, BoxError>;
}

/// The slice of `ProjectStore.get` the pipeline reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub workspace_root: String,
}

/// `ProjectStoreV2.get` (T13a's `projection::project_store::ProjectStore`).
#[async_trait]
pub trait ProjectDirectory: Send + Sync {
    async fn get(&self, project_id: &ProjectId) -> Result<Option<ProjectSummary>, BoxError>;
}

/// The one capability the pipeline uses of a live provider session runtime:
/// `stopTask` (optional per adapter). [`RuntimeTaskStopper`] adapts any
/// provider-core `ProviderSessionRuntime`.
#[async_trait]
pub trait SessionTaskStopper: Send + Sync {
    /// `Ok(false)` when the adapter has no task-stop hook.
    async fn stop_task(&self, input: ProviderAdapterV2StopTaskInput) -> Result<bool, BoxError>;
}

pub struct RuntimeTaskStopper(pub Arc<dyn ProviderSessionRuntime>);

#[async_trait]
impl SessionTaskStopper for RuntimeTaskStopper {
    async fn stop_task(&self, input: ProviderAdapterV2StopTaskInput) -> Result<bool, BoxError> {
        match self.0.stop_task(input) {
            Some(future) => future
                .await
                .map(|()| true)
                .map_err(|error| Box::new(error) as BoxError),
            None => Ok(false),
        }
    }
}

/// `ProviderSessionManagerV2.get` (T13c).
#[async_trait]
pub trait ProviderSessionLookup: Send + Sync {
    async fn get(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<Option<Arc<dyn SessionTaskStopper>>, BoxError>;
}

/// Whether the machine-wide or project-level settings enable an instance.
pub fn instance_enabled_for_project(
    settings: &ServerSettings,
    project_id: &ProjectId,
    instance_id: &ProviderInstanceId,
) -> bool {
    turin_contracts::settings::resolve_project_provider_instance_enabled(
        &settings.provider_instances,
        &settings.project_settings_overrides,
        Some(project_id),
        instance_id,
    )
}
