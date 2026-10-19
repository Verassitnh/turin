//! The store: runs [`ClientStateCore`] off the UI thread and publishes
//! immutable [`ClientState`] snapshots to GPUI.
//!
//! ```text
//! orchestrator ──ClientStateSender──▶ reducer task ──watch<Arc<ClientState>>──▶ ClientStore entity ──▶ views
//!      ▲                                   │
//!      └──────── ClientStateRequests ◀─────┘  (open/close thread, load earlier)
//! ```
//!
//! The reducer task batches whatever has queued up (streaming deltas arrive
//! faster than frames) into one state write and one publish. Views `observe`
//! the [`ClientStore`] entity and read `store.state()`; they never block on
//! the reducer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::{App, AppContext as _, Context, Entity, Global};
use tokio::sync::{mpsc, watch};

use crate::state::client_state::{ClientState, ClientStateCore};
use crate::state::contracts::{EnvironmentId, ThreadId};
use crate::state::events::{
    ClientStateEvent, ClientStateRequest, ClientStateRequests, ClientStateSender, FeedStatus,
    THREAD_SNAPSHOT_IDLE_TTL,
};

/// Events folded into one state write at most this many at a time, so a flood
/// cannot starve publishing.
const MAX_BATCH: usize = 512;

/// Everything the reducer task consumes.
enum Input {
    Event(ClientStateEvent),
    OpenThread(ThreadId),
    CloseThread(ThreadId),
    LoadEarlier(ThreadId, Option<String>),
    /// A closed thread's idle TTL elapsed (generation guards against a reopen).
    Expire(ThreadId, u64),
}

/// The UI's end: observe the published state, open/close threads, page history.
#[derive(Clone)]
pub struct ClientStateHandle {
    state: watch::Receiver<Arc<ClientState>>,
    inputs: mpsc::UnboundedSender<Input>,
    events: ClientStateSender,
}

impl ClientStateHandle {
    pub fn snapshot(&self) -> Arc<ClientState> {
        Arc::clone(&self.state.borrow())
    }

    /// A consumer needs this thread live. Balanced by [`Self::close_thread`].
    pub fn open_thread(&self, thread_id: ThreadId) {
        let _ = self.inputs.send(Input::OpenThread(thread_id));
    }

    /// A consumer left the thread. Its state is kept for [`THREAD_SNAPSHOT_IDLE_TTL`].
    pub fn close_thread(&self, thread_id: ThreadId) {
        let _ = self.inputs.send(Input::CloseThread(thread_id));
    }

    /// The timeline's "load earlier" control.
    pub fn load_earlier(&self, thread_id: ThreadId) {
        let _ = self.inputs.send(Input::LoadEarlier(thread_id, None));
    }

    /// The thread find's paging (`onLoadEarlier(throughEntryId)`): the page that
    /// reaches `entry_id`, a match in older history.
    pub fn load_earlier_through(&self, thread_id: ThreadId, entry_id: String) {
        let _ = self
            .inputs
            .send(Input::LoadEarlier(thread_id, Some(entry_id)));
    }

    /// For tests and glue: feed an event as the orchestrator would.
    pub fn sender(&self) -> ClientStateSender {
        self.events.clone()
    }
}

/// What [`spawn_client_state`] returns.
pub struct ClientStateRuntime {
    /// Give this to the orchestrator-side adapter: it feeds events through it.
    pub sender: ClientStateSender,
    /// The adapter drains this: which threads to stream, which pages to fetch.
    pub requests: ClientStateRequests,
    /// Give this to the UI.
    pub handle: ClientStateHandle,
}

struct Reducer {
    core: ClientStateCore,
    open_counts: HashMap<ThreadId, usize>,
    /// Threads the orchestrator is currently asked to stream.
    streaming: HashSet<ThreadId>,
    expire_generation: HashMap<ThreadId, u64>,
    requests: mpsc::UnboundedSender<ClientStateRequest>,
    inputs: mpsc::UnboundedSender<Input>,
    runtime: tokio::runtime::Handle,
}

