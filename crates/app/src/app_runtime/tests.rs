//! Read path end to end: a temp database, the composition root, and the real
//! client store. Each test asserts what the store holds, the same state the
//! views render.

use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;

use super::{Stores, dev_seed, serve};
use crate::state::client_state::ClientState;
use crate::state::contracts::ThreadId;
use crate::state::store::{ClientStateHandle, spawn_client_state};

const ATLAS_LOGIN: &str = "thread-atlas-login";
const ATLAS_UPLOADS: &str = "thread-atlas-uploads";

/// A store fed by the read path over `stores`. Seed before calling this: the
/// shell snapshot is read once, at start.
fn start(stores: Arc<Stores>) -> ClientStateHandle {
    let runtime = spawn_client_state(&Handle::current(), crate::state::local_environment_id());
    tokio::spawn(serve(stores, runtime.sender, runtime.requests));
    runtime.handle
}

/// Polls the published store until `ready` holds. Publishing is batched, so
/// the state is read fresh on each poll.
async fn wait_for(
    handle: &ClientStateHandle,
    what: &str,
    ready: impl Fn(&ClientState) -> bool,
) -> Arc<ClientState> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = handle.snapshot();
            if ready(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn thread_id(id: &str) -> ThreadId {
    ThreadId::new(id).expect("static id")
}

fn visible_items(state: &ClientState, id: &str) -> Option<usize> {
    state
        .thread(&thread_id(id))?
        .state
        .data
        .as_ref()
        .map(|data| data.visible_turn_items.len())
}

#[tokio::test]
async fn seeded_database_fills_the_sidebar() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stores = Arc::new(Stores::open(dir.path()).await.expect("open"));
    dev_seed::seed(&stores.sink).await.expect("seed");

    let handle = start(stores);
    let state = wait_for(&handle, "the shell", |state| {
        state
            .shell_view
            .as_ref()
            .is_some_and(|view| view.threads.len() == 4)
    })
    .await;

    let view = state.shell_view.as_ref().expect("shell");
    assert_eq!(view.projects.len(), 2, "projects");
    assert_eq!(view.threads.len(), 4, "threads");
}

#[tokio::test]
async fn opening_a_thread_shows_its_bounded_window_and_pages_older_history() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stores = Arc::new(Stores::open(dir.path()).await.expect("open"));
    dev_seed::seed(&stores.sink).await.expect("seed");
    let handle = start(stores);
    wait_for(&handle, "the shell", |state| state.shell_view.is_some()).await;

    handle.open_thread(thread_id(ATLAS_LOGIN));
    // 12 user turns, 10 filler + 2 static: the newest 10 turns are visible.
    // Turns 3-10 carry 2 items each, turns 11-12 carry 3 each (command, diff).
    let state = wait_for(&handle, "the windowed thread", |state| {
        visible_items(state, ATLAS_LOGIN).is_some()
    })
    .await;
    let entry = state.thread(&thread_id(ATLAS_LOGIN)).expect("thread");
    assert_eq!(visible_items(&state, ATLAS_LOGIN), Some(22));
    assert!(entry.state.history.has_more_history, "older turns exist");
    assert!(entry.state.history.history_cursor.is_some());

    handle.load_earlier(thread_id(ATLAS_LOGIN));
    // The older page is turns 1-2: four items, all of the thread's 26.
    let state = wait_for(&handle, "the older page", |state| {
        visible_items(state, ATLAS_LOGIN) == Some(26)
    })
    .await;
    let entry = state.thread(&thread_id(ATLAS_LOGIN)).expect("thread");
    assert!(
        !entry.state.history.has_more_history,
        "history reached its start"
    );
}

#[tokio::test]
async fn a_turn_committed_while_the_thread_is_open_arrives_live() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stores = Arc::new(Stores::open(dir.path()).await.expect("open"));
    dev_seed::seed(&stores.sink).await.expect("seed");
    let writer = Arc::clone(&stores);
    let handle = start(stores);
    wait_for(&handle, "the shell", |state| state.shell_view.is_some()).await;

    handle.open_thread(thread_id(ATLAS_UPLOADS));
    wait_for(&handle, "the uploads thread", |state| {
        visible_items(state, ATLAS_UPLOADS) == Some(2)
    })
    .await;

    dev_seed::append_turn(
        &writer.sink,
        ATLAS_UPLOADS,
        2,
        "Also cap the retry budget.",
        "Capped at ten minutes total.",
    )
    .await
    .expect("append");
    wait_for(&handle, "the live turn", |state| {
        visible_items(state, ATLAS_UPLOADS) == Some(4)
    })
    .await;
}

#[tokio::test]
async fn a_fresh_database_gives_an_empty_sidebar() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stores = Arc::new(Stores::open(dir.path()).await.expect("open"));
    let handle = start(stores);

    let state = wait_for(&handle, "the empty shell", |state| {
        state.shell_view.is_some()
    })
    .await;
    let view = state.shell_view.as_ref().expect("shell");
    assert!(view.projects.is_empty());
    assert!(view.threads.is_empty());
}
