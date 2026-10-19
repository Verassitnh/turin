//! The live [`DiffSource`]: the thread's real checkpoints and its workspace's git diff.
//!
//! Reads are cached per question. A miss starts the command on the runtime and answers
//! `Pending`; when it lands the panel's notifier re-renders (through a GPUI task, since the
//! commands finish on the tokio runtime). The owner calls [`LiveDiffSource::sync`] with the
//! thread's projection on every render: a changed set of checkpoints or workspace drops the
//! caches, so a new turn shows up without reopening the panel.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use futures::StreamExt as _;
use gpui_kit::App;
use parking_lot::Mutex;
use serde_json::json;
use turin_contracts::checkpoint_diff::OrchestrationGetTurnDiffInput;
use turin_contracts::review::{
    ReviewDiffFileChangeType, ReviewDiffFileContentsInput, ReviewDiffPreviewInput,
    ReviewDiffPreviewInputFile, ReviewDiffPreviewResult, ReviewDiffPreviewSource,
    ReviewDiffPreviewSourceKind,
};
use turin_contracts::{ThreadId, TrimmedNonEmptyString};
use turin_git::checkpointing::checkpoint_diff_query::CheckpointProjection;

use super::base_ref_choices::VcsRef;
use super::diff_panel_store::ScopedThreadRef;
use super::pierre::types::ChangeType;
use super::view_model::{
    DiffContext, DiffFileContents, DiffRefs, DiffSource, GitDiffFileStat, GitDiffPreview,
    GitDiffSource, GitSourceKind, Loadable,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;
use crate::components::chat::timeline::types::TurnDiffSummary;
use crate::hooks::use_turn_diff_summaries::use_turn_diff_summaries;
use crate::state::local_environment_id;
use crate::state::thread_projection::ThreadProjection;

type Notifier = Arc<dyn Fn(&mut App) + Send + Sync>;

/// How many refs the base-ref picker lists per kind.
const REF_LIMIT: u32 = 100;

fn source_kind(kind: GitSourceKind) -> ReviewDiffPreviewSourceKind {
    match kind {
        GitSourceKind::WorkingTree => ReviewDiffPreviewSourceKind::WorkingTree,
        GitSourceKind::BranchRange => ReviewDiffPreviewSourceKind::BranchRange,
    }
}

fn git_source_kind(kind: ReviewDiffPreviewSourceKind) -> GitSourceKind {
    match kind {
        ReviewDiffPreviewSourceKind::WorkingTree => GitSourceKind::WorkingTree,
        ReviewDiffPreviewSourceKind::BranchRange => GitSourceKind::BranchRange,
    }
}

fn change_type(kind: ChangeType) -> ReviewDiffFileChangeType {
    match kind {
        ChangeType::Change => ReviewDiffFileChangeType::Change,
        ChangeType::RenamePure => ReviewDiffFileChangeType::RenamePure,
        ChangeType::RenameChanged => ReviewDiffFileChangeType::RenameChanged,
        ChangeType::New => ReviewDiffFileChangeType::New,
        ChangeType::Deleted => ReviewDiffFileChangeType::Deleted,
    }
}

fn trimmed(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn loadable<T>(result: Result<T, CommandError>) -> Loadable<T> {
    match result {
        Ok(value) => Loadable::Ready(value),
        Err(error) => Loadable::Failed(error.message),
    }
}

fn diff_source(source: ReviewDiffPreviewSource) -> GitDiffSource {
    GitDiffSource {
        kind: git_source_kind(source.kind),
        base_ref: source.base_ref.map(|name| name.to_string()),
        head_ref: source.head_ref.map(|name| name.to_string()),
        diff: source.diff,
        diff_hash: source.diff_hash.to_string(),
        truncated: source.truncated,
        files: source.files.map(|files| {
            files
                .into_iter()
                .map(|file| GitDiffFileStat {
                    path: file.path,
                    previous_path: file.previous_path,
                    additions: file.additions.max(0.) as usize,
                    deletions: file.deletions.max(0.) as usize,
                })
                .collect()
        }),
    }
}

fn diff_preview(result: ReviewDiffPreviewResult) -> GitDiffPreview {
    GitDiffPreview {
        cwd: result.cwd.to_string(),
        generated_at: result.generated_at.to_string(),
        sources: result.sources.into_iter().map(diff_source).collect(),
    }
}

fn vcs_ref(reference: turin_contracts::git::VcsRef) -> VcsRef {
    VcsRef {
        name: reference.name.to_string(),
        current: reference.current,
        is_default: reference.is_default,
        is_remote: reference.is_remote.unwrap_or(false),
        remote_name: reference.remote_name.map(|name| name.to_string()),
        worktree_path: reference.worktree_path.map(|path| path.to_string()),
    }
}

#[derive(Default)]
struct Inner {
    cwd: Option<String>,
    turns: Vec<TurnDiffSummary>,
    checkpoints: Option<CheckpointProjection>,
    /// Bumped when the cached answers no longer describe the thread.
    generation: u64,
    patches: HashMap<(u64, u64, bool), Loadable<String>>,
    previews: HashMap<(Option<String>, bool), Loadable<GitDiffPreview>>,
    file_patches: HashMap<(GitSourceKind, Option<String>, String, bool), Loadable<GitDiffSource>>,
    refs: HashMap<String, Loadable<DiffRefs>>,
    /// `gitStatusQuery.data?.isRepo`; unknown until the first refs read lands.
    is_repo: Option<bool>,
}

struct Shared {
    inner: Mutex<Inner>,
    notifier: Mutex<Option<Notifier>>,
    wake: futures::channel::mpsc::UnboundedSender<()>,
}

impl Shared {
    /// Applies a command's answer unless the thread moved on while it ran.
    fn finish(&self, generation: u64, apply: impl FnOnce(&mut Inner)) {
        {
            let mut inner = self.inner.lock();
            if inner.generation != generation {
                return;
            }
            apply(&mut inner);
        }
        let _ = self.wake.unbounded_send(());
    }
}

pub struct LiveDiffSource {
    thread: ScopedThreadRef,
    thread_id: ThreadId,
    bus: CommandBus,
    shared: Arc<Shared>,
}

impl LiveDiffSource {
    pub fn new(thread_id: ThreadId, bus: CommandBus, cx: &mut App) -> Self {
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::default()),
            notifier: Mutex::new(None),
            wake,
        });
        let weak: Weak<Shared> = Arc::downgrade(&shared);
        cx.spawn(async move |cx| {
            while woken.next().await.is_some() {
                // One re-render covers every answer that landed meanwhile.
                while woken.try_recv().is_ok() {}
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                let notifier = shared.notifier.lock().clone();
                if let Some(notify) = notifier {
                    cx.update(|cx| notify(cx));
                }
            }
        })
        .detach();
        Self {
            thread: ScopedThreadRef::new(local_environment_id().as_str(), thread_id.as_str()),
            thread_id,
            bus,
            shared,
        }
    }

    /// Feeds the thread's current checkpoints and workspace. Returns whether the cached answers
    /// were dropped (the owner's next panel read starts them again).
    pub fn sync(&self, projection: &ThreadProjection, workspace_root: Option<String>) -> bool {
        let cwd = projection
            .thread
            .worktree_path
            .as_ref()
            .map(|path| path.to_string())
            .or(workspace_root);
        let turns = use_turn_diff_summaries(Some(projection)).turn_diff_summaries;
        let checkpoints = checkpoint_projection(projection);
        let mut inner = self.shared.inner.lock();
        if inner.cwd == cwd && inner.turns == turns && inner.checkpoints == checkpoints {
            return false;
        }
        let first = inner.cwd.is_none() && inner.turns.is_empty() && inner.checkpoints.is_none();
        inner.cwd = cwd;
        inner.turns = turns;
        inner.checkpoints = checkpoints;
        inner.generation += 1;
        inner.patches.clear();
        inner.previews.clear();
        inner.file_patches.clear();
        inner.refs.clear();
        inner.is_repo = None;
        drop(inner);
        if !first {
            let _ = self.shared.wake.unbounded_send(());
        }
        true
    }

    fn cwd(&self) -> Option<String> {
        self.shared.inner.lock().cwd.clone()
    }

    fn start<T: Send + 'static, Fut>(
        &self,
        run: impl FnOnce(crate::app_runtime::write::commands::Commands) -> Fut + Send + 'static,
        apply: impl FnOnce(&mut Inner, Loadable<T>) + Send + 'static,
    ) where
        Fut: std::future::Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        let generation = self.shared.inner.lock().generation;
        let shared = self.shared.clone();
        self.bus.spawn(run, move |result| {
            let answer = loadable(result);
            shared
                .clone()
                .finish(generation, |inner| apply(inner, answer));
        });
    }

    fn preview_input(
        cwd: &str,
        base_ref: Option<&str>,
        ignore_whitespace: bool,
        file: Option<ReviewDiffPreviewInputFile>,
    ) -> Option<ReviewDiffPreviewInput> {
        Some(ReviewDiffPreviewInput {
            cwd: trimmed(cwd)?,
            base_ref: base_ref.and_then(trimmed),
            ignore_whitespace: Some(ignore_whitespace),
            file,
        })
    }

    fn open_path(&self, path: &str, cx: &mut App) {
        let absolute = if std::path::Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            match self.cwd() {
                Some(cwd) => std::path::Path::new(&cwd).join(path),
                None => std::path::PathBuf::from(path),
            }
        };
        cx.open_url(&format!("file://{}", absolute.display()));
    }
}

