//! Port of upstream/packages/shared/src/atomicWrite.ts
//!
//! Copied from `turin-shared` (T05a) until that crate lands on this branch.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::symlink::resolve_symlink_target;

/// Replaces a file's contents via a sibling temp file and rename. A symlinked
/// target is resolved first so the link survives and its destination is
/// rewritten, since renaming over the link itself would swap it for a regular
/// file.
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
    // The temp directory is cleanup, not part of the write: failing to remove
    // it must not fail a write that already landed.
    let _ = fs::remove_dir_all(&temp_directory);
    write
}

fn make_temp_directory(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    for _ in 0..1024u32 {
        let path = parent.join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()));
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
    fn creates_a_missing_file_and_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("nested/settings.json");
        write_file_string_atomically(&file_path, "fresh").unwrap();
        assert_eq!(fs::read_to_string(file_path).unwrap(), "fresh");
        let leftovers: Vec<_> = fs::read_dir(root.path().join("nested"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("settings.json")]);
    }
}
