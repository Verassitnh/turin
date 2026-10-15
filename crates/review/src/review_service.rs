//! Port of upstream/apps/server/src/review/ReviewService.ts
//!
//! Diff preview and unchanged-file expansion for the review panel. Every cwd is bound to the
//! workspace (or a managed worktree, or a registered project) before any VCS command runs, so a
//! review read cannot reach arbitrary directories on the host.
//!
//! The Effect services this reads (`ServerConfig`, `ServerSettingsService`, `ProjectStoreV2`,
//! `VcsDriverRegistry`, `GitVcsDriver`, `HostProcess.HomeDirectory`) are passed in as the
//! [`ReviewConfig`] value and the traits below. The real git and registry types implement them
//! at the bottom of this file.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::base_schemas::UtcDateTime;
use turin_contracts::git::GitCommandError;
use turin_contracts::review::{
    ReviewDiffFileContentsInput, ReviewDiffFileContentsResult, ReviewDiffPreviewError,
    ReviewDiffPreviewInput, ReviewDiffPreviewResult,
};
use turin_contracts::vcs::{
    VcsDriverKind, VcsError, VcsRepositoryDetectionError, VcsUnsupportedOperationError,
};
use turin_git::git_vcs_driver_core::GitVcsDriverCore;
use turin_git::vcs_driver_registry::{VcsDriverRegistry, VcsDriverResolveInput};
use turin_git::vcs_project_config::RequestedVcsKind;

use crate::worktrees_directory::{
    WorktreesSettings, is_filesystem_root, managed_worktrees_directories, normalize_lexical,
};

/// `ServerConfig` fields a review cwd is checked against.
#[derive(Debug, Clone)]
pub struct ReviewConfig {
    /// `config.cwd`: the server's workspace root.
    pub workspace_root: PathBuf,
    /// `config.worktreesDir`: the default managed worktrees directory.
    pub worktrees_dir: PathBuf,
    /// `HostProcess.HomeDirectory`, used to expand `~` in worktree settings.
    pub home: String,
}

/// The git half of `GitVcsDriver` that review reads use.
#[async_trait]
pub trait ReviewGit: Send + Sync {
    async fn get_review_diff_preview(
        &self,
        input: &ReviewDiffPreviewInput,
    ) -> Result<ReviewDiffPreviewResult, GitCommandError>;
    async fn get_review_diff_file_contents(
        &self,
        input: &ReviewDiffFileContentsInput,
    ) -> Result<ReviewDiffFileContentsResult, GitCommandError>;
}

/// `VcsDriverRegistry.detect` with `requestedKind: "auto"`, reduced to the kind of the match.
#[async_trait]
pub trait ReviewVcsDetector: Send + Sync {
    async fn detect_kind(&self, cwd: &str) -> Result<Option<VcsDriverKind>, VcsError>;
}

/// `ServerSettingsService.getSettings`. An `Err` is read as empty settings, as upstream does.
#[async_trait]
pub trait ReviewSettings: Send + Sync {
    async fn worktrees_settings(&self) -> Result<WorktreesSettings, String>;
}

/// `ProjectStoreV2.listShells`, reduced to each project's workspace root. An `Err` is read as
/// no projects, as upstream does.
#[async_trait]
pub trait ReviewProjects: Send + Sync {
    async fn project_workspace_roots(&self) -> Result<Vec<String>, String>;
}

pub struct ReviewService {
    config: ReviewConfig,
    git: Arc<dyn ReviewGit>,
    vcs: Arc<dyn ReviewVcsDetector>,
    settings: Arc<dyn ReviewSettings>,
    projects: Arc<dyn ReviewProjects>,
}

impl ReviewService {
    pub fn new(
        config: ReviewConfig,
        git: Arc<dyn ReviewGit>,
        vcs: Arc<dyn ReviewVcsDetector>,
        settings: Arc<dyn ReviewSettings>,
        projects: Arc<dyn ReviewProjects>,
    ) -> Self {
        Self {
            config,
            git,
            vcs,
            settings,
            projects,
        }
    }

