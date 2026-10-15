//! Port of upstream/packages/shared/src/logging.ts

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotatingFileSinkConfigurationError {
    pub option: &'static str,
    pub received: u64,
    pub minimum: u64,
}

impl std::fmt::Display for RotatingFileSinkConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} must be >= {} (received {})",
            self.option, self.minimum, self.received
        )
    }
}

impl std::error::Error for RotatingFileSinkConfigurationError {}

#[derive(Debug)]
pub struct RotatingFileSinkError {
    pub operation: &'static str,
    pub file_path: PathBuf,
    pub cause: io::Error,
}

impl std::fmt::Display for RotatingFileSinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Failed to {} rotating log file {}",
            self.operation,
            self.file_path.display()
        )
    }
}

impl std::error::Error for RotatingFileSinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

#[derive(Debug)]
pub enum RotatingFileSinkOpenError {
    Configuration(RotatingFileSinkConfigurationError),
    Io(RotatingFileSinkError),
}

impl std::fmt::Display for RotatingFileSinkOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RotatingFileSinkOpenError {}

pub struct RotatingFileSink {
    file_path: PathBuf,
    max_bytes: u64,
    max_files: u64,
    throw_on_error: bool,
    current_size: u64,
}

impl RotatingFileSink {
    pub fn open(
        file_path: impl Into<PathBuf>,
        max_bytes: u64,
        max_files: u64,
        throw_on_error: bool,
    ) -> Result<Self, RotatingFileSinkOpenError> {
        if max_bytes < 1 {
            return Err(RotatingFileSinkOpenError::Configuration(
                RotatingFileSinkConfigurationError {
                    option: "maxBytes",
                    received: max_bytes,
                    minimum: 1,
                },
            ));
        }
        if max_files < 1 {
            return Err(RotatingFileSinkOpenError::Configuration(
                RotatingFileSinkConfigurationError {
                    option: "maxFiles",
                    received: max_files,
                    minimum: 1,
                },
            ));
        }
        let file_path = file_path.into();
        if let Some(parent) = file_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|cause| {
                RotatingFileSinkOpenError::Io(sink_error("initialize", &file_path, cause))
            })?;
        }
        let sink = Self {
            file_path,
            max_bytes,
            max_files,
            throw_on_error,
            current_size: 0,
        };
        sink.prune_overflow_backups()
            .map_err(RotatingFileSinkOpenError::Io)?;
        let current_size = sink.read_current_size().map_err(RotatingFileSinkOpenError::Io)?;
        Ok(Self {
            current_size,
            ..sink
        })
    }

    pub fn write(&mut self, chunk: impl AsRef<[u8]>) -> Result<(), RotatingFileSinkError> {
        let buffer = chunk.as_ref();
        if buffer.is_empty() {
            return Ok(());
        }
        let length = buffer.len() as u64;
        if let Err(error) = self.write_chunk(buffer, length) {
            if error.operation != "write" {
                return Err(error);
            }
            if self.throw_on_error {
                return Err(error);
            }
            self.current_size = self.read_current_size().unwrap_or(self.current_size);
        }
        Ok(())
    }

    fn write_chunk(&mut self, buffer: &[u8], length: u64) -> Result<(), RotatingFileSinkError> {
        if self.current_size > 0 && self.current_size + length > self.max_bytes {
            self.rotate()?;
        }
        append_bytes(&self.file_path, buffer)
            .map_err(|cause| sink_error("write", &self.file_path, cause))?;
        self.current_size += length;
        Ok(())
    }

    fn rotate(&mut self) -> Result<(), RotatingFileSinkError> {
        let rotated = (|| {
            rm_force(&self.with_suffix(self.max_files))?;
            for index in (1..self.max_files).rev() {
                let source = self.with_suffix(index);
                if path_exists(&source) {
                    fs::rename(&source, self.with_suffix(index + 1))?;
                }
            }
            if path_exists(&self.file_path) {
                fs::rename(&self.file_path, self.with_suffix(1))?;
            }
            Ok(())
        })();
        match rotated {
            Ok(()) => {
                self.current_size = 0;
                Ok(())
            }
            Err(cause) => {
                if self.throw_on_error {
                    Err(sink_error("rotate", &self.file_path, cause))
                } else {
                    self.current_size = self.read_current_size().unwrap_or(self.current_size);
                    Ok(())
                }
            }
        }
    }

    fn prune_overflow_backups(&self) -> Result<(), RotatingFileSinkError> {
        let Some(parent) = self.file_path.parent() else {
            return Ok(());
        };
        let Some(base_name) = self.file_path.file_name().and_then(|name| name.to_str()) else {
            return Ok(());
        };
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(cause) if self.throw_on_error => {
                return Err(sink_error("prune", &self.file_path, cause));
            }
            Err(_) => return Ok(()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(cause) if self.throw_on_error => {
                    return Err(sink_error("prune", &self.file_path, cause));
                }
                Err(_) => return Ok(()),
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(suffix) = name.strip_prefix(&format!("{base_name}.")) else {
                continue;
            };
            let Some(index) = js_integer(suffix) else {
                continue;
            };
            if index <= self.max_files as i64 {
                continue;
            }
            if let Err(cause) = rm_force(&entry.path()) {
                if self.throw_on_error {
                    return Err(sink_error("prune", &self.file_path, cause));
                }
            }
        }
        Ok(())
    }

    fn read_current_size(&self) -> Result<u64, RotatingFileSinkError> {
        match fs::metadata(&self.file_path) {
            Ok(metadata) => Ok(metadata.len()),
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(cause) => Err(sink_error("read", &self.file_path, cause)),
        }
    }

    fn with_suffix(&self, index: u64) -> PathBuf {
        PathBuf::from(format!("{}.{}", self.file_path.display(), index))
    }
}

