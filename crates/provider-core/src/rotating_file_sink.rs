//! Port of `makeAsyncRotatingFileSink` in upstream/packages/shared/src/logging.ts.
//!
//! Upstream keeps this in the shared logging package. Only the provider event
//! log writes through it, so it lives here until that package is ported.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use thiserror::Error;
use tokio::{io::AsyncWriteExt, sync::Mutex};

#[derive(Debug, Error)]
pub enum RotatingFileSinkError {
    #[error("rotating file sink option '{option}' must be >= {minimum}; received {received}")]
    Configuration {
        option: &'static str,
        received: u64,
        minimum: u64,
    },
    #[error("rotating file sink failed to {operation} '{}'", file_path.display())]
    Io {
        operation: &'static str,
        file_path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_error(
    operation: &'static str,
    file_path: &Path,
) -> impl FnOnce(std::io::Error) -> RotatingFileSinkError {
    let file_path = file_path.to_path_buf();
    move |source| RotatingFileSinkError::Io {
        operation,
        file_path,
        source,
    }
}

/// `<file>.<index>`, the name a rotated generation has.
fn numbered(file_path: &Path, index: u64) -> PathBuf {
    let mut name: OsString = file_path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// Size of `path`, or 0 when it does not exist yet.
async fn file_size(path: &Path) -> std::io::Result<u64> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Removes `path` if present. Missing files are not an error (`rm --force`).
async fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Deletes `<basename>.<n>` generations beyond `max_files`. A bare
/// `<basename>.` counts as generation 0, as `Number("")` does upstream, and
/// names whose suffix is not an integer are left alone.
async fn prune_generations(
    directory: &Path,
    basename: &str,
    max_files: u64,
) -> std::io::Result<()> {
    let prefix = format!("{basename}.");
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_string_lossy()
            .strip_prefix(&prefix)
            .map(str::to_owned)
        else {
            continue;
        };
        let generation = if suffix.is_empty() {
            Some(0)
        } else {
            suffix.parse::<u64>().ok()
        };
        if generation.is_some_and(|generation| generation > max_files) {
            remove_if_present(&entry.path()).await?;
        }
    }
    Ok(())
}

pub struct RotatingFileSink {
    file_path: PathBuf,
    max_bytes: u64,
    max_files: u64,
    /// Bytes in the live file. The lock also serializes rotate and append.
    current_size: Mutex<u64>,
}

impl RotatingFileSink {
    /// Creates the directory, prunes stale generations and reads the live size.
    pub async fn open(
        file_path: impl Into<PathBuf>,
        max_bytes: u64,
        max_files: u64,
    ) -> Result<Self, RotatingFileSinkError> {
        if max_bytes < 1 {
            return Err(RotatingFileSinkError::Configuration {
                option: "maxBytes",
                received: max_bytes,
                minimum: 1,
            });
        }
        if max_files < 1 {
            return Err(RotatingFileSinkError::Configuration {
                option: "maxFiles",
                received: max_files,
                minimum: 1,
            });
        }
        let file_path = file_path.into();
        let directory = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let basename = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(io_error("initialize", &file_path))?;
        prune_generations(&directory, &basename, max_files)
            .await
            .map_err(io_error("prune", &file_path))?;
        let current_size = file_size(&file_path)
            .await
            .map_err(io_error("read", &file_path))?;
        Ok(Self {
            file_path,
            max_bytes,
            max_files,
            current_size: Mutex::new(current_size),
        })
    }

    /// Appends `chunk`, first rotating if it would push a non-empty live file
    /// past `max_bytes`. A chunk larger than `max_bytes` still lands whole in a
    /// fresh file, as upstream does.
    pub async fn write(&self, chunk: &[u8]) -> Result<(), RotatingFileSinkError> {
        if chunk.is_empty() {
            return Ok(());
        }
        let mut size = self.current_size.lock().await;
        let length = chunk.len() as u64;
        let result = async {
            if *size > 0 && *size + length > self.max_bytes {
                self.rotate()
                    .await
                    .map_err(io_error("rotate", &self.file_path))?;
                *size = 0;
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)
                .await
                .map_err(io_error("write", &self.file_path))?;
            // tokio buffers file writes; flush so the bytes reach the file before the handle drops.
            file.write_all(chunk)
                .await
                .map_err(io_error("write", &self.file_path))?;
            file.flush()
                .await
                .map_err(io_error("write", &self.file_path))?;
            Ok::<(), RotatingFileSinkError>(())
        }
        .await;
        match result {
            Ok(()) => {
                *size += length;
                Ok(())
            }
            Err(error) => {
                // Re-read the real size: a partial append may have landed.
                if let Ok(actual) = file_size(&self.file_path).await {
                    *size = actual;
                }
                Err(error)
            }
        }
    }

    /// Shifts `<file>.n` to `<file>.n+1`, dropping the oldest generation, then
    /// moves the live file to `<file>.1`.
    async fn rotate(&self) -> std::io::Result<()> {
        remove_if_present(&numbered(&self.file_path, self.max_files)).await?;
        for index in (1..self.max_files).rev() {
            let source = numbered(&self.file_path, index);
            if tokio::fs::try_exists(&source).await? {
                tokio::fs::rename(&source, numbered(&self.file_path, index + 1)).await?;
            }
        }
        if tokio::fs::try_exists(&self.file_path).await? {
            tokio::fs::rename(&self.file_path, numbered(&self.file_path, 1)).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn rotates_when_the_next_chunk_would_exceed_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let sink = RotatingFileSink::open(&path, 10, 3).await.unwrap();
        sink.write(b"aaaaaa").await.unwrap();
        sink.write(b"bbbbbb").await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"bbbbbb");
        assert_eq!(std::fs::read(numbered(&path, 1)).unwrap(), b"aaaaaa");
    }

    #[tokio::test]
    async fn keeps_only_max_files_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let sink = RotatingFileSink::open(&path, 4, 2).await.unwrap();
        for chunk in [b"aaaa", b"bbbb", b"cccc", b"dddd"] {
            sink.write(chunk).await.unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"dddd");
        assert_eq!(std::fs::read(numbered(&path, 1)).unwrap(), b"cccc");
        assert_eq!(std::fs::read(numbered(&path, 2)).unwrap(), b"bbbb");
        assert!(!numbered(&path, 3).exists());
    }

    #[tokio::test]
    async fn open_prunes_generations_beyond_max_files_and_keeps_other_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        for name in [
            "events.log.1",
            "events.log.5",
            "events.log.bak",
            "other.log.9",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        RotatingFileSink::open(&path, 4, 2).await.unwrap();
        assert!(numbered(&path, 1).exists());
        assert!(!numbered(&path, 5).exists());
        assert!(dir.path().join("events.log.bak").exists());
        assert!(dir.path().join("other.log.9").exists());
    }

    #[tokio::test]
    async fn rejects_zero_limits() {
        let dir = tempfile::tempdir().unwrap();
        let error = RotatingFileSink::open(dir.path().join("e.log"), 0, 1)
            .await
            .err()
            .unwrap();
        assert!(matches!(
            error,
            RotatingFileSinkError::Configuration {
                option: "maxBytes",
                ..
            }
        ));
    }
}
