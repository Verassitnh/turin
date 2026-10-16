//! Viewer lookups of `PullRequestService.ts`: who the host says is signed in, once per host.

use std::collections::HashMap;
use std::sync::Arc;

use futures::FutureExt;
use futures::StreamExt;
use futures::stream;
use serde_json::json;
use turin_contracts::source_control::SourceControlProviderKind;

use super::errors::{PullRequestError, to_pull_request_error};
use super::projects::SupportedProject;
use super::provider_backoff::with_rate_limit_backoff;
use super::routing::routing_credential;
use super::{Inner, REPOSITORY_CONCURRENCY, VIEWER_CACHE_TTL_MS};
use crate::source_control_core::pull_request_provider::{
    PullRequestProviderApi, PullRequestProviderError, PullRequestProviderErrorReason,
};
use crate::source_control_core::rate_limit::RateLimitKey;

/// One viewer lookup per host, tried across that host's workspaces so a single broken checkout
/// cannot hide every healthy repository on it. Per host and not per provider kind: two GitHub
/// hosts are two accounts, and the wrong login would misattribute every review request.
///
/// Its failure doubles as the answer to "is this host set up", which is what the provider
/// switcher shows.
#[derive(Clone)]
pub(crate) struct ResolvedViewer {
    pub host: String,
    pub kind: SourceControlProviderKind,
    pub viewer: Option<String>,
    pub error: Option<PullRequestProviderError>,
}

/// Who is signed in moves on the timescale of `gh auth login`, not of a page visit, yet every
/// list read was asking each host's CLI again — a subprocess and a network round trip per host
/// per read, three reads per page. Only a success is believed for a while: a failure is the
/// "is this host set up" answer the provider switcher shows, and holding it would keep saying
/// signed-out after the reader has signed in.
#[derive(Clone)]
pub(crate) struct HeldViewer {
    pub at: i64,
    pub result: ResolvedViewer,
}

/// What `resolveViewers` needs of a `SupportedProject`.
#[derive(Clone)]
pub(crate) struct ViewerTarget {
    pub host: String,
    pub api: Arc<dyn PullRequestProviderApi>,
    pub workspace_root: String,
}

impl From<&SupportedProject> for ViewerTarget {
    fn from(project: &SupportedProject) -> Self {
        Self {
            host: project.host.clone(),
            api: project.api.clone(),
            workspace_root: project.project.workspace_root.as_str().to_owned(),
        }
    }
}

fn failed(
    host: &str,
    kind: &SourceControlProviderKind,
    error: PullRequestProviderError,
) -> ResolvedViewer {
    ResolvedViewer {
        host: host.to_owned(),
        kind: kind.clone(),
        viewer: None,
        error: Some(error),
    }
}

