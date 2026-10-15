//! Port of upstream/apps/server/src/worktreesDirectory.ts
//!
//! The upstream module is cut with thread launch (PORTING.md), but review cwd checks still need
//! the managed worktree roots: a worktree created under a custom `worktreesDirectory` is a valid
//! review cwd even outside the workspace root. Only those path helpers are kept.

use std::path::{Component, Path, PathBuf};

/// The `worktreesDirectory` settings pair `managedWorktreesDirectories` reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreesSettings {
    pub worktrees_directory: String,
    pub previous_worktrees_directories: Vec<String>,
}

/// Node `path.normalize`-style lexical cleanup: drops `.`, folds `..` into its parent, and never
/// climbs above a root. It does not touch the filesystem, so symlinks are left for `canonicalize`.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    // `/..` stays `/`; a relative path that already climbed keeps its `..`.
                    if !matches!(
                        normalized.components().next_back(),
                        Some(Component::RootDir)
                    ) {
                        normalized.push("..");
                    }
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// `expandHomePath` from provider-core: `~`, `~/…` and `~\…` expand to `home`; everything else,
/// including `~user`, is returned unchanged.
pub fn expand_home_path(value: &str, home: &str) -> String {
    if value.is_empty() {
        return value.to_owned();
    }
    if value == "~" {
        return home.to_owned();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return normalize_lexical(&Path::new(home).join(rest))
            .to_string_lossy()
            .into_owned();
    }
    value.to_owned()
}

/// Callers re-check after resolving symlinks, because a link can point at a root.
pub fn is_filesystem_root(directory: &Path) -> bool {
    directory.parent().is_none_or(|parent| parent == directory)
}

/// The directory a `worktreesDirectory` setting names: the default when the setting is empty,
/// `None` when it is not an absolute path on this machine (such as `D:\worktrees` synced to a
/// Linux server) or when it is a filesystem root, which would make every path look managed.
pub fn resolve_worktrees_directory(
    setting: &str,
    default_dir: &Path,
    home: &str,
) -> Option<PathBuf> {
    if setting.is_empty() {
        return Some(default_dir.to_path_buf());
    }
    let expanded = PathBuf::from(expand_home_path(setting, home));
    if !expanded.is_absolute() {
        return None;
    }
    let resolved = normalize_lexical(&expanded);
    if is_filesystem_root(&resolved) {
        None
    } else {
        Some(resolved)
    }
}

/// Every directory that holds T3-managed worktrees on this machine, de-duplicated in order.
pub fn managed_worktrees_directories(
    settings: &WorktreesSettings,
    default_dir: &Path,
    home: &str,
) -> Vec<PathBuf> {
    let mut directories = vec![default_dir.to_path_buf()];
    let settings_values = std::iter::once(&settings.worktrees_directory)
        .chain(settings.previous_worktrees_directories.iter());
    for setting in settings_values {
        if let Some(directory) = resolve_worktrees_directory(setting, default_dir, home)
            && !directories.contains(&directory)
        {
            directories.push(directory);
        }
    }
    directories
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_dots_and_never_climbs_above_root() {
        assert_eq!(
            normalize_lexical(Path::new("/a/./b/../c/")),
            PathBuf::from("/a/c")
        );
        assert_eq!(normalize_lexical(Path::new("/../..")), PathBuf::from("/"));
        assert_eq!(
            normalize_lexical(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
    }

    #[test]
    fn empty_setting_resolves_to_default_directory() {
        let default_dir = Path::new("/base/worktrees");
        assert_eq!(
            resolve_worktrees_directory("", default_dir, "/home/u"),
            Some(PathBuf::from("/base/worktrees"))
        );
    }

    #[test]
    fn home_relative_setting_expands_and_relative_or_root_settings_are_rejected() {
        let default_dir = Path::new("/base/worktrees");
        assert_eq!(
            resolve_worktrees_directory("~/wt/", default_dir, "/home/u"),
            Some(PathBuf::from("/home/u/wt"))
        );
        assert_eq!(
            resolve_worktrees_directory("wt", default_dir, "/home/u"),
            None
        );
        assert_eq!(
            resolve_worktrees_directory("/", default_dir, "/home/u"),
            None
        );
        assert_eq!(resolve_worktrees_directory("~", default_dir, "/"), None);
    }

    #[test]
    fn managed_directories_keep_default_first_and_skip_unusable_settings() {
        let settings = WorktreesSettings {
            worktrees_directory: "/".to_owned(),
            previous_worktrees_directories: vec![
                "/old".to_owned(),
                "relative".to_owned(),
                "/old/".to_owned(),
            ],
        };
        assert_eq!(
            managed_worktrees_directories(&settings, Path::new("/base/worktrees"), "/home/u"),
            vec![PathBuf::from("/base/worktrees"), PathBuf::from("/old")]
        );
    }
}
