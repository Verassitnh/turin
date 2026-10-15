//! Project resolution of `PullRequestService.ts`: which workspace projects this page can read,
//! and which project's checkout and credentials serve a reference.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use futures::stream;
use indexmap::IndexMap;
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::pull_request::{
    PullRequestRef, PullRequestUnavailableReason, PullRequestViewerPermissions,
    RepositoryIdentityLike, pull_request_host_of,
};
use turin_contracts::source_control::{SourceControlProviderInfo, SourceControlProviderKind};

use super::cursor::list_cursor_key;
use super::errors::{
    PullRequestError, operation_error, operation_error_with_cause, to_pull_request_error,
    unavailable_error,
};
use super::provider_backoff::with_rate_limit_backoff;
use super::{Inner, REPOSITORY_CONCURRENCY};
use crate::shared::git::normalize_git_remote_url;
use crate::shared::source_control::{
    canonical_repository_key, detect_source_control_provider_from_remote_url, is_ssh_remote_url,
    source_control_repository_selector,
};
use crate::source_control_core::pull_request_provider::{
    GetViewerPermissionsInput, ProviderChangeRequestRef, ProviderRepositoryRef,
    PullRequestProviderApi,
};
use crate::source_control_core::source_control_provider::SourceControlProviderContext;

/// A project this page can read: its remote is on a host with an implementation.
#[derive(Clone)]
pub struct SupportedProject {
    pub cursor_key: String,
    pub project: OrchestrationProjectShell,
    pub api: Arc<dyn PullRequestProviderApi>,
    pub repository: String,
    /// The host the repository lives on, which is the account boundary rather than the kind.
    pub host: String,
    /// The identity's canonical key, which is what this environment's own records are keyed by.
    /// Unique where `repository` is not.
    pub remote: String,
    /// The host's own key for this checkout's repository (`api.repositoryKey`), normalized, or
    /// `None` where `owner/name` on the host identifies it. When set, only a reference whose key
    /// matches is served by this checkout.
    pub repository_key: Option<String>,
}

impl SupportedProject {
    /// The provider's address of change request `number` in this project's checkout.
    pub(crate) fn provider_ref(&self, number: u64) -> ProviderChangeRequestRef {
        ProviderChangeRequestRef {
            repo: ProviderRepositoryRef {
                cwd: self.project.workspace_root.as_str().to_owned(),
                repository: self.repository.clone(),
                host: self.host.clone(),
            },
            number,
        }
    }
}

/// What the workspace has, split by whether this build can read it. Hosts with no
/// implementation are counted rather than dropped, so their projects are explained in the
/// provider list instead of quietly missing from the page.
pub(crate) struct WorkspaceProjects {
    pub supported: Vec<SupportedProject>,
    /// Keyed by host, as the readable ones are: an unimplemented host is its own switcher entry.
    pub unimplemented: IndexMap<String, (SourceControlProviderKind, u64)>,
    /// Every checkout on a host, including the ones the listing de-duplicated away. Asking who is
    /// signed in is a question about the host rather than about a repository, and any checkout can
    /// answer it — so a broken worktree is not allowed to take the host down with it just because
    /// it happened to be the one the listing kept.
    pub viewer_roots: HashMap<String, Vec<String>>,
}

/// `Pick<PullRequestListInput, "projectId" | "projectIds" | "host">`
#[derive(Default, Clone)]
pub(crate) struct ProjectFilter {
    pub project_id: Option<ProjectId>,
    pub project_ids: Option<Vec<ProjectId>>,
    pub host: Option<String>,
}

fn identity_of(project: &OrchestrationProjectShell) -> Option<&RepositoryIdentity> {
    project
        .repository_identity
        .as_ref()
        .and_then(Option::as_ref)
}

fn host_of(identity: &RepositoryIdentity, kind: &SourceControlProviderKind) -> String {
    pull_request_host_of(
        Some(&RepositoryIdentityLike {
            canonical_key: Some(identity.canonical_key.as_str()),
        }),
        kind,
    )
}

fn unknown_kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new("unknown").expect("valid kind")
}

