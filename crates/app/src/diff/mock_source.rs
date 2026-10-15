//! Fixture-backed [`DiffSource`]: turn checkpoints, a branch comparison and
//! the working tree, derived from [`super::mock_fixtures`].
//!
//! Dev switches for screenshots: `TURIN_DIFF_LOADING=1` keeps every query
//! pending, `TURIN_DIFF_LAZY=1` serves the branch comparison as a truncated
//! source with per-file patches (the lazy path of `useReviewFilePatches`).

use std::sync::Arc;

use gpui_kit::App;
use parking_lot::Mutex;

use super::base_ref_choices::VcsRef;
use super::diff_panel_store::ScopedThreadRef;
use super::diff_rendering::{RenderablePatch, RenderablePatchOptions, get_diff_line_stat, get_renderable_patch};
use super::mock_fixtures::{FixtureFile, branch_changes, turn_sets, uncommitted_changes};
use super::pierre::hydrate_partial_diff::unified_hunks;
use super::pierre::types::ChangeType;
use super::view_model::{
    DiffContext, DiffFileContents, DiffRefs, DiffSource, GitDiffFileStat, GitDiffPreview,
    GitDiffSource, GitSourceKind, Loadable,
};
use crate::components::chat::timeline::types::TurnDiffSummary;

const CWD: &str = "/Users/dev/code/turin";

fn object_id(seed: &str) -> String {
    let hash = super::diff_rendering::fnv1a32(seed, 0x811c_9dc5, 0x0100_0193);
    format!("{hash:08x}")[..7].to_string()
}

/// `git diff` text for one fixture file.
pub fn git_patch(file: &FixtureFile) -> String {
    let path = file.path;
    let previous = file.previous_path.unwrap_or(path);
    let old_id = if file.old.is_some() {
        object_id(&format!("old:{previous}"))
    } else {
        "0000000".into()
    };
    let new_id = if file.new.is_some() {
        object_id(&format!("new:{path}"))
    } else {
        "0000000".into()
    };
    let mut out = format!("diff --git a/{previous} b/{path}\n");
    match (file.old, file.new) {
        (None, _) => out.push_str("new file mode 100644\n"),
        (_, None) => out.push_str("deleted file mode 100644\n"),
        _ if file.previous_path.is_some() => {
            out.push_str(&format!(
                "similarity index 82%\nrename from {previous}\nrename to {path}\n"
            ));
        }
        _ => {}
    }
    if file.binary {
        out.push_str(&format!(
            "index {old_id}..{new_id}\nBinary files {} and {} differ\n",
            if file.old.is_some() { format!("a/{previous}") } else { "/dev/null".into() },
            if file.new.is_some() { format!("b/{path}") } else { "/dev/null".into() },
        ));
        return out;
    }
    if file.old.is_some() && file.new.is_some() && file.previous_path.is_none() {
        out.push_str(&format!("index {old_id}..{new_id} 100644\n"));
    } else {
        out.push_str(&format!("index {old_id}..{new_id}\n"));
    }
    out.push_str(&format!(
        "--- {}\n+++ {}\n",
        if file.old.is_some() { format!("a/{previous}") } else { "/dev/null".into() },
        if file.new.is_some() { format!("b/{path}") } else { "/dev/null".into() },
    ));
    out.push_str(&unified_hunks(file.old.unwrap_or(""), file.new.unwrap_or(""), Some(3)));
    out
}

fn patch_for(files: &[FixtureFile]) -> String {
    files.iter().map(git_patch).collect()
}

fn stats_for(files: &[FixtureFile]) -> Vec<GitDiffFileStat> {
    files
        .iter()
        .map(|file| {
            let (additions, deletions) = match get_renderable_patch(
                Some(&git_patch(file)),
                "mock",
                RenderablePatchOptions::default(),
            ) {
                Some(RenderablePatch::Files { files, .. }) => {
                    let stat = get_diff_line_stat(&files);
                    (stat.additions, stat.deletions)
                }
                _ => (0, 0),
            };
            GitDiffFileStat {
                path: file.path.to_string(),
                previous_path: file.previous_path.map(str::to_string),
                additions,
                deletions,
            }
        })
        .collect()
}