impl Reducer {
    /// Returns whether published state may have changed.
    fn handle(&mut self, input: Input) -> bool {
        match input {
            Input::Event(event) => self.core.apply(event),
            Input::OpenThread(thread_id) => {
                let count = self.open_counts.entry(thread_id.clone()).or_insert(0);
                *count += 1;
                // Reopening cancels a pending expiry.
                *self.expire_generation.entry(thread_id.clone()).or_insert(0) += 1;
                if self.streaming.insert(thread_id.clone()) {
                    let after_sequence = self
                        .core
                        .snapshot()
                        .thread(&thread_id)
                        .filter(|entry| entry.state.data.is_some())
                        .map(|entry| entry.state.applied_sequence);
                    let changed = self.core.apply(ClientStateEvent::ThreadFeed {
                        thread_id: thread_id.clone(),
                        status: FeedStatus::Connecting {
                            awaiting_completion: false,
                        },
                    });
                    let _ = self.requests.send(ClientStateRequest::OpenThread {
                        thread_id,
                        after_sequence,
                    });
                    return changed;
                }
                false
            }
            Input::CloseThread(thread_id) => {
                let Some(count) = self.open_counts.get_mut(&thread_id) else {
                    return false;
                };
                *count = count.saturating_sub(1);
                if *count > 0 {
                    return false;
                }
                self.open_counts.remove(&thread_id);
                if self.streaming.remove(&thread_id) {
                    let _ = self
                        .requests
                        .send(ClientStateRequest::CloseThread(thread_id.clone()));
                }
                let generation = {
                    let generation = self.expire_generation.entry(thread_id.clone()).or_insert(0);
                    *generation += 1;
                    *generation
                };
                let inputs = self.inputs.clone();
                let expiring = thread_id.clone();
                self.runtime.spawn(async move {
                    tokio::time::sleep(THREAD_SNAPSHOT_IDLE_TTL).await;
                    let _ = inputs.send(Input::Expire(expiring, generation));
                });
                self.core.apply(ClientStateEvent::ThreadFeed {
                    thread_id,
                    status: FeedStatus::Disconnected,
                })
            }
            Input::LoadEarlier(thread_id, through_entry_id) => match self
                .core
                .begin_load_earlier(&thread_id)
            {
                Some(cursor) => {
                    let _ = self.requests.send(ClientStateRequest::LoadEarlier {
                        thread_id,
                        cursor,
                        through_entry_id,
                    });
                    true
                }
                None => false,
            },
            Input::Expire(thread_id, generation) => {
                if self.expire_generation.get(&thread_id) == Some(&generation)
                    && !self.open_counts.contains_key(&thread_id)
                {
                    self.expire_generation.remove(&thread_id);
                    return self.core.apply(ClientStateEvent::ForgetThread(thread_id));
                }
                false
            }
        }
    }
}

/// Starts the reducer task on `runtime`.
pub fn spawn_client_state(
    runtime: &tokio::runtime::Handle,
    environment_id: EnvironmentId,
) -> ClientStateRuntime {
    let core = ClientStateCore::new(environment_id);
    let (state_tx, state_rx) = watch::channel(core.snapshot());
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Input>();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ClientStateEvent>();
    let (request_tx, request_rx) = mpsc::unbounded_channel::<ClientStateRequest>();

    // Producer events share the reducer's input channel.
    {
        let input_tx = input_tx.clone();
        runtime.spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if input_tx.send(Input::Event(event)).is_err() {
                    break;
                }
            }
        });
    }

    let mut reducer = Reducer {
        core,
        open_counts: HashMap::new(),
        streaming: HashSet::new(),
        expire_generation: HashMap::new(),
        requests: request_tx,
        inputs: input_tx.clone(),
        runtime: runtime.clone(),
    };
    runtime.spawn(async move {
        while let Some(first) = input_rx.recv().await {
            let mut changed = reducer.handle(first);
            let mut folded = 1;
            while folded < MAX_BATCH {
                match input_rx.try_recv() {
                    Ok(input) => {
                        changed |= reducer.handle(input);
                        folded += 1;
                    }
                    Err(_) => break,
                }
            }
            if changed {
                state_tx.send_replace(reducer.core.publish());
            }
        }
    });

    let sender = ClientStateSender { tx: event_tx };
    ClientStateRuntime {
        sender: sender.clone(),
        requests: request_rx,
        handle: ClientStateHandle {
            state: state_rx,
            inputs: input_tx,
            events: sender,
        },
    }
}

/// The GPUI entity views observe. Reads are plain field reads of an `Arc`.
pub struct ClientStore {
    state: Arc<ClientState>,
    handle: ClientStateHandle,
}

impl ClientStore {
    pub fn state(&self) -> &Arc<ClientState> {
        &self.state
    }

    pub fn handle(&self) -> &ClientStateHandle {
        &self.handle
    }
}

/// The app-wide store entity (`cx.global::<ClientStoreGlobal>()`).
pub struct ClientStoreGlobal(pub Entity<ClientStore>);

impl Global for ClientStoreGlobal {}