impl Inner {
    /// For identities recorded as `unknown` (a self-hosted install the remote's name does not
    /// give away), asks the source control registry which host serves the remote. One question per
    /// base URL, tried across that host's checkouts until one answers.
    async fn refine_unknown_project_kinds(
        self: &Arc<Self>,
        projects: &[OrchestrationProjectShell],
        filter: &ProjectFilter,
    ) -> HashMap<String, Option<SourceControlProviderInfo>> {
        struct Candidate {
            workspace_root: String,
            provider: SourceControlProviderInfo,
            remote_name: String,
            remote_url: String,
        }
        let mut refinements: IndexMap<String, Vec<Candidate>> = IndexMap::new();
        for project in projects {
            if let Some(project_id) = &filter.project_id
                && &project.id != project_id
            {
                continue;
            }
            let Some(identity) = identity_of(project) else {
                continue;
            };
            if identity.provider.as_ref().map(|p| p.as_str()) != Some("unknown")
                || source_control_repository_selector(Some(identity)).is_none()
            {
                continue;
            }
            let host = host_of(identity, &unknown_kind());
            // A legacy identity has no canonical host until its provider is refined, so it must
            // reach the refinement before a host filter can decide whether it belongs in the
            // result.
            if let Some(filter_host) = &filter.host
                && host != "unknown"
                && host != filter_host.to_lowercase()
                && !is_ssh_remote_url(identity.locator.remote_url.as_str())
            {
                continue;
            }
            let remote_name = identity.locator.remote_name.as_str().to_owned();
            let remote_url = identity.locator.remote_url.as_str().to_owned();
            if let Some(provider) = detect_source_control_provider_from_remote_url(&remote_url) {
                refinements
                    .entry(provider.base_url.clone())
                    .or_default()
                    .push(Candidate {
                        workspace_root: project.workspace_root.as_str().to_owned(),
                        provider,
                        remote_name,
                        remote_url,
                    });
            }
        }

        let requested_host = filter.host.clone();
        stream::iter(refinements)
            .map(|(base_url, candidates)| {
                let requested_host = requested_host.clone();
                let inner = self.clone();
                async move {
                    // `Effect.firstSuccessOf`: in order, the first that names a known provider.
                    for candidate in candidates {
                        let context = SourceControlProviderContext {
                            provider: candidate.provider,
                            remote_name: candidate.remote_name,
                            remote_url: candidate.remote_url.clone(),
                            requested_host: requested_host
                                .clone()
                                .filter(|_| is_ssh_remote_url(&candidate.remote_url)),
                        };
                        let refined = inner
                            .provider_context
                            .resolve(&candidate.workspace_root, &context)
                            .await;
                        if let Ok(Some(context)) = refined
                            && context.provider.kind.as_str() != "unknown"
                        {
                            return (base_url, Some(context.provider));
                        }
                    }
                    (base_url, None)
                }
            })
            .buffered(REPOSITORY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect()
    }

    pub(crate) async fn list_workspace_projects(
        self: &Arc<Self>,
        filter: &ProjectFilter,
    ) -> Result<WorkspaceProjects, PullRequestError> {
        let shells = match &filter.project_id {
            None => {
                self.projects
                    .list_shells(filter.project_ids.as_deref())
                    .await
            }
            Some(project_id) => self
                .projects
                .get_shell(project_id)
                .await
                .map(|shell| shell.into_iter().collect()),
        }
        .map_err(|cause| {
            operation_error_with_cause(
                "listProjects",
                "The project list could not be read.",
                serde_json::Value::String(cause.to_string()),
            )
        })?;

        let projects: Vec<OrchestrationProjectShell> = stream::iter(shells)
            .map(|project| {
                let inner = self.clone();
                async move {
                    if identity_of(&project).is_some() {
                        return project;
                    }
                    let resolved = inner
                        .repository_identities
                        .resolve(project.workspace_root.as_str())
                        .await;
                    OrchestrationProjectShell {
                        repository_identity: Some(resolved),
                        ..project
                    }
                }
            })
            .buffered(REPOSITORY_CONCURRENCY)
            .collect()
            .await;

        let refined_providers = self.refine_unknown_project_kinds(&projects, filter).await;

        let mut supported: Vec<SupportedProject> = Vec::new();
        let mut unimplemented: IndexMap<String, (SourceControlProviderKind, u64)> = IndexMap::new();
        let mut viewer_roots: HashMap<String, Vec<String>> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        for project in projects {
            if let Some(project_id) = &filter.project_id
                && &project.id != project_id
            {
                continue;
            }
            if let Some(project_ids) = &filter.project_ids
                && !project_ids.contains(&project.id)
            {
                continue;
            }
            let Some(identity) = identity_of(&project).cloned() else {
                continue;
            };
            let Some(mut kind) = identity
                .provider
                .as_ref()
                .and_then(|provider| SourceControlProviderKind::new(provider.as_str()).ok())
            else {
                continue;
            };
            let Some(repository) = source_control_repository_selector(Some(&identity)) else {
                continue;
            };
            // Worktrees of one repository are separate projects; reading the remote once keeps
            // the page from repeating every change request per local checkout. The host is part
            // of the key, so the same `owner/repo` on two hosts stays two repositories.
            if kind.as_str() == "unknown" {
                let refined = detect_source_control_provider_from_remote_url(
                    identity.locator.remote_url.as_str(),
                )
                .and_then(|provider| refined_providers.get(&provider.base_url).cloned().flatten());
                if let Some(refined) = refined {
                    kind = refined.kind;
                }
            }
            let host = host_of(&identity, &kind);
            if let Some(filter_host) = &filter.host
                && host != filter_host.to_lowercase()
            {
                continue;
            }
            let api = self.registry.get(&kind);
            let workspace_root = project.workspace_root.as_str().to_owned();
            // Recorded before the de-duplication below, so the viewer lookup keeps the alternates
            // the listing is about to drop.
            if api.is_some() {
                let roots = viewer_roots.entry(host.clone()).or_default();
                if !roots.contains(&workspace_root) {
                    roots.push(workspace_root);
                }
            }
            let repository_key = api
                .as_ref()
                .and_then(|api| api.repository_key(identity.canonical_key.as_str()));
            let key = list_cursor_key(&host, repository_key.as_deref().unwrap_or(&repository));
            if !seen.insert(key.clone()) {
                continue;
            }
            let Some(api) = api else {
                unimplemented.entry(host).or_insert((kind, 0)).1 += 1;
                continue;
            };
            supported.push(SupportedProject {
                cursor_key: key,
                project,
                api: with_rate_limit_backoff(api, &host, &self.rate_limits, false),
                remote: repository_key.clone().unwrap_or_else(|| {
                    normalize_git_remote_url(&format!("https://{host}/{repository}"))
                }),
                repository,
                host,
                repository_key: repository_key
                    .map(|key| canonical_repository_key(&key.to_lowercase())),
            });
        }
        Ok(WorkspaceProjects {
            supported,
            unimplemented,
            viewer_roots,
        })
    }

    /// The project whose checkout and credentials serve a reference. The project's own
    /// repository is the default; a reference that names a `host` may instead point at any
    /// repository on that host. Prefer its own checkout; providers with explicit repository
    /// targeting can fall back to another checkout on the host. Providers with explicit host
    /// targeting can also use the selected project's directory for supported public hosts.
    pub(crate) async fn require_project(
        self: &Arc<Self>,
        reference: &PullRequestRef,
    ) -> Result<SupportedProject, PullRequestError> {
        let workspace = self
            .list_workspace_projects(&ProjectFilter {
                project_id: Some(reference.project_id.clone()),
                ..ProjectFilter::default()
            })
            .await?;
        let own = workspace.supported.first();
        let repository = reference.repository.as_str().trim();
        let host = reference
            .host
            .as_ref()
            .map(|host| host.as_str().trim().to_lowercase());
        if let Some(own) = own
            && own.repository.to_lowercase() == repository.to_lowercase()
            // Hostless references only ever meant the project's own repository, and a hosted one
            // naming it still is; either way the project serves itself.
            && host.as_ref().is_none_or(|host| host == &own.host)
        {
            return Ok(own.clone());
        }
        let Some(host) = host else {
            if own.is_none() {
                return Err(unavailable_error(
                    PullRequestUnavailableReason::ProviderUnsupported,
                ));
            }
            // The repository travels through the client, so it is checked against the project's
            // own remote rather than being handed to a provider verbatim.
            return Err(operation_error(
                "resolveRepository",
                "The change request does not belong to the selected project.",
            ));
        };
        let repository_key =
            canonical_repository_key(&format!("{host}/{repository}").to_lowercase());
        // Azure SSH and legacy clone hosts differ from the browser URL's host. Compare
        // the complete repository identity before narrowing those checkouts by host.
        let candidates = self
            .list_workspace_projects(&if repository_key.starts_with("dev.azure.com/") {
                ProjectFilter::default()
            } else {
                ProjectFilter {
                    host: Some(host.clone()),
                    ..ProjectFilter::default()
                }
            })
            .await?
            .supported;
        let on_host: Vec<&SupportedProject> = candidates
            .iter()
            .filter(|candidate| candidate.host == host)
            .collect();
        let route = candidates
            .iter()
            .find(|candidate| candidate.repository_key.as_deref() == Some(repository_key.as_str()))
            .or_else(|| {
                on_host.iter().copied().find(|candidate| {
                    candidate.repository_key.is_none()
                        && candidate.repository.to_lowercase() == repository.to_lowercase()
                })
            })
            .or_else(|| {
                on_host
                    .iter()
                    .copied()
                    .find(|candidate| candidate.repository_key.is_none())
            });
        let Some(route) = route else {
            // These providers target the repository explicitly and authenticate by host;
            // a linked PR in a non-repository project still needs its snapshot refreshed.
            // (Upstream also lists bitbucket.org and codeberg.org here; those hosts are cut.)
            let provider = if host == "github.com" {
                detect_source_control_provider_from_remote_url(&format!(
                    "https://{host}/{repository}.git"
                ))
            } else {
                None
            };
            let api = provider.and_then(|provider| self.registry.get(&provider.kind));
            if let Some(api) = api {
                let selected = self
                    .projects
                    .get_shell(&reference.project_id)
                    .await
                    .map_err(|cause| {
                        operation_error_with_cause(
                            "resolveRepository",
                            "The selected project could not be read.",
                            serde_json::Value::String(cause.to_string()),
                        )
                    })?;
                if let Some(selected) = selected {
                    return Ok(SupportedProject {
                        cursor_key: list_cursor_key(&host, repository),
                        project: selected,
                        api: with_rate_limit_backoff(api, &host, &self.rate_limits, false),
                        repository: repository.to_owned(),
                        remote: normalize_git_remote_url(&format!("https://{host}/{repository}")),
                        host,
                        repository_key: None,
                    });
                }
            }
            return Err(unavailable_error(
                PullRequestUnavailableReason::ProviderUnsupported,
            ));
        };
        Ok(
            if route.repository_key.is_some()
                || route.repository.to_lowercase() == repository.to_lowercase()
            {
                route.clone()
            } else {
                SupportedProject {
                    repository: repository.to_owned(),
                    remote: normalize_git_remote_url(&format!("https://{host}/{repository}")),
                    ..route.clone()
                }
            },
        )
    }

    /// `canonicalRef`: the reference spelled the way the project's remote spells it.
    pub(crate) async fn canonical_ref(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestRef, PullRequestError> {
        let project = self.require_project(input).await?;
        Ok(PullRequestRef {
            project_id: project.project.id.clone(),
            host: Some(super::errors::tne(&project.host)),
            repository: super::errors::tne(&project.repository),
            ..input.clone()
        })
    }

    /// What the signed-in account may do with this change request, asked of the host itself. Every
    /// write goes through it: the page hides what a viewer may not do, and a request that arrived
    /// without passing through the page — or after the access behind it was withdrawn — must not
    /// be handed to a provider on the client's word. Read freshly for that reason, rather than
    /// taken from whatever the detail said when the page loaded.
    pub(crate) async fn viewer_permissions_of(
        &self,
        project: &SupportedProject,
        reference: &PullRequestRef,
        operation: &str,
        include_update_branch: bool,
    ) -> Result<PullRequestViewerPermissions, PullRequestError> {
        project
            .api
            .get_viewer_permissions(GetViewerPermissionsInput {
                request: project.provider_ref(reference.number.get()),
                include_update_branch,
            })
            .await
            .map_err(|error| to_pull_request_error(operation, error))
    }
}
