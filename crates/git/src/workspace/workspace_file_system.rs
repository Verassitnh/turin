//! Port of upstream/apps/server/src/workspace/WorkspaceFileSystem.ts
//!
//! Reads may be workspace-relative or an absolute host path. Writes never
//! leave the root. Closing a file does not surface `close` errors: Rust drops
//! the descriptor without returning them, unlike Node's `handle.close()`.

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::filesystem::{
    FilesystemEntryKind, FilesystemEntryMetadata, FilesystemGetMetadataInput,
    FilesystemGetMetadataResult,
};
use turin_contracts::project::{
    ProjectFileOperation, ProjectReadFileInput, ProjectReadFileResult, ProjectWriteFileInput,
    ProjectWriteFileResult,
};

use super::file_header_mime_type::file_header_mime_type;
use super::workspace_entries::WorkspaceEntries;
use super::workspace_paths::{
    WorkspacePathOutsideRootError, WorkspacePaths, escapes_workspace_root, expand_home_path,
    node_dirname, node_extname, node_is_absolute, node_relative,
};

const PROJECT_READ_FILE_MAX_BYTES: u64 = 1024 * 1024;
const HEADER_BYTES: usize = 512;

#[derive(Debug)]
pub struct WorkspaceFileSystemOperationError {
    pub workspace_root: String,
    pub relative_path: String,
    pub resolved_path: String,
    pub operation_path: String,
    pub operation: ProjectFileOperation,
    pub cause: io::Error,
}

impl fmt::Display for WorkspaceFileSystemOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Workspace file operation '{}' failed at '{}' for resolved path '{}' (requested as '{}' in '{}').",
            operation_name(self.operation),
            self.operation_path,
            self.resolved_path,
            self.relative_path,
            self.workspace_root
        )
    }
}

impl std::error::Error for WorkspaceFileSystemOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

fn operation_name(operation: ProjectFileOperation) -> &'static str {
    match operation {
        ProjectFileOperation::RealpathWorkspaceRoot => "realpath-workspace-root",
        ProjectFileOperation::RealpathTarget => "realpath-target",
        ProjectFileOperation::Open => "open",
        ProjectFileOperation::Stat => "stat",
        ProjectFileOperation::Read => "read",
        ProjectFileOperation::Close => "close",
        ProjectFileOperation::MakeDirectory => "make-directory",
        ProjectFileOperation::WriteFile => "write-file",
    }
}

#[derive(Debug, Clone, Error)]
#[error(
    "Workspace file '{relative_path}' resolves outside workspace root '{workspace_root}': {resolved_path}"
)]
pub struct WorkspaceFilePathEscapeError {
    pub workspace_root: String,
    pub relative_path: String,
    pub resolved_workspace_root: String,
    pub resolved_path: String,
}

#[derive(Debug, Clone, Error)]
#[error("Workspace path '{relative_path}' in '{workspace_root}' is not a file: {resolved_path}")]
pub struct WorkspacePathNotFileError {
    pub workspace_root: String,
    pub relative_path: String,
    pub resolved_path: String,
}

#[derive(Debug, Clone, Error)]
#[error(
    "Workspace file '{relative_path}' in '{workspace_root}' is binary and cannot be previewed as text."
)]
pub struct WorkspaceBinaryFileError {
    pub workspace_root: String,
    pub relative_path: String,
    pub resolved_path: String,
}

