//! The ports `PullRequestService` reads the rest of the server through. Each one mirrors a call
//! site of `PullRequestService.ts` exactly; the orchestration crate implements them.
//!
//! Provider refinement (`SourceControlProviderRegistry.resolveHandle`) needs no port of its own:
//! it is the crate's [`ResolveProviderContext`], which the source control registry implements.

use async_trait::async_trait;
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_project::OrchestrationProjectShell;

use crate::source_control_core::Cause;

/// `ProjectService.listShells` / `ProjectService.getShell`: the workspace's projects.
#[async_trait]
pub trait PullRequestProjectSource: Send + Sync {
    /// Active project shells, in creation order. `project_ids` narrows the list to those projects
    /// (`listShells({ projectIds })`); `None` is every project (`listShells()`). The error is
    /// `ProjectOperationError`, reported to the reader as "The project list could not be read."
    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, Cause>;

    /// `getShell(projectId)`; `Ok(None)` for an unknown project.
    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, Cause>;
}

/// `RepositoryIdentityResolver.resolve(cwd, { refresh })`: the identity of the checkout's remote,
/// for a project whose shell has none cached. `None` when the directory has no usable remote.
/// `refresh` bypasses the resolver's cache.
#[async_trait]
pub trait RepositoryIdentityResolver: Send + Sync {
    async fn resolve(&self, workspace_root: &str, refresh: bool) -> Option<RepositoryIdentity>;
}

/// `ServerSettingsService.getSettings` followed by
/// `resolveProjectSettings(settings, projectId).settings.removeAgentCreditsOnMerge`: whether a
/// merge from this project strips agent credits from its message.
#[async_trait]
pub trait PullRequestSettings: Send + Sync {
    /// The error is `ServerSettingsError`, reported as "Could not read merge settings."
    async fn remove_agent_credits_on_merge(&self, project_id: &ProjectId) -> Result<bool, Cause>;
}
