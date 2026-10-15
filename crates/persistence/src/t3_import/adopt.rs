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
