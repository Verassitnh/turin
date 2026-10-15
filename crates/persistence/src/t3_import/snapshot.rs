//! Online backup. Same rule as `initializeV2Database`: only `Done` is a
//! finished copy. `Busy` and `Locked` wait and retry the same step. Any error
//! deletes the destination file, so a partial copy is never returned.

use std::fs::{OpenOptions, Permissions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use super::T3ImportError;

const BUSY: Duration = Duration::from_secs(5);
const STEP_PAUSE: Duration = Duration::from_millis(25);
/// Consecutive busy/locked results before giving up. 30s covers a checkpoint
/// on a database T3 is still writing.
const MAX_PAUSES: u32 = 1_200;
const SNAPSHOT_DIR: &str = "/tmp";

/// Bounds on one copy. A writer that commits between steps makes SQLite start
/// the copy again from page 1, and each restart still returns `More`. Without
/// these limits a busy T3 database could keep the copy running forever.
#[derive(Debug, Clone, Copy)]
pub(super) struct CopyLimits {
    /// Pages per `sqlite3_backup_step`. Large enough that a 1 GB file is not
    /// one syscall per 256 KB, small enough that a writer can commit between
    /// steps.
    pub(super) step_pages: i32,
    /// Restarts before the copy fails as busy. Each restart is followed by
    /// [`STEP_PAUSE`].
    pub(super) max_restarts: u32,
    /// Wall-clock budget for the whole copy. Never reset, not even by `More`.
    pub(super) deadline: Duration,
}

const COPY_LIMITS: CopyLimits = CopyLimits {
    step_pages: 1024,
    max_restarts: 100,
    deadline: Duration::from_secs(60),
};

/// A snapshot file owned by a temporary directory.
pub struct T3Snapshot {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl T3Snapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn snapshot(src: &Path) -> Result<T3Snapshot, T3ImportError> {
    let dir = private_tempdir("turin-t3-import-", Path::new(SNAPSHOT_DIR))?;
    let path = dir.path().join("statev2.sqlite");
    backup_to(src, &path)?;
    Ok(T3Snapshot { path, _dir: dir })
}

/// A temporary directory only the owner can enter. It holds a full copy of
/// the T3 database, so it must not depend on the process umask (0755 at 022).
pub(crate) fn private_tempdir(
    prefix: &str,
    parent: &Path,
) -> Result<tempfile::TempDir, T3ImportError> {
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .map_err(|err| T3ImportError::io("create t3 import directory", err))
}

pub(crate) fn backup_to(src: &Path, dest: &Path) -> Result<(), T3ImportError> {
    backup_with_limits(src, dest, COPY_LIMITS)
}

pub(super) fn backup_with_limits(
    src: &Path,
    dest: &Path,
    limits: CopyLimits,
) -> Result<(), T3ImportError> {
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| T3ImportError::io("create t3 import destination", err))?;
    }
    // Armed only once the source is open and `dest` exists. A failure to open
    // the source, or a `dest` that already exists, must not delete a file the
    // caller already had.
    let mut partial = PartialCopy::new(dest);
    let source = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.open", &err))?;
    source.busy_timeout(BUSY).map_err(|err| {
        crate::errors::PersistenceError::sql("t3Import.snapshot.busyTimeout", &err)
    })?;
    // Defense in depth: this connection must not write even if a later edit
    // adds a statement. `query_only` is connection-local.
    source
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.queryOnly", &err))?;

    // Created 0600 before SQLite writes a byte, so the copy is never readable by
    // another user. `create_new` refuses an existing file rather than replacing it.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .map_err(|err| T3ImportError::io("create t3 import copy", err))?;
    partial.arm();
    let mut copied = Connection::open(dest)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.create", &err))?;
    copied.busy_timeout(BUSY).map_err(|err| {
        crate::errors::PersistenceError::sql("t3Import.snapshot.busyTimeout", &err)
    })?;
    let backup = Backup::new(&source, &mut copied)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.backup", &err))?;
    let started = Instant::now();
    let mut pauses = 0u32;
    let mut restarts = 0u32;
    // `progress().remaining` only falls while the copy moves forward. A
    // restart puts it back at the page count, so a value that does not fall
    // means SQLite started over.
    let mut last_remaining = i32::MAX;
    let busy = || -> T3ImportError {
        crate::errors::PersistenceError::sql("t3Import.snapshot.step", &busy_err()).into()
    };
    loop {
        if started.elapsed() > limits.deadline {
            return Err(busy());
        }
        match backup
            .step(limits.step_pages)
            .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.step", &err))?
        {
            StepResult::Done => break,
            StepResult::More => {
                pauses = 0;
                let remaining = backup.progress().remaining;
                if remaining >= last_remaining {
                    restarts += 1;
                    if restarts > limits.max_restarts {
                        return Err(busy());
                    }
                    thread::sleep(STEP_PAUSE);
                }
                last_remaining = remaining;
            }
            StepResult::Busy | StepResult::Locked => {
                pauses += 1;
                if pauses > MAX_PAUSES {
                    return Err(busy());
                }
                thread::sleep(STEP_PAUSE);
            }
            _ => return Err(busy()),
        }
    }
    drop(backup);
    drop(copied);
    partial.disarm();
    Ok(())
}

/// Deletes `dest` and its SQLite sidecars unless [`Self::disarm`] ran.
struct PartialCopy {
    dest: PathBuf,
    armed: bool,
}

impl PartialCopy {
    fn new(dest: &Path) -> Self {
        Self {
            dest: dest.to_path_buf(),
            armed: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartialCopy {
    fn drop(&mut self) {
        if self.armed {
            discard_partial(&self.dest);
        }
    }
}

pub(crate) fn discard_partial(dest: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = dest.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(dest.with_file_name(name));
    }
}

fn busy_err() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::DatabaseBusy,
            extended_code: rusqlite::ffi::SQLITE_BUSY,
        },
        Some("sqlite backup did not finish".into()),
    )
}

/// Opens a database this module never writes: the live T3 file, or a copy
/// that is only read. `immutable` is not used: it skips the WAL and ignores
/// concurrent writers, so a file in use would read as stale or torn.
pub(crate) fn open_read_only(path: &Path) -> Result<Connection, T3ImportError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.openReadOnly", &err))?;
    conn.busy_timeout(BUSY)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.busyTimeout", &err))?;
    conn.execute_batch("PRAGMA query_only = ON;")
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.queryOnly", &err))?;
    Ok(conn)
}

/// Read-write open. Only for copies this module made (snapshots, the import
/// work copy, and the fresh-schema reference).
pub(crate) fn open_snapshot(path: &Path) -> Result<Connection, T3ImportError> {
    let conn = Connection::open(path)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.open", &err))?;
    conn.busy_timeout(BUSY)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.busyTimeout", &err))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.foreignKeys", &err))?;
    Ok(conn)
}
