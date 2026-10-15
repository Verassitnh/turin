//! Port of upstream/packages/source-control-github/src/server/driver.ts
//!
//! The GitHub driver: repository operations and discovery through `gh` and git, and pull requests
//! through the GitHub REST and GraphQL APIs.
//!
//! Upstream's `make` is `Effect.all({ sourceControl, discovery, pullRequests })`, each reading
//! its dependencies from the Effect context. Here [`make`] takes them explicitly. Every reader
//! must be handed the *same* `GitHubApi` so they share one view of each host's quota and rate-limit
//! pause; [`build`] constructs that one transport (as the application's layer wiring does upstream)
//! and returns it beside the driver so other readers can share it.

use std::sync::Arc;

use turin_contracts::source_control::SourceControlProviderKind;

use super::api::{GitHubApi, GitHubApiClient, GitHubApiDependencies, GitHubHttp};
use super::pull_request_api::GitHubPullRequestApi;
use super::pull_request_provider;
use super::source_control_provider;
use crate::shared::host_process::Environment;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::driver::{SourceControlDriver, SourceControlDriverInstance};
use crate::source_control_core::host::SourceControlHost;

pub const KIND: &str = "github";

pub fn kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new(KIND).expect("github is a valid kind")
}

/// `driver.make`: the repository provider, the discovery probe and the pull request provider,
/// all over the one `api`.
pub async fn make(
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    clock: Arc<dyn Clock>,
) -> SourceControlDriverInstance {
    let source_control =
        source_control_provider::make(api.clone(), host.clone(), environment.clone());
    let discovery = source_control_provider::make_discovery(api.clone(), host.clone(), environment);
    let pull_requests =
        pull_request_provider::make(Arc::new(GitHubPullRequestApi::new(api, host, clock)));
    SourceControlDriverInstance {
        source_control: Arc::new(source_control),
        discovery,
        pull_requests: Some(Arc::new(pull_requests)),
    }
}

/// `defineSourceControlDriver({ kind, make })`.
pub async fn driver(
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    clock: Arc<dyn Clock>,
) -> SourceControlDriver {
    SourceControlDriver {
        kind: kind(),
        instance: make(api, host, environment, clock).await,
    }
}

/// A driver together with the transport it was built over.
pub struct GitHubDriver {
    pub driver: SourceControlDriver,
    /// The credentials, quota and rate-limit pauses every GitHub reader of the application
    /// should share (`GitHubApi.layerWithDependencies`).
    pub dependencies: GitHubApiDependencies,
}

/// Builds the one shared transport (`GitHubApiClient::with_dependencies`) and the driver over it.
/// Pass `ReqwestGitHubHttp::new()` as `http` outside tests.
pub async fn build(
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    clock: Arc<dyn Clock>,
    http: Arc<dyn GitHubHttp>,
) -> GitHubDriver {
    let dependencies =
        GitHubApiClient::with_dependencies(host.clone(), environment.clone(), clock.clone(), http);
    let api: Arc<dyn GitHubApi> = dependencies.api.clone();
    GitHubDriver {
        driver: driver(api, host, environment, clock).await,
        dependencies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pull_request_api::test_support::Harness;
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::clock::ManualClock;
    use crate::source_control_core::discovery::SourceControlProviderDiscoverySpec;
    use crate::source_control_core::pull_request_provider::PullRequestOperation;
    use crate::source_control_core::test_host::ScriptedHost;

    #[tokio::test]
    async fn wires_discovery_and_both_providers_for_github() {
        let harness = Harness::new();
        let driver = driver(
            harness.api.clone(),
            Arc::new(ScriptedHost::default()),
            MapEnvironment::new([("GH_TOKEN", "t")]),
            ManualClock::new(0),
        )
        .await;
        assert_eq!(driver.kind.as_ref(), "github");

        let instance = driver.instance;
        assert_eq!(instance.source_control.kind().as_ref(), "github");
        assert_eq!(instance.discovery.kind().as_ref(), "github");
        assert!(matches!(
            instance.discovery,
            SourceControlProviderDiscoverySpec::ManagedCli(_)
        ));
        let pull_requests = instance.pull_requests.expect("GitHub reads pull requests");
        assert_eq!(pull_requests.kind().as_ref(), "github");
        assert!(pull_requests.capabilities().diff);
        assert!(pull_requests.supports(PullRequestOperation::VerifyCredential));
    }

    #[tokio::test]
    async fn build_shares_one_transport_with_the_caller() {
        struct NoHttp;
        #[async_trait::async_trait]
        impl GitHubHttp for NoHttp {
            async fn execute(
                &self,
                _request: super::super::api::GitHubHttpRequest,
            ) -> Result<super::super::api::GitHubHttpResponse, crate::source_control_core::Cause>
            {
                panic!("the driver must not reach the network while it is being built")
            }
        }
        let built = build(
            Arc::new(ScriptedHost::default()),
            MapEnvironment::new::<[(&str, &str); 0], _, _>([]),
            ManualClock::new(0),
            Arc::new(NoHttp),
        )
        .await;
        assert_eq!(built.driver.kind.as_ref(), "github");
        assert!(built.driver.instance.pull_requests.is_some());
        // The transport the caller gets back is the one the driver reads through.
        assert!(Arc::strong_count(&built.dependencies.api) >= 2);
    }
}
