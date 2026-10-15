//! Port of upstream/packages/shared/src/KeyedCoalescingWorker.ts
//!
//! Enqueues for an active or already-queued key are merged instead of creating
//! another queued item. A key that receives updates while it runs is returned
//! to the tail, so other queued keys run first. `drain_key` resolves only when
//! that key has no queued, pending, or active work left.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::drainable_worker::panic_message;

struct State<K, V> {
    latest_by_key: HashMap<K, V>,
    queued_keys: HashSet<K>,
    active_keys: HashSet<K>,
    queue: VecDeque<K>,
    shut_down: bool,
}

struct Shared<K, V> {
    state: Mutex<State<K, V>>,
    work: Notify,
    drained: Notify,
    merge: Box<dyn Fn(V, V) -> V + Send + Sync>,
}

fn idle<K: Eq + Hash, V>(state: &State<K, V>, key: &K) -> bool {
    !state.latest_by_key.contains_key(key)
        && !state.queued_keys.contains(key)
        && !state.active_keys.contains(key)
}

fn complete_key<K: Eq + Hash + Clone, V>(shared: &Shared<K, V>, key: K) {
    let mut state = shared.state.lock();
    state.active_keys.remove(&key);
    if state.shut_down {
        shared.drained.notify_waiters();
        return;
    }
    if state.latest_by_key.contains_key(&key) && !state.queued_keys.contains(&key) {
        state.queued_keys.insert(key.clone());
        state.queue.push_back(key);
        shared.work.notify_one();
    }
    shared.drained.notify_waiters();
}