fn vcs_ref(name: &str, remote: Option<&str>) -> VcsRef {
    VcsRef {
        name: name.into(),
        current: false,
        is_default: name == "main" || name == "origin/main",
        is_remote: remote.is_some(),
        remote_name: remote.map(str::to_string),
        worktree_path: None,
    }
}

pub struct MockDiffSource {
    turns: Vec<TurnDiffSummary>,
    thread: ScopedThreadRef,
    notifier: Mutex<Option<Arc<dyn Fn(&mut App) + Send + Sync>>>,
    loading: bool,
    lazy: bool,
}

impl MockDiffSource {
    pub fn new(turns: Vec<TurnDiffSummary>) -> Self {
        let env_flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
        Self {
            turns,
            thread: ScopedThreadRef::new("environment-local", "thread-startup"),
            notifier: Mutex::new(None),
            loading: env_flag("TURIN_DIFF_LOADING"),
            lazy: env_flag("TURIN_DIFF_LAZY"),
        }
    }

    pub fn thread(&self) -> &ScopedThreadRef {
        &self.thread
    }

    fn working_set(&self, kind: GitSourceKind) -> Vec<FixtureFile> {
        match kind {
            GitSourceKind::WorkingTree => uncommitted_changes(),
            GitSourceKind::BranchRange => branch_changes(),
        }
    }
}

impl DiffSource for MockDiffSource {
    fn context(&self) -> DiffContext {
        DiffContext {
            thread: Some(self.thread.clone()),
            active_cwd: Some(CWD.into()),
            repository_root: Some(CWD.into()),
            is_git_repo: true,
            can_read_files: true,
        }
    }

    fn turns(&self) -> Vec<TurnDiffSummary> {
        self.turns.clone()
    }

    fn checkpoint_diff(&self, _from: u64, to: u64, ignore_whitespace: bool) -> Loadable<String> {
        if self.loading {
            return Loadable::Pending;
        }
        let sets = turn_sets();
        let set = &sets[(to.max(1) as usize - 1) % sets.len()];
        let patch = patch_for(set);
        let _ = ignore_whitespace;
        Loadable::Ready(patch)
    }

    fn git_diff_preview(&self, base_ref: Option<&str>, _ignore_whitespace: bool) -> Loadable<GitDiffPreview> {
        if self.loading {
            return Loadable::Pending;
        }
        let base = base_ref.unwrap_or("origin/main").to_string();
        let branch = branch_changes();
        let working = uncommitted_changes();
        let lazy = self.lazy;
        Loadable::Ready(GitDiffPreview {
            cwd: CWD.into(),
            generated_at: "2026-10-15T12:00:00.000Z".into(),
            sources: vec![
                GitDiffSource {
                    kind: GitSourceKind::BranchRange,
                    base_ref: Some(base),
                    head_ref: Some("feature/diff-panel".into()),
                    diff: if lazy { String::new() } else { patch_for(&branch) },
                    diff_hash: object_id("branch"),
                    truncated: lazy,
                    files: Some(stats_for(&branch)),
                },
                GitDiffSource {
                    kind: GitSourceKind::WorkingTree,
                    base_ref: None,
                    head_ref: Some("feature/diff-panel".into()),
                    diff: patch_for(&working),
                    diff_hash: object_id("working"),
                    truncated: false,
                    files: Some(stats_for(&working)),
                },
            ],
        })
    }

    fn git_file_patch(
        &self,
        kind: GitSourceKind,
        base_ref: Option<&str>,
        file: &GitDiffFileStat,
        _ignore_whitespace: bool,
    ) -> Loadable<GitDiffSource> {
        let Some(fixture) = self
            .working_set(kind)
            .into_iter()
            .find(|candidate| candidate.path == file.path)
        else {
            return Loadable::Failed("Diff no longer available. Refresh the comparison.".into());
        };
        Loadable::Ready(GitDiffSource {
            kind,
            base_ref: base_ref.map(str::to_string),
            head_ref: Some("feature/diff-panel".into()),
            diff: git_patch(&fixture),
            diff_hash: object_id(&file.path),
            truncated: false,
            files: Some(vec![file.clone()]),
        })
    }

