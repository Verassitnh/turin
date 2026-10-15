//! Port of upstream/apps/server/src/workspace/WorkspacePaths.ts
//!
//! Path joining follows Node's posix `path` module, which is what the upstream
//! server uses on macOS and Linux. Lexical only: symlinks stay unresolved
//! until a caller asks for `realpath`.

use std::fmt;

use thiserror::Error;
use tokio::fs;
use turin_contracts::base_schemas::js_trim;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Workspace root does not exist: {normalized_workspace_root}")]
pub struct WorkspaceRootNotExistsError {
    pub workspace_root: String,
    pub normalized_workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Failed to create workspace root: {normalized_workspace_root}")]
pub struct WorkspaceRootCreateFailedError {
    pub workspace_root: String,
    pub normalized_workspace_root: String,
    pub cause: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceRootStatPhase {
    ValidateExisting,
    VerifyCreated,
}

impl WorkspaceRootStatPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ValidateExisting => "validate-existing",
            Self::VerifyCreated => "verify-created",
        }
    }
}

impl fmt::Display for WorkspaceRootStatPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Failed to stat workspace root '{normalized_workspace_root}' during '{phase}'.")]
pub struct WorkspaceRootStatFailedError {
    pub workspace_root: String,
    pub normalized_workspace_root: String,
    pub phase: WorkspaceRootStatPhase,
    pub cause: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Workspace root is not a directory: {normalized_workspace_root}")]
pub struct WorkspaceRootNotDirectoryError {
    pub workspace_root: String,
    pub normalized_workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Workspace file path must be relative to the project root: {relative_path}")]
pub struct WorkspacePathOutsideRootError {
    pub workspace_root: String,
    pub relative_path: String,
}

#[derive(Debug, Error)]
pub enum NormalizeWorkspaceRootError {
    #[error(transparent)]
    NotExists(#[from] WorkspaceRootNotExistsError),
    #[error(transparent)]
    CreateFailed(#[from] WorkspaceRootCreateFailedError),
    #[error(transparent)]
    StatFailed(#[from] WorkspaceRootStatFailedError),
    #[error(transparent)]
    NotDirectory(#[from] WorkspaceRootNotDirectoryError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspacePath {
    pub absolute_path: String,
    pub relative_path: String,
}

enum RootKind {
    Directory,
    Other,
}

/// Normalize workspace roots and resolve root-relative paths.
pub struct WorkspacePaths {
    home: String,
}

impl WorkspacePaths {
    pub fn new(home: impl Into<String>) -> Self {
        Self { home: home.into() }
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    /// Normalize a user-provided workspace root and verify it exists as a directory.
    pub async fn normalize_workspace_root(
        &self,
        workspace_root: &str,
        create_if_missing: bool,
    ) -> Result<String, NormalizeWorkspaceRootError> {
        let expanded = expand_home_path(js_trim(workspace_root), &self.home);
        let normalized = node_resolve(&[&expanded]);
        let mut kind = stat_workspace_root(
            workspace_root,
            &normalized,
            WorkspaceRootStatPhase::ValidateExisting,
        )
        .await?;
        if kind.is_none() && create_if_missing {
            if let Err(cause) = fs::create_dir_all(&normalized).await {
                return Err(WorkspaceRootCreateFailedError {
                    workspace_root: workspace_root.to_owned(),
                    normalized_workspace_root: normalized,
                    cause: cause.to_string(),
                }
                .into());
            }
            kind = stat_workspace_root(
                workspace_root,
                &normalized,
                WorkspaceRootStatPhase::VerifyCreated,
            )
            .await?;
        }
        match kind {
            None => Err(WorkspaceRootNotExistsError {
                workspace_root: workspace_root.to_owned(),
                normalized_workspace_root: normalized,
            }
            .into()),
            Some(RootKind::Other) => Err(WorkspaceRootNotDirectoryError {
                workspace_root: workspace_root.to_owned(),
                normalized_workspace_root: normalized,
            }
            .into()),
            Some(RootKind::Directory) => Ok(normalized),
        }
    }

    /// Resolve a relative path within a validated workspace root.
    ///
    /// Rejects absolute paths and traversal attempts outside the workspace root.
    pub async fn resolve_relative_path_within_root(
        &self,
        workspace_root: &str,
        relative_path: &str,
    ) -> Result<ResolvedWorkspacePath, WorkspacePathOutsideRootError> {
        let normalized_input = js_trim(relative_path);
        if node_is_absolute(normalized_input) {
            return Err(outside(workspace_root, relative_path));
        }
        let absolute_path = node_resolve(&[workspace_root, normalized_input]);
        let relative_to_root =
            to_posix_relative_path(&node_relative(workspace_root, &absolute_path));
        if relative_to_root.is_empty()
            || relative_to_root == "."
            || relative_to_root.starts_with("../")
            || relative_to_root == ".."
            || node_is_absolute(&relative_to_root)
        {
            return Err(outside(workspace_root, relative_path));
        }
        Ok(ResolvedWorkspacePath {
            absolute_path,
            relative_path: relative_to_root,
        })
    }
}

fn outside(workspace_root: &str, relative_path: &str) -> WorkspacePathOutsideRootError {
    WorkspacePathOutsideRootError {
        workspace_root: workspace_root.to_owned(),
        relative_path: relative_path.to_owned(),
    }
}

async fn stat_workspace_root(
    workspace_root: &str,
    normalized: &str,
    phase: WorkspaceRootStatPhase,
) -> Result<Option<RootKind>, WorkspaceRootStatFailedError> {
    match fs::metadata(normalized).await {
        Ok(metadata) if metadata.is_dir() => Ok(Some(RootKind::Directory)),
        Ok(_) => Ok(Some(RootKind::Other)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(WorkspaceRootStatFailedError {
            workspace_root: workspace_root.to_owned(),
            normalized_workspace_root: normalized.to_owned(),
            phase,
            cause: error.to_string(),
        }),
    }
}

/// `~/…` and `~\…` expand to `home`. `~user` is left alone. Empty stays empty.
pub(crate) fn expand_home_path(value: &str, home: &str) -> String {
    if value.is_empty() {
        return value.to_owned();
    }
    if value == "~" {
        return home.to_owned();
    }
    if value.starts_with("~/") || value.starts_with("~\\") {
        return node_join(&[home, &value[2..]]);
    }
    value.to_owned()
}

pub(crate) fn is_windows_absolute_path(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value)
}

fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

fn is_windows_drive_path(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() || chars.next() != Some(':') {
        return false;
    }
    matches!(chars.next(), Some('/' | '\\') | None)
}

pub(crate) fn is_explicit_relative_path(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
}

/// Node `path.isAbsolute` on this host. Windows drive paths are absolute only
/// when the process itself is Windows; upstream uses the platform Path service.
pub(crate) fn node_is_absolute(value: &str) -> bool {
    if value.starts_with('/') {
        return true;
    }
    cfg!(windows) && (is_windows_absolute_path(value) || value.starts_with('\\'))
}

pub(crate) fn to_posix_relative_path(input: &str) -> String {
    input.replace('\\', "/")
}

/// Node `path.posix.resolve`: right to left until an absolute segment, then
/// normalize `.` and `..`. Does not touch the filesystem.
pub(crate) fn node_resolve(segments: &[&str]) -> String {
    let mut owned: Vec<String> = Vec::new();
    for segment in segments.iter().rev() {
        if segment.is_empty() {
            continue;
        }
        let absolute = segment_is_absolute(segment);
        owned.push((*segment).to_owned());
        if absolute {
            break;
        }
    }
    let needs_cwd = match owned.last() {
        Some(segment) => !segment_is_absolute(segment),
        None => true,
    };
    if needs_cwd {
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_owned());
        owned.push(cwd);
    }
    owned.reverse();
    normalize_posix(&owned.join("/"))
}

fn segment_is_absolute(segment: &str) -> bool {
    segment.starts_with('/')
        || (cfg!(windows) && (is_windows_absolute_path(segment) || segment.starts_with('\\')))
}

/// Node `path.posix.join`: concatenate, then normalize. An absolute segment
/// does not reset the result (that is `resolve`'s job).
pub(crate) fn node_join(parts: &[&str]) -> String {
    let filtered: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    if filtered.is_empty() {
        return ".".to_owned();
    }
    normalize_posix(&filtered.join("/"))
}

pub(crate) fn node_relative(from: &str, to: &str) -> String {
    let from = node_resolve(&[from]);
    let to = node_resolve(&[to]);
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    let to_parts: Vec<&str> = to
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut out = Vec::new();
    out.extend(std::iter::repeat_n("..", from_parts.len() - common));
    out.extend(to_parts[common..].iter().copied());
    out.join("/")
}

pub(crate) fn node_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return if path.starts_with('/') || path.starts_with('\\') {
            "/".to_owned()
        } else {
            ".".to_owned()
        };
    }
    match trimmed.rfind(['/', '\\']) {
        Some(0) => "/".to_owned(),
        Some(index) => trimmed[..index].to_owned(),
        None => ".".to_owned(),
    }
}

pub(crate) fn node_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind(['/', '\\']) {
        Some(index) => trimmed[index + 1..].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// Node `path.extname`: the last `.segment`, or empty when the only dot is
/// the first character of the basename (`.npmrc`).
pub(crate) fn node_extname(path: &str) -> String {
    let name = node_basename(path);
    match name.rfind('.') {
        Some(index) if index > 0 => name[index..].to_owned(),
        _ => String::new(),
    }
}

fn normalize_posix(path: &str) -> String {
    let absolute = path.starts_with('/') || (cfg!(windows) && path.starts_with('\\'));
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() && !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if absolute {
        format!("/{}", parts.join("/"))
    } else if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

pub(crate) fn escapes_workspace_root(relative: &str) -> bool {
    relative == ".."
        || relative.starts_with(&format!("..{}", std::path::MAIN_SEPARATOR))
        || relative.starts_with("../")
        || node_is_absolute(relative)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        NormalizeWorkspaceRootError, WorkspacePaths, WorkspaceRootNotDirectoryError,
        WorkspaceRootNotExistsError,
    };

    fn paths() -> WorkspacePaths {
        WorkspacePaths::new("/tmp")
    }

    #[tokio::test]
    async fn resolves_an_existing_directory() {
        let cwd = tempfile::tempdir().unwrap();
        let cwd = cwd.path().to_string_lossy().into_owned();
        let resolved = paths().normalize_workspace_root(&cwd, false).await.unwrap();
        assert_eq!(resolved, cwd);
    }

    #[tokio::test]
    async fn rejects_missing_directories() {
        let cwd = tempfile::tempdir().unwrap();
        let missing = cwd.path().join("missing");
        let error = paths()
            .normalize_workspace_root(&missing.to_string_lossy(), false)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("Workspace root does not exist:"),
            "{error}"
        );
        assert!(matches!(
            error,
            NormalizeWorkspaceRootError::NotExists(WorkspaceRootNotExistsError { .. })
        ));
    }

    #[tokio::test]
    async fn creates_missing_directories_when_create_if_missing_is_enabled() {
        let cwd = tempfile::tempdir().unwrap();
        let missing = cwd.path().join("nested").join("new-project");
        let missing_path = missing.to_string_lossy().into_owned();
        let resolved = paths()
            .normalize_workspace_root(&missing_path, true)
            .await
            .unwrap();
        let stat = tokio::fs::metadata(&resolved).await.unwrap();
        assert_eq!(resolved, missing_path);
        assert!(stat.is_dir());
    }

    #[tokio::test]
    async fn rejects_file_paths() {
        let cwd = tempfile::tempdir().unwrap();
        let file_path = cwd.path().join("README.md");
        std::fs::write(&file_path, "# hi\n").unwrap();
        let error = paths()
            .normalize_workspace_root(&file_path.to_string_lossy(), false)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Workspace root is not a directory:"),
            "{error}"
        );
        assert!(matches!(
            error,
            NormalizeWorkspaceRootError::NotDirectory(WorkspaceRootNotDirectoryError { .. })
        ));
    }

    #[tokio::test]
    async fn resolves_relative_paths_inside_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let resolved = paths()
            .resolve_relative_path_within_root(&cwd_path, "plans/effect-rpc.md")
            .await
            .unwrap();
        assert_eq!(
            resolved.absolute_path,
            Path::new(&cwd_path)
                .join("plans/effect-rpc.md")
                .to_string_lossy()
        );
        assert_eq!(resolved.relative_path, "plans/effect-rpc.md");
    }

    #[tokio::test]
    async fn rejects_paths_that_escape_the_workspace_root() {
        let cwd = tempfile::tempdir().unwrap();
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let error = paths()
            .resolve_relative_path_within_root(&cwd_path, "../escape.md")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Workspace file path must be relative to the project root: ../escape.md"),
            "{error}"
        );
    }

    // Upstream's "preserves non-NotFound stat failures" cases replace the
    // Effect FileSystem service with a mock. This crate has one filesystem,
    // so those two tests are not ported.
}