pub struct KeyedCoalescingWorker<K, V> {
    shared: Arc<Shared<K, V>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl<K, V> KeyedCoalescingWorker<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Send + 'static,
{
    pub fn spawn<M, F, Fut>(merge: M, process: F) -> Self
    where
        M: Fn(V, V) -> V + Send + Sync + 'static,
        F: Fn(K, V) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                latest_by_key: HashMap::<K, V>::new(),
                queued_keys: HashSet::<K>::new(),
                active_keys: HashSet::<K>::new(),
                queue: VecDeque::new(),
                shut_down: false,
            }),
            work: Notify::new(),
            drained: Notify::new(),
            merge: Box::new(merge),
        });
        let worker = Arc::clone(&shared);
        let process = Arc::new(process);
        let task = tokio::spawn(async move {
            loop {
                enum Next<K, V> {
                    Stop,
                    Wait,
                    Item(K, V),
                }
                let next = {
                    let mut state = worker.state.lock();
                    if state.shut_down {
                        Next::Stop
                    } else if let Some(key) = state.queue.pop_front() {
                        state.queued_keys.remove(&key);
                        if let Some(value) = state.latest_by_key.remove(&key) {
                            state.active_keys.insert(key.clone());
                            Next::Item(key, value)
                        } else {
                            worker.drained.notify_waiters();
                            Next::Wait
                        }
                    } else {
                        Next::Wait
                    }
                };
                match next {
                    Next::Stop => break,
                    Next::Wait => {
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
                    Next::Item(key, value) => {
                        // A panic ends this item only. Upstream's `ignoreCause`
                        // + `ensuring(completeKey)` keep the loop alive, and
                        // completion runs on both paths so the key is never
                        // left active.
                        let outcome = AssertUnwindSafe(async { process(key.clone(), value).await })
                            .catch_unwind()
                            .await;
                        if let Err(panic) = outcome {
                            tracing::error!(
                                "KeyedCoalescingWorker item failed {}",
                                panic_message(panic.as_ref())
                            );
                        }
                        complete_key(&worker, key);
                    }
                }
            }
        });
        Self {
            shared,
            task: Mutex::new(Some(task)),
        }
    }

    pub fn enqueue(&self, key: K, value: V) {
        let mut state = self.shared.state.lock();
        if state.shut_down {
            return;
        }
        let merged = match state.latest_by_key.remove(&key) {
            Some(current) => (self.shared.merge)(current, value),
            None => value,
        };
        state.latest_by_key.insert(key.clone(), merged);
        if state.queued_keys.contains(&key) || state.active_keys.contains(&key) {
            self.shared.drained.notify_waiters();
            return;
        }
        state.queued_keys.insert(key.clone());
        state.queue.push_back(key);
        self.shared.work.notify_one();
    }

    pub async fn drain_key(&self, key: &K) {
        loop {
            let notified = self.shared.drained.notified();
            tokio::pin!(notified);
            if idle(&self.shared.state.lock(), key) {
                return;
            }
            notified.await;
        }
    }

    pub async fn close(self) {
        {
            let mut state = self.shared.state.lock();
            state.shut_down = true;
            state.queue.clear();
            state.latest_by_key.clear();
            state.queued_keys.clear();
        }
        self.shared.work.notify_waiters();
        self.shared.drained.notify_waiters();
        let task = self.task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

impl<K, V> Drop for KeyedCoalescingWorker<K, V> {
    fn drop(&mut self) {
        self.shared.state.lock().shut_down = true;
        if let Some(task) = self.task.lock().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Mutex as AsyncMutex, oneshot};

    #[tokio::test]
    async fn lets_a_queued_key_drain_before_a_continuously_updated_key() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        type WorkerSlot = Arc<AsyncMutex<Option<Arc<KeyedCoalescingWorker<String, i32>>>>>;
        let slot: WorkerSlot = Arc::new(AsyncMutex::new(None));
        let (busy_tx, busy_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let busy_tx = Arc::new(AsyncMutex::new(Some(busy_tx)));
        let release_rx = Arc::new(AsyncMutex::new(Some(release_rx)));
        let worker = Arc::new(KeyedCoalescingWorker::spawn(|_current: i32, next| next, {
            let slot = Arc::clone(&slot);
            let seen = Arc::clone(&processed);
            let busy_tx = Arc::clone(&busy_tx);
            let release_rx = Arc::clone(&release_rx);
            move |key: String, value: i32| {
                let seen = Arc::clone(&seen);
                let slot = Arc::clone(&slot);
                let busy_tx = Arc::clone(&busy_tx);
                let release_rx = Arc::clone(&release_rx);
                async move {
                    seen.lock().await.push(format!("{key}:{value}"));
                    if key != "busy" {
                        return;
                    }
                    if value == 1 {
                        if let Some(tx) = busy_tx.lock().await.take() {
                            let _ = tx.send(());
                        }
                        if let Some(rx) = release_rx.lock().await.take() {
                            let _ = rx.await;
                        }
                    }
                    if value < 4 {
                        let worker = slot.lock().await.clone().expect("worker");
                        worker.enqueue(key, value + 1);
                    }
                }
            }
        }));
        *slot.lock().await = Some(Arc::clone(&worker));
        worker.enqueue("busy".into(), 1);
        busy_rx.await.unwrap();
        worker.enqueue("quiet".into(), 1);
        let _ = release_tx.send(());
        worker.drain_key(&"quiet".into()).await;
        worker.drain_key(&"busy".into()).await;
        assert_eq!(
            processed.lock().await.as_slice(),
            [
                "busy:1".to_string(),
                "quiet:1".to_string(),
                "busy:2".to_string(),
                "busy:3".to_string(),
                "busy:4".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn a_panicking_item_releases_its_key_and_the_worker_keeps_running() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let seen = Arc::clone(&processed);
        let worker = KeyedCoalescingWorker::spawn(
            |_current: i32, next| next,
            move |key: String, value: i32| {
                let seen = Arc::clone(&seen);
                async move {
                    if value == 1 {
                        panic!("boom {key}");
                    }
                    seen.lock().await.push(format!("{key}:{value}"));
                }
            },
        );
        worker.enqueue("a".into(), 1);
        worker.enqueue("b".into(), 2);
        // Backstop: before the fix, these waits never resolved.
        let timeout = std::time::Duration::from_secs(5);
        tokio::time::timeout(timeout, worker.drain_key(&"a".into()))
            .await
            .expect("the panicking key drains");
        tokio::time::timeout(timeout, worker.drain_key(&"b".into()))
            .await
            .expect("the queued key behind it drains");
        // The key is no longer active, so new work for it runs.
        worker.enqueue("a".into(), 3);
        tokio::time::timeout(timeout, worker.drain_key(&"a".into()))
            .await
            .expect("the released key accepts new work");
        assert_eq!(
            processed.lock().await.as_slice(),
            ["b:2".to_string(), "a:3".to_string()]
        );
        worker.close().await;
    }

    #[tokio::test]
    async fn merges_updates_while_waiting_behind_another_key() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let (busy_tx, busy_rx) = oneshot::channel();
        let (release_busy_tx, release_busy_rx) = oneshot::channel::<()>();
        let (quiet_tx, quiet_rx) = oneshot::channel();
        let (release_quiet_tx, release_quiet_rx) = oneshot::channel::<()>();
        let (merged_tx, merged_rx) = oneshot::channel();
        let (release_merged_tx, release_merged_rx) = oneshot::channel::<()>();
        let busy_tx = Arc::new(AsyncMutex::new(Some(busy_tx)));
        let quiet_tx = Arc::new(AsyncMutex::new(Some(quiet_tx)));
        let merged_tx = Arc::new(AsyncMutex::new(Some(merged_tx)));
        let release_busy_rx = Arc::new(AsyncMutex::new(Some(release_busy_rx)));
        let release_quiet_rx = Arc::new(AsyncMutex::new(Some(release_quiet_rx)));
        let release_merged_rx = Arc::new(AsyncMutex::new(Some(release_merged_rx)));
        let worker = Arc::new(KeyedCoalescingWorker::spawn(
            |current: String, next| format!("{current}+{next}"),
            {
                let seen = Arc::clone(&processed);
                move |key: String, value: String| {
                    let seen = Arc::clone(&seen);
                    let busy_tx = Arc::clone(&busy_tx);
                    let quiet_tx = Arc::clone(&quiet_tx);
                    let merged_tx = Arc::clone(&merged_tx);
                    let release_busy_rx = Arc::clone(&release_busy_rx);
                    let release_quiet_rx = Arc::clone(&release_quiet_rx);
                    let release_merged_rx = Arc::clone(&release_merged_rx);
                    async move {
                        seen.lock().await.push(format!("{key}:{value}"));
                        if value == "first" {
                            if let Some(tx) = busy_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_busy_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                        } else if key == "quiet" {
                            if let Some(tx) = quiet_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_quiet_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                        } else if value == "a+b+c" {
                            if let Some(tx) = merged_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_merged_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                        }
                    }
                }
            },
        ));
        worker.enqueue("busy".into(), "first".into());
        busy_rx.await.unwrap();
        worker.enqueue("quiet".into(), "quiet".into());
        worker.enqueue("busy".into(), "a".into());
        let _ = release_busy_tx.send(());
        quiet_rx.await.unwrap();
        assert_eq!(
            processed.lock().await.as_slice(),
            ["busy:first".to_string(), "quiet:quiet".to_string()]
        );
        worker.enqueue("busy".into(), "b".into());
        worker.enqueue("busy".into(), "c".into());
        let draining = {
            let worker = Arc::clone(&worker);
            tokio::spawn(async move { worker.drain_key(&"busy".into()).await })
        };
        assert!(!draining.is_finished());
        let _ = release_quiet_tx.send(());
        merged_rx.await.unwrap();
        assert!(!draining.is_finished());
        let _ = release_merged_tx.send(());
        draining.await.unwrap();
        worker.drain_key(&"quiet".into()).await;
        assert_eq!(
            processed.lock().await.as_slice(),
            [
                "busy:first".to_string(),
                "quiet:quiet".to_string(),
                "busy:a+b+c".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn waits_for_work_enqueued_during_processing_before_draining() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let (first_tx, first_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel::<()>();
        let (second_tx, second_rx) = oneshot::channel();
        let (release_second_tx, release_second_rx) = oneshot::channel::<()>();
        let first_tx = Arc::new(AsyncMutex::new(Some(first_tx)));
        let second_tx = Arc::new(AsyncMutex::new(Some(second_tx)));
        let release_first_rx = Arc::new(AsyncMutex::new(Some(release_first_rx)));
        let release_second_rx = Arc::new(AsyncMutex::new(Some(release_second_rx)));
        let worker = Arc::new(KeyedCoalescingWorker::spawn(
            |_current: String, next| next,
            {
                let seen = Arc::clone(&processed);
                move |key: String, value: String| {
                    let seen = Arc::clone(&seen);
                    let first_tx = Arc::clone(&first_tx);
                    let second_tx = Arc::clone(&second_tx);
                    let release_first_rx = Arc::clone(&release_first_rx);
                    let release_second_rx = Arc::clone(&release_second_rx);
                    async move {
                        seen.lock().await.push(format!("{key}:{value}"));
                        if value == "first" {
                            if let Some(tx) = first_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_first_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                        } else if value == "second" {
                            if let Some(tx) = second_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_second_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                        }
                    }
                }
            },
        ));
        worker.enqueue("terminal-1".into(), "first".into());
        first_rx.await.unwrap();
        let draining = {
            let worker = Arc::clone(&worker);
            tokio::spawn(async move { worker.drain_key(&"terminal-1".into()).await })
        };
        worker.enqueue("terminal-1".into(), "second".into());
        let _ = release_first_tx.send(());
        second_rx.await.unwrap();
        assert!(!draining.is_finished());
        let _ = release_second_tx.send(());
        draining.await.unwrap();
        assert_eq!(
            processed.lock().await.as_slice(),
            [
                "terminal-1:first".to_string(),
                "terminal-1:second".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn requeues_pending_work_after_a_processor_failure() {
        let processed = Arc::new(AsyncMutex::new(Vec::new()));
        let (first_tx, first_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (second_tx, second_rx) = oneshot::channel();
        let first_tx = Arc::new(AsyncMutex::new(Some(first_tx)));
        let second_tx = Arc::new(AsyncMutex::new(Some(second_tx)));
        let release_rx = Arc::new(AsyncMutex::new(Some(release_rx)));
        let worker = Arc::new(KeyedCoalescingWorker::spawn(
            |_current: String, next| next,
            {
                let seen = Arc::clone(&processed);
                move |key: String, value: String| {
                    let seen = Arc::clone(&seen);
                    let first_tx = Arc::clone(&first_tx);
                    let second_tx = Arc::clone(&second_tx);
                    let release_rx = Arc::clone(&release_rx);
                    async move {
                        seen.lock().await.push(format!("{key}:{value}"));
                        if value == "first" {
                            if let Some(tx) = first_tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = release_rx.lock().await.take() {
                                let _ = rx.await;
                            }
                            // Upstream returns Effect.fail here. The worker ignores the
                            // failure and still completes the key.
                            return;
                        }
                        if value == "second" {
                            let _ = second_tx.lock().await.take().map(|tx| tx.send(()));
                        }
                    }
                }
            },
        ));
        worker.enqueue("terminal-1".into(), "first".into());
        first_rx.await.unwrap();
        worker.enqueue("terminal-2".into(), "quiet".into());
        worker.enqueue("terminal-1".into(), "second".into());
        let _ = release_tx.send(());
        second_rx.await.unwrap();
        worker.drain_key(&"terminal-1".into()).await;
        assert_eq!(
            processed.lock().await.as_slice(),
            [
                "terminal-1:first".to_string(),
                "terminal-2:quiet".to_string(),
                "terminal-1:second".to_string(),
            ]
        );
    }
}
