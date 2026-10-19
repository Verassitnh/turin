//! The typed in-process event source of the view-model layer.
//!
//! Upstream feeds its reducers from RPC subscriptions (`subscribeShell`,
//! `subscribeThread`) plus HTTP snapshots. Turin's orchestrator lives in the
//! same process, so it feeds the same frames directly:
//!
//! * the **shell** feed carries [`ShellStreamItem`]s (project/thread rows the
//!   orchestrator's shell projection produces);
//! * a **thread** feed carries [`ThreadStreamItem`]s for each thread the UI has
//!   opened: a snapshot (the full projection, optionally windowed) followed by
//!   the domain events committed after it.
//!
//! The UI asks for threads through [`ClientStateRequest`]s on a second channel
//! the orchestrator-side adapter drains.

use tokio::sync::mpsc;

use crate::state::contracts::{ThreadId, UtcDateTime};
use crate::state::shell_reducer::ShellStreamItem;
use crate::state::thread_history_merge::ThreadHistoryPageMeta;
use crate::state::thread_lifecycle::{OptimisticThreadUpdate, OptimisticUpdateId};
use crate::state::thread_projection::VisibleTurnItem;
use crate::state::thread_state::ThreadStreamItem;

/// What happened to a feed's connection. In-process feeds rarely drop; the
/// orchestrator uses these around (re)starts and when it fails to project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedStatus {
    /// The feed is (re)starting; `awaiting_completion` when it will send `Synchronized`.
    Connecting { awaiting_completion: bool },
    /// The feed ended.
    Disconnected,
    /// The feed failed; the state keeps its data and shows the message.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClientStateEvent {
    /// A batch of shell frames, applied with one state write.
    Shell(Vec<ShellStreamItem>),
    ShellFeed(FeedStatus),
    /// A batch of frames for one open thread, applied with one state write.
    Thread {
        thread_id: ThreadId,
        items: Vec<ThreadStreamItem>,
    },
    ThreadFeed {
        thread_id: ThreadId,
        status: FeedStatus,
    },
    /// An older page of a windowed thread (the answer to [`ClientStateRequest::LoadEarlier`]).
    ThreadHistoryPage {
        thread_id: ThreadId,
        /// The cursor the request was made with; stale responses are ignored.
        request_cursor: String,
        items: Vec<VisibleTurnItem>,
        meta: ThreadHistoryPageMeta,
    },
    /// Loading an older page failed.
    ThreadHistoryFailed {
        thread_id: ThreadId,
        request_cursor: String,
        message: String,
    },
    /// Forget a thread nobody is looking at any more (idle TTL).
    ForgetThread(ThreadId),
    /// The Scratch project folder (`ServerConfig.scratchWorkspaceRoot`), if any.
    ScratchWorkspaceRoot(Option<String>),
    /// A lifecycle command started: show `update` on the thread's shell row
    /// until the command fails or the shell reaches its sequence.
    OptimisticBegin {
        id: OptimisticUpdateId,
        thread_id: ThreadId,
        update: OptimisticThreadUpdate,
        now: UtcDateTime,
    },
    /// The command was accepted at `sequence`.
    OptimisticConfirm {
        id: OptimisticUpdateId,
        sequence: u64,
    },
    /// The command failed or was dropped.
    OptimisticDiscard {
        id: OptimisticUpdateId,
    },
}

/// What the UI asks the orchestrator-side adapter for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientStateRequest {
    /// Start feeding `ClientStateEvent::Thread` for this thread: a snapshot,
    /// then events after it. `after_sequence` is the last sequence the store
    /// already applied (a thread reopened within the idle TTL): the feed may
    /// resume from there instead of sending a snapshot.
    OpenThread {
        thread_id: ThreadId,
        after_sequence: Option<u64>,
    },
    /// Nobody is looking at the thread; the feed may stop. Its state is kept
    /// for [`THREAD_SNAPSHOT_IDLE_TTL`] so back navigation is instant.
    CloseThread(ThreadId),
    /// Fetch the page of history before `cursor` (`OrchestrationV2ThreadHistoryPage`).
    /// `through_entry_id` (`throughEntryId`, a find match) widens the page until it
    /// reaches that entry.
    LoadEarlier {
        thread_id: ThreadId,
        cursor: String,
        through_entry_id: Option<String>,
    },
}

/// Recent thread snapshots stay for back navigation after their last consumer leaves.
pub const THREAD_SNAPSHOT_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The orchestrator's end of the feed. Cheap to clone.
#[derive(Clone)]
pub struct ClientStateSender {
    pub(crate) tx: mpsc::UnboundedSender<ClientStateEvent>,
}

impl ClientStateSender {
    /// Never blocks and never fails the producer: if the store is gone the
    /// event is dropped.
    pub fn send(&self, event: ClientStateEvent) {
        let _ = self.tx.send(event);
    }

    pub fn shell(&self, items: Vec<ShellStreamItem>) {
        self.send(ClientStateEvent::Shell(items));
    }

    pub fn thread(&self, thread_id: ThreadId, items: Vec<ThreadStreamItem>) {
        self.send(ClientStateEvent::Thread { thread_id, items });
    }
}

/// The adapter's end of the request channel.
pub type ClientStateRequests = mpsc::UnboundedReceiver<ClientStateRequest>;
