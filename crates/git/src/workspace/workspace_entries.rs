//! Port of upstream/apps/server/src/workspace/WorkspaceEntries.ts

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use std::sync::LazyLock;
use thiserror::Error;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::filesystem::{
    FilesystemBrowseEntry, FilesystemBrowseInput, FilesystemBrowseResult,
};
use turin_contracts::project::{
    ProjectEntry, ProjectEntryKind, ProjectListEntriesInput, ProjectListEntriesResult,
    ProjectSearchContentsInput, ProjectSearchContentsResult, ProjectSearchEntriesInput,
    ProjectSearchEntriesResult,
};

use crate::vcs_process::{VcsProcess, VcsProcessInput};

use super::workspace_paths::{
    NormalizeWorkspaceRootError, WorkspacePaths, WorkspaceRootCreateFailedError,
    WorkspaceRootNotDirectoryError, WorkspaceRootNotExistsError, WorkspaceRootStatFailedError,
    escapes_workspace_root, expand_home_path, is_explicit_relative_path, is_windows_absolute_path,
    node_basename, node_dirname, node_join, node_relative, node_resolve,
};
use super::workspace_search_index::{
    ContentSearchInput, WORKSPACE_SEARCH_INDEX_VARIANTS, WorkspaceSearchIndexCreateFailed,
    WorkspaceSearchIndexMap, WorkspaceSearchIndexOpenError, WorkspaceSearchIndexScanTimedOut,
    WorkspaceSearchIndexSearchFailed, WorkspaceSearchIndexVariant, workspace_search_index_key,
};

const CHECK_IGNORE_CHUNK: usize = 1000;

#[derive(Debug, Clone, Error)]
pub struct WorkspaceEntriesWindowsPathUnsupportedError {
    pub cwd: Option<String>,
    pub partial_path: String,
    pub platform: String,
}

impl fmt::Display for WorkspaceEntriesWindowsPathUnsupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cwd = self
            .cwd
            .as_ref()
            .map(|cwd| format!(" from '{cwd}'"))
            .unwrap_or_default();
        write!(
            f,
            "Windows-style workspace path '{}' is not supported on '{}'{}.",
            self.partial_path, self.platform, cwd
        )
    }
}

#[derive(Debug, Clone, Error)]
#[error("A current project is required to browse relative workspace path '{partial_path}'.")]
pub struct WorkspaceEntriesCurrentProjectRequiredError {
    pub partial_path: String,
}

#[derive(Debug, Clone, Error)]
pub struct WorkspaceEntriesReadDirectoryError {
    pub cwd: Option<String>,
    pub partial_path: String,
    pub parent_path: String,
    pub cause: String,
}

