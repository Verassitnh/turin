//! Port of upstream/packages/provider-core/src/server/ProviderContinuationRequests.ts.
//!
//! The default value drops offers and `take` never resolves, so adapter
//! construction stays dependency-free in tests. The live queue is what the
//! continuation worker drains.

use tokio::sync::mpsc;
use turin_contracts::{
    MessageId, ProviderThreadId, RunId, ThreadId, orchestration_v2::OrchestrationV2Notification,
    provider_instance::ProviderDriverKind,
};

use crate::rt::BoxFuture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderContinuationDelivery {
    /// The adapter already buffered the CLI's wake output. The dispatched
    /// message only triggers ingestion.
    AdapterBuffered,
    /// App-owned work with no buffered provider output. The text is the entire
    /// wake and must reach the provider as a real prompt.
    MessageText,
}

pub struct ProviderContinuationDelegatedCompletion {
    pub parent_run_id: RunId,
    pub generation: i64,
    pub message_id: MessageId,
}

pub type DispatchIfCurrent = Box<
    dyn Fn(BoxFuture<'static, Result<(), String>>) -> BoxFuture<'static, Result<Option<()>, String>>
        + Send
        + Sync,
>;

pub struct ProviderContinuationRequest {
    pub thread_id: ThreadId,
    pub provider_thread_id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub detail: Option<String>,
    pub notification: Option<OrchestrationV2Notification>,
    /// Durable ownership for an app-owned delegated-task completion delivery.
    pub delegated_completion: Option<ProviderContinuationDelegatedCompletion>,
    pub delivery: Option<ProviderContinuationDelivery>,
    /// The message the continuation run starts from, when the adapter picks it.
    pub message_id: Option<MessageId>,
    pub dispatch_if_current: Option<DispatchIfCurrent>,
    /// Clears a pending offer that the continuation worker intentionally drops.
    pub clear_if_current: Option<Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

pub struct ProviderContinuationRequests {
    offer_tx: Option<mpsc::UnboundedSender<ProviderContinuationRequest>>,
    take_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<ProviderContinuationRequest>>>,
}

impl ProviderContinuationRequests {
    /// Drops every offer. `take` waits forever.
    pub fn dropping() -> Self {
        Self {
            offer_tx: None,
            take_rx: tokio::sync::Mutex::new(None),
        }
    }

    /// Unbounded queue. `offer` never waits.
    pub fn queue() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            offer_tx: Some(tx),
            take_rx: tokio::sync::Mutex::new(Some(rx)),
        }
    }

    pub fn offer(&self, request: ProviderContinuationRequest) {
        if let Some(tx) = &self.offer_tx {
            let _ = tx.send(request);
        }
    }

    pub async fn take(&self) -> ProviderContinuationRequest {
        let mut guard = self.take_rx.lock().await;
        match guard.as_mut() {
            Some(rx) => match rx.recv().await {
                Some(request) => request,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }
}