/// The slice of the projection the checkpoint diff query reads.
pub(crate) fn checkpoint_projection(projection: &ThreadProjection) -> Option<CheckpointProjection> {
    let contract = projection.to_contract();
    serde_json::from_value(json!({
        "runs": contract.runs,
        "checkpointScopes": contract.checkpoint_scopes,
        "checkpoints": contract.checkpoints,
    }))
    .ok()
}

impl DiffSource for LiveDiffSource {
    fn context(&self) -> DiffContext {
        let inner = self.shared.inner.lock();
        DiffContext {
            thread: Some(self.thread.clone()),
            active_cwd: inner.cwd.clone(),
            repository_root: inner.cwd.clone(),
            is_git_repo: inner.is_repo.unwrap_or(true),
            can_read_files: inner.cwd.is_some(),
        }
    }

    fn turns(&self) -> Vec<TurnDiffSummary> {
        self.shared.inner.lock().turns.clone()
    }

    fn checkpoint_diff(
        &self,
        from_turn_count: u64,
        to_turn_count: u64,
        ignore_whitespace: bool,
    ) -> Loadable<String> {
        let key = (from_turn_count, to_turn_count, ignore_whitespace);
        let context = {
            let inner = self.shared.inner.lock();
            if let Some(cached) = inner.patches.get(&key) {
                return cached.clone();
            }
            inner.checkpoints.clone()
        };
        let Some(context) = context else {
            return Loadable::Failed("This thread has no checkpoints.".into());
        };
        let input = OrchestrationGetTurnDiffInput {
            from_turn_count,
            to_turn_count,
            thread_id: self.thread_id.clone(),
            ignore_whitespace: Some(ignore_whitespace),
        };
        self.shared
            .inner
            .lock()
            .patches
            .insert(key, Loadable::Pending);
        self.start(
            move |commands| async move { commands.checkpoint_turn_diff(context, &input).await },
            move |inner, answer| {
                inner.patches.insert(key, answer);
            },
        );
        Loadable::Pending
    }

