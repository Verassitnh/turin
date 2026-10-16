//! Routing identity of `PullRequestService.ts`: `routingCredential` (a fiber-local reference
//! upstream, a task-local here), `routing`, `routingIdentity` and `withRoutingCredential`.

use std::future::Future;
use std::sync::Arc;

use turin_contracts::pull_request::{
    GithubProvider, PullRequestRef, PullRequestRoutingIdentityInput,
    PullRequestRoutingIdentityResult, PullRequestRoutingResult, PullRequestUnavailableReason,
};

use super::Inner;
use super::errors::{
    PullRequestError, operation_error, tne, to_pull_request_error, unavailable_error,
};
use super::projects::ProjectFilter;
use crate::source_control_core::ambient;
use crate::source_control_core::pull_request_provider::{
    PullRequestOperation, VerifiedCredentialInput,
};

/// `routingCredential`: the verified credential a routed operation runs under, so reads made
/// under it are cached apart from every other credential's and name its account as the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingCredential {
    pub credential_fingerprint: String,
    pub viewer: String,
}

tokio::task_local! {
    static ROUTING_CREDENTIAL: Option<RoutingCredential>;
}

/// The routing credential in force, or `None` (upstream's default).
pub(crate) fn routing_credential() -> Option<RoutingCredential> {
    ROUTING_CREDENTIAL.try_with(Clone::clone).unwrap_or(None)
}

pub(crate) async fn with_routing_credential_value<F: Future>(
    value: Option<RoutingCredential>,
    fut: F,
) -> F::Output {
    ROUTING_CREDENTIAL.scope(value, fut).await
}

impl Inner {
    pub(crate) async fn routing_identity(
        self: &Arc<Self>,
        input: &PullRequestRoutingIdentityInput,
    ) -> Result<PullRequestRoutingIdentityResult, PullRequestError> {
        let host = input.host.as_str().to_lowercase();
        let workspace = self
            .list_workspace_projects(&ProjectFilter {
                host: Some(host.clone()),
                ..ProjectFilter::default()
            })
            .await?;
        let project = workspace.supported.iter().find(|candidate| {
            self.registry
                .get(&candidate.api.kind())
                .is_some_and(|api| api.supports(PullRequestOperation::RoutingIdentity))
        });
        let api = project.and_then(|project| self.registry.get(&project.api.kind()));
        let (Some(project), Some(api)) = (project, api) else {
            return Err(unavailable_error(
                PullRequestUnavailableReason::ProviderUnsupported,
            ));
        };
        let identity = api
            .get_routing_identity(VerifiedCredentialInput {
                cwd: project.project.workspace_root.as_str().to_owned(),
                host: host.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("routeIdentity", error))?;
        // Only GitHub reports a routing identity, and the contract names it.
        Ok(PullRequestRoutingIdentityResult {
            account_id: tne(identity.account_id),
            host: tne(host),
            provider: GithubProvider,
            viewer: tne(identity.viewer),
        })
    }

    pub(crate) async fn routing(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<PullRequestRoutingResult, PullRequestError> {
        let project = self.require_project(input).await?;
        let api = self.registry.get(&project.api.kind());
        let Some(api) = api.filter(|api| api.supports(PullRequestOperation::RoutingIdentity))
        else {
            return Err(unavailable_error(
                PullRequestUnavailableReason::ProviderUnsupported,
            ));
        };
        let identity = api
            .get_routing_identity(VerifiedCredentialInput {
                cwd: project.project.workspace_root.as_str().to_owned(),
                host: project.host.clone(),
            })
            .await
            .map_err(|error| to_pull_request_error("routeIdentity", error))?;
        if identity.viewer.trim().is_empty() || identity.account_id.trim().is_empty() {
            return Err(operation_error(
                "routeIdentity",
                "The signed-in account could not be verified.",
            ));
        }
        Ok(PullRequestRoutingResult {
            account_id: tne(identity.account_id),
            host: tne(&project.host),
            provider: project.api.kind(),
            viewer: tne(identity.viewer),
            project_title: project.project.title.clone(),
            workspace_root: project.project.workspace_root.clone(),
        })
    }

    /// Runs `operation` under the verified credential of the account the reference expects, so a
    /// routed operation can never run as a different account than the one that asked for it.
    /// Failures of the verification itself are one rejection; the operation's own result passes
    /// through untouched.
    pub(crate) async fn with_routing_credential<T, E, F>(
        self: &Arc<Self>,
        input: &PullRequestRef,
        operation: F,
    ) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: From<PullRequestError>,
    {
        let Some(expected_account_id) = input.expected_account_id.as_ref() else {
            return operation.await;
        };
        let rejected = || {
            operation_error(
                "routeIdentity",
                "The GitHub account could not be verified before starting the operation.",
            )
        };
        let project = self
            .require_project(input)
            .await
            .map_err(|_| E::from(rejected()))?;
        let api = self.registry.get(&project.api.kind());
        let host_matches = input
            .host
            .as_ref()
            .is_some_and(|host| host.as_str().to_lowercase() == project.host.to_lowercase());
        let Some(api) =
            api.filter(|api| api.supports(PullRequestOperation::VerifyCredential) && host_matches)
        else {
            return Err(rejected().into());
        };
        let (identity, pin) = api
            .verify_credential(VerifiedCredentialInput {
                cwd: project.project.workspace_root.as_str().to_owned(),
                host: project.host.clone(),
            })
            .await
            .map_err(|_| E::from(rejected()))?;
        if identity.account_id != expected_account_id.as_str() {
            return Err(rejected().into());
        }
        let routing = RoutingCredential {
            credential_fingerprint: identity.credential_fingerprint.clone(),
            viewer: identity.viewer.clone(),
        };
        ambient::with_credential_scope(
            identity.credential_fingerprint,
            ambient::with_pinned_credential(
                Some(pin),
                with_routing_credential_value(Some(routing), operation),
            ),
        )
        .await
    }
}
