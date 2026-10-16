//! Port of the `openScratch` case of
//! upstream/packages/client-runtime/src/state/projectCommands.test.ts, plus
//! the keys and paths the atom family built.
//!
//! The three `writeFile metadata` cases test the file-metadata atoms and the
//! RPC session, which `state/file_metadata.rs` replaces; its own tests cover
//! reuse of a current read and the metadata batching.

use std::sync::Arc;

use super::*;

struct FakeEnsure(Result<String, String>);

#[async_trait]
impl EnsureScratchProject for FakeEnsure {
    async fn ensure_scratch(&self) -> Result<String, String> {
        self.0.clone()
    }
}

struct FakeStore {
    tx: watch::Sender<Option<String>>,
    asked: parking_lot::Mutex<Vec<String>>,
}

impl FakeStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: watch::channel(None).0,
            asked: parking_lot::Mutex::new(Vec::new()),
        })
    }
}

impl ProjectArrivals<String> for FakeStore {
    fn watch_project(&self, project_id: &str) -> watch::Receiver<Option<String>> {
        self.asked.lock().push(project_id.to_owned());
        self.tx.subscribe()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_once_the_created_project_reaches_the_client_store() {
    let store = FakeStore::new();
    let ensure = FakeEnsure(Ok("scratch".into()));
    let opening = {
        let store = store.clone();
        tokio::spawn(async move { open_scratch(&ensure, store.as_ref()).await })
    };
    tokio::task::yield_now().await;
    store.tx.send_replace(Some("PROJECT".into()));
    assert_eq!(opening.await.unwrap(), Ok("PROJECT".to_owned()));
    assert_eq!(*store.asked.lock(), ["scratch"]);
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_immediately_when_the_project_is_already_stored() {
    let store = FakeStore::new();
    store.tx.send_replace(Some("PROJECT".into()));
    let ensure = FakeEnsure(Ok("scratch".into()));
    assert_eq!(
        open_scratch(&ensure, store.as_ref()).await,
        Ok("PROJECT".to_owned())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fails_with_the_not_loaded_error_when_the_project_never_arrives() {
    let store = FakeStore::new();
    let ensure = FakeEnsure(Ok("scratch".into()));
    let error = open_scratch_within(&ensure, store.as_ref(), Duration::from_millis(20))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        OpenScratchError::NotLoaded(ScratchProjectNotLoadedError {
            project_id: "scratch".into()
        })
    );
    assert_eq!(
        error.to_string(),
        "The folder for threads without a project has not reached this device yet. Try again."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_ensure_is_reported_without_waiting_on_the_store() {
    let store = FakeStore::new();
    let ensure = FakeEnsure(Err("no scratch root".into()));
    let error = open_scratch(&ensure, store.as_ref()).await.unwrap_err();
    assert_eq!(error, OpenScratchError::Ensure("no scratch root".into()));
    assert!(store.asked.lock().is_empty());
}

#[test]
fn keys_serialize_per_project_environment_and_file() {
    assert_eq!(project_command_key("env", "p1"), r#"["env","p1"]"#);
    assert_eq!(environment_command_key("env"), "env");
    assert_eq!(
        file_write_key("env", "/repo", "./foo"),
        r#"["env","/repo","./foo"]"#
    );
    assert_eq!(
        optimistic_project_file_key(&OptimisticProjectFileTarget {
            environment_id: "env".into(),
            cwd: "/repo".into(),
            relative_path: "a\"b".into(),
        }),
        r#"["env","/repo","a\"b"]"#
    );
}

#[test]
fn a_write_invalidates_the_canonical_and_the_requested_path() {
    assert_eq!(
        written_file_paths("/repo", "./foo", "foo"),
        ["/repo/foo", "/repo/./foo"]
    );
    // The same path twice collapses.
    assert_eq!(written_file_paths("/repo", "foo", "foo"), ["/repo/foo"]);
    // Absolute and home-relative paths pass through.
    assert_eq!(
        written_file_paths("/repo", "/etc/x", "~/y"),
        ["~/y", "/etc/x"]
    );
}
