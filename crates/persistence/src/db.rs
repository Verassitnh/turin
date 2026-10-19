//! Port of upstream/apps/server/src/persistence/Sqlite.ts
//! and the writer/reader split in packages/shared/src/nodeSqliteClient.ts.
//!
//! One writer connection, taken with `BEGIN IMMEDIATE` (nodeSqliteClient.ts
//! around the transaction acquirer). File databases also get a read-only
//! connection, the same role as `readerWorker: true`. Checkpoints for file
//! databases run on a side connection (`checkpointWorker: true`): passive
//! checkpoints after 1000 WAL frames, a 100ms idle flush, and the writer
//! itself only auto-checkpoints at 10x that so a slow flush cannot stall
//! commits. The side connection is opened and configured before the writer
//! autocheckpoint is raised. SQLite older than 3.51 has no `NOOP` checkpoint
//! mode, so the side connection stays off and the writer keeps the default.
//! Bundled SQLite is 3.53, so file databases start the worker.
//!
//! Port of upstream/packages/shared/src/nodeSqliteClient.ts (writer/reader split; node:sqlite is replaced by rusqlite).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

use crate::errors::PersistenceError;
use crate::migrations::{RunMigrationsOptions, run_migrations};

/// Size the `-wal` file is cut back to on the first commit after a WAL reset.
pub const WAL_SIZE_LIMIT_BYTES: i64 = 32 * 1024 * 1024;

/// SQLite's default `wal_autocheckpoint`, in WAL frames.
pub(crate) const CHECKPOINT_FRAMES: i64 = 1000;
/// While the checkpoint side connection runs, the writer still checkpoints a WAL this long.
pub(crate) const WRITER_BACKSTOP_FRAMES: i64 = 10 * CHECKPOINT_FRAMES;
/// How long writes must pause before the side connection checkpoints the rest of a long WAL.
const CHECKPOINT_IDLE: Duration = Duration::from_millis(100);

/// Async handle around the writer and, for files, a read-only connection.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    checkpoint: Mutex<Option<Checkpoint>>,
    writer_gate: Arc<Semaphore>,
    reader_gate: Arc<Semaphore>,
    writer: Mutex<Connection>,
    /// `None` for `:memory:` (a second connection would not see it) and when a
    /// read-only open is impossible. Reads then use the writer.
    reader: Option<Mutex<Connection>>,
    checkpoint_notify: Arc<CheckpointNotify>,
}

struct CheckpointNotify {
    lock: StdMutex<()>,
    cv: Condvar,
    stop: AtomicBool,
    pending: AtomicBool,
    /// The side connection died after the writer autocheckpoint was raised.
    failed: AtomicBool,
}

struct Checkpoint {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Aborts a spawned task if the future holding it is dropped first.
pub(crate) struct AbortOnDrop<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    pub(crate) async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        let handle = self.handle.take().expect("join handle");
        handle.await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Db {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let path = path.as_ref().to_path_buf();
        let opened = AbortOnDrop::new(tokio::task::spawn_blocking(move || open_file(&path)))
            .join()
            .await
            .map_err(join_error)??;
        Ok(opened)
    }

    pub async fn open_in_memory() -> Result<Self, PersistenceError> {
        let opened = AbortOnDrop::new(tokio::task::spawn_blocking(open_memory))
            .join()
            .await
            .map_err(join_error)??;
        Ok(opened)
    }

    /// `layerConfig`: import a sibling V1 database when present, then open.
    pub async fn open_configured(path: impl AsRef<Path>) -> Result<Self, PersistenceError> {
        let path = path.as_ref().to_path_buf();
        crate::initialize_v2_database::initialize_v2_database(&path).await?;
        Self::open(path).await
    }

