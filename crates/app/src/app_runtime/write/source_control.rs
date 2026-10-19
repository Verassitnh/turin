//! Live source-control composition over the runtime's settings, projects and DB.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::{ProjectId, provider_instance::ProviderInstanceId};
use turin_git::git_manager::GitManager;
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::git_vcs_driver_core::GitVcsDriverCore;
use turin_git::process::ProcessRunner;
use turin_git::server_source_control_host::{ServerSourceControlHost, SettingsGetter};
use turin_git::source_control_bridge::SourceControlChangeRequestHost;
use turin_git::vcs_driver_registry::VcsDriverRegistry;
use turin_git::vcs_process::VcsProcess;
use turin_git::vcs_project_config::VcsProjectConfig;
use turin_orchestration::projection::project_store::ProjectStore;
use turin_provider_core::provider_host_live::DesktopSettingsHandle;
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::server_provider::ServerProvider;
use turin_provider_core::text_generation::{self as generation, ProviderTextGeneration};
use turin_providers::provider_registry::ProviderRegistry;
use turin_source_control::pull_request::provider_registry::PullRequestProviderRegistry;
use turin_source_control::pull_request::read_cache::PullRequestReadCache;
use turin_source_control::pull_request::review_guide_input::{
    ReviewGuideGenerationInput, ReviewGuideStackLayerState,
};
use turin_source_control::pull_request::review_guide_service::*;
use turin_source_control::pull_request::service::ports::*;
use turin_source_control::pull_request::service::{
    PullRequestService, PullRequestServiceDependencies,
};
use turin_source_control::source_control::built_in_drivers::build_built_in_drivers;
use turin_source_control::source_control::discovery::SourceControlDiscovery;
use turin_source_control::source_control::provider_registry::{
    SourceControlProviderRegistration, SourceControlProviderRegistry,
};
use turin_source_control::source_control_core::Cause;

use super::created_pr_linker::LateCreatedPullRequestLinker;
use crate::app_runtime::Stores;

pub struct SourceControlServices {
    pub host: Arc<ServerSourceControlHost>,
    pub providers: Arc<SourceControlProviderRegistry>,
    pub discovery: Arc<SourceControlDiscovery>,
    pub git: Arc<GitVcsDriverCore>,
    pub git_manager: Arc<GitManager>,
    /// Detects repositories, prunes and creates worktrees (`GitWorkflowService`).
    pub git_workflow: Arc<turin_git::git_workflow_service::GitWorkflowService>,
    /// Bound to the runtime once it exists (`WriteRuntime::build`).
    pub created_pr_linker: Arc<LateCreatedPullRequestLinker>,
    pub pull_requests: Arc<PullRequestService>,
    pub review_guides: Arc<PullRequestReviewGuideService>,
}

