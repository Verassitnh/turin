//! The view-model core: what the UI reads, and the reducer that keeps it current.
//!
//! [`ClientStateCore`] is single-threaded and synchronous (no IO, no locks);
//! the store runs it on its own task and publishes immutable
//! [`ClientState`] snapshots. A snapshot shares every row, collection and
//! derived view with its predecessor unless an event changed it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::state::contracts::{EnvironmentId, ThreadId};
use crate::state::events::{ClientStateEvent, FeedStatus};
use crate::state::shell_state::{EnvironmentShellState, EnvironmentShellStatus};
use crate::state::thread_detail::{ThreadDetailSelectors, ThreadDetailView};
use crate::state::thread_history_merge::{
    ThreadHistoryMeta, apply_history_page_meta, clear_active_history_loading,
    is_active_history_request_cursor, merge_older_history_into_projection,
};
use crate::state::thread_shell::{ShellView, ShellViewBuilder};
use crate::state::thread_state::{EnvironmentThreadState, EnvironmentThreadStatus};

/// One open (or recently open) thread: its state machine and derived views.
#[derive(Clone, Debug)]
pub struct ThreadEntry {
    pub state: EnvironmentThreadState,
    /// Selectors over `state.data`; `None` until a projection arrives.
    pub detail: Option<ThreadDetailView>,
}

/// The immutable snapshot the UI reads.
#[derive(Clone, Debug)]
pub struct ClientState {
    /// Bumped for every published change.
    pub revision: u64,
    pub environment_id: EnvironmentId,
    pub shell: EnvironmentShellState,
    /// Present once a shell snapshot arrived.
    pub shell_view: Option<ShellView>,
    pub threads: Arc<HashMap<ThreadId, Arc<ThreadEntry>>>,
}

impl ClientState {
    pub fn shell_status(&self) -> EnvironmentShellStatus {
        self.shell.status
    }

    pub fn thread(&self, thread_id: &ThreadId) -> Option<&Arc<ThreadEntry>> {
        self.threads.get(thread_id)
    }

    /// `threadDetail` for a thread, when its projection is loaded.
    pub fn thread_detail(&self, thread_id: &ThreadId) -> Option<&ThreadDetailView> {
        self.threads.get(thread_id)?.detail.as_ref()
    }

    pub fn thread_status(&self, thread_id: &ThreadId) -> EnvironmentThreadStatus {
        self.threads
            .get(thread_id)
            .map_or(EnvironmentThreadStatus::Empty, |entry| entry.state.status)
    }
}

struct ThreadSlot {
    state: EnvironmentThreadState,
    selectors: ThreadDetailSelectors,
    entry: Arc<ThreadEntry>,
}

impl ThreadSlot {
    fn new() -> Self {
        let state = EnvironmentThreadState::default();
        Self {
            entry: Arc::new(ThreadEntry {
                state: state.clone(),
                detail: None,
            }),
            state,
            selectors: ThreadDetailSelectors::new(),
        }
    }

    /// Rebuild the published entry after `state` changed.
    fn refresh(&mut self) {
        let detail = self
            .state
            .data
            .as_ref()
            .map(|projection| self.selectors.select(projection));
        self.entry = Arc::new(ThreadEntry {
            state: self.state.clone(),
            detail,
        });
    }
}

pub struct ClientStateCore {
    environment_id: EnvironmentId,
    shell: EnvironmentShellState,
    shell_views: ShellViewBuilder,
    scratch_workspace_root: Option<String>,
    threads: HashMap<ThreadId, ThreadSlot>,
    published: Arc<ClientState>,
    revision: u64,
}

impl ClientStateCore {
    pub fn new(environment_id: EnvironmentId) -> Self {
        let shell = EnvironmentShellState::default();
        let published = Arc::new(ClientState {
            revision: 0,
            environment_id: environment_id.clone(),
            shell: shell.clone(),
            shell_view: None,
            threads: Arc::new(HashMap::new()),
        });
        Self {
            environment_id,
            shell,
            shell_views: ShellViewBuilder::new(),
            scratch_workspace_root: None,
            threads: HashMap::new(),
            published,
            revision: 0,
        }
    }