    /// Run `f` inside `BEGIN IMMEDIATE` on the writer. The lock is held only on
    /// the blocking thread; it is never held across `.await`. Commit on `Ok`,
    /// roll back on `Err`. Publish or anything else async after this returns.
    pub async fn write<R, F>(&self, f: F) -> Result<R, PersistenceError>
    where
        R: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<R, PersistenceError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let permit = acquire_gate(&inner.writer_gate).await?;
        let value = AbortOnDrop::new(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = inner.writer.lock();
            restore_writer_autocheckpoint(&conn, &inner.checkpoint_notify);
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|err| PersistenceError::sql("sqlite.begin", &err))?;
            let value = f(&tx)?;
            tx.commit()
                .map_err(|err| PersistenceError::sql("sqlite.commit", &err))?;
            Ok(value)
        }))
        .join()
        .await
        .map_err(join_error)??;
        self.inner.checkpoint_notify.kick();
        Ok(value)
    }

    /// Writer `wal_autocheckpoint`. File databases raise it only after the
    /// checkpoint side connection is open.
    #[cfg(test)]
    pub(crate) async fn writer_autocheckpoint(&self) -> Result<i64, PersistenceError> {
        let inner = Arc::clone(&self.inner);
        let permit = acquire_gate(&inner.writer_gate).await?;
        AbortOnDrop::new(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = inner.writer.lock();
            conn.query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
                .map_err(|err| PersistenceError::sql("sqlite.pragma", &err))
        }))
        .join()
        .await
        .map_err(join_error)?
    }

    /// Run `f` on the read-only connection when one is open, otherwise on the writer.
    pub async fn read<R, F>(&self, f: F) -> Result<R, PersistenceError>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R, PersistenceError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let gate = if inner.reader.is_some() {
            &inner.reader_gate
        } else {
            &inner.writer_gate
        };
        let permit = acquire_gate(gate).await?;
        AbortOnDrop::new(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Some(reader) = &inner.reader {
                let conn = reader.lock();
                return f(&conn);
            }
            let conn = inner.writer.lock();
            f(&conn)
        }))
        .join()
        .await
        .map_err(join_error)?
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        {
            // Set `stop` under the lock, as `kick` does, so the wakeup cannot
            // land between the worker's check and its condvar wait.
            let _guard = self
                .checkpoint_notify
                .lock
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            self.checkpoint_notify.stop.store(true, Ordering::SeqCst);
            self.checkpoint_notify.cv.notify_all();
        }
        // `checkpoint` is dropped first by field order only if declared first.
        // Take it explicitly so its thread joins before the connections close.
        drop(self.checkpoint.lock().take());
        let conn = self.writer.lock();
        restore_writer_autocheckpoint(&conn, &self.checkpoint_notify);
    }
}

async fn acquire_gate(gate: &Arc<Semaphore>) -> Result<OwnedSemaphorePermit, PersistenceError> {
    gate.clone()
        .acquire_owned()
        .await
        .map_err(|_| PersistenceError::Closed)
}

fn join_error(err: tokio::task::JoinError) -> PersistenceError {
    if !err.is_panic() {
        return PersistenceError::Closed;
    }
    let rendered = err.to_string();
    let message = match err.try_into_panic() {
        Ok(payload) => panic_message(payload.as_ref()).unwrap_or(rendered),
        Err(err) => err.to_string(),
    };
    PersistenceError::Panicked { message }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return Some((*message).to_owned());
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return Some(message.clone());
    }
    None
}

fn open_file(path: &Path) -> Result<Db, PersistenceError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|err| {
            PersistenceError::migration(
                crate::errors::MigrationErrorKind::Failed,
                format!("Could not create database directory: {err}"),
            )
        })?;
    }
    let writer =
        Connection::open(path).map_err(|err| PersistenceError::sql("sqlite.open", &err))?;
    finish_writer(&writer)?;
    let reader = open_reader(path);
    let notify = Arc::new(CheckpointNotify {
        lock: StdMutex::new(()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
        pending: AtomicBool::new(false),
        failed: AtomicBool::new(false),
    });
    let checkpoint = start_checkpoint_worker(path, &writer, Arc::clone(&notify));
    Ok(Db {
        inner: Arc::new(Inner {
            checkpoint: Mutex::new(checkpoint),
            writer_gate: Arc::new(Semaphore::new(1)),
            reader_gate: Arc::new(Semaphore::new(1)),
            writer: Mutex::new(writer),
            reader,
            checkpoint_notify: notify,
        }),
    })
}

fn open_memory() -> Result<Db, PersistenceError> {
    let writer =
        Connection::open_in_memory().map_err(|err| PersistenceError::sql("sqlite.open", &err))?;
    finish_writer(&writer)?;
    Ok(Db {
        inner: Arc::new(Inner {
            checkpoint: Mutex::new(None),
            writer_gate: Arc::new(Semaphore::new(1)),
            reader_gate: Arc::new(Semaphore::new(1)),
            writer: Mutex::new(writer),
            reader: None,
            checkpoint_notify: Arc::new(CheckpointNotify {
                lock: StdMutex::new(()),
                cv: Condvar::new(),
                stop: AtomicBool::new(false),
                pending: AtomicBool::new(false),
                failed: AtomicBool::new(false),
            }),
        }),
    })
}