    pub async fn get_diff_preview(
        &self,
        input: &ReviewDiffPreviewInput,
    ) -> Result<ReviewDiffPreviewResult, ReviewDiffPreviewError> {
        self.assert_workspace_bound_cwd("ReviewService.getDiffPreview", input.cwd.as_str())
            .await
            .map_err(ReviewDiffPreviewError::Vcs)?;

        let Some(kind) = self
            .vcs
            .detect_kind(input.cwd.as_str())
            .await
            .map_err(ReviewDiffPreviewError::Vcs)?
        else {
            return Ok(ReviewDiffPreviewResult {
                cwd: input.cwd.clone(),
                generated_at: UtcDateTime::now(),
                sources: Vec::new(),
            });
        };

        if kind != VcsDriverKind::Git {
            return Err(ReviewDiffPreviewError::Vcs(VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation: "ReviewService.getDiffPreview".to_owned(),
                    kind,
                    detail: format!("The {kind} VCS driver does not support review diff previews."),
                },
            )));
        }
        self.git
            .get_review_diff_preview(input)
            .await
            .map_err(ReviewDiffPreviewError::GitCommand)
    }

    pub async fn get_diff_file_contents(
        &self,
        input: &ReviewDiffFileContentsInput,
    ) -> Result<ReviewDiffFileContentsResult, ReviewDiffPreviewError> {
        self.assert_workspace_bound_cwd("ReviewService.getDiffFileContents", input.cwd.as_str())
            .await
            .map_err(ReviewDiffPreviewError::Vcs)?;

        let kind = self
            .vcs
            .detect_kind(input.cwd.as_str())
            .await
            .map_err(ReviewDiffPreviewError::Vcs)?;
        if kind != Some(VcsDriverKind::Git) {
            return Err(ReviewDiffPreviewError::Vcs(VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation: "ReviewService.getDiffFileContents".to_owned(),
                    kind: kind.unwrap_or(VcsDriverKind::Unknown),
                    detail: "Unchanged diff expansion currently requires a Git repository."
                        .to_owned(),
                },
            )));
        }
        self.git
            .get_review_diff_file_contents(input)
            .await
            .map_err(ReviewDiffPreviewError::GitCommand)
    }

    /// Rejects a cwd outside the workspace root, every managed worktrees directory, and every
    /// registered project root. Upstream attributes the failure to the calling operation.
    async fn assert_workspace_bound_cwd(
        &self,
        operation: &'static str,
        cwd: &str,
    ) -> Result<(), VcsError> {
        let settings = self.settings.worktrees_settings().await.unwrap_or_default();
        let candidate = canonicalize_path(Path::new(cwd))?;
        let workspace_root = canonicalize_path(&self.config.workspace_root)?;
        // A managed root that cannot be resolved, or resolves to a filesystem root through a
        // symlink, is skipped rather than failing every review.
        let worktrees_roots: Vec<PathBuf> =
            managed_worktrees_directories(&settings, &self.config.worktrees_dir, &self.config.home)
                .iter()
                .filter_map(|directory| canonicalize_path(directory).ok())
                .filter(|root| !is_filesystem_root(root))
                .collect();

        if is_within_root(&candidate, &workspace_root)
            || worktrees_roots
                .iter()
                .any(|root| is_within_root(&candidate, root))
        {
            return Ok(());
        }

        // Registered projects can live outside the server cwd, which is the home directory in
        // packaged desktop builds, e.g. a repository on another Windows drive. Unreadable or
        // unresolvable project roots grant nothing.
        let projects = self
            .projects
            .project_workspace_roots()
            .await
            .unwrap_or_default();
        for project in projects {
            if let Ok(root) = canonicalize_path(Path::new(&project))
                && is_within_root(&candidate, &root)
            {
                return Ok(());
            }
        }

        Err(VcsError::RepositoryDetection(
            VcsRepositoryDetectionError::VcsRepositoryDetectionError {
                operation: operation.to_owned(),
                cwd: cwd.to_owned(),
                detail: if operation == "ReviewService.getDiffPreview" {
                    "Review diff preview cwd must stay within the configured workspace root."
                        .to_owned()
                } else {
                    "Review diff file contents cwd must stay within the configured workspace root."
                        .to_owned()
                },
                cause: None,
            },
        ))
    }
}

/// `path.resolve` followed by `fs.realPath`. A path that does not exist yet keeps its resolved
/// form; any other failure (such as an interior NUL byte) is reported, not ignored.
fn canonicalize_path(value: &Path) -> Result<PathBuf, VcsError> {
    let absolute = std::path::absolute(value).unwrap_or_else(|_| value.to_path_buf());
    let resolved = normalize_lexical(&absolute);
    match std::fs::canonicalize(&resolved) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(VcsError::RepositoryDetection(
            VcsRepositoryDetectionError::VcsRepositoryDetectionError {
                operation: "ReviewService.assertWorkspaceBoundCwd.canonicalizePath".to_owned(),
                cwd: resolved.display().to_string(),
                detail: "Failed to resolve a path while validating the review workspace."
                    .to_owned(),
                cause: Some(serde_json::Value::String(error.to_string())),
            },
        )),
    }
}