impl Inner {
    /// The lookup behind a flight: the first checkout that can say who is signed in.
    async fn look_up_viewer(
        self: Arc<Self>,
        host: String,
        kind: SourceControlProviderKind,
        roots: Vec<String>,
    ) -> ResolvedViewer {
        let Some(registered) = self.registry.get(&kind) else {
            // Upstream dies here; a caller only gets this far with a kind it just read from the
            // registry, so this is a defect, reported as a failed lookup rather than a panic.
            return failed(
                &host,
                &kind,
                PullRequestProviderError::new(
                    kind.clone(),
                    "getViewer",
                    PullRequestProviderErrorReason::Failed,
                    format!("Missing pull request provider: {kind}"),
                ),
            );
        };
        // Let through a pause: a press the reader is waiting on has to be answered, and the
        // callers that are not that press are held back at the gate in `resolve_viewers`
        // instead, before they reach this lookup at all.
        let api = with_rate_limit_backoff(registered, &host, &self.rate_limits, true);
        let mut last_error = None;
        for cwd in &roots {
            match api.get_viewer(cwd, Some(&host)).await {
                Ok(viewer) => {
                    let result = ResolvedViewer {
                        host: host.clone(),
                        kind: kind.clone(),
                        viewer: Some(viewer),
                        error: None,
                    };
                    let at = self.clock.now_millis();
                    self.state.lock().viewers_by_host.insert(
                        host.clone(),
                        HeldViewer {
                            at,
                            result: result.clone(),
                        },
                    );
                    return result;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let error = last_error.unwrap_or_else(|| {
            PullRequestProviderError::new(
                kind.clone(),
                "getViewer",
                PullRequestProviderErrorReason::Failed,
                "No checkout on this host could be asked.",
            )
        });
        failed(&host, &kind, error)
    }

    pub(crate) async fn resolve_viewers(
        self: &Arc<Self>,
        projects: &[ViewerTarget],
        viewer_roots: &HashMap<String, Vec<String>>,
        allow_paused: bool,
    ) -> Vec<ResolvedViewer> {
        let mut hosts: Vec<&str> = Vec::new();
        for project in projects {
            if !hosts.contains(&project.host.as_str()) {
                hosts.push(&project.host);
            }
        }
        let reads: Vec<_> = hosts
            .into_iter()
            .map(|host| self.resolve_viewer_of_host(host, projects, viewer_roots, allow_paused))
            .collect();
        stream::iter(reads)
            .buffered(REPOSITORY_CONCURRENCY)
            .collect()
            .await
    }

    async fn resolve_viewer_of_host(
        self: &Arc<Self>,
        host: &str,
        projects: &[ViewerTarget],
        viewer_roots: &HashMap<String, Vec<String>>,
        allow_paused: bool,
    ) -> ResolvedViewer {
        let now = self.clock.now_millis();
        let held = self.state.lock().viewers_by_host.get(host).cloned();
        if let Some(held) = held
            && now - held.at <= VIEWER_CACHE_TTL_MS
        {
            return held.result;
        }
        let for_host: Vec<&ViewerTarget> = projects
            .iter()
            .filter(|project| project.host == host)
            .collect();
        let api = for_host[0].api.clone();
        // Every checkout on the host, not just the ones that survived de-duplication: one
        // unreadable worktree would otherwise report the whole host as signed out.
        let mut roots: Vec<String> = viewer_roots.get(host).cloned().unwrap_or_else(|| {
            for_host
                .iter()
                .map(|project| project.workspace_root.clone())
                .collect()
        });
        // Nothing about the caller is in the key. A listing and a press for the same host and
        // roots are the same lookup, and putting them on separate flights would spawn two of
        // this host's CLIs on a cold page load, which is the coalescing this exists for.
        roots.sort();
        roots.dedup();
        let kind = api.kind();
        let key = json!([host, kind.as_str(), roots]).to_string();
        if !allow_paused {
            // The pause is checked here rather than inside the lookup, so that it holds back the
            // callers nobody is waiting on without splitting the flight they share with a press.
            // A failed lookup is held nowhere, so letting a background read through would spawn
            // this host's CLI on every refresh for as long as the pause lasted, and re-extend it.
            let checked = self.rate_limits.check(
                &RateLimitKey {
                    provider: kind.clone(),
                    host: host.to_owned(),
                },
                false,
            );
            if let Err(error) = checked {
                return failed(
                    host,
                    &kind,
                    PullRequestProviderError {
                        provider: kind.clone(),
                        operation: "getViewer".to_owned(),
                        reason: PullRequestProviderErrorReason::RateLimited,
                        detail: error.detail(),
                        retry_at: Some(error.retry_at),
                        cause: Some(crate::source_control_core::cause_from_message(
                            error.detail(),
                        )),
                    },
                );
            }
        }
        let inner = self.clone();
        let host = host.to_owned();
        self.viewer_flights
            .get(&key, move || {
                async move { Ok(inner.look_up_viewer(host, kind, roots).await) }.boxed()
            })
            .await
            .expect("a viewer lookup always answers")
    }

    /// Who this project's host says the reader is. Shared with the listing's own lookup — the same
    /// ten-minute answer per host — so a page that has already listed anything pays nothing for
    /// it, and a host that cannot say leaves it `None` rather than failing the read it decorates.
    pub(crate) async fn viewer_of(self: &Arc<Self>, project: &SupportedProject) -> Option<String> {
        if let Some(credential) = routing_credential() {
            return Some(credential.viewer);
        }
        self.resolve_viewers(&[project.into()], &HashMap::new(), false)
            .await
            .into_iter()
            .next()
            .and_then(|resolved| resolved.viewer)
    }

    /// Who the host says the reader is, for the paths whose rows are keyed by it. A lookup that
    /// failed is refused rather than answered as the unnamed reader: a momentarily signed-out CLI
    /// would otherwise hide every tick this reader has made and file the next press under rows
    /// that are orphaned once it recovers. The reader is waiting on every one of these paths, so
    /// the lookup is let through a host's backoff rather than turning a pause into a refusal, and
    /// only here, where the bypass is bounded by what the reader does.
    pub(crate) async fn required_viewer_of(
        self: &Arc<Self>,
        project: &ViewerTarget,
        operation: &str,
    ) -> Result<Option<String>, PullRequestError> {
        let resolved = self
            .resolve_viewers(std::slice::from_ref(project), &HashMap::new(), true)
            .await
            .into_iter()
            .next();
        match resolved {
            Some(ResolvedViewer {
                error: Some(error), ..
            }) => Err(to_pull_request_error(operation, error)),
            Some(resolved) => Ok(resolved.viewer),
            None => Ok(None),
        }
    }
}