#[derive(Debug, Error)]
pub enum WorkspaceFileReadError {
    #[error(transparent)]
    Operation(#[from] WorkspaceFileSystemOperationError),
    #[error(transparent)]
    Escape(#[from] WorkspaceFilePathEscapeError),
    #[error(transparent)]
    NotFile(#[from] WorkspacePathNotFileError),
    #[error(transparent)]
    Binary(#[from] WorkspaceBinaryFileError),
    #[error(transparent)]
    OutsideRoot(#[from] WorkspacePathOutsideRootError),
}

#[derive(Debug, Error)]
pub enum WorkspaceFileWriteError {
    #[error(transparent)]
    Operation(#[from] WorkspaceFileSystemOperationError),
    #[error(transparent)]
    OutsideRoot(#[from] WorkspacePathOutsideRootError),
}

struct ReadTarget {
    relative_path: String,
    real_target_path: String,
}

/// Workspace-root-relative file reads and writes.
pub struct WorkspaceFileSystem {
    paths: Arc<WorkspacePaths>,
    entries: Arc<WorkspaceEntries>,
    home: String,
}

impl WorkspaceFileSystem {
    pub fn new(
        paths: Arc<WorkspacePaths>,
        entries: Arc<WorkspaceEntries>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            paths,
            entries,
            home: home.into(),
        }
    }

    pub async fn get_metadata(
        &self,
        input: &FilesystemGetMetadataInput,
    ) -> FilesystemGetMetadataResult {
        let mut seen = std::collections::HashSet::new();
        let mut unique = Vec::new();
        for path in &input.paths {
            if seen.insert(path.as_str().to_owned()) {
                unique.push(path.as_str().to_owned());
            }
        }
        let home = self.home.clone();
        let results: Vec<Option<FilesystemEntryMetadata>> = stream::iter(unique.clone())
            .map(|path| {
                let home = home.clone();
                async move { metadata_for_path(&path, &home).await }
            })
            .buffered(8)
            .collect()
            .await;
        let by_path: std::collections::HashMap<_, _> = unique.into_iter().zip(results).collect();
        FilesystemGetMetadataResult {
            entries: input
                .paths
                .iter()
                .map(|path| by_path.get(path.as_str()).cloned().flatten())
                .collect(),
        }
    }

    pub async fn read_file(
        &self,
        input: &ProjectReadFileInput,
    ) -> Result<ProjectReadFileResult, WorkspaceFileReadError> {
        let target = self.resolve_read_target(input).await?;
        let mut file =
            open_read_nonblock(Path::new(&target.real_target_path)).map_err(|cause| {
                operation_error(
                    input,
                    &target.real_target_path,
                    &target.real_target_path,
                    ProjectFileOperation::Open,
                    cause,
                )
            })?;
        let stat = file.metadata().await.map_err(|cause| {
            operation_error(
                input,
                &target.real_target_path,
                &target.real_target_path,
                ProjectFileOperation::Stat,
                cause,
            )
        })?;
        if !stat.is_file() {
            return Err(WorkspacePathNotFileError {
                workspace_root: input.cwd.as_str().to_owned(),
                relative_path: input.relative_path.as_str().to_owned(),
                resolved_path: target.real_target_path,
            }
            .into());
        }
        let byte_length = stat.len();
        let bytes_to_read = byte_length.min(PROJECT_READ_FILE_MAX_BYTES) as usize;
        let mut buffer = vec![0u8; bytes_to_read];
        let mut read_total = 0;
        while read_total < bytes_to_read {
            let n = file
                .read(&mut buffer[read_total..])
                .await
                .map_err(|cause| {
                    operation_error(
                        input,
                        &target.real_target_path,
                        &target.real_target_path,
                        ProjectFileOperation::Read,
                        cause,
                    )
                })?;
            if n == 0 {
                break;
            }
            read_total += n;
        }
        let file_bytes = &buffer[..read_total];
        if file_bytes.contains(&0) {
            return Err(WorkspaceBinaryFileError {
                workspace_root: input.cwd.as_str().to_owned(),
                relative_path: input.relative_path.as_str().to_owned(),
                resolved_path: target.real_target_path,
            }
            .into());
        }
        Ok(ProjectReadFileResult {
            relative_path: TrimmedNonEmptyString::new(&target.relative_path)
                .expect("read target path is non-empty"),
            contents: String::from_utf8_lossy(file_bytes).into_owned(),
            byte_length,
            truncated: byte_length > PROJECT_READ_FILE_MAX_BYTES,
        })
    }

    pub async fn write_file(
        &self,
        input: &ProjectWriteFileInput,
    ) -> Result<ProjectWriteFileResult, WorkspaceFileWriteError> {
        let target = self
            .paths
            .resolve_relative_path_within_root(input.cwd.as_str(), input.relative_path.as_str())
            .await?;
        let parent = node_dirname(&target.absolute_path);
        if let Err(cause) = tokio::fs::create_dir_all(&parent).await {
            return Err(write_operation_error(
                input,
                &target.absolute_path,
                &parent,
                ProjectFileOperation::MakeDirectory,
                cause,
            )
            .into());
        }
        if let Err(cause) = tokio::fs::write(&target.absolute_path, input.contents.as_bytes()).await
        {
            return Err(write_operation_error(
                input,
                &target.absolute_path,
                &target.absolute_path,
                ProjectFileOperation::WriteFile,
                cause,
            )
            .into());
        }
        self.entries.refresh(input.cwd.as_str()).await;
        Ok(ProjectWriteFileResult {
            relative_path: TrimmedNonEmptyString::new(&target.relative_path)
                .expect("written path is non-empty"),
        })
    }

    async fn resolve_read_target(
        &self,
        input: &ProjectReadFileInput,
    ) -> Result<ReadTarget, WorkspaceFileReadError> {
        let requested_path = js_trim(input.relative_path.as_str()).to_owned();
        if node_is_absolute(&requested_path) {
            let real_target_path = canonicalize(&requested_path).await.map_err(|cause| {
                operation_error(
                    input,
                    &requested_path,
                    &requested_path,
                    ProjectFileOperation::RealpathTarget,
                    cause,
                )
            })?;
            return Ok(ReadTarget {
                relative_path: requested_path,
                real_target_path,
            });
        }

        let target = self
            .paths
            .resolve_relative_path_within_root(input.cwd.as_str(), input.relative_path.as_str())
            .await?;
        let real_workspace_root = canonicalize(input.cwd.as_str()).await.map_err(|cause| {
            operation_error(
                input,
                &target.absolute_path,
                input.cwd.as_str(),
                ProjectFileOperation::RealpathWorkspaceRoot,
                cause,
            )
        })?;
        let real_target_path = canonicalize(&target.absolute_path).await.map_err(|cause| {
            operation_error(
                input,
                &target.absolute_path,
                &target.absolute_path,
                ProjectFileOperation::RealpathTarget,
                cause,
            )
        })?;
        let relative_real_path = node_relative(&real_workspace_root, &real_target_path);
        if escapes_workspace_root(&relative_real_path) {
            return Err(WorkspaceFilePathEscapeError {
                workspace_root: input.cwd.as_str().to_owned(),
                relative_path: input.relative_path.as_str().to_owned(),
                resolved_workspace_root: real_workspace_root,
                resolved_path: real_target_path,
            }
            .into());
        }
        Ok(ReadTarget {
            relative_path: target.relative_path,
            real_target_path,
        })
    }
}

fn operation_error(
    input: &ProjectReadFileInput,
    resolved_path: &str,
    operation_path: &str,
    operation: ProjectFileOperation,
    cause: io::Error,
) -> WorkspaceFileSystemOperationError {
    WorkspaceFileSystemOperationError {
        workspace_root: input.cwd.as_str().to_owned(),
        relative_path: input.relative_path.as_str().to_owned(),
        resolved_path: resolved_path.to_owned(),
        operation_path: operation_path.to_owned(),
        operation,
        cause,
    }
}

fn write_operation_error(
    input: &ProjectWriteFileInput,
    resolved_path: &str,
    operation_path: &str,
    operation: ProjectFileOperation,
    cause: io::Error,
) -> WorkspaceFileSystemOperationError {
    WorkspaceFileSystemOperationError {
        workspace_root: input.cwd.as_str().to_owned(),
        relative_path: input.relative_path.as_str().to_owned(),
        resolved_path: resolved_path.to_owned(),
        operation_path: operation_path.to_owned(),
        operation,
        cause,
    }
}

async fn canonicalize(path: &str) -> io::Result<String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| path.to_string_lossy().into_owned())
}

async fn metadata_for_path(requested_path: &str, home: &str) -> Option<FilesystemEntryMetadata> {
    let expanded = expand_home_path(requested_path, home);
    if !node_is_absolute(&expanded) {
        return None;
    }
    let stat = tokio::fs::metadata(&expanded).await.ok()?;
    if stat.is_dir() {
        return Some(entry_metadata(FilesystemEntryKind::Directory, None, None));
    }
    if !stat.is_file() {
        return Some(entry_metadata(FilesystemEntryKind::Other, None, None));
    }
    // Named extensions already choose the icon. Extensionless regular files
    // contribute at most 512 bytes, never the whole file.
    let mime_type = if node_extname(&expanded).is_empty() && stat.len() > 0 {
        read_header_mime(&expanded)
            .await
            .and_then(|mime| TrimmedNonEmptyString::new(mime).ok())
    } else {
        None
    };
    Some(entry_metadata(
        FilesystemEntryKind::File,
        Some(stat.len()),
        mime_type,
    ))
}

fn entry_metadata(
    kind: FilesystemEntryKind,
    byte_length: Option<u64>,
    mime_type: Option<TrimmedNonEmptyString>,
) -> FilesystemEntryMetadata {
    FilesystemEntryMetadata {
        kind,
        byte_length,
        mime_type,
    }
}

async fn read_header_mime(path: &str) -> Option<&'static str> {
    let mut file = open_read_nonblock(Path::new(path)).ok()?;
    let stat = file.metadata().await.ok()?;
    if !stat.is_file() {
        return None;
    }
    let mut buffer = [0u8; HEADER_BYTES];
    let mut read_total = 0;
    while read_total < buffer.len() {
        match file.read(&mut buffer[read_total..]).await {
            Ok(0) => break,
            Ok(n) => read_total += n,
            Err(_) => return None,
        }
    }
    file_header_mime_type(&buffer[..read_total])
}

fn open_read_nonblock(path: &Path) -> io::Result<tokio::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    Ok(tokio::fs::File::from_std(options.open(path)?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        WorkspaceBinaryFileError, WorkspaceFilePathEscapeError, WorkspaceFileReadError,
        WorkspaceFileSystem, WorkspaceFileWriteError, WorkspacePathNotFileError,
    };
    use crate::process::ProcessRunner;
    use crate::vcs_process::VcsProcess;
    use crate::workspace::workspace_entries::WorkspaceEntries;
    use crate::workspace::workspace_paths::WorkspacePaths;
    use crate::workspace::workspace_search_index::{
        FileFinderCreateOptions, FileFinderEngine, FileFinderFactory, FinderResult,
        WorkspaceSearchIndexMap,
    };
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::filesystem::{FilesystemEntryKind, FilesystemGetMetadataInput};
    use turin_contracts::project::{
        ProjectFileOperation, ProjectReadFileInput, ProjectWriteFileInput,
    };

    struct UnusedFactory;

    impl FileFinderFactory for UnusedFactory {
        fn create(
            &self,
            _: &FileFinderCreateOptions,
        ) -> Result<FinderResult<Arc<dyn FileFinderEngine>>, String> {
            panic!("search index engine is not linked");
        }
    }

    fn filesystem(home: &str) -> WorkspaceFileSystem {
        let paths = Arc::new(WorkspacePaths::new(home));
        let indexes = Arc::new(WorkspaceSearchIndexMap::new(Arc::new(UnusedFactory)));
        let entries = Arc::new(WorkspaceEntries::new(
            Arc::clone(&paths),
            indexes,
            Arc::new(VcsProcess::new(ProcessRunner)),
            home,
        ));
        WorkspaceFileSystem::new(paths, entries, home)
    }

    fn ne(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn write_text(cwd: &std::path::Path, relative: &str, contents: &str) {
        let path = cwd.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[tokio::test]
    async fn expands_home_relative_paths() {
        let home = tempfile::tempdir().unwrap();
        write_text(home.path(), "file", "hello");
        let service = filesystem(&home.path().to_string_lossy());
        let result = service
            .get_metadata(&FilesystemGetMetadataInput {
                paths: vec![ne("~/file")],
            })
            .await;
        assert_eq!(result.entries.len(), 1);
        let entry = result.entries[0].as_ref().unwrap();
        assert_eq!(entry.kind, FilesystemEntryKind::File);
        assert_eq!(entry.byte_length, Some(5));
        assert!(entry.mime_type.is_none());
    }

    #[tokio::test]
    async fn reports_kinds_for_extensionless_files_dotfiles_and_dotted_directories() {
        let cwd = tempfile::tempdir().unwrap();
        write_text(cwd.path(), "custom-name", "hello");
        write_text(cwd.path(), ".npmrc", "registry=test");
        std::fs::create_dir(cwd.path().join("folder.ts")).unwrap();
        let file = cwd.path().join("custom-name");
        let service = filesystem("/tmp");
        let result = service
            .get_metadata(&FilesystemGetMetadataInput {
                paths: vec![
                    ne(&file.to_string_lossy()),
                    ne(&cwd.path().join(".npmrc").to_string_lossy()),
                    ne(&cwd.path().join("folder.ts").to_string_lossy()),
                    ne(&file.to_string_lossy()),
                    ne(&cwd.path().join("missing").to_string_lossy()),
                    ne("relative-path"),
                ],
            })
            .await;
        assert_eq!(result.entries.len(), 6);
        assert_eq!(
            result.entries[0].as_ref().unwrap().kind,
            FilesystemEntryKind::File
        );
        assert_eq!(result.entries[0].as_ref().unwrap().byte_length, Some(5));
        assert!(result.entries[0].as_ref().unwrap().mime_type.is_none());
        assert_eq!(
            result.entries[1].as_ref().unwrap().kind,
            FilesystemEntryKind::File
        );
        assert_eq!(result.entries[1].as_ref().unwrap().byte_length, Some(13));
        assert_eq!(
            result.entries[2].as_ref().unwrap().kind,
            FilesystemEntryKind::Directory
        );
        assert_eq!(result.entries[3], result.entries[0]);
        assert!(result.entries[4].is_none());
        assert!(result.entries[5].is_none());
    }

    #[tokio::test]
    async fn detects_an_extensionless_script_and_image_without_returning_contents() {
        let cwd = tempfile::tempdir().unwrap();
        let script = "#!/usr/bin/env python3\nprint('hello')\n";
        write_text(cwd.path(), "run", script);
        let image = cwd.path().join("image");
        std::fs::write(&image, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        let service = filesystem("/tmp");
        let result = service
            .get_metadata(&FilesystemGetMetadataInput {
                paths: vec![
                    ne(&cwd.path().join("run").to_string_lossy()),
                    ne(&image.to_string_lossy()),
                ],
            })
            .await;
        let script_entry = result.entries[0].as_ref().unwrap();
        assert_eq!(script_entry.kind, FilesystemEntryKind::File);
        assert_eq!(script_entry.byte_length, Some(script.len() as u64));
        assert_eq!(
            script_entry.mime_type.as_ref().unwrap().as_str(),
            "text/x-python"
        );
        let image_entry = result.entries[1].as_ref().unwrap();
        assert_eq!(image_entry.byte_length, Some(8));
        assert_eq!(
            image_entry.mime_type.as_ref().unwrap().as_str(),
            "image/png"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn follows_file_and_directory_symlinks() {
        let cwd = tempfile::tempdir().unwrap();
        write_text(cwd.path(), "target", "text");
        std::fs::create_dir(cwd.path().join("directory")).unwrap();
        let file_link = cwd.path().join("file-link");
        let directory_link = cwd.path().join("directory-link");
        std::os::unix::fs::symlink(cwd.path().join("target"), &file_link).unwrap();
        std::os::unix::fs::symlink(cwd.path().join("directory"), &directory_link).unwrap();
        let service = filesystem("/tmp");
        let result = service
            .get_metadata(&FilesystemGetMetadataInput {
                paths: vec![
                    ne(&file_link.to_string_lossy()),
                    ne(&directory_link.to_string_lossy()),
                ],
            })
            .await;
        assert_eq!(
            result.entries[0].as_ref().unwrap().kind,
            FilesystemEntryKind::File
        );
        assert_eq!(result.entries[0].as_ref().unwrap().byte_length, Some(4));
        assert_eq!(
            result.entries[1].as_ref().unwrap().kind,
            FilesystemEntryKind::Directory
        );
    }

    #[tokio::test]
    async fn reads_utf8_files_relative_to_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        write_text(cwd.path(), "src/index.ts", "export const answer = 42;\n");
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let result = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("src/index.ts"),
            })
            .await
            .unwrap();
        assert_eq!(result.relative_path.as_str(), "src/index.ts");
        assert_eq!(result.contents, "export const answer = 42;\n");
        assert_eq!(result.byte_length, 26);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn reads_host_files_outside_the_workspace_root_by_absolute_path() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_text(outside.path(), "cleanup-report.md", "# Report\n");
        let absolute = outside.path().join("cleanup-report.md");
        let absolute_path = absolute.to_string_lossy().into_owned();
        let service = filesystem("/tmp");
        let result = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd.path().to_string_lossy()),
                relative_path: ne(&absolute_path),
            })
            .await
            .unwrap();
        assert_eq!(result.relative_path.as_str(), absolute_path);
        assert_eq!(result.contents, "# Report\n");
        assert_eq!(result.byte_length, 9);
        assert!(!result.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_fifo_without_blocking_on_open() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let fifo = outside.path().join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        let service = filesystem("/tmp");
        let fifo_path = fifo.to_string_lossy().into_owned();
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd.path().to_string_lossy()),
                relative_path: ne(&fifo_path),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceFileReadError::NotFile(WorkspacePathNotFileError { .. })
        ));
        let metadata = service
            .get_metadata(&FilesystemGetMetadataInput {
                paths: vec![ne(&fifo_path)],
            })
            .await;
        assert_eq!(
            metadata.entries[0].as_ref().unwrap().kind,
            FilesystemEntryKind::Other
        );
    }