/// Creates the store entity and keeps it current from the reducer task.
pub fn bind_client_store(handle: ClientStateHandle, cx: &mut App) -> Entity<ClientStore> {
    let mut changes = handle.state.clone();
    let store = cx.new(|_| ClientStore {
        state: handle.snapshot(),
        handle,
    });
    let weak = store.downgrade();
    cx.spawn(async move |cx| {
        while changes.changed().await.is_ok() {
            let next = Arc::clone(&changes.borrow_and_update());
            let alive = weak.update(
                cx,
                |store: &mut ClientStore, cx: &mut Context<ClientStore>| {
                    store.state = next;
                    cx.notify();
                },
            );
            if alive.is_err() {
                break;
            }
        }
    })
    .detach();
    store
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ShellLocation;
    use crate::state::events::ClientStateEvent;
    use crate::state::shell_reducer::{ShellSnapshot, ShellStreamEvent, ShellStreamItem};
    use crate::state::test_fixtures::{v2_project, v2_projection, v2_thread_shell};
    use crate::state::thread_state::ThreadStreamItem;
    use std::time::Duration;

    fn thread_id() -> ThreadId {
        ThreadId::new("thread-v2").unwrap()
    }

    fn snapshot_item(sequence: u64) -> ShellStreamItem {
        ShellStreamItem::Snapshot {
            snapshot: ShellSnapshot {
                schema_version: crate::state::contracts::PositiveInt::new(1).unwrap(),
                snapshot_sequence: sequence,
                threads: Arc::new(vec![Arc::new(v2_thread_shell())]),
                archived_threads: Arc::new(vec![]),
                projects: Arc::new(vec![Arc::new(v2_project())]),
            },
            resolved_repository_identity_roots: None,
        }
    }

    async fn next_state(handle: &mut watch::Receiver<Arc<ClientState>>) -> Arc<ClientState> {
        tokio::time::timeout(Duration::from_secs(5), handle.changed())
            .await
            .expect("a publish")
            .expect("open");
        Arc::clone(&handle.borrow_and_update())
    }

    #[tokio::test]
    async fn publishes_shell_state_off_the_producers_thread() {
        let mut runtime = spawn_client_state(
            &tokio::runtime::Handle::current(),
            EnvironmentId::new("local").unwrap(),
        );
        let mut states = runtime.handle.state.clone();
        runtime.sender.shell(vec![snapshot_item(1)]);
        let state = next_state(&mut states).await;
        assert_eq!(state.shell_view.as_ref().unwrap().threads.len(), 1);
        runtime.sender.shell(vec![ShellStreamItem::Event(
            ShellStreamEvent::ThreadUpdated {
                sequence: 2,
                location: OrchestrationV2ShellLocation::Active,
                thread: Arc::new(crate::state::contracts::OrchestrationV2ThreadShell {
                    title: "Renamed".into(),
                    ..v2_thread_shell()
                }),
            },
        )]);
        let state = next_state(&mut states).await;
        assert_eq!(
            state.shell_view.as_ref().unwrap().threads[0].title,
            "Renamed"
        );
        assert!(runtime.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn opening_a_thread_requests_its_feed_once_and_closing_stops_it() {
        let mut runtime = spawn_client_state(
            &tokio::runtime::Handle::current(),
            EnvironmentId::new("local").unwrap(),
        );
        let handle = runtime.handle.clone();
        handle.open_thread(thread_id());
        handle.open_thread(thread_id());
        let request = tokio::time::timeout(Duration::from_secs(5), runtime.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request,
            ClientStateRequest::OpenThread {
                thread_id: thread_id(),
                after_sequence: None
            }
        );
        // Both consumers share one feed.
        handle.close_thread(thread_id());
        handle
            .sender()
            .send(ClientStateEvent::ScratchWorkspaceRoot(None));
        assert!(runtime.requests.try_recv().is_err());
        handle.close_thread(thread_id());
        let request = tokio::time::timeout(Duration::from_secs(5), runtime.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request, ClientStateRequest::CloseThread(thread_id()));
    }

    #[tokio::test]
    async fn a_reopened_thread_resumes_after_the_applied_sequence() {
        let mut runtime = spawn_client_state(
            &tokio::runtime::Handle::current(),
            EnvironmentId::new("local").unwrap(),
        );
        let mut states = runtime.handle.state.clone();
        let handle = runtime.handle.clone();
        handle.open_thread(thread_id());
        let _ = runtime.requests.recv().await;
        runtime.sender.thread(
            thread_id(),
            vec![ThreadStreamItem::Snapshot {
                snapshot_sequence: 9,
                projection: Arc::new(v2_projection().into()),
                history: None,
            }],
        );
        loop {
            let state = next_state(&mut states).await;
            if state
                .thread(&thread_id())
                .is_some_and(|entry| entry.state.data.is_some())
            {
                break;
            }
        }
        handle.close_thread(thread_id());
        let _ = runtime.requests.recv().await;
        handle.open_thread(thread_id());
        let request = runtime.requests.recv().await.unwrap();
        assert_eq!(
            request,
            ClientStateRequest::OpenThread {
                thread_id: thread_id(),
                after_sequence: Some(9)
            }
        );
        // The data is still there while the feed resumes.
        assert!(
            handle
                .snapshot()
                .thread(&thread_id())
                .unwrap()
                .state
                .data
                .is_some()
        );
    }
}