    pub fn snapshot(&self) -> Arc<ClientState> {
        Arc::clone(&self.published)
    }

    /// Applies one event. Returns whether the state changed (call
    /// [`Self::publish`] after a batch).
    pub fn apply(&mut self, event: ClientStateEvent) -> bool {
        match event {
            ClientStateEvent::Shell(items) => self.shell.apply_items(items),
            ClientStateEvent::ShellFeed(status) => {
                let before = self.shell.clone();
                match status {
                    FeedStatus::Connecting {
                        awaiting_completion,
                    } => {
                        self.shell.set_awaiting_completion(awaiting_completion);
                        self.shell.set_synchronizing();
                    }
                    FeedStatus::Disconnected => self.shell.set_disconnected(),
                    FeedStatus::Failed(_) => self.shell.set_stream_error(),
                }
                self.shell != before
            }
            ClientStateEvent::Thread { thread_id, items } => {
                let slot = self
                    .threads
                    .entry(thread_id)
                    .or_insert_with(ThreadSlot::new);
                let changed = slot.state.apply_items(items);
                if changed {
                    slot.refresh();
                }
                changed
            }
            ClientStateEvent::ThreadFeed { thread_id, status } => {
                let slot = self
                    .threads
                    .entry(thread_id)
                    .or_insert_with(ThreadSlot::new);
                let before = slot.state.clone();
                match status {
                    FeedStatus::Connecting {
                        awaiting_completion,
                    } => slot.state.set_connecting(awaiting_completion),
                    FeedStatus::Disconnected => slot.state.set_disconnected(),
                    FeedStatus::Failed(message) => slot.state.set_stream_error(message),
                }
                let changed = slot.state != before;
                if changed {
                    slot.refresh();
                }
                changed
            }
            ClientStateEvent::ThreadHistoryPage {
                thread_id,
                request_cursor,
                items,
                meta,
            } => {
                let Some(slot) = self.threads.get_mut(&thread_id) else {
                    return false;
                };
                // Only the response to the active request applies.
                if !is_active_history_request_cursor(&request_cursor, &slot.state.history) {
                    return false;
                }
                if let Some(projection) = &slot.state.data
                    && let Some(merged) = merge_older_history_into_projection(projection, &items)
                {
                    slot.state.data = Some(Arc::new(merged));
                }
                slot.state.history = apply_history_page_meta(&slot.state.history, &meta);
                slot.refresh();
                true
            }
            ClientStateEvent::ThreadHistoryFailed {
                thread_id,
                request_cursor,
                message,
            } => {
                let Some(slot) = self.threads.get_mut(&thread_id) else {
                    return false;
                };
                if !is_active_history_request_cursor(&request_cursor, &slot.state.history) {
                    return false;
                }
                slot.state.history = ThreadHistoryMeta {
                    error: Some(message),
                    ..clear_active_history_loading(&request_cursor, &slot.state.history)
                };
                slot.state.history.loading = false;
                slot.refresh();
                true
            }
            ClientStateEvent::ForgetThread(thread_id) => self.threads.remove(&thread_id).is_some(),
            ClientStateEvent::ScratchWorkspaceRoot(root) => {
                if self.scratch_workspace_root == root {
                    return false;
                }
                self.scratch_workspace_root = root;
                true
            }
        }
    }

    /// Marks a thread's history as loading and returns the cursor to request,
    /// if there is an earlier page and none is in flight.
    pub fn begin_load_earlier(&mut self, thread_id: &ThreadId) -> Option<String> {
        let slot = self.threads.get_mut(thread_id)?;
        let history = &slot.state.history;
        if history.loading || !history.has_more_history {
            return None;
        }
        let cursor = history.history_cursor.clone()?;
        slot.state.history.loading = true;
        slot.state.history.error = None;
        slot.refresh();
        Some(cursor)
    }

