//! Scoped task ownership (PORTING.md: never drop a bare `JoinHandle`).
//!
//! Effect's `forkScoped` interrupts the fiber when its scope closes. A plain
//! tokio `JoinHandle` detaches on drop instead, so every background loop in
//! this crate is wrapped in an [`OwnedTask`] that aborts it when its owner
//! drops it.

use std::future::Future;

/// A spawned task that is aborted when dropped.
#[derive(Debug)]
pub struct OwnedTask(tokio::task::JoinHandle<()>);

impl OwnedTask {
    pub fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self(tokio::spawn(future))
    }

    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    /// Aborts the task and waits for it to stop.
    pub async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
