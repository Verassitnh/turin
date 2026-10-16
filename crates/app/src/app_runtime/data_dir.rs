//! Where Turin keeps its own state.
//!
//! Upstream derives `stateDir` from `baseDir` (`apps/server/src/config.ts`) and
//! keeps `statev2.sqlite` under it. Turin's base is `~/.turin`, and the
//! database sits directly in it, as the T3 importer documents (PORTING.md).
//! `TURIN_DATA_DIR` replaces the base for dev runs and tests.
//!
//! N1 owns the shared paths module. This resolver is the one place to swap
//! for it when it lands.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DATA_DIR_ENV: &str = "TURIN_DATA_DIR";
pub const DATABASE_FILE_NAME: &str = "statev2.sqlite";

/// `TURIN_DATA_DIR` when set and non-empty, else `$HOME/.turin`. `None` when
/// neither is available.
pub fn resolve_data_dir() -> Option<PathBuf> {
    resolve_from(std::env::var_os(DATA_DIR_ENV), std::env::var_os("HOME"))
}

fn resolve_from(override_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home.filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".turin"))
}

pub fn database_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DATABASE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_home() {
        assert_eq!(
            resolve_from(Some("/tmp/dev-turin".into()), Some("/Users/x".into())),
            Some(PathBuf::from("/tmp/dev-turin"))
        );
    }

    #[test]
    fn empty_override_falls_back_to_home() {
        assert_eq!(
            resolve_from(Some("".into()), Some("/Users/x".into())),
            Some(PathBuf::from("/Users/x/.turin"))
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
