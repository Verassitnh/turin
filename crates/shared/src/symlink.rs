//! Port of upstream/packages/shared/src/symlink.ts

use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

const MAX_SYMLINK_HOPS: usize = 40;

/// Follows a chain of symlinks to the file it finally names, which may not
/// exist yet. A path that is not a symlink resolves to itself.
pub fn resolve_symlink_target(file_path: &Path) -> io::Result<PathBuf> {
    let mut current = lexical_absolute(file_path)?;
    for _hop in 0..MAX_SYMLINK_HOPS {
        let link = match std::fs::read_link(&current) {
            Ok(link) => link,
            Err(error) if is_not_a_symlink(&error) => return Ok(current),
            Err(error) => return Err(error),
        };
        let parent = current.parent().unwrap_or(Path::new("/"));
        let link_directory = match std::fs::canonicalize(parent) {
            Ok(directory) => directory,
            Err(error) if error.kind() == ErrorKind::NotFound => parent.to_path_buf(),
            Err(error) => return Err(error),
        };
        current = resolve_against(&link_directory, &link);
    }
    Err(io::Error::other("Too many levels of symbolic links"))
}

fn is_not_a_symlink(error: &io::Error) -> bool {
    // Node reports a missing path as NotFound and a non-link as EINVAL.
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput)
}

fn lexical_absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(normalize_lexical(path))
    } else {
        Ok(normalize_lexical(&std::env::current_dir()?.join(path)))
    }
}

fn resolve_against(base: &Path, relative: &Path) -> PathBuf {
    if relative.is_absolute() {
        normalize_lexical(relative)
    } else {
        normalize_lexical(&base.join(relative))
    }
}

/// Node `path.resolve` normalization: `.` and `..` are lexical, `..` does not
/// climb above the root.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    if path.is_absolute() {
        out.push(Component::RootDir);
    }
    for component in path.components() {
        match component {
            Component::RootDir => out.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                if out
                    .components()
                    .next_back()
                    .is_some_and(|part| matches!(part, Component::Normal(_)))
                {
                    out.pop();
                }
            }
            Component::Normal(part) => out.push(part),
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_a_regular_file_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("settings.json");
        fs::write(&file, "before").unwrap();
        assert_eq!(resolve_symlink_target(&file).unwrap(), file);
    }

    #[test]
    fn follows_a_relative_link_through_a_symlinked_parent() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("dotfiles/config/settings.json");
        let linked_state = root.path().join("dotfiles/state");
        let home = root.path().join("home");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::create_dir_all(&linked_state).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(&destination, "before").unwrap();
        std::os::unix::fs::symlink(&linked_state, home.join("state")).unwrap();
        let link = home.join("state/settings.json");
        std::os::unix::fs::symlink("../config/settings.json", &link).unwrap();
        // macOS temp dirs sit behind `/var` -> `/private/var`; upstream's
        // realPath resolves the link's directory, so compare canonical paths.
        assert_eq!(
            resolve_symlink_target(&link).unwrap(),
            fs::canonicalize(&destination).unwrap()
        );
    }

    #[test]
    fn fails_on_a_symlink_cycle() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        std::os::unix::fs::symlink(&second, &first).unwrap();
        std::os::unix::fs::symlink(&first, &second).unwrap();
        let error = resolve_symlink_target(&first).unwrap_err();
        assert_eq!(error.to_string(), "Too many levels of symbolic links");
    }
}
