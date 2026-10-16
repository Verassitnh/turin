//! Port of upstream/apps/server/src/provider/ProviderOrchestrationAdapterInfrastructure.ts.
//!
//! Infrastructure shared by the V2 adapters materialized inside provider
//! instances. Upstream merges one Effect layer per service; Effect layer
//! memoization is what makes the continuation queue a single instance. Here the
//! one value is built once and both sides take a clone of the `Arc`: the
//! adapters offer continuation requests, the orchestration runtime's
//! continuation worker takes them. Building a second
//! [`ProviderContinuationRequests`] splits the queue, and nothing would ever
//! receive what the adapters offer.
//!
//! Upstream also provides `ClaudeAgentSdkQueryRunner`, `CodexAppServerClientFactory`,
//! `CursorAgentSdkRunner`, `CursorSdk` and `CursorKeychain`. The Claude and Codex
//! pieces are owned by their driver crates; the Cursor SDK runner, SDK and
//! keychain are replaced by `cursor-agent acp` (see PORTING.md).

use std::sync::Arc;

use turin_provider_core::{
    id_allocator::IdAllocator, provider_continuation_requests::ProviderContinuationRequests,
};

#[derive(Clone)]
pub struct ProviderOrchestrationAdapterInfrastructure {
    pub id_allocator: Arc<IdAllocator>,
    /// Upstream `ProviderContinuationRequests.layer`: an unbounded queue.
    pub continuation_requests: Arc<ProviderContinuationRequests>,
}

impl ProviderOrchestrationAdapterInfrastructure {
    pub fn new() -> Self {
        Self {
            id_allocator: Arc::new(IdAllocator),
            continuation_requests: Arc::new(ProviderContinuationRequests::queue()),
        }
    }
}

impl Default for ProviderOrchestrationAdapterInfrastructure {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! No upstream test file. These cover what the layer guarantees upstream:
    //! one shared queue, and fresh ids per allocation.

    use turin_contracts::{ProviderThreadId, ThreadId, provider_instance::ProviderDriverKind};

    use super::*;

    fn request() -> turin_provider_core::provider_continuation_requests::ProviderContinuationRequest
    {
        turin_provider_core::provider_continuation_requests::ProviderContinuationRequest {
            thread_id: ThreadId::new("thread-1").unwrap(),
            provider_thread_id: ProviderThreadId::new("provider-thread-1").unwrap(),
            driver: ProviderDriverKind::new("claudeAgent").unwrap(),
            detail: Some("wake".to_owned()),
            notification: None,
            delegated_completion: None,
            delivery: None,
            message_id: None,
            dispatch_if_current: None,
            clear_if_current: None,
        }
    }

    #[tokio::test]
    async fn clones_share_one_continuation_queue() {
        let infrastructure = ProviderOrchestrationAdapterInfrastructure::new();
        let worker_side = infrastructure.clone();

        infrastructure.continuation_requests.offer(request());

        let taken = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            worker_side.continuation_requests.take(),
        )
        .await
        .expect("the worker side receives what the adapter side offered");
        assert_eq!(taken.detail.as_deref(), Some("wake"));
    }

    #[test]
    fn separate_infrastructure_values_do_not_share_a_queue() {
        let first = ProviderOrchestrationAdapterInfrastructure::new();
        let second = ProviderOrchestrationAdapterInfrastructure::new();
        assert!(!Arc::ptr_eq(
            &first.continuation_requests,
            &second.continuation_requests
        ));
    }
}
