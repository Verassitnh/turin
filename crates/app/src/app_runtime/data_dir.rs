//! Where Turin keeps its own state.
//!
//! Default layout comes from `turin_runtime::config` (`TURIN_HOME` /
//! `~/.turin/userdata/statev2.sqlite`). `TURIN_DATA_DIR` keeps the flat dev
//! layout (`<dir>/statev2.sqlite`) used by tests and seed examples.

use std::path::{Path, PathBuf};

use turin_runtime::config::{
    DeriveServerPathsOptions, derive_server_paths, resolve_base_dir_from_env,
};

pub const DATA_DIR_ENV: &str = "TURIN_DATA_DIR";
pub const DATABASE_FILE_NAME: &str = "statev2.sqlite";

/// `TURIN_DATA_DIR` when set and non-empty, else the runtime `state_dir`
/// (`~/.turin/userdata` by default). `None` when home is unavailable.
pub fn resolve_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV).filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let base = resolve_base_dir_from_env().ok()?;
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
        std::env::set_var(DATA_DIR_ENV, "/tmp/dev-turin");
        assert_eq!(resolve_data_dir(), Some(PathBuf::from("/tmp/dev-turin")));
        std::env::remove_var(DATA_DIR_ENV);
    }

    #[test]
    fn empty_override_uses_runtime_state_dir() {
        std::env::set_var(DATA_DIR_ENV, "");
        let base = resolve_base_dir_from_env().expect("home");
        let expected =
            derive_server_paths(&base, None, DeriveServerPathsOptions::default()).state_dir;
        assert_eq!(resolve_data_dir(), Some(expected));
        std::env::remove_var(DATA_DIR_ENV);
    }

    #[test]
    fn database_lives_directly_in_the_data_dir() {
        assert_eq!(
            database_path(Path::new("/tmp/x")),
            PathBuf::from("/tmp/x/statev2.sqlite")
        );
    }
}