    /// Rebuilds the published snapshot from the current state and returns it.
    pub fn publish(&mut self) -> Arc<ClientState> {
        self.revision += 1;
        let shell_view = self.shell.snapshot.as_ref().map(|snapshot| {
            self.shell_views.build(
                &self.environment_id,
                snapshot,
                self.scratch_workspace_root.as_deref(),
            )
        });
        let threads: HashMap<ThreadId, Arc<ThreadEntry>> = self
            .threads
            .iter()
            .map(|(id, slot)| (id.clone(), Arc::clone(&slot.entry)))
            .collect();
        self.published = Arc::new(ClientState {
            revision: self.revision,
            environment_id: self.environment_id.clone(),
            shell: self.shell.clone(),
            shell_view,
            threads: Arc::new(threads),
        });
        Arc::clone(&self.published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{OrchestrationV2ShellLocation, OrchestrationV2ThreadShell};
    use crate::state::shell_reducer::{ShellSnapshot, ShellStreamEvent, ShellStreamItem};
    use crate::state::test_fixtures::{from_json, v2_project, v2_projection, v2_thread_shell};
    use crate::state::thread_history_merge::ThreadHistoryPageMeta;
    use crate::state::thread_state::ThreadStreamItem;
    use serde_json::json;

    fn env() -> EnvironmentId {
        EnvironmentId::new("local").unwrap()
    }

    fn thread_id() -> ThreadId {
        ThreadId::new("thread-v2").unwrap()
    }

    fn shell_snapshot(sequence: u64) -> ShellStreamItem {
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

    fn thread_snapshot(sequence: u64) -> ThreadStreamItem {
        ThreadStreamItem::Snapshot {
            snapshot_sequence: sequence,
            projection: Arc::new(v2_projection().into()),
            history: None,
        }
    }

    fn apply_and_publish(core: &mut ClientStateCore, event: ClientStateEvent) -> Arc<ClientState> {
        assert!(core.apply(event));
        core.publish()
    }

    #[test]
    fn publishes_a_shell_view_once_the_first_snapshot_lands() {
        let mut core = ClientStateCore::new(env());
        assert!(core.snapshot().shell_view.is_none());
        let state = apply_and_publish(&mut core, ClientStateEvent::Shell(vec![shell_snapshot(1)]));
        let view = state.shell_view.as_ref().unwrap();
        assert_eq!(view.threads.len(), 1);
        assert_eq!(view.projects.len(), 1);
        assert_eq!(state.shell_status(), EnvironmentShellStatus::Live);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn a_shell_event_only_replaces_the_row_it_touched() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Shell(vec![shell_snapshot(1)]));
        let before = core.publish();
        let renamed = Arc::new(OrchestrationV2ThreadShell {
            title: "Renamed".into(),
            ..v2_thread_shell()
        });
        let after = apply_and_publish(
            &mut core,
            ClientStateEvent::Shell(vec![ShellStreamItem::Event(
                ShellStreamEvent::ThreadUpdated {
                    sequence: 2,
                    location: OrchestrationV2ShellLocation::Active,
                    thread: renamed,
                },
            )]),
        );
        let (old_view, new_view) = (
            before.shell_view.as_ref().unwrap(),
            after.shell_view.as_ref().unwrap(),
        );
        assert_eq!(new_view.threads[0].title, "Renamed");
        assert!(Arc::ptr_eq(&old_view.projects, &new_view.projects));
    }

    #[test]
    fn opens_a_thread_and_derives_its_views() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::ThreadFeed {
            thread_id: thread_id(),
            status: FeedStatus::Connecting {
                awaiting_completion: false,
            },
        });
        assert_eq!(
            core.publish().thread_status(&thread_id()),
            EnvironmentThreadStatus::Synchronizing
        );
        let state = apply_and_publish(
            &mut core,
            ClientStateEvent::Thread {
                thread_id: thread_id(),
                items: vec![thread_snapshot(5)],
            },
        );
        assert_eq!(
            state.thread_status(&thread_id()),
            EnvironmentThreadStatus::Live
        );
        let detail = state.thread_detail(&thread_id()).unwrap();
        assert!(detail.pending_requests.approvals.is_empty());
        assert!(detail.runtime.is_none());
    }