fn finish_writer(conn: &Connection) -> Result<(), PersistenceError> {
    conn.set_prepared_statement_cache_capacity(200);
    apply_writer_pragmas(conn)?;
    run_migrations(
        conn,
        RunMigrationsOptions {
            to_migration_inclusive: None,
        },
    )?;
    // Without sqlite_stat1 the planner guesses at index selectivity. 0x10012
    // analyzes tables whose indexes lack stats or whose row count changed 10x.
    conn.execute_batch("PRAGMA optimize = 0x10012;")
        .map_err(|err| PersistenceError::sql("sqlite.optimize", &err))?;
    Ok(())
}

fn apply_writer_pragmas(conn: &Connection) -> Result<(), PersistenceError> {
    // busy_timeout: CLI and server write from separate processes; wait rather
    // than fail with SQLITE_BUSY.
    // synchronous NORMAL: FULL fsyncs the WAL on every commit. Every provider
    // event is its own commit, so that stalls the writer. In WAL mode NORMAL
    // only fsyncs at checkpoints. An app crash loses nothing; an OS crash or
    // power loss can roll back commits since the last checkpoint.
    // checkpoint_fullfsync: checkpoints are the only sync, and macOS fsync()
    // does not flush the drive cache. Other platforms ignore it.
    // journal_size_limit: PASSIVE checkpoints never shrink the -wal file, so
    // it otherwise keeps its largest size until the last connection closes.
    let sql = format!(
        "
        PRAGMA busy_timeout = 5000;
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA checkpoint_fullfsync = ON;
        PRAGMA journal_size_limit = {WAL_SIZE_LIMIT_BYTES};
        "
    );
    conn.execute_batch(&sql)
        .map_err(|err| PersistenceError::sql("sqlite.pragma", &err))
}

/// Read-only open. Refuses a database that is not in WAL mode, matching the
/// reader worker, and leaves reads on the writer when that happens.
fn open_reader(path: &Path) -> Option<Mutex<Connection>> {
    let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(%err, "SQLite reader unavailable; reads use the writer");
            return None;
        }
    };
    conn.set_prepared_statement_cache_capacity(200);
    if let Err(err) = conn.execute_batch("PRAGMA busy_timeout = 5000;") {
        tracing::warn!(%err, "SQLite reader unavailable; reads use the writer");
        return None;
    }
    let mode: Result<String, _> = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0));
    match mode {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => Some(Mutex::new(conn)),
        Ok(mode) => {
            tracing::warn!(
                %mode,
                "SQLite reader unavailable; database is not in wal mode"
            );
            None
        }
        Err(err) => {
            tracing::warn!(%err, "SQLite reader unavailable; reads use the writer");
            None
        }
    }
}

fn start_checkpoint_worker(
    path: &Path,
    writer: &Connection,
    notify: Arc<CheckpointNotify>,
) -> Option<Checkpoint> {
    let version: String = match writer.query_row("SELECT sqlite_version()", [], |row| row.get(0)) {
        Ok(version) => version,
        Err(err) => {
            warn_checkpoint_unavailable(&err.to_string());
            return None;
        }
    };
    if !sqlite_supports_noop_checkpoint(&version) {
        warn_checkpoint_unavailable(&version);
        return None;
    }
    // Open and configure the side connection before the writer stops
    // checkpointing itself. A failure here leaves the default autocheckpoint.
    let conn = match Connection::open(path) {
        Ok(conn) => conn,
        Err(err) => {
            warn_checkpoint_unavailable(&err.to_string());
            return None;
        }
    };
    if let Err(err) = conn.execute_batch("PRAGMA checkpoint_fullfsync = ON;") {
        warn_checkpoint_unavailable(&err.to_string());
        return None;
    }
    if let Err(err) = wal_frames(&conn) {
        warn_checkpoint_unavailable(&err.to_string());
        return None;
    }
    if let Err(err) = writer.execute_batch(&format!(
        "PRAGMA wal_autocheckpoint = {WRITER_BACKSTOP_FRAMES};"
    )) {
        warn_checkpoint_unavailable(&err.to_string());
        return None;
    }
    let thread_notify = Arc::clone(&notify);
    let thread = match std::thread::Builder::new()
        .name("turin-sqlite-checkpoint".into())
        .spawn(move || checkpoint_loop(conn, thread_notify))
    {
        Ok(thread) => thread,
        Err(err) => {
            notify.failed.store(true, Ordering::SeqCst);
            restore_writer_autocheckpoint(writer, &notify);
            warn_checkpoint_unavailable(&err.to_string());
            return None;
        }
    };
    Some(Checkpoint {
        thread: Some(thread),
    })
}

