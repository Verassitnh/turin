//! Port of upstream/packages/shared/src/atomicWrite.ts
//!
//! Replaces a file via a sibling temp file and rename. A symlinked target is
//! resolved first so the link survives and its destination is rewritten.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::symlink::resolve_symlink_target;

pub fn write_file_string_atomically(file_path: &Path, contents: &str) -> io::Result<()> {
    let target_path = resolve_symlink_target(file_path)?;
    let target_directory = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&target_directory)?;
    let prefix = format!(
        "{}.",
        target_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
    );
    let temp_directory = make_temp_directory(&target_directory, &prefix)?;
    let write = (|| {
        let temp_path = temp_directory.join("contents.tmp");
        fs::write(&temp_path, contents)?;
        fs::rename(&temp_path, &target_path)?;
        Ok(())
    })();
    // Failing to remove the temp directory must not fail a write that landed.
    let _ = fs::remove_dir_all(&temp_directory);
    write
}

fn make_temp_directory(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    for salt in 0..1024u32 {
        let path = parent.join(format!("{prefix}{}-{salt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("could not create a temp directory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn keeps_a_symlinked_file_linked_and_rewrites_its_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("dotfiles/settings.json");
        let link = root.path().join("home/settings.json");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        fs::write(&destination, "before").unwrap();
        symlink(&destination, &link).unwrap();

        write_file_string_atomically(&link, "after").unwrap();

        assert_eq!(fs::read_link(&link).unwrap(), destination);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "after");
    }

    #[test]
    fn keeps_a_dangling_symlink_linked_and_creates_its_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("dotfiles/settings.json");
        let link = root.path().join("home/settings.json");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&destination, &link).unwrap();

        write_file_string_atomically(&link, "fresh").unwrap();

        assert_eq!(fs::read_link(&link).unwrap(), destination);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "fresh");
    }

    #[test]
    fn fails_on_a_symlink_cycle_without_replacing_either_link() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        symlink(&second, &first).unwrap();
        symlink(&first, &second).unwrap();

        assert!(write_file_string_atomically(&first, "after").is_err());
        assert_eq!(fs::read_link(&first).unwrap(), second);
        assert_eq!(fs::read_link(&second).unwrap(), first);
    }

    #[test]
    fn resolves_a_relative_link_through_a_symlinked_parent_directory() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("dotfiles/config/settings.json");
        let linked_state = root.path().join("dotfiles/state");
        let home = root.path().join("home");
        let link = home.join("state/settings.json");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::create_dir_all(&linked_state).unwrap();
        fs::create_dir_all(&home).unwrap();
        symlink(&linked_state, home.join("state")).unwrap();
        fs::write(&destination, "before").unwrap();
        symlink("../config/settings.json", &link).unwrap();

        write_file_string_atomically(&link, "after").unwrap();

        assert_eq!(
            fs::read_link(&link).unwrap(),
            PathBuf::from("../config/settings.json")
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "after");
    }

    #[test]
    fn creates_a_missing_file_and_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("nested/settings.json");
        write_file_string_atomically(&file_path, "fresh").unwrap();
        assert_eq!(fs::read_to_string(file_path).unwrap(), "fresh");
    }
}