struct AsyncState {
    current_size: u64,
}

pub struct AsyncRotatingFileSink {
    file_path: PathBuf,
    max_bytes: u64,
    max_files: u64,
    state: Mutex<AsyncState>,
    fail_appends: AtomicUsize,
}

impl AsyncRotatingFileSink {
    pub async fn open(
        file_path: impl Into<PathBuf>,
        max_bytes: u64,
        max_files: u64,
    ) -> Result<Arc<Self>, RotatingFileSinkOpenError> {
        if max_bytes < 1 {
            return Err(RotatingFileSinkOpenError::Configuration(
                RotatingFileSinkConfigurationError {
                    option: "maxBytes",
                    received: max_bytes,
                    minimum: 1,
                },
            ));
        }
        if max_files < 1 {
            return Err(RotatingFileSinkOpenError::Configuration(
                RotatingFileSinkConfigurationError {
                    option: "maxFiles",
                    received: max_files,
                    minimum: 1,
                },
            ));
        }
        let file_path = file_path.into();
        if let Some(parent) = file_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|cause| {
                RotatingFileSinkOpenError::Io(sink_error("initialize", &file_path, cause))
            })?;
        }
        let sink = Arc::new(Self {
            file_path,
            max_bytes,
            max_files,
            state: Mutex::new(AsyncState { current_size: 0 }),
            fail_appends: AtomicUsize::new(0),
        });
        sink.prune()
            .map_err(RotatingFileSinkOpenError::Io)?;
        let size = sink
            .read_size()
            .map_err(RotatingFileSinkOpenError::Io)?;
        sink.state.lock().await.current_size = size;
        Ok(sink)
    }

    #[cfg(test)]
    pub fn fail_next_appends(&self, count: usize) {
        self.fail_appends.store(count, Ordering::SeqCst);
    }

    pub async fn write(self: &Arc<Self>, chunk: impl AsRef<[u8]>) -> Result<(), RotatingFileSinkError> {
        let buffer = chunk.as_ref().to_vec();
        let mut state = self.state.lock().await;
        if buffer.is_empty() {
            return Ok(());
        }
        let length = buffer.len() as u64;
        let written = async {
            if state.current_size > 0 && state.current_size + length > self.max_bytes {
                self.rotate(&mut state).await?;
            }
            self.append(&buffer)?;
            state.current_size += length;
            Ok(())
        }
        .await;
        if written.is_err() {
            if let Ok(size) = self.read_size() {
                state.current_size = size;
            }
        }
        written
    }

    async fn rotate(&self, state: &mut AsyncState) -> Result<(), RotatingFileSinkError> {
        let result = (|| {
            rm_force(&suffixed(&self.file_path, self.max_files))?;
            for index in (1..self.max_files).rev() {
                let source = suffixed(&self.file_path, index);
                if path_exists(&source) {
                    fs::rename(&source, suffixed(&self.file_path, index + 1))?;
                }
            }
            if path_exists(&self.file_path) {
                fs::rename(&self.file_path, suffixed(&self.file_path, 1))?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                state.current_size = 0;
                Ok(())
            }
            Err(cause) => Err(sink_error("rotate", &self.file_path, cause)),
        }
    }

    fn append(&self, bytes: &[u8]) -> Result<(), RotatingFileSinkError> {
        if self.fail_appends.load(Ordering::SeqCst) > 0 {
            self.fail_appends.fetch_sub(1, Ordering::SeqCst);
            return Err(sink_error(
                "write",
                &self.file_path,
                io::Error::new(io::ErrorKind::IsADirectory, "mock write failure"),
            ));
        }
        append_bytes(&self.file_path, bytes)
            .map_err(|cause| sink_error("write", &self.file_path, cause))
    }

    fn prune(&self) -> Result<(), RotatingFileSinkError> {
        let Some(parent) = self.file_path.parent() else {
            return Ok(());
        };
        let Some(base_name) = self.file_path.file_name().and_then(|name| name.to_str()) else {
            return Ok(());
        };
        for entry in fs::read_dir(parent).map_err(|cause| sink_error("prune", &self.file_path, cause))?
        {
            let entry = entry.map_err(|cause| sink_error("prune", &self.file_path, cause))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(suffix) = name.strip_prefix(&format!("{base_name}.")) else {
                continue;
            };
            let Some(index) = js_integer(suffix) else {
                continue;
            };
            if index <= self.max_files as i64 {
                continue;
            }
            rm_force(&entry.path()).map_err(|cause| sink_error("prune", &self.file_path, cause))?;
        }
        Ok(())
    }

    fn read_size(&self) -> Result<u64, RotatingFileSinkError> {
        match fs::metadata(&self.file_path) {
            Ok(metadata) => Ok(metadata.len()),
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(cause) => Err(sink_error("read", &self.file_path, cause)),
        }
    }
}

