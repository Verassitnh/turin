//! The seam between the diff panel and whatever feeds it.
//!
//! Upstream's `DiffPanel` reads thread/checkpoint state from the client
//! runtime (`useThreadProjection`, `useCheckpointDiff`), the review RPCs
//! (`reviewEnvironment.diffPreview`, `diffFilePatch`, `diffFileContents`) and
//! the VCS queries (`vcsEnvironment.status`, `listRefs`). Turin's panel talks
//! to one [`DiffSource`]; T06 (git) and the orchestrator implement it later.
//! [`super::mock_source::MockDiffSource`] is the fixture-backed one.
//!
//! Every read returns what is known *now* ([`Loadable`]); an async source
//! caches results and calls the notifier installed with
//! [`DiffSource::set_notifier`] when one lands, which re-renders the panel.

use std::sync::Arc;

use gpui_kit::App;

use super::base_ref_choices::VcsRef;
use super::diff_panel_store::ScopedThreadRef;
use super::pierre::types::{ChangeType, FileContents};
use crate::components::chat::timeline::types::TurnDiffSummary;

/// `AsyncResult` as the panel needs it: `isPending` / data / `error`.
#[derive(Clone, Debug, PartialEq)]
pub enum Loadable<T> {
    Pending,
    Ready(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// `ReviewDiffPreviewSourceKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitSourceKind {
    WorkingTree,
    BranchRange,
}

/// `ReviewDiffFileStat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffFileStat {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: usize,
    pub deletions: usize,
}

/// `ReviewDiffPreviewSource`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffSource {
    pub kind: GitSourceKind,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub diff: String,
    pub diff_hash: String,
    pub truncated: bool,
    /// Complete statistics, independent of patch limits.
    pub files: Option<Vec<GitDiffFileStat>>,
}

/// `ReviewDiffPreviewResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffPreview {
    pub cwd: String,
    /// `generatedAt` (ISO).
    pub generated_at: String,
    pub sources: Vec<GitDiffSource>,
}

/// `ReviewDiffFileContentsResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

/// What the panel knows about the thread it shows (`useThreadShell`,
/// `useProject`, `useFilesystemReadAccess`, `vcsEnvironment.status`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffContext {
    pub thread: Option<ScopedThreadRef>,
    pub active_cwd: Option<String>,
    pub repository_root: Option<String>,
    /// `gitStatusQuery.data?.isRepo ?? true`.
    pub is_git_repo: bool,
    /// `fileAccess.canReadFiles`.
    pub can_read_files: bool,
}

/// Branch comparison choices (`vcsEnvironment.listRefs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffRefs {
    pub local: Vec<VcsRef>,
    pub remote: Vec<VcsRef>,
}

/// The data and callbacks behind the diff panel.
pub trait DiffSource: 'static {
    fn context(&self) -> DiffContext;

    /// `deriveThreadCheckpointSummaries`, in projection order.
    fn turns(&self) -> Vec<TurnDiffSummary>;

    /// `useCheckpointDiff`: the unified patch for turns `from..=to`.
    fn checkpoint_diff(&self, from_turn_count: u64, to_turn_count: u64, ignore_whitespace: bool)
    -> Loadable<String>;

    /// `reviewEnvironment.diffPreview`.
    fn git_diff_preview(&self, base_ref: Option<&str>, ignore_whitespace: bool)
    -> Loadable<GitDiffPreview>;

    /// `reviewEnvironment.diffFilePatch`: one file's patch for a truncated
    /// source.
    fn git_file_patch(
        &self,
        kind: GitSourceKind,
        base_ref: Option<&str>,
        file: &GitDiffFileStat,
        ignore_whitespace: bool,
    ) -> Loadable<GitDiffSource>;

    /// `reviewEnvironment.diffFileContents`: the full old/new text behind a
    /// partial diff, used to expand hidden context.
    #[allow(clippy::too_many_arguments)]
    fn diff_file_contents(
        &self,
        kind: GitSourceKind,
        base_ref: Option<&str>,
        head_ref: Option<&str>,
        old_path: &str,
        new_path: &str,
        change_type: ChangeType,
    ) -> Result<DiffFileContents, String>;

    fn list_refs(&self, query: &str) -> DiffRefs;

    /// `openDiffFilePrimaryAction`'s sinks.
    fn open_in_thread_file_viewer(&self, thread: &ScopedThreadRef, workspace_file_path: &str, cx: &mut App);
    fn open_in_editor(&self, target_path: &str, cx: &mut App);

    /// `refreshPreviewQuery` (focus refresh, the refresh button).
    fn refresh(&self, cx: &mut App);

    /// Installed by the panel; call it when data changes.
    fn set_notifier(&self, notify: Arc<dyn Fn(&mut App) + Send + Sync>);
}

/// `FileDiffContentsLoader` over [`DiffSource::diff_file_contents`]
/// (`createGitDiffFileContentsLoader`): `(oldFile, newFile)` for a file.
#[allow(clippy::type_complexity)]
pub fn load_diff_files(
    source: &dyn DiffSource,
    kind: GitSourceKind,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
    cache_key: &str,
    file: &super::pierre::types::FileDiffMetadata,
) -> Result<(Option<FileContents>, FileContents), String> {
    let new_path = super::diff_rendering::resolve_file_diff_path(file);
    let old_path = file.prev_name.clone().unwrap_or_else(|| new_path.clone());
    let contents = source.diff_file_contents(kind, base_ref, head_ref, &old_path, &new_path, file.r#type)?;
    let new_file = FileContents {
        name: new_path.clone(),
        contents: contents.new_contents,
        cache_key: Some(format!("{cache_key}:new:{new_path}")),
    };
    if file.r#type == ChangeType::RenamePure {
        return Ok((None, new_file));
    }
    Ok((
        Some(FileContents {
            name: old_path.clone(),
            contents: contents.old_contents,
            cache_key: Some(format!("{cache_key}:old:{old_path}")),
        }),
        new_file,
    ))
}
