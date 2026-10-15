//! Online backup. Same rule as `initializeV2Database`: only `Done` is a
//! finished copy. `Busy` and `Locked` wait and retry the same step. Any error
//! deletes the destination file, so a partial copy is never returned.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use super::T3ImportError;

const BUSY: Duration = Duration::from_secs(5);
/// Pages per `sqlite3_backup_step`. Large enough that a 1 GB file is not one
/// syscall per 256 KB, small enough that a writer can commit between steps.
const STEP_PAGES: i32 = 1024;
const STEP_PAUSE: Duration = Duration::from_millis(25);
/// Consecutive busy/locked results before giving up. 30s covers a checkpoint
/// on a database T3 is still writing.
const MAX_PAUSES: u32 = 1_200;
const SNAPSHOT_DIR: &str = "/tmp";

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
    let dir = tempfile::TempDir::with_prefix_in("turin-t3-import-", SNAPSHOT_DIR)
        .map_err(|err| T3ImportError::io("create t3 import snapshot directory", err))?;
    let path = dir.path().join("statev2.sqlite");
    backup_to(src, &path)?;
    Ok(T3Snapshot { path, _dir: dir })
}

pub(crate) fn backup_to(src: &Path, dest: &Path) -> Result<(), T3ImportError> {
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| T3ImportError::io("create t3 import destination", err))?;
    }
    // Armed only once the source is open, immediately before creating `dest`.
    // A failure to open the source must not delete a file the caller already had.
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

    partial.arm();
    let mut copied = Connection::open(dest)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.create", &err))?;
    copied.busy_timeout(BUSY).map_err(|err| {
        crate::errors::PersistenceError::sql("t3Import.snapshot.busyTimeout", &err)
    })?;
    let backup = Backup::new(&source, &mut copied)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.backup", &err))?;
    let mut pauses = 0u32;
    loop {
        match backup
            .step(STEP_PAGES)
            .map_err(|err| crate::errors::PersistenceError::sql("t3Import.snapshot.step", &err))?
        {
            StepResult::Done => break,
            StepResult::More => pauses = 0,
            StepResult::Busy | StepResult::Locked => {
                pauses += 1;
                if pauses > MAX_PAUSES {
                    return Err(crate::errors::PersistenceError::sql(
                        "t3Import.snapshot.step",
                        &busy_err(),
                    )
                    .into());
                }
                thread::sleep(STEP_PAUSE);
            }
            _ => {
                return Err(crate::errors::PersistenceError::sql(
                    "t3Import.snapshot.step",
                    &busy_err(),
                )
                .into());
            }
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

pub(crate) fn open_snapshot(path: &Path) -> Result<Connection, T3ImportError> {
    let conn = Connection::open(path)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.open", &err))?;
    conn.busy_timeout(BUSY)
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.busyTimeout", &err))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|err| crate::errors::PersistenceError::sql("t3Import.foreignKeys", &err))?;
    Ok(conn)
}