fn suffixed(path: &Path, index: u64) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}

fn sink_error(operation: &'static str, file_path: &Path, cause: io::Error) -> RotatingFileSinkError {
    RotatingFileSinkError {
        operation,
        file_path: file_path.to_path_buf(),
        cause,
    }
}

fn append_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `fs.rmSync(path, { force: true })` without `recursive`.
fn rm_force(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(metadata) if metadata.is_dir() => {
            Err(io::Error::new(io::ErrorKind::IsADirectory, "is a directory"))
        }
        Ok(_) => fs::remove_file(path),
    }
}

/// `Number(suffix)` when it is an integer. An empty suffix is `0`.
fn js_integer(suffix: &str) -> Option<i64> {
    let trimmed = suffix.trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        trimmed.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn reports_invalid_configuration() {
        for (option, max_bytes, max_files) in [("maxBytes", 0, 1), ("maxFiles", 1, 0)] {
            let error = RotatingFileSink::open("/unused/log.ndjson", max_bytes, max_files, false)
                .unwrap_err();
            let RotatingFileSinkOpenError::Configuration(error) = error else {
                panic!("expected configuration error");
            };
            assert_eq!(error.option, option);
            assert_eq!(error.received, 0);
            assert_eq!(error.minimum, 1);
            assert_eq!(error.to_string(), format!("{option} must be >= 1 (received 0)"));
        }
    }

    #[test]
    fn preserves_directory_initialization_failures() {
        let directory = temp_dir();
        let parent = directory.path().join("not-a-directory");
        fs::write(&parent, "occupied").unwrap();
        let file_path = parent.join("log.ndjson");
        let error = RotatingFileSink::open(&file_path, 1, 1, false).unwrap_err();
        let RotatingFileSinkOpenError::Io(error) = error else {
            panic!("expected io error");
        };
        assert_eq!(error.operation, "initialize");
        assert_eq!(error.file_path, file_path);
    }

    #[cfg(unix)]
    #[test]
    fn only_treats_a_missing_log_file_as_an_empty_current_size() {
        let directory = temp_dir();
        let file_path = directory.path().join("a".repeat(300));
        let error = RotatingFileSink::open(&file_path, 1, 1, false).unwrap_err();
        let RotatingFileSinkOpenError::Io(error) = error else {
            panic!("expected io error");
        };
        assert_eq!(error.operation, "read");
        assert_eq!(error.file_path, file_path);
        assert_eq!(
            error.cause.raw_os_error(),
            Some(63),
            "ENAMETOOLONG"
        );
    }

    #[test]
    fn starts_an_absent_log_file_at_zero_bytes() {
        let directory = temp_dir();
        let file_path = directory.path().join("log.ndjson");
        let mut sink = RotatingFileSink::open(&file_path, 100, 1, false).unwrap();
        sink.write("entry").unwrap();
        assert_eq!(fs::read_to_string(file_path).unwrap(), "entry");
    }

    #[test]
    fn preserves_write_failures() {
        let directory = temp_dir();
        let file_path = directory.path().join("log.ndjson");
        fs::create_dir(&file_path).unwrap();
        let mut sink =
            RotatingFileSink::open(&file_path, 9_007_199_254_740_991, 1, true).unwrap();
        let error = sink.write("entry").unwrap_err();
        assert_eq!(error.operation, "write");
        assert_eq!(error.file_path, file_path);
        assert_eq!(error.cause.kind(), io::ErrorKind::IsADirectory);
    }

    #[test]
    fn preserves_rotation_failures() {
        let directory = temp_dir();
        let file_path = directory.path().join("log.ndjson");
        fs::write(&file_path, "a").unwrap();
        fs::create_dir(format!("{}.1", file_path.display())).unwrap();
        let mut sink = RotatingFileSink::open(&file_path, 1, 1, true).unwrap();
        let error = sink.write("b").unwrap_err();
        assert_eq!(error.operation, "rotate");
        assert_eq!(error.file_path, file_path);
    }

    #[test]
    fn never_reports_a_rotation_failure_after_appending() {
        let directory = temp_dir();
        let file_path = directory.path().join("log.ndjson");
        fs::create_dir(format!("{}.1", file_path.display())).unwrap();
        let mut sink = RotatingFileSink::open(&file_path, 1, 1, true).unwrap();
        sink.write("oversized").unwrap();
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "oversized");
        let error = sink.write("next").unwrap_err();
        assert_eq!(error.operation, "rotate");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "oversized");
    }

    #[test]
    fn preserves_backup_pruning_failures() {
        let directory = temp_dir();
        let file_path = directory.path().join("log.ndjson");
        let overflow = format!("{}.2", file_path.display());
        fs::create_dir(&overflow).unwrap();
        fs::write(Path::new(&overflow).join("entry"), "occupied").unwrap();
        let error = RotatingFileSink::open(&file_path, 1, 1, true).unwrap_err();
        let RotatingFileSinkOpenError::Io(error) = error else {
            panic!("expected io error");
        };
        assert_eq!(error.operation, "prune");
        assert_eq!(error.file_path, file_path);
    }

    #[tokio::test]
    async fn serializes_concurrent_appends_across_rotation() {
        let directory = temp_dir();
        let file_path = directory.path().join("async.log");
        fs::write(format!("{}.50", file_path.display()), "obsolete").unwrap();
        let lines: Vec<String> = (0..100)
            .map(|index| format!("{index:03}\n"))
            .collect();
        let sink = AsyncRotatingFileSink::open(&file_path, 32, 20)
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for line in &lines {
            let sink = Arc::clone(&sink);
            let line = line.clone();
            tasks.push(tokio::spawn(async move { sink.write(line).await.unwrap() }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let resumed = AsyncRotatingFileSink::open(&file_path, 32, 20)
            .await
            .unwrap();
        resumed.write("100\n").await.unwrap();
        assert!(!Path::new(&format!("{}.50", file_path.display())).exists());
        let mut backups: Vec<String> = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("async.log."))
            .collect();
        backups.sort_by(|left, right| {
            let left = left[10..].parse::<i64>().unwrap_or(0);
            let right = right[10..].parse::<i64>().unwrap_or(0);
            right.cmp(&left)
        });
        let mut contents = Vec::new();
        for name in backups {
            let chunk = fs::read(directory.path().join(&name)).unwrap();
            assert!(chunk.len() <= 32);
            contents.extend(chunk);
        }
        let current = fs::read(&file_path).unwrap();
        assert!(current.len() <= 32);
        contents.extend(current);
        let mut expected = lines.join("");
        expected.push_str("100\n");
        assert_eq!(String::from_utf8(contents).unwrap(), expected);
    }

    #[tokio::test]
    async fn reports_a_failed_append_and_recovers_its_size() {
        let directory = temp_dir();
        let file_path = directory.path().join("recovery.log");
        fs::write(&file_path, "old\n").unwrap();
        let sink = AsyncRotatingFileSink::open(&file_path, 8, 2).await.unwrap();
        sink.fail_next_appends(1);
        let failed = sink.write("new\n").await.unwrap_err();
        assert_eq!(failed.operation, "write");
        sink.write("new\n").await.unwrap();
        sink.write("end\n").await.unwrap();
        assert_eq!(
            fs::read_to_string(format!("{}.1", file_path.display())).unwrap(),
            "old\nnew\n"
        );
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "end\n");
    }
}
