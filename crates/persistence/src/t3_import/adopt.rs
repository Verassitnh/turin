//! Place a verified copy at `{turin_data_dir}/statev2.sqlite`.
//!
//! The copy is built in a sibling directory so the final hard link stays on
//! the same filesystem. `hard_link` fails when the destination exists, which
//! is the first-run check: Turin does not replace a database it may already
//! be writing.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::T3ImportError;
use super::snapshot::{open_snapshot, remove_stale_dirs};
use super::verify::{ImportReport, migrate_and_verify};

/// File name inside the Turin data directory. Mirrors T3's `statev2.sqlite`.
pub const TURIN_DATABASE_FILE: &str = "statev2.sqlite";

/// Directory inside the Turin data directory that holds a verified import until the next start.
/// It must not begin with `.t3-import-`: the stale-work cleanup removes those after an hour.
pub const STAGED_IMPORT_DIR: &str = "t3-import-staged";

/// What [`adopt_staged`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdoptStaged {
    /// No import was staged.
    NothingStaged,
    /// The staged database replaced Turin's empty one.
    Adopted,
    /// Turin's database already had projects or threads, so the staged copy was dropped.
    KeptExisting,
}

/// Verify a T3 database and park the copy until the next start.
///
/// The running app has `statev2.sqlite` open, so the copy cannot take its place now.
/// [`adopt_staged`] swaps it in before the database is next opened.
pub(crate) fn stage_into(
    src: &Path,
    turin_data_dir: &Path,
    backup_to: fn(&Path, &Path) -> Result<(), T3ImportError>,
) -> Result<ImportReport, T3ImportError> {
    let staging = turin_data_dir.join(STAGED_IMPORT_DIR);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|err| T3ImportError::io("clear the earlier staged import", err))?;
    }
    import_into(src, &staging, backup_to).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })
}

/// Swap a staged import in for Turin's database when that database holds no projects or threads.
/// Call before opening `statev2.sqlite`. The replaced file stays beside it as
/// `statev2.sqlite.before-t3-import`.
pub(crate) fn adopt_staged(turin_data_dir: &Path) -> Result<AdoptStaged, T3ImportError> {
    let staging = turin_data_dir.join(STAGED_IMPORT_DIR);
    let staged = staging.join(TURIN_DATABASE_FILE);
    if !staged.is_file() {
        return Ok(AdoptStaged::NothingStaged);
    }
    let live = turin_data_dir.join(TURIN_DATABASE_FILE);
    if live.exists() && database_has_content(&live) {
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(AdoptStaged::KeptExisting);
    }
    for suffix in ["", "-wal", "-shm"] {
        let from = turin_data_dir.join(format!("{TURIN_DATABASE_FILE}{suffix}"));
        if from.exists() {
            let to = turin_data_dir.join(format!("{TURIN_DATABASE_FILE}.before-t3-import{suffix}"));
            std::fs::rename(&from, &to)
                .map_err(|err| T3ImportError::io("set the empty Turin database aside", err))?;
        }
    }
    std::fs::rename(&staged, &live)
        .map_err(|err| T3ImportError::io("move the staged import into place", err))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(AdoptStaged::Adopted)
}

/// A database that cannot be read counts as having content: it is never replaced.
fn database_has_content(path: &Path) -> bool {
    let Ok(conn) = super::snapshot::open_read_only(path) else {
        return true;
    };
    ["projection_projects", "projection_threads"]
        .into_iter()
        .any(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_or(true, |count| count > 0)
        })
}

pub(crate) fn import_into(
    src: &Path,
    turin_data_dir: &Path,
    backup_to: fn(&Path, &Path) -> Result<(), T3ImportError>,
) -> Result<ImportReport, T3ImportError> {
    std::fs::create_dir_all(turin_data_dir)
        .map_err(|err| T3ImportError::io("create turin data directory", err))?;
    remove_stale_dirs(turin_data_dir, ".t3-import-", SystemTime::now());
    let destination = turin_data_dir.join(TURIN_DATABASE_FILE);
    if destination.exists() {
        return Err(T3ImportError::DestinationExists {
            path: destination.display().to_string(),
        });
    }
    if same_file(src, &destination) {
        return Err(T3ImportError::SameFile);
    }

    let work = WorkDir::new(turin_data_dir)?;
    let snapshot = work.0.join(TURIN_DATABASE_FILE);
    backup_to(src, &snapshot)?;
    let conn = open_snapshot(&snapshot)?;
    let report = migrate_and_verify(&conn)?;
    drop(conn);

    if destination.exists() {
        return Err(T3ImportError::DestinationExists {
            path: destination.display().to_string(),
        });
    }
    std::fs::hard_link(&snapshot, &destination).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            T3ImportError::DestinationExists {
                path: destination.display().to_string(),
            }
        } else {
            T3ImportError::io("link imported database into place", err)
        }
    })?;
    drop(work);
    Ok(report)
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(parent: &Path) -> Result<Self, T3ImportError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = parent.join(format!(".t3-import-{nanos}"));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|err| T3ImportError::io("create t3 import work directory", err))?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn same_file(src: &Path, destination: &Path) -> bool {
    match (src.canonicalize(), destination.canonicalize()) {
        (Ok(src), Ok(destination)) => src == destination,
        _ => false,
    }
}
