//! Port of upstream/apps/server/src/persistence/initializeV2Database.ts
//!
//! Seed V2 once. Its copied legacy tables remain the source for lazy transcript import.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rusqlite::backup::StepResult;
use rusqlite::{Connection, OpenFlags, backup::Backup};

use crate::errors::PersistenceError;

pub async fn initialize_v2_database(destination: impl AsRef<Path>) -> Result<(), PersistenceError> {
    let destination = destination.as_ref().to_path_buf();
    crate::db::AbortOnDrop::new(tokio::task::spawn_blocking(move || {
        initialize_blocking(&destination)
    }))
    .join()
    .await
    .map_err(|_| PersistenceError::Closed)?
}

fn initialize_blocking(destination: &Path) -> Result<(), PersistenceError> {
    let source = sibling_v1_path(destination);
    let result = (|| {
        if destination.exists() || !source.exists() {
            return Ok(());
        }
        let directory = destination
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let temporary = directory.join(format!(
            ".v2-import-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir(&temporary).map_err(|err| import_err(&source, destination, err))?;
        let cleanup = temporary.clone();
        let outcome = (|| {
            let snapshot = temporary.join("snapshot.sqlite");
            backup_sqlite(&source, &snapshot)
                .map_err(|err| import_err(&source, destination, err))?;
            match std::fs::hard_link(&snapshot, destination) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(err) => Err(import_err(&source, destination, err)),
            }
        })();
        let _ = std::fs::remove_dir_all(cleanup);
        outcome
    })();
    result.map_err(|err| match err {
        PersistenceError::V2DatabaseImport { .. } => err,
        other => PersistenceError::V2DatabaseImport {
            source_path: source.display().to_string(),
            destination_path: destination.display().to_string(),
            cause: Box::new(other),
        },
    })
}

fn backup_sqlite(source: &Path, snapshot: &Path) -> Result<(), rusqlite::Error> {
    let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut dst = Connection::open(snapshot)?;
    let backup = Backup::new(&src, &mut dst)?;
    // `step` returns `Ok(Busy)` / `Ok(Locked)` for a partial copy. Only `Done`
    // may be linked. Anything else is an import error and the temp dir is removed.
    let mut pauses = 0u32;
    loop {
        match backup.step(64)? {
            StepResult::Done => return Ok(()),
            StepResult::More => pauses = 0,
            StepResult::Busy | StepResult::Locked => {
                pauses += 1;
                if pauses > 100 {
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error {
                            code: rusqlite::ErrorCode::DatabaseBusy,
                            extended_code: rusqlite::ffi::SQLITE_BUSY,
                        },
                        Some("sqlite backup did not finish".into()),
                    ));
                }
                thread::sleep(Duration::from_millis(25));
            }
            _ => {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ErrorCode::DatabaseBusy,
                        extended_code: rusqlite::ffi::SQLITE_BUSY,
                    },
                    Some("sqlite backup did not finish".into()),
                ));
            }
        }
    }
}

fn import_err(
    source: &Path,
    destination: &Path,
    err: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> PersistenceError {
    PersistenceError::V2DatabaseImport {
        source_path: source.display().to_string(),
        destination_path: destination.display().to_string(),
        cause: err.into(),
    }
}

pub fn sibling_v1_path(destination: impl AsRef<Path>) -> PathBuf {
    destination
        .as_ref()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("state.sqlite")
}