/// Component-wise containment. Both sides are canonical absolute paths here.
fn is_within_root(candidate: &Path, root: &Path) -> bool {
    candidate.starts_with(root)
}

#[async_trait]
impl ReviewGit for GitVcsDriverCore {
    async fn get_review_diff_preview(
        &self,
        input: &ReviewDiffPreviewInput,
    ) -> Result<ReviewDiffPreviewResult, GitCommandError> {
        GitVcsDriverCore::get_review_diff_preview(self, input).await
    }

    async fn get_review_diff_file_contents(
        &self,
        input: &ReviewDiffFileContentsInput,
    ) -> Result<ReviewDiffFileContentsResult, GitCommandError> {
        GitVcsDriverCore::get_review_diff_file_contents(self, input).await
    }
}

#[async_trait]
impl ReviewVcsDetector for VcsDriverRegistry {
    async fn detect_kind(&self, cwd: &str) -> Result<Option<VcsDriverKind>, VcsError> {
        let handle = self
            .detect(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: Some(RequestedVcsKind::Auto),
            })
            .await?;
        Ok(handle.map(|handle| handle.kind))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tempfile::TempDir;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::review::{ReviewDiffFileChangeType, ReviewDiffPreviewSourceKind};

    use super::*;

    /// Upstream's `GitVcsDriver` mock is never reached: every rejected cwd fails before detection,
    /// and the accepted ones only return an empty preview.
    struct UnusedGit;

    #[async_trait]
    impl ReviewGit for UnusedGit {
        async fn get_review_diff_preview(
            &self,
            _input: &ReviewDiffPreviewInput,
        ) -> Result<ReviewDiffPreviewResult, GitCommandError> {
            unreachable!("git preview is not reached in ReviewService tests")
        }

        async fn get_review_diff_file_contents(
            &self,
            _input: &ReviewDiffFileContentsInput,
        ) -> Result<ReviewDiffFileContentsResult, GitCommandError> {
            unreachable!("git file contents are not reached in ReviewService tests")
        }
    }

