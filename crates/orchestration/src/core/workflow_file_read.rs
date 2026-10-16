//! Port of upstream/apps/server/src/orchestration-v2/workflowFileRead.ts
//!
//! Contained reads of files the Claude harness persisted for a workflow run.
//!
//! Containment rules (lifted from the reviewed #3650 inspection service):
//! - the resolved realpath must live under the configured Claude projects
//!   directory (where the harness persists workflow scripts and transcripts) —
//!   realpath re-containment defeats symlink escapes, including a symlinked
//!   leaf file;
//! - only the caller's expected extension is served;
//! - reads are size-capped rather than failed, with a truncation marker.
//!
//! The client-supplied path is a hint from the workflow's runHandles; it is
//! never trusted beyond these checks.
//!
//! `HostProcess.HomeDirectory` is passed in as `home_directory` (the caller
//! reads it once from `turin_provider_core::host_process::HostProcess`).

use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2WorkflowFileErrorReason as Reason, OrchestrationWorkflowFileError,
};

/// The file the read resolved to, with its (possibly capped) contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainedWorkflowFile {
    pub path: String,
    pub contents: String,
    pub truncated: bool,
}

/// The arguments of `readContainedWorkflowFile`.
#[derive(Debug, Clone, Copy)]
pub struct ContainedWorkflowFileRequest<'a> {
    pub path: &'a str,
    /// Expected extension including the dot, e.g. `.js`.
    pub extension: &'a str,
    pub byte_cap: usize,
    /// Keep the last `byte_cap` bytes instead of the first.
    pub tail: bool,
    pub config_dir: Option<&'a str>,
}

/// `OrchestrationWorkflowFileError` for `reason`. The cause is kept as text
/// because the wire shape carries it as an opaque value.
pub(crate) fn workflow_file_error(
    reason: Reason,
    path: impl Into<String>,
    cause: Option<&io::Error>,
) -> OrchestrationWorkflowFileError {
    OrchestrationWorkflowFileError::OrchestrationWorkflowFileError {
        reason,
        path: path.into(),
        cause: cause.map(|cause| json!(cause.to_string())),
    }
}

/// `readContainedWorkflowFile`. Root is `<configDir or ~/.claude>/projects`.
pub async fn read_contained_workflow_file(
    request: ContainedWorkflowFileRequest<'_>,
    home_directory: &str,
) -> Result<ContainedWorkflowFile, OrchestrationWorkflowFileError> {
    if !Path::new(request.path).is_absolute() || node_extname(request.path) != request.extension {
        return Err(workflow_file_error(Reason::InvalidPath, request.path, None));
    }

    let root = tokio::fs::canonicalize(workflow_files_root(home_directory, request.config_dir))
        .await
        .map_err(|cause| {
            workflow_file_error(Reason::RootUnavailable, request.path, Some(&cause))
        })?;

    // Realpath the FILE itself (not just its directory): a symlink named
    // like a transcript inside a contained directory must not escape.
    let resolved = tokio::fs::canonicalize(request.path)
        .await
        .map_err(|cause| workflow_file_error(Reason::NotFound, request.path, Some(&cause)))?;
    let resolved_path = resolved.to_string_lossy().into_owned();

    // `Path::starts_with` compares whole components, which is the upstream
    // `root + sep` check without string-prefix pitfalls.
    if !resolved.starts_with(&root) {
        return Err(workflow_file_error(
            Reason::OutsideRoot,
            resolved_path,
            None,
        ));
    }
    if node_extname(&resolved_path) != request.extension {
        return Err(workflow_file_error(
            Reason::WrongExtension,
            resolved_path,
            None,
        ));
    }

    // fd inode vs path inode catches a swapped leaf, not a swapped intermediate
    // dir; root is 0700 ~/.claude, so that already needs the uid we run as.
    // Both containment checks fail with their own tagged reason; "read-failed"
    // is reserved for genuine platform failures with the real cause attached.
    let (read_path, byte_cap, tail) = (resolved.clone(), request.byte_cap, request.tail);
    let outcome = tokio::task::spawn_blocking(move || read_bounded(&read_path, byte_cap, tail))
        .await
        .map_err(io::Error::other)
        .and_then(|result| result)
        .map_err(|cause| {
            workflow_file_error(Reason::ReadFailed, resolved_path.clone(), Some(&cause))
        })?;

    match outcome {
        Ok(read) => Ok(ContainedWorkflowFile {
            path: resolved_path,
            contents: read.contents,
            truncated: read.truncated,
        }),
        Err(reason) => Err(workflow_file_error(reason, resolved_path, None)),
    }
}

/// `path.join(configDir?.trim() || join(home, ".claude"), "projects")`.
fn workflow_files_root(home_directory: &str, config_dir: Option<&str>) -> PathBuf {
    let config_dir = config_dir
        .map(turin_contracts::base_schemas::js_trim)
        .filter(|dir| !dir.is_empty());
    let base = match config_dir {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(home_directory).join(".claude"),
    };
    base.join("projects")
}

/// Node's `path.extname`: the last dot of the basename, unless it is the first
/// character (dotfiles have no extension).
fn node_extname(path: &str) -> &str {
    let Some(base) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return "";
    };
    match base.rfind('.') {
        Some(index) if index > 0 => &base[index..],
        _ => "",
    }
}

struct BoundedRead {
    contents: String,
    truncated: bool,
}

/// The blocking half: open without blocking on FIFOs, check the handle is a
/// regular file still at the path, then read at most `byte_cap` bytes (the
/// tail when `tail`). Containment failures come back as `Ok(Err(reason))`.
fn read_bounded(
    resolved: &Path,
    byte_cap: usize,
    tail: bool,
) -> io::Result<Result<BoundedRead, Reason>> {
    let mut file = open_nonblocking(resolved)?;
    let stat = file.metadata()?;
    if !stat.is_file() {
        return Ok(Err(Reason::NotRegularFile));
    }
    let path_stat = fs::symlink_metadata(resolved)?;
    if !same_inode(&stat, &path_stat) {
        return Ok(Err(Reason::ChangedDuringRead));
    }

    let size = stat.len();
    let length = size.min(byte_cap as u64 + u64::from(tail));
    let truncated = size > length;
    let offset = if tail { size - length } else { 0 };
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = Vec::with_capacity(length as usize);
    (&mut file).take(length).read_to_end(&mut buffer)?;
    Ok(Ok(BoundedRead {
        contents: String::from_utf8_lossy(&buffer).into_owned(),
        truncated,
    }))
}

#[cfg(unix)]
fn open_nonblocking(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_nonblocking(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn same_inode(fd: &Metadata, path: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    fd.ino() == path.ino() && fd.dev() == path.dev()
}

#[cfg(not(unix))]
fn same_inode(_fd: &Metadata, _path: &Metadata) -> bool {
    true
}