    fn git_diff_preview(
        &self,
        base_ref: Option<&str>,
        ignore_whitespace: bool,
    ) -> Loadable<GitDiffPreview> {
        let key = (base_ref.map(str::to_owned), ignore_whitespace);
        if let Some(cached) = self.shared.inner.lock().previews.get(&key) {
            return cached.clone();
        }
        let Some(cwd) = self.cwd() else {
            return Loadable::Failed("This thread has no workspace.".into());
        };
        let Some(input) = Self::preview_input(&cwd, base_ref, ignore_whitespace, None) else {
            return Loadable::Failed("This thread has no workspace.".into());
        };
        self.shared
            .inner
            .lock()
            .previews
            .insert(key.clone(), Loadable::Pending);
        self.start(
            move |commands| async move { commands.review_diff_preview(&input).await },
            move |inner, answer| {
                let answer = match answer {
                    Loadable::Ready(result) => Loadable::Ready(diff_preview(result)),
                    Loadable::Failed(message) => Loadable::Failed(message),
                    Loadable::Pending => Loadable::Pending,
                };
                inner.previews.insert(key, answer);
            },
        );
        Loadable::Pending
    }

    fn git_file_patch(
        &self,
        kind: GitSourceKind,
        base_ref: Option<&str>,
        file: &GitDiffFileStat,
        ignore_whitespace: bool,
    ) -> Loadable<GitDiffSource> {
        let key = (
            kind,
            base_ref.map(str::to_owned),
            file.path.clone(),
            ignore_whitespace,
        );
        if let Some(cached) = self.shared.inner.lock().file_patches.get(&key) {
            return cached.clone();
        }
        let Some(cwd) = self.cwd() else {
            return Loadable::Failed("This thread has no workspace.".into());
        };
        let input = Self::preview_input(
            &cwd,
            base_ref,
            ignore_whitespace,
            Some(ReviewDiffPreviewInputFile {
                path: file.path.clone(),
                previous_path: file.previous_path.clone(),
                source_kind: source_kind(kind),
            }),
        );
        let Some(input) = input else {
            return Loadable::Failed("This thread has no workspace.".into());
        };
        self.shared
            .inner
            .lock()
            .file_patches
            .insert(key.clone(), Loadable::Pending);
        self.start(
            move |commands| async move { commands.review_diff_preview(&input).await },
            move |inner, answer| {
                let answer = match answer {
                    Loadable::Ready(result) => {
                        match result
                            .sources
                            .into_iter()
                            .find(|source| git_source_kind(source.kind) == key.0)
                        {
                            Some(source) => Loadable::Ready(diff_source(source)),
                            None => Loadable::Failed(
                                "Diff no longer available. Refresh the comparison.".into(),
                            ),
                        }
                    }
                    Loadable::Failed(message) => Loadable::Failed(message),
                    Loadable::Pending => Loadable::Pending,
                };
                inner.file_patches.insert(key, answer);
            },
        );
        Loadable::Pending
    }