    fn diff_file_contents(
        &self,
        kind: GitSourceKind,
        _base_ref: Option<&str>,
        _head_ref: Option<&str>,
        _old_path: &str,
        new_path: &str,
        _change_type: ChangeType,
    ) -> Result<DiffFileContents, String> {
        self.working_set(kind)
            .into_iter()
            .find(|file| file.path == new_path)
            .map(|file| DiffFileContents {
                old_contents: file.old.unwrap_or("").to_string(),
                new_contents: file.new.unwrap_or("").to_string(),
            })
            .ok_or_else(|| format!("No contents for {new_path}"))
    }

    fn list_refs(&self, query: &str) -> DiffRefs {
        let query = query.trim().to_lowercase();
        let keep = |reference: &VcsRef| query.is_empty() || reference.name.to_lowercase().contains(&query);
        DiffRefs {
            local: ["main", "develop", "release/1.4"]
                .iter()
                .map(|name| vcs_ref(name, None))
                .filter(keep)
                .collect(),
            remote: [
                ("origin/main", "origin"),
                ("origin/develop", "origin"),
                ("upstream/main", "upstream"),
                ("origin/release/2.0", "origin"),
            ]
            .iter()
            .map(|(name, remote)| vcs_ref(name, Some(remote)))
            .filter(keep)
            .collect(),
        }
    }

    fn open_in_thread_file_viewer(&self, thread: &ScopedThreadRef, path: &str, _: &mut App) {
        tracing::info!(thread = %thread.key(), path, "mock: open diff file in thread file viewer");
    }

    fn open_in_editor(&self, target_path: &str, _: &mut App) {
        tracing::info!(target_path, "mock: open diff file in editor");
    }

    fn refresh(&self, cx: &mut App) {
        tracing::info!("mock: refresh diff");
        if let Some(notify) = self.notifier.lock().clone() {
            notify(cx);
        }
    }

    fn set_notifier(&self, notify: Arc<dyn Fn(&mut App) + Send + Sync>) {
        *self.notifier.lock() = Some(notify);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::diff_rendering::resolve_file_diff_path;

    fn parsed(patch: &str) -> Vec<crate::diff::pierre::types::FileDiffMetadata> {
        match get_renderable_patch(Some(patch), "t", RenderablePatchOptions::default()) {
            Some(RenderablePatch::Files { files, .. }) => files,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fixture_patches_parse_with_the_expected_change_types() {
        let files = parsed(&patch_for(&branch_changes()));
        let types: Vec<(String, ChangeType)> = files
            .iter()
            .map(|f| (resolve_file_diff_path(f), f.r#type))
            .collect();
        assert!(types.contains(&("README.md".into(), ChangeType::Change)));
        assert!(types.contains(&("apps/web/legacy/unused.js".into(), ChangeType::Deleted)));
        assert!(types.contains(&("apps/web/src/lib/names.ts".into(), ChangeType::RenameChanged)));
        assert!(types.contains(&("apps/web/src/lib/diffCollapse.ts".into(), ChangeType::New)));
        let binary = files.iter().find(|f| f.name.ends_with("logo.png")).unwrap();
        assert!(binary.hunks.is_empty());
    }

    #[test]
    fn the_panel_fixture_has_separate_hunks_with_hidden_context() {
        let files = parsed(&git_patch(&branch_changes().into_iter().find(|f| f.path.ends_with("DiffPanel.tsx")).unwrap()));
        let hunks = &files[0].hunks;
        assert!(hunks.len() >= 3, "{}", hunks.len());
        assert!(hunks.iter().skip(1).all(|h| h.collapsed_before > 0));
    }

    #[test]
    fn stats_match_the_patches() {
        let stats = stats_for(&branch_changes());
        let readme = stats.iter().find(|s| s.path == "README.md").unwrap();
        assert_eq!((readme.additions, readme.deletions), (2, 0));
    }
}
