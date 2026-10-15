//! Port of upstream/apps/server/src/persistence/Sqlite.ts
//! and the writer/reader split in packages/shared/src/nodeSqliteClient.ts.
//!
//! One writer connection, taken with `BEGIN IMMEDIATE` (nodeSqliteClient.ts
//! around the transaction acquirer). File databases also get a read-only
//! connection, the same role as `readerWorker: true`. Checkpoints for file
//! databases run on a side connection (`checkpointWorker: true`): passive
//! checkpoints after 1000 WAL frames, a 100ms idle flush, and the writer
//! itself only auto-checkpoints at 10x that so a slow flush cannot stall
//! commits. SQLite older than 3.51 has no `NOOP` checkpoint mode, so the
//! side connection stays off and the writer keeps the default.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use tokio::task::JoinHandle;

use crate::errors::PersistenceError;
use crate::migrations::{RunMigrationsOptions, run_migrations};

/// Size the `-wal` file is cut back to on the first commit after a WAL reset.
pub const WAL_SIZE_LIMIT_BYTES: i64 = 32 * 1024 * 1024;

/// SQLite's default `wal_autocheckpoint`, in WAL frames.
const CHECKPOINT_FRAMES: i64 = 1000;
/// While the checkpoint side connection runs, the writer still checkpoints a WAL this long.
const WRITER_BACKSTOP_FRAMES: i64 = 10 * CHECKPOINT_FRAMES;
/// How long writes must pause before the side connection checkpoints the rest of a long WAL.
const CHECKPOINT_IDLE: Duration = Duration::from_millis(100);

/// Async handle around the writer and, for files, a read-only connection.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    checkpoint: Mutex<Option<Checkpoint>>,
    writer: Mutex<Connection>,
    /// `None` for `:memory:` (a second connection would not see it) and when a
    /// read-only open is impossible. Reads then use the writer.
    reader: Option<Mutex<Connection>>,
    checkpoint_notify: Arc<CheckpointNotify>,
}

struct CheckpointNotify {
    lock: Mutex<()>,
    cv: Condvar,
    stop: AtomicBool,
    pending: AtomicBool,
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
            .map_err(|_| PersistenceError::Closed)??;
        Ok(opened)
    }

    pub async fn open_in_memory() -> Result<Self, PersistenceError> {
        let opened = AbortOnDrop::new(tokio::task::spawn_blocking(open_memory))
            .join()
            .await
            .map_err(|_| PersistenceError::Closed)??;
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
        let value = AbortOnDrop::new(tokio::task::spawn_blocking(move || {
            let mut conn = lock_mutex(&inner.writer)?;
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
        .map_err(|_| PersistenceError::Closed)??;
        self.inner.checkpoint_notify.kick();
        Ok(value)
    }

    /// Run `f` on the read-only connection when one is open, otherwise on the writer.
    pub async fn read<R, F>(&self, f: F) -> Result<R, PersistenceError>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R, PersistenceError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        AbortOnDrop::new(tokio::task::spawn_blocking(move || {
            if let Some(reader) = &inner.reader {
                let conn = lock_mutex(reader)?;
                return f(&conn);
            }
            let conn = lock_mutex(&inner.writer)?;
            f(&conn)
        }))
        .join()
        .await
        .map_err(|_| PersistenceError::Closed)?
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.checkpoint_notify.stop.store(true, Ordering::SeqCst);
        self.checkpoint_notify.cv.notify_one();
        // `checkpoint` is dropped first by field order only if declared first.
        // Take it explicitly so its thread joins before the connections close.
        drop(self.checkpoint.lock().ok().and_then(|mut slot| slot.take()));
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, PersistenceError> {
    mutex.lock().map_err(|_| {
        PersistenceError::migration(
            crate::errors::MigrationErrorKind::Failed,
            "SQLite connection lock poisoned",
        )
    })
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
        lock: Mutex::new(()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
        pending: AtomicBool::new(false),
    });
    let checkpoint = start_checkpoint_worker(path, &writer, Arc::clone(&notify));
    Ok(Db {
        inner: Arc::new(Inner {
            checkpoint: Mutex::new(checkpoint),
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
            writer: Mutex::new(writer),
            reader: None,
            checkpoint_notify: Arc::new(CheckpointNotify {
                lock: Mutex::new(()),
                cv: Condvar::new(),
                stop: AtomicBool::new(false),
                pending: AtomicBool::new(false),
            }),
        }),
    })
}

fn finish_writer(conn: &Connection) -> Result<(), PersistenceError> {
    apply_writer_pragmas(conn)?;
    run_migrations(
        conn,
        RunMigrationsOptions {
            to_migration_inclusive: None,
        },
    )?;
    conn.execute_batch("PRAGMA optimize = 0x10012;")
        .map_err(|err| PersistenceError::sql("sqlite.optimize", &err))?;
    Ok(())
}

fn apply_writer_pragmas(conn: &Connection) -> Result<(), PersistenceError> {
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
            tracing::warn!(
                %err,
                "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
            );
            return None;
        }
    };
    if !sqlite_supports_noop_checkpoint(&version) {
        tracing::warn!(
            %version,
            "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
        );
        return None;
    }
    if let Err(err) = writer.execute_batch(&format!(
        "PRAGMA wal_autocheckpoint = {WRITER_BACKSTOP_FRAMES};"
    )) {
        tracing::warn!(
            %err,
            "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
        );
        return None;
    }
    let path = path.to_path_buf();
    let thread_notify = Arc::clone(&notify);
    let thread = std::thread::Builder::new()
        .name("turin-sqlite-checkpoint".into())
        .spawn(move || checkpoint_loop(path, thread_notify))
        .ok()?;
    Some(Checkpoint {
        thread: Some(thread),
    })
}

fn sqlite_supports_noop_checkpoint(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    major > 3 || (major == 3 && minor >= 51)
}

fn checkpoint_loop(path: std::path::PathBuf, notify: Arc<CheckpointNotify>) {
    let Ok(conn) = Connection::open(&path) else {
        tracing::warn!(
            "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
        );
        return;
    };
    if conn
        .execute_batch("PRAGMA checkpoint_fullfsync = ON;")
        .is_err()
    {
        tracing::warn!(
            "SQLite checkpoint worker unavailable; checkpoints run on the main connection"
        );
        return;
    }
    let mut guard = match notify.lock.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    while !notify.stop.load(Ordering::SeqCst) {
        let signaled = notify.pending.swap(false, Ordering::SeqCst);
        if signaled {
            drop(guard);
            checkpoint_if_due(&conn, false);
            guard = match notify.lock.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
        }
        let (next, timeout) = notify
            .cv
            .wait_timeout(guard, CHECKPOINT_IDLE)
            .unwrap_or_else(|err| err.into_inner());
        guard = next;
        if timeout.timed_out() {
            drop(guard);
            checkpoint_if_due(&conn, true);
            guard = match notify.lock.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
        }
    }
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
        self.pending.store(true, Ordering::SeqCst);
        self.cv.notify_one();
    }
}