impl fmt::Display for WorkspaceEntriesReadDirectoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cwd = self
            .cwd
            .as_ref()
            .map(|cwd| format!(" from '{cwd}'"))
            .unwrap_or_default();
        write!(
            f,
            "Failed to read workspace directory '{}' while browsing '{}'{}.",
            self.parent_path, self.partial_path, cwd
        )
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceEntriesBrowseError {
    #[error(transparent)]
    WindowsPathUnsupported(#[from] WorkspaceEntriesWindowsPathUnsupportedError),
    #[error(transparent)]
    CurrentProjectRequired(#[from] WorkspaceEntriesCurrentProjectRequiredError),
    #[error(transparent)]
    ReadDirectory(#[from] WorkspaceEntriesReadDirectoryError),
}

#[derive(Debug, Error)]
pub enum WorkspaceEntriesError {
    #[error(transparent)]
    ReadDirectory(#[from] WorkspaceEntriesReadDirectoryError),
    #[error(transparent)]
    RootNotExists(#[from] WorkspaceRootNotExistsError),
    #[error(transparent)]
    RootCreateFailed(#[from] WorkspaceRootCreateFailedError),
    #[error(transparent)]
    RootStatFailed(#[from] WorkspaceRootStatFailedError),
    #[error(transparent)]
    RootNotDirectory(#[from] WorkspaceRootNotDirectoryError),
    #[error(transparent)]
    SearchIndexCreateFailed(#[from] WorkspaceSearchIndexCreateFailed),
    #[error(transparent)]
    SearchIndexScanTimedOut(#[from] WorkspaceSearchIndexScanTimedOut),
    #[error(transparent)]
    SearchIndexSearchFailed(#[from] WorkspaceSearchIndexSearchFailed),
}

impl From<NormalizeWorkspaceRootError> for WorkspaceEntriesError {
    fn from(error: NormalizeWorkspaceRootError) -> Self {
        match error {
            NormalizeWorkspaceRootError::NotExists(error) => Self::RootNotExists(error),
            NormalizeWorkspaceRootError::CreateFailed(error) => Self::RootCreateFailed(error),
            NormalizeWorkspaceRootError::StatFailed(error) => Self::RootStatFailed(error),
            NormalizeWorkspaceRootError::NotDirectory(error) => Self::RootNotDirectory(error),
        }
    }
}

impl From<WorkspaceSearchIndexOpenError> for WorkspaceEntriesError {
    fn from(error: WorkspaceSearchIndexOpenError) -> Self {
        match error {
            WorkspaceSearchIndexOpenError::Create(error) => Self::SearchIndexCreateFailed(error),
            WorkspaceSearchIndexOpenError::ScanTimedOut(error) => {
                Self::SearchIndexScanTimedOut(error)
            }
        }
    }
}

struct DirChild {
    name: String,
    is_directory: bool,
    is_file: bool,
}

/// Browse, list, and search workspace entries.
pub struct WorkspaceEntries {
    paths: Arc<WorkspacePaths>,
    indexes: Arc<WorkspaceSearchIndexMap>,
    vcs: Arc<VcsProcess>,
    home: String,
    platform: String,
}

impl WorkspaceEntries {
    pub fn new(
        paths: Arc<WorkspacePaths>,
        indexes: Arc<WorkspaceSearchIndexMap>,
        vcs: Arc<VcsProcess>,
        home: impl Into<String>,
    ) -> Self {
        Self {
            paths,
            indexes,
            vcs,
            home: home.into(),
            platform: host_platform().to_owned(),
        }
    }

    pub async fn browse(
        &self,
        input: &FilesystemBrowseInput,
    ) -> Result<FilesystemBrowseResult, WorkspaceEntriesBrowseError> {
        let partial_path = input.partial_path.as_str();
        let resolved = self.resolve_browse_target(input).await?;
        let ends_with_separator = partial_path.ends_with(['/', '\\']) || partial_path == "~";
        let parent_path = if ends_with_separator {
            resolved.clone()
        } else {
            node_dirname(&resolved)
        };
        let prefix = if ends_with_separator {
            String::new()
        } else {
            node_basename(&resolved)
        };
        let children = match read_dir_children(&parent_path).await {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Vec::new(),
            Err(error) => {
                return Err(WorkspaceEntriesReadDirectoryError {
                    cwd: input.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()),
                    partial_path: partial_path.to_owned(),
                    parent_path,
                    cause: error.to_string(),
                }
                .into());
            }
        };
        let show_hidden = ends_with_separator || prefix.starts_with('.');
        let lower_prefix = prefix.to_lowercase();
        let mut entries = Vec::new();
        for child in children {
            if child.is_directory
                && child.name.to_lowercase().starts_with(&lower_prefix)
                && (show_hidden || !child.name.starts_with('.'))
            {
                let Some(name) = TrimmedNonEmptyString::new(&child.name).ok() else {
                    continue;
                };
                let full_path = node_join(&[&parent_path, child.name.as_str()]);
                let Some(full_path) = TrimmedNonEmptyString::new(&full_path).ok() else {
                    continue;
                };
                entries.push(FilesystemBrowseEntry { name, full_path });
            }
        }
        entries.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
        let parent_path = TrimmedNonEmptyString::new(&parent_path).map_err(|_| {
            WorkspaceEntriesReadDirectoryError {
                cwd: input.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()),
                partial_path: partial_path.to_owned(),
                parent_path: parent_path.clone(),
                cause: "parent path is empty".to_owned(),
            }
        })?;
        Ok(FilesystemBrowseResult {
            parent_path,
            entries,
        })
    }

    pub async fn list(
        &self,
        input: &ProjectListEntriesInput,
    ) -> Result<ProjectListEntriesResult, WorkspaceEntriesError> {
        let normalized_cwd = self.normalize_workspace_root(input.cwd.as_str()).await?;
        if let Some(directory_path) = &input.directory_path {
            return self
                .list_directory(&normalized_cwd, directory_path.as_str())
                .await;
        }
        let index = self
            .indexes
            .get(&workspace_search_index_key(
                &normalized_cwd,
                WorkspaceSearchIndexVariant::Paths,
            ))
            .await?;
        index.list().await.map_err(WorkspaceEntriesError::from)
    }

    pub async fn search(
        &self,
        input: &ProjectSearchEntriesInput,
    ) -> Result<ProjectSearchEntriesResult, WorkspaceEntriesError> {
        let normalized_cwd = self.normalize_workspace_root(input.cwd.as_str()).await?;
        let normalized_query = normalize_search_query(input.query.as_str());
        let index = self
            .indexes
            .get(&workspace_search_index_key(
                &normalized_cwd,
                WorkspaceSearchIndexVariant::Paths,
            ))
            .await?;
        index
            .search(
                &normalized_query,
                input.limit.get(),
                input.kind,
                input.image_only.unwrap_or(false),
            )
            .await
            .map_err(WorkspaceEntriesError::from)
    }

    pub async fn search_contents(
        &self,
        input: &ProjectSearchContentsInput,
    ) -> Result<ProjectSearchContentsResult, WorkspaceEntriesError> {
        let normalized_cwd = self.normalize_workspace_root(input.cwd.as_str()).await?;
        let index = self
            .indexes
            .get(&workspace_search_index_key(
                &normalized_cwd,
                WorkspaceSearchIndexVariant::Content,
            ))
            .await?;
        index
            .search_contents(&ContentSearchInput {
                query: input.query.clone(),
                limit: input.limit.get(),
                case_sensitive: input.case_sensitive,
                whole_word: input.whole_word,
                use_regex: input.use_regex,
            })
            .await
            .map_err(WorkspaceEntriesError::from)
    }

    /// Infallible, matching upstream: a refresh failure is logged and the
    /// index for that variant is dropped.
    pub async fn refresh(&self, cwd: &str) {
        let normalized_cwd = match self.paths.normalize_workspace_root(cwd, false).await {
            Ok(path) => path,
            Err(_) => cwd.to_owned(),
        };
        for variant in WORKSPACE_SEARCH_INDEX_VARIANTS {
            let key = workspace_search_index_key(&normalized_cwd, variant);
            if !self.indexes.contains(&key).await {
                continue;
            }
            let index = match self.indexes.get(&key).await {
                Ok(index) => index,
                Err(cause) => {
                    tracing::warn!(
                        cwd,
                        variant = variant.as_str(),
                        %cause,
                        "Failed to refresh workspace search index"
                    );
                    self.indexes.invalidate(&key).await;
                    continue;
                }
            };
            if let Err(cause) = index.refresh().await {
                tracing::warn!(
                    cwd,
                    variant = variant.as_str(),
                    %cause,
                    "Failed to refresh workspace search index"
                );
                self.indexes.invalidate(&key).await;
            }
        }
    }

    async fn normalize_workspace_root(&self, cwd: &str) -> Result<String, WorkspaceEntriesError> {
        self.paths
            .normalize_workspace_root(cwd, false)
            .await
            .map_err(WorkspaceEntriesError::from)
    }

    async fn resolve_browse_target(
        &self,
        input: &FilesystemBrowseInput,
    ) -> Result<String, WorkspaceEntriesBrowseError> {
        let partial_path = input.partial_path.as_str();
        if self.platform != "win32" && is_windows_absolute_path(partial_path) {
            return Err(WorkspaceEntriesWindowsPathUnsupportedError {
                cwd: input.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()),
                partial_path: partial_path.to_owned(),
                platform: self.platform.clone(),
            }
            .into());
        }
        if !is_explicit_relative_path(partial_path) {
            return Ok(node_resolve(&[&expand_home_path(partial_path, &self.home)]));
        }
        let Some(cwd) = &input.cwd else {
            return Err(WorkspaceEntriesCurrentProjectRequiredError {
                partial_path: partial_path.to_owned(),
            }
            .into());
        };
        Ok(node_resolve(&[
            &expand_home_path(cwd.as_str(), &self.home),
            partial_path,
        ]))
    }

    async fn list_directory(
        &self,
        normalized_cwd: &str,
        directory_path: &str,
    ) -> Result<ProjectListEntriesResult, WorkspaceEntriesError> {
        let to_error = |cause: String| WorkspaceEntriesReadDirectoryError {
            cwd: Some(normalized_cwd.to_owned()),
            partial_path: directory_path.to_owned(),
            parent_path: node_resolve(&[normalized_cwd, directory_path]),
            cause,
        };
        let target = if directory_path.is_empty() {
            super::workspace_paths::ResolvedWorkspacePath {
                absolute_path: normalized_cwd.to_owned(),
                relative_path: String::new(),
            }
        } else {
            self.paths
                .resolve_relative_path_within_root(normalized_cwd, directory_path)
                .await
                .map_err(|error| to_error(error.to_string()))?
        };
        let entries =
            list_directory_children(normalized_cwd, &target.absolute_path, &target.relative_path)
                .await
                .map_err(|cause| to_error(cause.to_string()))?;
        let mut ignored = HashSet::new();
        let mut offset = 0;
        while offset < entries.len() {
            let end = (offset + CHECK_IGNORE_CHUNK).min(entries.len());
            let chunk = &entries[offset..end];
            offset = end;
            let stdin = format!(
                "{}\0",
                chunk
                    .iter()
                    .map(|entry| entry.path.as_str())
                    .collect::<Vec<_>>()
                    .join("\0")
            );
            // Ignore classification is optional in non-git workspaces or when git is unavailable.
            let result = match self
                .vcs
                .run(VcsProcessInput {
                    operation: "WorkspaceEntries.list".to_owned(),
                    command: "git".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        "core.fsmonitor=false".to_owned(),
                        "check-ignore".to_owned(),
                        "-z".to_owned(),
                        "--stdin".to_owned(),
                    ],
                    cwd: normalized_cwd.to_owned(),
                    spawn_cwd: None,
                    stdin: Some(stdin),
                    on_stdout_chunk: None,
                    env: None,
                    clear_env: Vec::new(),
                    allow_non_zero_exit: true,
                    timeout: Some(Duration::from_millis(10_000)),
                    max_output_bytes: Some(16 * 1024 * 1024),
                    output_mode: None,
                    append_truncation_marker: false,
                })
                .await
            {
                Ok(result) => result,
                Err(_) => break,
            };
            if result.exit_code != 0 && result.exit_code != 1 {
                break;
            }
            for ignored_path in result.stdout.split('\0') {
                ignored.insert(ignored_path.to_owned());
            }
        }
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                if ignored.contains(entry.path.as_str()) {
                    entry.ignored = Some(true);
                }
                entry
            })
            .collect();
        Ok(ProjectListEntriesResult {
            entries,
            truncated: false,
        })
    }
}

async fn list_directory_children(
    normalized_cwd: &str,
    absolute_path: &str,
    relative_path: &str,
) -> Result<Vec<ProjectEntry>, String> {
    let root = canonicalize_path(normalized_cwd).await?;
    let directory = canonicalize_path(absolute_path).await?;
    let relative = node_relative(&root, &directory);
    if escapes_workspace_root(&relative)
        || relative
            .split(std::path::MAIN_SEPARATOR)
            .any(|part| part == ".git")
        || relative_path.split('/').any(|part| part == ".git")
    {
        return Err("Directory must be inside the workspace and outside .git.".to_owned());
    }
    let children = read_dir_children(&directory)
        .await
        .map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for child in children {
        if child.name == ".git" || (!child.is_directory && !child.is_file) {
            continue;
        }
        let path = if relative_path.is_empty() {
            child.name.clone()
        } else {
            format!("{relative_path}/{}", child.name)
        };
        let Some(path) = TrimmedNonEmptyString::new(&path).ok() else {
            continue;
        };
        entries.push(ProjectEntry {
            path,
            kind: if child.is_directory {
                ProjectEntryKind::Directory
            } else {
                ProjectEntryKind::File
            },
            ignored: None,
        });
    }
    Ok(entries)
}

async fn canonicalize_path(path: &str) -> Result<String, String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

async fn read_dir_children(path: &str) -> std::io::Result<Vec<DirChild>> {
    let mut reader = tokio::fs::read_dir(path).await?;
    let mut children = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let file_type = entry.file_type().await?;
        children.push(DirChild {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_directory: file_type.is_dir(),
            is_file: file_type.is_file(),
        });
    }
    Ok(children)
}

fn host_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `normalizeSearchQuery` with `/^[@./]+/` stripped once, then lowercased.
fn normalize_search_query(input: &str) -> String {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return String::new();
    }
    static LEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[@./]+").unwrap());
    LEADING.replace(trimmed, "").to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{
        WorkspaceEntries, WorkspaceEntriesBrowseError, WorkspaceEntriesError, host_platform,
    };
    use crate::process::ProcessRunner;
    use crate::vcs_process::VcsProcess;
    use crate::workspace::workspace_paths::WorkspacePaths;
    use crate::workspace::workspace_search_index::{
        FileFinderFactory, FinderResult, WorkspaceSearchIndexMap,
    };
    use turin_contracts::base_schemas::{TrimmedNonEmptyString, TrimmedString};
    use turin_contracts::filesystem::FilesystemBrowseInput;
    use turin_contracts::project::{ProjectEntryKind, ProjectListEntriesInput};

    struct UnusedFactory;

    impl FileFinderFactory for UnusedFactory {
        fn create(
            &self,
            _: &crate::workspace::workspace_search_index::FileFinderCreateOptions,
        ) -> Result<
            FinderResult<Arc<dyn crate::workspace::workspace_search_index::FileFinderEngine>>,
            String,
        > {
            panic!("search index engine is not linked");
        }
    }

    fn entries(home: &str) -> WorkspaceEntries {
        let paths = Arc::new(WorkspacePaths::new(home));
        let indexes = Arc::new(WorkspaceSearchIndexMap::new(Arc::new(UnusedFactory)));
        WorkspaceEntries::new(
            paths,
            indexes,
            Arc::new(VcsProcess::new(ProcessRunner)),
            home,
        )
    }

    fn write_file(cwd: &Path, relative: &str, contents: &str) {
        let path = cwd.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn ne(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn list_input(cwd: &str, directory: Option<&str>) -> ProjectListEntriesInput {
        ProjectListEntriesInput {
            cwd: ne(cwd),
            directory_path: directory.map(TrimmedString::new),
        }
    }

    fn browse_input(partial: &str, cwd: Option<&str>) -> FilesystemBrowseInput {
        FilesystemBrowseInput {
            partial_path: ne(partial),
            cwd: cwd.map(ne),
        }
    }

    #[tokio::test]
    async fn lists_immediate_children_including_ignored_and_empty_directories() {
        let cwd = tempfile::tempdir().unwrap();
        git(cwd.path(), &["init"]);
        write_file(cwd.path(), "tracked.txt", "");
        git(cwd.path(), &["add", "tracked.txt"]);
        write_file(
            cwd.path(),
            ".gitignore",
            "node_modules/\n.env\ntracked.txt\n",
        );
        write_file(cwd.path(), ".env", "secret=value");
        write_file(cwd.path(), "node_modules/pkg/index.js", "");
        write_file(cwd.path(), "src/index.ts", "");
        std::fs::create_dir(cwd.path().join("empty")).unwrap();

        let service = entries("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let root = service
            .list(&list_input(&cwd_path, Some("")))
            .await
            .unwrap();
        let listed: HashMap<&str, (ProjectEntryKind, Option<bool>)> = root
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), (entry.kind, entry.ignored)))
            .collect();
        assert_eq!(
            listed.get(".env"),
            Some(&(ProjectEntryKind::File, Some(true)))
        );
        assert_eq!(
            listed.get("node_modules"),
            Some(&(ProjectEntryKind::Directory, Some(true)))
        );
        assert_eq!(
            listed.get("src"),
            Some(&(ProjectEntryKind::Directory, None))
        );
        assert_eq!(
            listed.get("empty"),
            Some(&(ProjectEntryKind::Directory, None))
        );
        assert_eq!(
            listed.get("tracked.txt"),
            Some(&(ProjectEntryKind::File, None))
        );
        assert!(
            root.entries
                .iter()
                .all(|entry| !entry.path.as_str().contains('/'))
        );
        assert!(
            root.entries
                .iter()
                .all(|entry| entry.path.as_str() != ".git")
        );
        assert!(!root.truncated);

        let pkg = service
            .list(&list_input(&cwd_path, Some("node_modules/pkg")))
            .await
            .unwrap();
        assert_eq!(pkg.entries.len(), 1);
        assert_eq!(pkg.entries[0].path.as_str(), "node_modules/pkg/index.js");
        assert_eq!(pkg.entries[0].kind, ProjectEntryKind::File);
        assert_eq!(pkg.entries[0].ignored, Some(true));
        assert!(!pkg.truncated);

        let empty = service
            .list(&list_input(&cwd_path, Some("empty")))
            .await
            .unwrap();
        assert!(empty.entries.is_empty());
        assert!(!empty.truncated);
    }

    #[tokio::test]
    async fn rejects_directory_traversal_git_internals_and_symlinks_outside_the_workspace() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(cwd.path(), ".git/HEAD", "");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), cwd.path().join("external")).unwrap();
        let service = entries("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let mut paths = vec![
            "../".to_owned(),
            outside.path().to_string_lossy().into_owned(),
            ".git".to_owned(),
            "missing".to_owned(),
        ];
        #[cfg(unix)]
        paths.push("external".to_owned());
        for directory_path in paths {
            let error = service
                .list(&list_input(&cwd_path, Some(&directory_path)))
                .await
                .unwrap_err();
            assert!(
                matches!(error, WorkspaceEntriesError::ReadDirectory(_)),
                "{directory_path}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn browses_a_workspace_with_more_than_25_000_entries_without_truncation() {
        let cwd = tempfile::tempdir().unwrap();
        for directory in 0..26 {
            let directory_path = cwd.path().join(format!("folder-{directory}"));
            std::fs::create_dir(&directory_path).unwrap();
            for index in 0..1000 {
                std::fs::write(directory_path.join(format!("file-{index}.txt")), b"").unwrap();
            }
        }
        let service = entries("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let root = service
            .list(&list_input(&cwd_path, Some("")))
            .await
            .unwrap();
        assert_eq!(root.entries.len(), 26);
        assert!(!root.truncated);
        for directory in &root.entries {
            let result = service
                .list(&list_input(&cwd_path, Some(directory.path.as_str())))
                .await
                .unwrap();
            assert_eq!(result.entries.len(), 1000);
            assert!(!result.truncated);
            let needle = format!("{}/file-999.txt", directory.path.as_str());
            assert!(result.entries.iter().any(|entry| {
                entry.path.as_str() == needle && entry.kind == ProjectEntryKind::File
            }));
        }
    }

    #[tokio::test]
    async fn browse_returns_matching_directories_and_excludes_files() {
        let cwd = tempfile::tempdir().unwrap();
        write_file(cwd.path(), "alphabet.txt", "ignore me");
        write_file(cwd.path(), "alpha/index.ts", "export {};\n");
        write_file(cwd.path(), "alpine/index.ts", "export {};\n");
        let service = entries("/tmp");
        let partial = cwd.path().join("alp");
        let result = service
            .browse(&browse_input(&partial.to_string_lossy(), None))
            .await
            .unwrap();
        assert_eq!(result.parent_path.as_str(), cwd.path().to_string_lossy());
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].name.as_str(), "alpha");
        assert_eq!(result.entries[1].name.as_str(), "alpine");
        assert_eq!(
            result.entries[0].full_path.as_str(),
            cwd.path().join("alpha").to_string_lossy()
        );
        assert_eq!(
            result.entries[1].full_path.as_str(),
            cwd.path().join("alpine").to_string_lossy()
        );
    }

    #[tokio::test]
    async fn browse_shows_dot_directories_in_directory_mode_and_hidden_prefix_mode() {
        let cwd = tempfile::tempdir().unwrap();
        write_file(cwd.path(), ".config/settings.json", "{}");
        write_file(cwd.path(), "config/settings.json", "{}");
        let service = entries("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let directory = service
            .browse(&browse_input(&format!("{cwd_path}/"), None))
            .await
            .unwrap();
        let names: Vec<_> = directory
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, [".config", "config"]);
        let hidden = service
            .browse(&browse_input(&format!("{cwd_path}/.c"), None))
            .await
            .unwrap();
        assert_eq!(hidden.parent_path.as_str(), cwd_path);
        assert_eq!(hidden.entries.len(), 1);
        assert_eq!(hidden.entries[0].name.as_str(), ".config");
        assert_eq!(
            hidden.entries[0].full_path.as_str(),
            Path::new(&cwd_path).join(".config").to_string_lossy()
        );
    }

    #[tokio::test]
    async fn browse_supports_relative_paths_when_cwd_is_provided() {
        let cwd = tempfile::tempdir().unwrap();
        write_file(cwd.path(), "packages/pkg.json", "{}");
        let service = entries("/tmp");
        let cwd_path = cwd.path().to_string_lossy().into_owned();
        let result = service
            .browse(&browse_input("./pack", Some(&cwd_path)))
            .await
            .unwrap();
        assert_eq!(result.parent_path.as_str(), cwd_path);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name.as_str(), "packages");
        assert_eq!(
            result.entries[0].full_path.as_str(),
            cwd.path().join("packages").to_string_lossy()
        );
    }

    #[tokio::test]
    async fn browse_rejects_relative_paths_without_cwd() {
        let service = entries("/tmp");
        let error = service
            .browse(&browse_input("./src", None))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceEntriesBrowseError::CurrentProjectRequired(_)
        ));
        assert_eq!(
            error.to_string(),
            "A current project is required to browse relative workspace path './src'."
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn browse_returns_an_empty_listing_when_the_os_denies_directory_access() {
        use std::os::unix::fs::PermissionsExt;
        let cwd = tempfile::tempdir().unwrap();
        let path = cwd.path().to_path_buf();
        struct Reset(PathBuf);
        impl Drop for Reset {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _reset = Reset(path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let service = entries("/tmp");
        let partial = format!("{}/", path.to_string_lossy());
        let result = service.browse(&browse_input(&partial, None)).await.unwrap();
        assert_eq!(result.parent_path.as_str(), path.to_string_lossy());
        assert!(result.entries.is_empty());
    }

    #[tokio::test]
    async fn browse_rejects_windows_paths_on_non_windows_hosts() {
        if host_platform() == "win32" {
            return;
        }
        let service = entries("/tmp");
        let error = service
            .browse(&browse_input("C:\\Windows\\Temp", None))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceEntriesBrowseError::WindowsPathUnsupported(_)
        ));
        assert!(
            error.to_string().contains("is not supported on 'darwin'")
                || error.to_string().contains("is not supported on 'linux'")
        );
    }

    // Index-backed list/search/searchContents tests need `@ff-labs/fff-node`.
    // The algorithm those tests exercise lives in `workspace_search_index`.
}