impl SourceControlServices {
    pub async fn build(
        stores: &Stores,
        data_dir: &Path,
        settings: DesktopSettingsHandle,
        providers: Arc<ProviderRegistry>,
        instances: Arc<ProviderInstanceRegistry>,
    ) -> Self {
        let clock = turin_source_control::source_control_core::clock::system_clock();
        let cwd = turin_source_control::shared::host_process::working_directory();
        let settings_get: SettingsGetter = {
            let settings = settings.clone();
            Arc::new(move || {
                let settings = settings.clone();
                Box::pin(async move { Ok(settings.settings().await) })
            })
        };
        let git = Arc::new(GitVcsDriverCore::new());
        let host = Arc::new(ServerSourceControlHost::new(
            settings_get,
            Arc::new(VcsProcess::new(ProcessRunner)),
            git.clone(),
            Arc::new(VcsDriverRegistry::new(
                Arc::new(VcsProjectConfig::new()),
                Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
            )),
        ));
        let built = build_built_in_drivers(
            host.clone(),
            turin_source_control::shared::host_process::process_environment(),
            clock.clone(),
            Arc::new(turin_source_control::github::api::ReqwestGitHubHttp::new()),
        )
        .await;
        let mut registrations = Vec::new();
        let mut pr_providers = Vec::new();
        for driver in built.drivers {
            registrations.push(SourceControlProviderRegistration {
                kind: driver.kind,
                provider: driver.instance.source_control,
                discovery: driver.instance.discovery,
            });
            pr_providers.extend(driver.instance.pull_requests);
        }
        let source_control = Arc::new(SourceControlProviderRegistry::with_providers(
            registrations,
            host.clone(),
            clock.clone(),
            cwd.clone(),
        ));
        let discovery = Arc::new(SourceControlDiscovery::new(
            host.clone(),
            source_control.clone(),
            cwd,
        ));
        let projects = Arc::new(LiveProjects(stores.projects.clone()));
        let live_settings = Arc::new(LiveSettings {
            settings: settings.clone(),
            providers,
        });
        let identities = Arc::new(LiveIdentities(
            turin_projects::repository_identity_resolver::RepositoryIdentityResolver::new(
                Arc::new(ProcessRunner),
                Default::default(),
            ),
        ));
        let pull_requests = Arc::new(PullRequestService::new(PullRequestServiceDependencies {
            registry: PullRequestProviderRegistry::from_providers(pr_providers),
            settings: live_settings.clone(),
            projects: projects.clone(),
            repository_identities: identities,
            provider_context: source_control.clone(),
            rate_limits: built.github_dependencies.limits,
            db: stores.db.clone(),
            read_cache: Arc::new(
                PullRequestReadCache::file_backed(&data_dir.join("caches"), clock.clone()).await,
            ),
            clock: clock.clone(),
        }));
        let review_guides = Arc::new(PullRequestReviewGuideService::new(
            PullRequestReviewGuideDependencies {
                pull_requests: pull_requests.clone(),
                db: stores.db.clone(),
                projects,
                settings: live_settings,
                generator: Arc::new(LiveGuideGenerator {
                    settings,
                    instances,
                }),
                host: host.clone(),
                clock,
                worktrees_dir: data_dir.join("userdata/worktrees"),
            },
        ));
        let created_pr_linker = LateCreatedPullRequestLinker::new();
        let mut git_manager = GitManager::with_host(
            git.clone(),
            Arc::new(SourceControlChangeRequestHost::new(source_control.clone())),
        );
        git_manager.set_linker(created_pr_linker.clone());
        let git_manager = Arc::new(git_manager);
        let git_workflow = Arc::new(turin_git::git_workflow_service::GitWorkflowService::new(
            Arc::new(VcsDriverRegistry::new(
                Arc::new(VcsProjectConfig::new()),
                Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
            )),
            git.clone(),
            git_manager.clone(),
        ));
        Self {
            host,
            providers: source_control,
            discovery,
            git,
            git_manager,
            git_workflow,
            created_pr_linker,
            pull_requests,
            review_guides,
        }
    }
}

struct LiveProjects(Arc<ProjectStore>);

