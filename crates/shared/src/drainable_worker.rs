//! Port of upstream/packages/shared/src/DrainableWorker.ts
//!
//! Effect `TxQueue.unbounded` + a scoped fiber becomes an owned queue and a
//! task aborted when the worker is closed. `drain` resolves when nothing is
//! queued or running. Closing the worker drops queued items and interrupts
//! the current item; an item that fails is logged and skipped.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerItemError {
    Fail(String),
    Die(String),
    Interrupt,
}

struct State<A> {
    queue: VecDeque<A>,
    outstanding: usize,
    shut_down: bool,
}

struct Shared<A> {
    state: Mutex<State<A>>,
    work: Notify,
    drained: Notify,
    errors: Mutex<Vec<String>>,
}

/// A queue-based worker that exposes `drain`.
pub struct DrainableWorker<A> {
    shared: Arc<Shared<A>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl<A> DrainableWorker<A>
where
    A: Send + 'static,
{
    pub fn spawn<F, Fut>(process: F) -> Self
    where
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), WorkerItemError>> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                outstanding: 0,
                shut_down: false,
            }),
            work: Notify::new(),
            drained: Notify::new(),
            errors: Mutex::new(Vec::new()),
        });
        let worker = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            let process = Arc::new(process);
            loop {
                enum Next<A> {
                    Stop,
                    Wait,
                    Item(A),
                }
                let next = {
                    let mut state = worker.state.lock();
                    if state.shut_down {
                        Next::Stop
                    } else if let Some(item) = state.queue.pop_front() {
                        Next::Item(item)
                    } else {
                        Next::Wait
                    }
                };
                match next {
                    Next::Stop => break,
                    Next::Wait => {
                        // Subscribe before rechecking so an enqueue cannot be lost.
                        let notified = worker.work.notified();
                        tokio::pin!(notified);
                        let waiting = {
                            let state = worker.state.lock();
                            state.queue.is_empty() && !state.shut_down
                        };
                        if waiting {
                            notified.await;
                        }
                    }
                    Next::Item(item) => {
                        let guard = OutstandingGuard {
                            shared: Arc::clone(&worker),
                        };
                        match process(item).await {
                            Ok(()) | Err(WorkerItemError::Interrupt) => {}
                            Err(WorkerItemError::Fail(message) | WorkerItemError::Die(message)) => {
                                tracing::error!("DrainableWorker item failed {message}");
                                worker.errors.lock().push(message);
                            }
                        }
                        drop(guard);
                    }
                }
            }
        });
        Self {
            shared,
            task: Mutex::new(Some(task)),
        }
    }

    /// Enqueue a work item. A shut-down worker refuses it.
    pub fn enqueue(&self, item: A) {
        let mut state = self.shared.state.lock();
        if state.shut_down {
            return;
        }
        state.outstanding += 1;
        state.queue.push_back(item);
        self.shared.work.notify_one();
    }

    /// Resolves when the queue is empty and the worker is idle.
    pub async fn drain(&self) {
        loop {
            let notified = self.shared.drained.notified();
            tokio::pin!(notified);
            if self.shared.state.lock().outstanding == 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn logged_errors(&self) -> Vec<String> {
        self.shared.errors.lock().clone()
    }

    /// Scope close: drop queued items, interrupt the current item, then resolve.
    pub async fn close(self) {
        shutdown_queue(&self.shared);
        let task = self.task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

fn shutdown_queue<A>(shared: &Shared<A>) {
    let mut state = shared.state.lock();
    state.shut_down = true;
    let dropped = state.queue.len();
    state.queue.clear();
    state.outstanding = state.outstanding.saturating_sub(dropped);
    if state.outstanding == 0 {
        shared.drained.notify_waiters();
    }
    shared.work.notify_waiters();
}

impl<A> Drop for DrainableWorker<A> {
    fn drop(&mut self) {
        shutdown_queue(&self.shared);
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
    }
}

struct OutstandingGuard<A> {
    shared: Arc<Shared<A>>,
}

impl<A> Drop for OutstandingGuard<A> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock();
        state.outstanding = state.outstanding.saturating_sub(1);
        if state.outstanding == 0 {
            self.shared.drained.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Mutex as AsyncMutex, oneshot};

    #[tokio::test]
    async fn logs_a_failed_item_and_keeps_processing() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let seen = Arc::clone(&processed);
        let worker = DrainableWorker::spawn(move |item: String| {
            let seen = Arc::clone(&seen);
            async move {
                if item == "fail" {
                    return Err(WorkerItemError::Fail("typed failure".into()));
                }
                if item == "die" {
                    return Err(WorkerItemError::Die("defect".into()));
                }
                if item == "interrupt" {
                    return Err(WorkerItemError::Interrupt);
                }
                seen.lock().await.push(item);
                Ok(())
            }
        });
        worker.enqueue("fail".into());
        worker.enqueue("die".into());
        worker.enqueue("interrupt".into());
        worker.enqueue("ok".into());
        worker.drain().await;
        assert_eq!(processed.lock().await.as_slice(), ["ok"]);
        assert_eq!(
            worker.logged_errors(),
            ["typed failure".to_string(), "defect".to_string()]
        );
        worker.close().await;
    }

    #[tokio::test]
    async fn stops_quietly_when_closed_during_an_item() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let (started_tx, started_rx) = oneshot::channel();
        let started_tx = Arc::new(AsyncMutex::new(Some(started_tx)));
        let seen = Arc::clone(&processed);
        let worker = DrainableWorker::spawn(move |item: String| {
            let seen = Arc::clone(&seen);
            let started_tx = Arc::clone(&started_tx);
            async move {
                seen.lock().await.push(item);
                if let Some(tx) = started_tx.lock().await.take() {
                    let _ = tx.send(());
                }
                std::future::pending::<()>().await;
                Ok(())
            }
        });
        worker.enqueue("block".into());
        worker.enqueue("later".into());
        started_rx.await.unwrap();
        worker.close().await;
        assert_eq!(processed.lock().await.as_slice(), ["block"]);
    }

    #[tokio::test]
    async fn waits_for_work_enqueued_during_processing() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let (first_tx, first_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel::<()>();
        let (second_tx, second_rx) = oneshot::channel();
        let (release_second_tx, release_second_rx) = oneshot::channel::<()>();
        let release_first_rx = Arc::new(AsyncMutex::new(Some(release_first_rx)));
        let release_second_rx = Arc::new(AsyncMutex::new(Some(release_second_rx)));
        let first_tx = Arc::new(AsyncMutex::new(Some(first_tx)));
        let second_tx = Arc::new(AsyncMutex::new(Some(second_tx)));
        let seen = Arc::clone(&processed);
        let worker = Arc::new(DrainableWorker::spawn(move |item: String| {
            let seen = Arc::clone(&seen);
            let first_tx = Arc::clone(&first_tx);
            let second_tx = Arc::clone(&second_tx);
            let release_first_rx = Arc::clone(&release_first_rx);
            let release_second_rx = Arc::clone(&release_second_rx);
            async move {
                if item == "first" {
                    if let Some(tx) = first_tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = release_first_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                }
                if item == "second" {
                    if let Some(tx) = second_tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = release_second_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                }
                seen.lock().await.push(item);
                Ok(())
            }
        }));
        worker.enqueue("first".into());
        first_rx.await.unwrap();
        let draining = {
            let worker = Arc::clone(&worker);
            tokio::spawn(async move { worker.drain().await })
        };
        worker.enqueue("second".into());
        let _ = release_first_tx.send(());
        second_rx.await.unwrap();
        assert!(!draining.is_finished());
        let _ = release_second_tx.send(());
        draining.await.unwrap();
        assert_eq!(
            processed.lock().await.as_slice(),
            ["first".to_string(), "second".to_string()]
        );
        // Arc close: take the worker out.
        let worker = Arc::try_unwrap(worker).unwrap_or_else(|_| panic!("worker still shared"));
        worker.close().await;
    }
}