    #[test]
    fn untouched_threads_keep_their_entries_across_publishes() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![thread_snapshot(1)],
        });
        let before = core.publish();
        core.apply(ClientStateEvent::Shell(vec![shell_snapshot(1)]));
        let after = core.publish();
        assert!(Arc::ptr_eq(
            before.thread(&thread_id()).unwrap(),
            after.thread(&thread_id()).unwrap()
        ));
        assert!(after.revision > before.revision);
    }

    #[test]
    fn no_op_events_report_no_change() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![thread_snapshot(5)],
        });
        assert!(!core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![ThreadStreamItem::Event {
                sequence: 3,
                event: Box::new(from_json(json!({
                    "id": "old", "type": "run.created", "threadId": "thread-v2",
                    "occurredAt": "2026-06-20T00:00:00.000Z",
                    "payload": serde_json::to_value(crate::state::test_fixtures::v2_run("r", 1, "queued")).unwrap(),
                }))),
            }],
        }));
        assert!(!core.apply(ClientStateEvent::ScratchWorkspaceRoot(None)));
    }

    #[test]
    fn the_scratch_root_marks_the_scratch_project() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Shell(vec![shell_snapshot(1)]));
        assert!(!core.publish().shell_view.as_ref().unwrap().projects[0].is_scratch);
        assert!(core.apply(ClientStateEvent::ScratchWorkspaceRoot(Some(
            "/workspace/project/".into()
        ))));
        assert!(core.publish().shell_view.as_ref().unwrap().projects[0].is_scratch);
    }

    #[test]
    fn loads_earlier_history_only_for_the_active_cursor() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![ThreadStreamItem::Snapshot {
                snapshot_sequence: 1,
                projection: Arc::new(v2_projection().into()),
                history: Some(ThreadHistoryMeta {
                    history_cursor: Some("c1".into()),
                    has_more_history: true,
                    ..ThreadHistoryMeta::default()
                }),
            }],
        });
        assert_eq!(core.begin_load_earlier(&thread_id()).as_deref(), Some("c1"));
        // A second request while one is in flight is refused.
        assert_eq!(core.begin_load_earlier(&thread_id()), None);
        // A stale response does nothing.
        assert!(!core.apply(ClientStateEvent::ThreadHistoryPage {
            thread_id: thread_id(),
            request_cursor: "stale".into(),
            items: vec![],
            meta: ThreadHistoryPageMeta {
                next_cursor: None,
                has_more_history: false
            },
        }));
        // The active response applies and ends paging.
        assert!(core.apply(ClientStateEvent::ThreadHistoryPage {
            thread_id: thread_id(),
            request_cursor: "c1".into(),
            items: vec![],
            meta: ThreadHistoryPageMeta {
                next_cursor: None,
                has_more_history: false
            },
        }));
        let state = core.publish();
        let history = &state.thread(&thread_id()).unwrap().state.history;
        assert!(!history.loading && !history.has_more_history && history.expanded);
    }

    #[test]
    fn a_failed_page_load_surfaces_the_error_and_stops_loading() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![ThreadStreamItem::Snapshot {
                snapshot_sequence: 1,
                projection: Arc::new(v2_projection().into()),
                history: Some(ThreadHistoryMeta {
                    history_cursor: Some("c1".into()),
                    has_more_history: true,
                    ..ThreadHistoryMeta::default()
                }),
            }],
        });
        core.begin_load_earlier(&thread_id());
        assert!(core.apply(ClientStateEvent::ThreadHistoryFailed {
            thread_id: thread_id(),
            request_cursor: "c1".into(),
            message: "Could not load earlier activity.".into(),
        }));
        let history = core
            .publish()
            .thread(&thread_id())
            .unwrap()
            .state
            .history
            .clone();
        assert!(!history.loading);
        assert_eq!(
            history.error.as_deref(),
            Some("Could not load earlier activity.")
        );
    }

    #[test]
    fn forgetting_a_thread_drops_its_entry() {
        let mut core = ClientStateCore::new(env());
        core.apply(ClientStateEvent::Thread {
            thread_id: thread_id(),
            items: vec![thread_snapshot(1)],
        });
        assert!(core.apply(ClientStateEvent::ForgetThread(thread_id())));
        assert!(core.publish().thread(&thread_id()).is_none());
        assert!(!core.apply(ClientStateEvent::ForgetThread(thread_id())));
    }
}