    #[tokio::test]
    async fn rejects_reads_outside_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let service = filesystem("/tmp");
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd.path().to_string_lossy()),
                relative_path: ne("../escape.md"),
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Workspace file path must be relative to the project root: ../escape.md")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlinks_that_resolve_outside_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_text(outside.path(), "secret.txt", "outside\n");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            cwd.path().join("linked-secret.txt"),
        )
        .unwrap();
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("linked-secret.txt"),
            })
            .await
            .unwrap_err();
        let WorkspaceFileReadError::Escape(error) = error else {
            panic!("expected escape");
        };
        let resolved_root = tokio::fs::canonicalize(&cwd_path).await.unwrap();
        let resolved_path = tokio::fs::canonicalize(outside.path().join("secret.txt"))
            .await
            .unwrap();
        assert_eq!(error.workspace_root, cwd_path);
        assert_eq!(error.relative_path, "linked-secret.txt");
        assert_eq!(
            error.resolved_workspace_root,
            resolved_root.to_string_lossy()
        );
        assert_eq!(error.resolved_path, resolved_path.to_string_lossy());
        let _ = WorkspaceFilePathEscapeError {
            workspace_root: error.workspace_root,
            relative_path: error.relative_path,
            resolved_workspace_root: error.resolved_workspace_root,
            resolved_path: error.resolved_path,
        };
    }

    #[tokio::test]
    async fn rejects_directories_without_manufacturing_an_io_cause() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join("src")).unwrap();
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("src"),
            })
            .await
            .unwrap_err();
        let WorkspaceFileReadError::NotFile(error) = error else {
            panic!("expected not-file, got {error}");
        };
        let resolved = tokio::fs::canonicalize(cwd.path().join("src"))
            .await
            .unwrap();
        assert_eq!(error.workspace_root, cwd_path);
        assert_eq!(error.relative_path, "src");
        assert_eq!(error.resolved_path, resolved.to_string_lossy());
    }

    #[tokio::test]
    async fn rejects_binary_files_without_leaking_their_contents() {
        let cwd = tempfile::tempdir().unwrap();
        let absolute = cwd.path().join("asset.bin");
        std::fs::write(&absolute, [0x61, 0, 0x62]).unwrap();
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("asset.bin"),
            })
            .await
            .unwrap_err();
        let WorkspaceFileReadError::Binary(error) = error else {
            panic!("expected binary, got {error}");
        };
        let resolved = tokio::fs::canonicalize(&absolute).await.unwrap();
        assert_eq!(error.workspace_root, cwd_path);
        assert_eq!(error.relative_path, "asset.bin");
        assert_eq!(error.resolved_path, resolved.to_string_lossy());
        assert!(!error.to_string().contains("contents"));
        let _ = WorkspaceBinaryFileError {
            workspace_root: String::new(),
            relative_path: String::new(),
            resolved_path: String::new(),
        };
    }

    #[tokio::test]
    async fn preserves_the_real_cause_and_path_for_io_failures() {
        let cwd = tempfile::tempdir().unwrap();
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let resolved = cwd
            .path()
            .join("missing.txt")
            .to_string_lossy()
            .into_owned();
        let error = service
            .read_file(&ProjectReadFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("missing.txt"),
            })
            .await
            .unwrap_err();
        let WorkspaceFileReadError::Operation(error) = error else {
            panic!("expected operation error, got {error}");
        };
        assert_eq!(error.workspace_root, cwd_path);
        assert_eq!(error.relative_path, "missing.txt");
        assert_eq!(error.resolved_path, resolved);
        assert_eq!(error.operation_path, resolved);
        assert_eq!(error.operation, ProjectFileOperation::RealpathTarget);
        assert_eq!(error.cause.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn writes_files_relative_to_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let service = filesystem("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let result = service
            .write_file(&ProjectWriteFileInput {
                cwd: ne(&cwd_path),
                relative_path: ne("plans/effect-rpc.md"),
                contents: "# Plan\n".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(result.relative_path.as_str(), "plans/effect-rpc.md");
        let saved = std::fs::read_to_string(cwd.path().join("plans/effect-rpc.md")).unwrap();
        assert_eq!(saved, "# Plan\n");
    }

    #[tokio::test]
    async fn rejects_writes_by_absolute_path() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let absolute = outside.path().join("cleanup-report.md");
        let service = filesystem("/tmp");
        let error = service
            .write_file(&ProjectWriteFileInput {
                cwd: ne(&cwd.path().to_string_lossy()),
                relative_path: ne(&absolute.to_string_lossy()),
                contents: "# Edited\n".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, WorkspaceFileWriteError::OutsideRoot(_)));
    }

    #[tokio::test]
    async fn rejects_writes_outside_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let service = filesystem("/tmp");
        let error = service
            .write_file(&ProjectWriteFileInput {
                cwd: ne(&cwd.path().to_string_lossy()),
                relative_path: ne("../escape.md"),
                contents: "# nope\n".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Workspace file path must be relative to the project root: ../escape.md")
        );
        let escaped = cwd.path().join("..").join("escape.md");
        assert!(tokio::fs::metadata(&escaped).await.is_err());
    }
}