    fn diff_file_contents(
        &self,
        kind: GitSourceKind,
        base_ref: Option<&str>,
        head_ref: Option<&str>,
        old_path: &str,
        new_path: &str,
        change: ChangeType,
    ) -> Result<DiffFileContents, String> {
        let cwd = self
            .cwd()
            .ok_or_else(|| "This thread has no workspace.".to_owned())?;
        let invalid = || "The diff names an empty path.".to_owned();
        let input = ReviewDiffFileContentsInput {
            cwd: trimmed(&cwd).ok_or_else(invalid)?,
            source_kind: source_kind(kind),
            change_type: change_type(change),
            base_ref: base_ref.and_then(trimmed),
            head_ref: head_ref.and_then(trimmed),
            old_path: trimmed(old_path).ok_or_else(invalid)?,
            new_path: trimmed(new_path).ok_or_else(invalid)?,
        };
        // A short `git show` pair; the panel asks from a click handler.
        self.bus
            .run_blocking(
                |commands| async move { commands.review_diff_file_contents(&input).await },
            )
            .map(|contents| DiffFileContents {
                old_contents: contents.old_contents,
                new_contents: contents.new_contents,
            })
            .map_err(|error| error.message)
    }

    fn list_refs(&self, query: &str) -> DiffRefs {
        let key = query.trim().to_owned();
        if let Some(cached) = self.shared.inner.lock().refs.get(&key) {
            return match cached {
                Loadable::Ready(refs) => refs.clone(),
                _ => DiffRefs::default(),
            };
        }
        let Some(cwd) = self.cwd() else {
            return DiffRefs::default();
        };
        let Ok(input) = serde_json::from_value::<turin_contracts::git::VcsListRefsInput>(json!({
            "cwd": cwd,
            "query": (!key.is_empty()).then_some(key.as_str()),
            "refKind": "all",
            "limit": REF_LIMIT,
        })) else {
            return DiffRefs::default();
        };
        self.shared
            .inner
            .lock()
            .refs
            .insert(key.clone(), Loadable::Pending);
        let is_repo_key = key.clone();
        self.start(
            move |commands| async move { commands.git_list_refs(&input).await },
            move |inner, answer| {
                let answer = match answer {
                    Loadable::Ready(result) => {
                        inner.is_repo = Some(result.is_repo);
                        let (remote, local): (Vec<_>, Vec<_>) = result
                            .refs
                            .into_iter()
                            .map(vcs_ref)
                            .partition(|reference| reference.is_remote);
                        Loadable::Ready(DiffRefs { local, remote })
                    }
                    Loadable::Failed(message) => Loadable::Failed(message),
                    Loadable::Pending => Loadable::Pending,
                };
                inner.refs.insert(is_repo_key, answer);
            },
        );
        DiffRefs::default()
    }