#[async_trait]
impl PullRequestProjectSource for LiveProjects {
    async fn list_shells(
        &self,
        ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, Cause> {
        self.0
            .list_shells(ids.map(<[ProjectId]>::to_vec))
            .await
            .map_err(|error| Arc::new(error) as Cause)
    }
    async fn get_shell(&self, id: &ProjectId) -> Result<Option<OrchestrationProjectShell>, Cause> {
        self.0
            .get_shell(id.clone())
            .await
            .map_err(|error| Arc::new(error) as Cause)
    }
}

#[async_trait]
impl ReviewGuideProjects for LiveProjects {
    async fn get_shell(&self, id: &ProjectId) -> Option<ReviewGuideProject> {
        match self.0.get_shell(id.clone()).await {
            Ok(project) => project.map(|project| ReviewGuideProject {
                workspace_root: project.workspace_root.to_string(),
                repository_identity: project.repository_identity.flatten(),
            }),
            Err(error) => {
                tracing::warn!(%error, "reading review guide project");
                None
            }
        }
    }
}

struct LiveIdentities(turin_projects::repository_identity_resolver::RepositoryIdentityResolver);
#[async_trait]
impl RepositoryIdentityResolver for LiveIdentities {
    async fn resolve(&self, cwd: &str, refresh: bool) -> Option<RepositoryIdentity> {
        self.0
            .resolve(
                cwd,
                turin_projects::repository_identity_resolver::ResolveOptions { refresh },
            )
            .await
    }
}

struct LiveSettings {
    settings: DesktopSettingsHandle,
    providers: Arc<ProviderRegistry>,
}

fn project_settings(
    settings: &turin_contracts::settings::ServerSettings,
    id: &ProjectId,
) -> turin_contracts::settings::ServerSettings {
    turin_shared::project_settings::resolve_project_settings(
        settings,
        Some(id),
        None,
        &|instances, enabled, selection| {
            turin_shared::server_settings::fallback_text_generation_model_selection(
                instances,
                |id, _| enabled(id),
                true,
                selection,
            )
        },
    )
    .settings
}

#[async_trait]
impl PullRequestSettings for LiveSettings {
    async fn remove_agent_credits_on_merge(&self, id: &ProjectId) -> Result<bool, Cause> {
        Ok(project_settings(&self.settings.settings().await, id).remove_agent_credits_on_merge)
    }
}

struct ProviderStatus(ServerProvider);
impl turin_shared::server_settings::ProviderStatus for ProviderStatus {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.0.instance_id
    }
    fn enabled(&self) -> bool {
        self.0.enabled
    }
    fn is_available(&self) -> bool {
        self.0.is_available()
    }
    fn is_text_generation_capable(&self) -> bool {
        self.0.is_text_generation_capable()
    }
}

#[async_trait]
impl ReviewGuideSettings for LiveSettings {
    async fn review_guides_enabled(&self) -> Result<bool, String> {
        Ok(self.settings.settings().await.review_guides_enabled)
    }
    async fn for_project(&self, id: &ProjectId) -> Result<ReviewGuideProjectSettings, String> {
        let settings = project_settings(&self.settings.settings().await, id);
        let providers: Vec<_> = self
            .providers
            .get_providers()
            .into_iter()
            .map(ProviderStatus)
            .collect();
        Ok(ReviewGuideProjectSettings {
            model_selection: turin_shared::server_settings::resolve_review_guide_model_selection(
                &settings,
                Some(&providers),
                Some(id),
            ),
            instructions: Some(settings.review_guide_instructions.as_str().to_owned())
                .filter(|text| !text.trim().is_empty()),
        })
    }
}

struct LiveGuideGenerator {
    settings: DesktopSettingsHandle,
    instances: Arc<ProviderInstanceRegistry>,
}
#[async_trait]
impl PullRequestReviewGuideGenerator for LiveGuideGenerator {
    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<generation::ReviewGuideGenerationResult, ReviewGuideGenerationError> {
        let generator =
            super::text_generation::build(&self.settings.settings().await, self.instances.clone());
        generator
            .generate_review_guide(generation::ReviewGuideGenerationInput {
                cwd: input.cwd,
                number: input.number,
                title: input.title,
                description: input.description,
                base_branch: input.base_branch,
                base_commit: input.base_commit,
                settled_review: input.settled_review,
                instructions: input.instructions,
                model_selection: input.model_selection,
                stack: input.stack.map(|layers| {
                    layers
                        .into_iter()
                        .map(|layer| generation::ReviewGuideStackLayer {
                            number: layer.number,
                            title: layer.title,
                            head_sha: layer.head_sha,
                            state: match layer.state {
                                ReviewGuideStackLayerState::Open => {
                                    generation::ReviewGuideStackState::Open
                                }
                                ReviewGuideStackLayerState::Closed => {
                                    generation::ReviewGuideStackState::Closed
                                }
                                ReviewGuideStackLayerState::Merged => {
                                    generation::ReviewGuideStackState::Merged
                                }
                            },
                        })
                        .collect()
                }),
            })
            .await
            .map_err(|error| ReviewGuideGenerationError {
                detail: error.to_string(),
            })
    }
}
