//! Where Turin keeps its own state.
//!
//! Default layout comes from `turin_runtime::config` (`TURIN_HOME` /
//! `~/.turin/userdata/statev2.sqlite`). `TURIN_DATA_DIR` keeps the flat dev
//! layout (`<dir>/statev2.sqlite`) used by tests and seed examples.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use turin_runtime::config::{DeriveServerPathsOptions, derive_server_paths, resolve_base_dir};

pub const DATA_DIR_ENV: &str = "TURIN_DATA_DIR";
pub const DATABASE_FILE_NAME: &str = "statev2.sqlite";

/// `TURIN_DATA_DIR` when set and non-empty, else the runtime `state_dir`
/// (`~/.turin/userdata` by default). `None` when home is unavailable.
pub fn resolve_data_dir() -> Option<PathBuf> {
    resolve_from(
        std::env::var_os(DATA_DIR_ENV),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn resolve_from(override_dir: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let home = home?;
    let base = resolve_base_dir(None, &home).ok()?;
    Some(derive_server_paths(&base, None, DeriveServerPathsOptions::default()).state_dir)
}

pub fn database_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DATABASE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_runtime_default() {
        assert_eq!(
            resolve_from(
                Some("/tmp/dev-turin".into()),
                Some(PathBuf::from("/Users/x"))
            ),
            Some(PathBuf::from("/tmp/dev-turin"))
        );
    }

    #[test]
    fn empty_override_uses_runtime_state_dir() {
        assert_eq!(
            resolve_from(Some("".into()), Some(PathBuf::from("/Users/x"))),
            Some(PathBuf::from("/Users/x/.turin/userdata"))
        );
    }

    #[test]
    fn nothing_to_resolve_from_is_none() {
        assert_eq!(resolve_from(None, None), None);
    }

    #[test]
    fn database_lives_directly_in_the_data_dir() {
        assert_eq!(
            database_path(Path::new("/tmp/x")),
            PathBuf::from("/tmp/x/statev2.sqlite")
        );
    }
}