    fn open_in_thread_file_viewer(
        &self,
        _: &ScopedThreadRef,
        workspace_file_path: &str,
        cx: &mut App,
    ) {
        // The thread file viewer is cut (PORTING.md); the system opens the file instead.
        self.open_path(workspace_file_path, cx);
    }

    /// `useOpenInPreferredEditor`. A launch that fails (no editor installed)
    /// falls back to the system's default app for the file.
    fn open_in_editor(&self, target_path: &str, cx: &mut App) {
        let opened = crate::hooks::open_in_editor::EditorLauncherService::global().spawn_open(
            self.bus.handle(),
            target_path.to_owned(),
            cx,
        );
        let path = target_path.to_owned();
        cx.spawn(async move |cx| {
            if let Err(message) = opened.await {
                tracing::warn!(%message, path, "could not open the file in an editor");
                cx.update(|cx| cx.open_url(&format!("file://{path}")));
            }
        })
        .detach();
    }

    fn refresh(&self, cx: &mut App) {
        {
            let mut inner = self.shared.inner.lock();
            inner.generation += 1;
            inner.patches.clear();
            inner.previews.clear();
            inner.file_patches.clear();
            inner.refs.clear();
        }
        if let Some(notify) = self.shared.notifier.lock().clone() {
            notify(cx);
        }
    }

    fn set_notifier(&self, notify: Arc<dyn Fn(&mut App) + Send + Sync>) {
        *self.shared.notifier.lock() = Some(notify);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::test_fixtures::{from_json, v2_projection};

    fn checkpoint(id: &str, run: &str, ordinal: u64) -> serde_json::Value {
        json!({
            "id": id, "threadId": "thread-v2", "scopeId": "scope-1", "runId": run,
            "nodeId": "node-1", "parentCheckpointId": null, "ordinalWithinScope": ordinal,
            "appRunOrdinal": ordinal, "ref": format!("refs/t3/checkpoints/{id}"),
            "status": "ready",
            "files": [{"path": "a.txt", "kind": "modified", "additions": 1, "deletions": 0}],
            "capturedAt": "2026-06-20T00:00:00.000Z",
        })
    }

    #[test]
    fn the_checkpoint_query_context_carries_runs_scopes_and_checkpoints() {
        let mut contract = v2_projection();
        contract.checkpoints = vec![from_json(checkpoint("cp-1", "run-1", 1))];
        contract.runs = vec![crate::state::test_fixtures::v2_run("run-1", 1, "completed")];
        contract.checkpoint_scopes = vec![from_json(json!({
            "id": "scope-1", "threadId": "thread-v2", "runId": "run-1", "nodeId": "node-1",
            "parentScopeId": null, "providerThreadId": null, "kind": "root_run",
            "ordinalWithinParent": 0, "advancesAppRunCount": true, "cwd": "/work/tree",
            "createdAt": "2026-06-20T00:00:00.000Z",
        }))];
        let context = checkpoint_projection(&contract.into()).expect("context decodes");
        assert_eq!(context.runs.len(), 1);
        assert_eq!(context.checkpoint_scopes[0].cwd, "/work/tree");
        assert_eq!(context.checkpoints.len(), 1);
    }

    #[test]
    fn review_sources_convert_with_their_stats() {
        let source: ReviewDiffPreviewSource = from_json(json!({
            "id": "branch", "kind": "branch-range", "title": "Branch", "baseRef": "main",
            "headRef": "feature", "diff": "diff --git a/x b/x\n", "diffHash": "abc",
            "truncated": false,
            "files": [{"path": "x", "previousPath": null, "additions": 3, "deletions": 1}],
        }));
        let converted = diff_source(source);
        assert_eq!(converted.kind, GitSourceKind::BranchRange);
        assert_eq!(converted.base_ref.as_deref(), Some("main"));
        let files = converted.files.expect("stats");
        assert_eq!((files[0].additions, files[0].deletions), (3, 1));
    }
}
