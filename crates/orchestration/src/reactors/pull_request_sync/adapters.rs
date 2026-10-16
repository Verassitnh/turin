//! Adapters from T07's `PullRequestService` and the thread command path to the sweep's seams.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::orchestration_v2::OrchestrationV2ServerCommand;
use turin_contracts::pull_request::{
    PullRequestInvalidateInput, PullRequestRef, PullRequestStack, PullRequestSummary,
};
use turin_contracts::thread_pull_request::{
    ThreadNativeStackKind, ThreadPullRequestStack, ThreadPullRequestStackLayer,
};
use turin_source_control::pull_request::service::PullRequestService;
use turin_source_control::pull_request::service::errors::PullRequestError;

use super::sweep::{ServerCommandDispatch, SyncHost, SyncHostError};
use crate::pipeline::orchestrator_port::BoxError;
use crate::pipeline::thread_management_service::ThreadManagementService;

/// The sweep's host reads over T07's service. Reads ask the service with stale-while-revalidate
/// off, as upstream's sweep does.
pub struct ServicePullRequestHost {
    service: Arc<PullRequestService>,
}

impl ServicePullRequestHost {
    pub fn new(service: Arc<PullRequestService>) -> Self {
        Self { service }
    }
}

/// The retry time of a rate-limited read. Upstream walks the cause chain for a
/// `PullRequestProviderError` with reason `rate-limited`; here the chain is the JSON cause.
pub fn rate_limited_retry_at(error: &PullRequestError) -> Option<i64> {
    let PullRequestError::Operation(operation) = error else {
        return None;
    };
    let turin_contracts::pull_request::PullRequestOperationError::Operation { cause, .. } =
        operation;
    let cause = cause.as_ref()?;
    if cause.get("_tag").and_then(|tag| tag.as_str()) != Some("PullRequestProviderError") {
        return None;
    }
    if cause.get("reason").and_then(|reason| reason.as_str()) != Some("rate-limited") {
        return None;
    }
    cause.get("retryAt").and_then(|retry_at| retry_at.as_i64())
}

fn to_sync_error(error: PullRequestError) -> SyncHostError {
    SyncHostError {
        message: error.to_string(),
        retry_at: rate_limited_retry_at(&error),
    }
}

/// A stack as the thread stores it: `kind: native`, layers with number, branch and state.
fn native_stack(stack: PullRequestStack) -> ThreadPullRequestStack {
    ThreadPullRequestStack {
        kind: ThreadNativeStackKind,
        id: stack.id,
        number: stack.number,
        url: stack.url,
        base: stack.base,
        layers: stack
            .layers
            .into_iter()
            .map(|layer| ThreadPullRequestStackLayer {
                number: layer.number,
                head_branch: layer.head_branch,
                state: layer.state,
            })
            .collect(),
    }
}

#[async_trait]
impl SyncHost for ServicePullRequestHost {
    async fn invalidate(&self, reference: &PullRequestRef) {
        // `notifyReaders` is off for sweep invalidations, as upstream's default.
        self.service
            .invalidate(
                &PullRequestInvalidateInput {
                    reference: Some(reference.clone()),
                    files_viewed_only: None,
                },
                false,
            )
            .await;
    }

    async fn summary(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestSummary, SyncHostError> {
        self.service
            .summary(reference, Some(false))
            .await
            .map_err(to_sync_error)
    }

    async fn stack(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Option<ThreadPullRequestStack>, SyncHostError> {
        let stack = self
            .service
            .stack(reference, Some(false))
            .await
            .map_err(to_sync_error)?;
        Ok(stack.map(native_stack))
    }
}

#[async_trait]
impl ServerCommandDispatch for ThreadManagementService {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError> {
        // `dispatch` lives on the core behind `Deref`; the service itself has no inherent one.
        let core: &crate::pipeline::thread_management_service::ThreadManagementCore = self;
        core.dispatch(command)
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }
}

#[cfg(test)]
mod tests;