    struct FakeVcs {
        kind: Option<VcsDriverKind>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeVcs {
        fn new(kind: Option<VcsDriverKind>) -> Arc<Self> {
            Arc::new(Self {
                kind,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ReviewVcsDetector for FakeVcs {
        async fn detect_kind(&self, cwd: &str) -> Result<Option<VcsDriverKind>, VcsError> {
            self.calls.lock().unwrap().push(cwd.to_owned());
            Ok(self.kind)
        }
    }

    struct FakeSettings(WorktreesSettings);

    #[async_trait]
    impl ReviewSettings for FakeSettings {
        async fn worktrees_settings(&self) -> Result<WorktreesSettings, String> {
            Ok(self.0.clone())
        }
    }

    struct FakeProjects(Result<Vec<String>, String>);

    #[async_trait]
    impl ReviewProjects for FakeProjects {
        async fn project_workspace_roots(&self) -> Result<Vec<String>, String> {
            self.0.clone()
        }
    }

    struct Fixture {
        workspace: TempDir,
        base: TempDir,
    }

    fn fixture() -> Fixture {
        Fixture {
            workspace: TempDir::with_prefix("t3-review-workspace-").unwrap(),
            base: TempDir::with_prefix("t3-review-base-").unwrap(),
        }
    }

    fn service(
        fixture: &Fixture,
        vcs: Arc<FakeVcs>,
        settings: WorktreesSettings,
        projects: Result<Vec<String>, String>,
    ) -> ReviewService {
        ReviewService::new(
            ReviewConfig {
                workspace_root: fixture.workspace.path().to_path_buf(),
                worktrees_dir: fixture.base.path().join("worktrees"),
                home: "/home/test".to_owned(),
            },
            Arc::new(UnusedGit),
            vcs,
            Arc::new(FakeSettings(settings)),
            Arc::new(FakeProjects(projects)),
        )
    }

    fn preview_input(cwd: &str) -> ReviewDiffPreviewInput {
        ReviewDiffPreviewInput {
            cwd: TrimmedNonEmptyString::new(cwd).unwrap(),
            base_ref: None,
            ignore_whitespace: None,
            file: None,
        }
    }

    fn file_contents_input(cwd: &str) -> ReviewDiffFileContentsInput {
        ReviewDiffFileContentsInput {
            cwd: TrimmedNonEmptyString::new(cwd).unwrap(),
            source_kind: ReviewDiffPreviewSourceKind::WorkingTree,
            change_type: ReviewDiffFileChangeType::Change,
            base_ref: Some(TrimmedNonEmptyString::new("HEAD").unwrap()),
            head_ref: None,
            old_path: TrimmedNonEmptyString::new("file.ts").unwrap(),
            new_path: TrimmedNonEmptyString::new("file.ts").unwrap(),
        }
    }

    fn outside_error(operation: &str, cwd: &str) -> ReviewDiffPreviewError {
        ReviewDiffPreviewError::Vcs(VcsError::RepositoryDetection(
            VcsRepositoryDetectionError::VcsRepositoryDetectionError {
                operation: operation.to_owned(),
                cwd: cwd.to_owned(),
                detail: if operation == "ReviewService.getDiffPreview" {
                    "Review diff preview cwd must stay within the configured workspace root."
                        .to_owned()
                } else {
                    "Review diff file contents cwd must stay within the configured workspace root."
                        .to_owned()
                },
                cause: None,
            },
        ))
    }

    #[tokio::test]
    async fn rejects_diff_preview_cwd_outside_the_configured_workspace_roots() {
        let fixture = fixture();
        let outside = TempDir::with_prefix("t3-review-outside-").unwrap();
        let vcs = FakeVcs::new(None);
        let cwd = outside.path().to_str().unwrap();
        let error = service(
            &fixture,
            vcs.clone(),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_preview(&preview_input(cwd))
        .await
        .unwrap_err();

        assert_eq!(error, outside_error("ReviewService.getDiffPreview", cwd));
        assert!(vcs.calls().is_empty());
    }

    #[tokio::test]
    async fn attributes_file_content_workspace_violations_to_the_file_content_operation() {
        let fixture = fixture();
        let outside = TempDir::with_prefix("t3-review-outside-").unwrap();
        let vcs = FakeVcs::new(None);
        let cwd = outside.path().to_str().unwrap();
        let error = service(
            &fixture,
            vcs.clone(),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_file_contents(&file_contents_input(cwd))
        .await
        .unwrap_err();

        assert_eq!(
            error,
            outside_error("ReviewService.getDiffFileContents", cwd)
        );
        assert!(vcs.calls().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allows_previous_custom_worktree_locations_but_never_a_filesystem_root() {
        let fixture = fixture();
        let previous = TempDir::with_prefix("t3-review-old-worktrees-").unwrap();
        let outside = TempDir::with_prefix("t3-review-outside-").unwrap();
        let previous_cwd = previous.path().to_str().unwrap();

        let result = service(
            &fixture,
            FakeVcs::new(None),
            WorktreesSettings {
                worktrees_directory: "/".to_owned(),
                previous_worktrees_directories: vec![previous_cwd.to_owned()],
            },
            Ok(vec![]),
        )
        .get_diff_preview(&preview_input(previous_cwd))
        .await
        .unwrap();
        assert_eq!(result.cwd.as_str(), previous_cwd);

        let root_link = fixture.base.path().join("root-link");
        std::os::unix::fs::symlink("/", &root_link).unwrap();
        let outside_cwd = outside.path().to_str().unwrap();
        for worktrees_directory in ["/", root_link.to_str().unwrap()] {
            let error = service(
                &fixture,
                FakeVcs::new(None),
                WorktreesSettings {
                    worktrees_directory: worktrees_directory.to_owned(),
                    previous_worktrees_directories: vec![],
                },
                Ok(vec![]),
            )
            .get_diff_preview(&preview_input(outside_cwd))
            .await
            .unwrap_err();
            assert_eq!(
                error,
                outside_error("ReviewService.getDiffPreview", outside_cwd)
            );
        }
    }

    #[tokio::test]
    async fn allows_registered_project_roots_outside_the_server_cwd() {
        let fixture = fixture();
        let project_root = TempDir::with_prefix("t3-review-project-").unwrap();
        let project_child = project_root.path().join("packages").join("app");
        std::fs::create_dir_all(&project_child).unwrap();
        // A sibling that shares the project root's string prefix but is not inside it.
        let sibling = TempDir::with_prefix("t3-review-sibling-").unwrap();
        let project_cwd = project_root.path().to_str().unwrap();
        let child_cwd = project_child.to_str().unwrap();
        let sibling_cwd = sibling.path().to_str().unwrap();

        let review = |cwd: &str, projects: Result<Vec<String>, String>| {
            let service = service(
                &fixture,
                FakeVcs::new(None),
                WorktreesSettings::default(),
                projects,
            );
            let cwd = cwd.to_owned();
            async move { service.get_diff_preview(&preview_input(&cwd)).await }
        };

        let root_result = review(project_cwd, Ok(vec![project_cwd.to_owned()]))
            .await
            .unwrap();
        assert_eq!(root_result.cwd.as_str(), project_cwd);
        let child_result = review(child_cwd, Ok(vec![project_cwd.to_owned()]))
            .await
            .unwrap();
        assert_eq!(child_result.cwd.as_str(), child_cwd);

        for (cwd, projects) in [
            (sibling_cwd, Ok(vec![project_cwd.to_owned()])),
            (project_cwd, Ok(vec![])),
            (project_cwd, Err("offline".to_owned())),
        ] {
            let error = review(cwd, projects).await.unwrap_err();
            assert!(matches!(
                error,
                ReviewDiffPreviewError::Vcs(VcsError::RepositoryDetection(_))
            ));
        }
    }

    #[tokio::test]
    async fn allows_diff_preview_cwd_inside_the_configured_workspace_root() {
        let fixture = fixture();
        let vcs = FakeVcs::new(None);
        let workspace_cwd = fixture.workspace.path().to_str().unwrap();
        let result = service(
            &fixture,
            vcs.clone(),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_preview(&preview_input(workspace_cwd))
        .await
        .unwrap();

        assert_eq!(result.cwd.as_str(), workspace_cwd);
        assert!(result.sources.is_empty());
        assert_eq!(vcs.calls(), vec![workspace_cwd.to_owned()]);
    }

    #[tokio::test]
    async fn preserves_unexpected_path_resolution_failures() {
        let fixture = fixture();
        let vcs = FakeVcs::new(None);
        let invalid_cwd = format!("{}\0invalid", fixture.workspace.path().display());
        let error = service(
            &fixture,
            vcs.clone(),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_preview(&preview_input(&invalid_cwd))
        .await
        .unwrap_err();

        let ReviewDiffPreviewError::Vcs(VcsError::RepositoryDetection(
            VcsRepositoryDetectionError::VcsRepositoryDetectionError {
                operation,
                cwd,
                detail,
                cause,
            },
        )) = error
        else {
            panic!("expected a repository detection error, got {error:?}");
        };
        assert_eq!(
            operation,
            "ReviewService.assertWorkspaceBoundCwd.canonicalizePath"
        );
        assert_eq!(cwd, invalid_cwd);
        assert!(detail.contains("Failed to resolve a path"));
        assert!(cause.is_some_and(|cause| cause.is_string()));
        assert!(vcs.calls().is_empty());
    }

    #[tokio::test]
    async fn non_git_repositories_are_unsupported_for_preview_and_expansion() {
        let fixture = fixture();
        let workspace_cwd = fixture.workspace.path().to_str().unwrap();

        let preview = service(
            &fixture,
            FakeVcs::new(Some(VcsDriverKind::Jj)),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_preview(&preview_input(workspace_cwd))
        .await
        .unwrap_err();
        assert_eq!(
            preview,
            ReviewDiffPreviewError::Vcs(VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation: "ReviewService.getDiffPreview".to_owned(),
                    kind: VcsDriverKind::Jj,
                    detail: "The jj VCS driver does not support review diff previews.".to_owned(),
                },
            ))
        );

        let expansion = service(
            &fixture,
            FakeVcs::new(None),
            WorktreesSettings::default(),
            Ok(vec![]),
        )
        .get_diff_file_contents(&file_contents_input(workspace_cwd))
        .await
        .unwrap_err();
        assert_eq!(
            expansion,
            ReviewDiffPreviewError::Vcs(VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation: "ReviewService.getDiffFileContents".to_owned(),
                    kind: VcsDriverKind::Unknown,
                    detail: "Unchanged diff expansion currently requires a Git repository."
                        .to_owned(),
                },
            ))
        );
    }
}