fn warn_checkpoint_unavailable(reason: &str) {
    tracing::warn!(
        %reason,
        "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
    );
}

fn restore_writer_autocheckpoint(conn: &Connection, notify: &CheckpointNotify) {
    if !notify.failed.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Err(err) =
        conn.execute_batch(&format!("PRAGMA wal_autocheckpoint = {CHECKPOINT_FRAMES};"))
    {
        tracing::warn!(
            %err,
            "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
        );
        notify.failed.store(true, Ordering::SeqCst);
    }
}

fn sqlite_supports_noop_checkpoint(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    major > 3 || (major == 3 && minor >= 51)
}

fn checkpoint_loop(conn: Connection, notify: Arc<CheckpointNotify>) {
    let mut guard = match notify.lock.lock() {
        Ok(guard) => guard,
        Err(_) => {
            mark_checkpoint_failed(&notify);
            return;
        }
    };
    while !notify.stop.load(Ordering::SeqCst) {
        while !notify.pending.load(Ordering::SeqCst) && !notify.stop.load(Ordering::SeqCst) {
            // Timed, so a missed wakeup costs at most one idle interval.
            guard = match notify.cv.wait_timeout(guard, CHECKPOINT_IDLE) {
                Ok((guard, _)) => guard,
                Err(_) => {
                    mark_checkpoint_failed(&notify);
                    return;
                }
            };
        }
        if notify.stop.load(Ordering::SeqCst) {
            break;
        }
        notify.pending.store(false, Ordering::SeqCst);
        drop(guard);
        checkpoint_if_due(&conn, false);
        guard = match notify.lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                mark_checkpoint_failed(&notify);
                return;
            }
        };
        if notify.stop.load(Ordering::SeqCst) || notify.pending.load(Ordering::SeqCst) {
            continue;
        }
        let idle_started = std::time::Instant::now();
        let mut idle_elapsed = false;
        while !notify.pending.load(Ordering::SeqCst) && !notify.stop.load(Ordering::SeqCst) {
            let remaining = CHECKPOINT_IDLE.saturating_sub(idle_started.elapsed());
            if remaining.is_zero() {
                idle_elapsed = true;
                break;
            }
            let (next, timeout) = match notify.cv.wait_timeout(guard, remaining) {
                Ok(waited) => waited,
                Err(err) => err.into_inner(),
            };
            guard = next;
            if timeout.timed_out() {
                idle_elapsed = true;
                break;
            }
        }
        if notify.stop.load(Ordering::SeqCst) {
            break;
        }
        if notify.pending.load(Ordering::SeqCst) {
            continue;
        }
        if idle_elapsed {
            drop(guard);
            checkpoint_if_due(&conn, true);
            guard = match notify.lock.lock() {
                Ok(guard) => guard,
                Err(_) => {
                    mark_checkpoint_failed(&notify);
                    return;
                }
            };
        }
    }
}

fn mark_checkpoint_failed(notify: &CheckpointNotify) {
    if notify.stop.load(Ordering::SeqCst) {
        return;
    }
    notify.failed.store(true, Ordering::SeqCst);
    warn_checkpoint_unavailable("checkpoint thread stopped");
}

fn checkpoint_if_due(conn: &Connection, idle: bool) {
    let Ok((log, checkpointed)) = wal_frames(conn) else {
        return;
    };
    let should = if idle {
        log >= CHECKPOINT_FRAMES && checkpointed < log
    } else {
        log - checkpointed >= CHECKPOINT_FRAMES
    };
    if !should {
        return;
    }
    if let Err(err) = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(())) {
        tracing::warn!(%err, "SQLite passive checkpoint failed");
    }
}

fn wal_frames(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
        Ok((row.get(1)?, row.get(2)?))
    })
}

impl CheckpointNotify {
    fn kick(&self) {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        self.pending.store(true, Ordering::SeqCst);
        self.cv.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Opening and dropping a file database right away used to deadlock:
    /// `Inner::drop` raised `stop` without the checkpoint lock, so the worker
    /// could miss the wakeup and `Checkpoint::drop` joined it forever.
    #[test]
    fn repeated_open_and_drop_of_file_db_does_not_hang() {
        const ITERATIONS: usize = 500;
        const DEADLINE: Duration = Duration::from_secs(60);

        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("reopen.sqlite");
            runtime.block_on(async {
                for _ in 0..ITERATIONS {
                    let db = Db::open(&path).await.unwrap();
                    drop(db);
                }
            });
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(DEADLINE)
            .expect("open/drop loop did not finish; Db drop deadlocked");
    }
}
