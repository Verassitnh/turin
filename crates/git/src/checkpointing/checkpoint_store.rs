//! Port of upstream/apps/server/src/checkpointing/CheckpointStore.ts
//!
//! Upstream resolves a driver from `VcsDriverRegistry` on every call. This
//! crate has no registry, so the driver is chosen in [`CheckpointStore::new`].
//! `is_git_repository` matches `detect({ cwd, requestedKind: "git" }) !== null`
//! on that driver. Checkpoint methods require [`VcsDriver::supports_checkpoints`].

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use turin_contracts::vcs::{VcsError, VcsUnsupportedOperationError};

use crate::vcs_driver::{
    VcsCaptureCheckpointInput, VcsDeleteCheckpointRefsInput, VcsDiffCheckpointsInput, VcsDriver,
    VcsHasCheckpointInput, VcsListAuthoredPathsInput, VcsRestoreCheckpointInput,
};

pub struct CheckpointStore {
    driver: Arc<dyn VcsDriver>,
}

impl CheckpointStore {
    pub fn new(driver: Arc<dyn VcsDriver>) -> Self {
        Self { driver }
    }

    /// Check whether `cwd` is inside a Git worktree.
    pub async fn is_git_repository(&self, cwd: &str) -> Result<bool, VcsError> {
        Ok(self.driver.detect_repository(cwd).await?.is_some())
    }

    /// Capture a checkpoint commit and store it at the provided checkpoint ref.
    ///
    /// Uses an isolated temporary Git index and writes a hidden ref.
    pub async fn capture_checkpoint(
        &self,
        input: &VcsCaptureCheckpointInput,
    ) -> Result<(), VcsError> {
        self.ensure_checkpoints("CheckpointStore.captureCheckpoint")?;
        self.driver.capture_checkpoint(input).await
    }

    /// Check whether a checkpoint ref exists.
    pub async fn has_checkpoint_ref(
        &self,
        input: &VcsHasCheckpointInput,
    ) -> Result<bool, VcsError> {
        self.ensure_checkpoints("CheckpointStore.hasCheckpointRef")?;
        self.driver.has_checkpoint_ref(input).await
    }

    /// Restore workspace and staging state to a checkpoint.
    ///
    /// Optionally falls back to current `HEAD` when the checkpoint ref is missing.
    pub async fn restore_checkpoint(
        &self,
        input: &VcsRestoreCheckpointInput,
    ) -> Result<bool, VcsError> {
        self.ensure_checkpoints("CheckpointStore.restoreCheckpoint")?;
        self.driver.restore_checkpoint(input).await
    }

    /// Compute a diff between two checkpoint refs. Defaults to a full patch
    /// when the caller sets [`crate::vcs_driver::CheckpointDiffFormat::Patch`].
    ///
    /// Numstat output has NUL-delimited paths for file summaries.
    /// Can optionally treat a missing "from" ref as `HEAD`.
    pub async fn diff_checkpoints(
        &self,
        input: &VcsDiffCheckpointsInput,
    ) -> Result<String, VcsError> {
        self.ensure_checkpoints("CheckpointStore.diffCheckpoints")?;
        self.driver.diff_checkpoints(input).await
    }

    /// List paths changed by work done after the "from" checkpoint: uncommitted
    /// edits at either checkpoint, commits made after "from", and commits that
    /// left HEAD. Commits a pull, merge, or rebase brought in are older than
    /// "from", so their paths are not listed.
    ///
    /// Returns null when HEAD did not move or a checkpoint does not record HEAD.
    /// Then every changed path belongs to the turn.
    pub async fn list_authored_paths(
        &self,
        input: &VcsListAuthoredPathsInput,
    ) -> Result<Option<std::collections::HashSet<String>>, VcsError> {
        self.ensure_checkpoints("CheckpointStore.listAuthoredPaths")?;
        self.driver.list_authored_paths(input).await
    }

    /// Delete the provided checkpoint refs.
    ///
    /// Best-effort delete: missing refs are tolerated.
    pub async fn delete_checkpoint_refs(
        &self,
        input: &VcsDeleteCheckpointRefsInput,
    ) -> Result<(), VcsError> {
        self.ensure_checkpoints("CheckpointStore.deleteCheckpointRefs")?;
        self.driver.delete_checkpoint_refs(input).await
    }

    fn ensure_checkpoints(&self, operation: &str) -> Result<(), VcsError> {
        if self.driver.supports_checkpoints() {
            return Ok(());
        }
        let kind = self.driver.capabilities().kind;
        Err(VcsError::UnsupportedOperation(
            VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                operation: operation.to_owned(),
                kind,
                detail: format!("{kind} driver does not implement checkpoint operations."),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use turin_contracts::base_schemas::ThreadId;
    use turin_contracts::git::VcsInitInput;
    use turin_contracts::vcs::{
        VcsDriverCapabilities, VcsDriverKind, VcsError, VcsIgnoreClassifier, VcsListRemotesResult,
        VcsListWorkspaceFilesResult, VcsRepositoryIdentity, VcsUnsupportedOperationError,
    };

    use super::*;
    use crate::checkpointing::diffs::{TurnDiffFileSummary, parse_turn_diff_files_from_numstat};
    use crate::checkpointing::utils::checkpoint_ref_for_thread_turn;
    use crate::git_vcs_driver::GitVcsDriver;
    use crate::process::ProcessRunner;
    use crate::vcs_driver::{CheckpointDiffFormat, VcsDriver, VcsExecuteInput};
    use crate::vcs_process::{VcsProcess, VcsProcessOutput};

    fn store() -> CheckpointStore {
        CheckpointStore::new(Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))))
    }

    fn git(cwd: &Path, args: &[&str]) {
        git_env(cwd, args, &[]);
    }

    fn git_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .envs(env.iter().copied())
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_allow_failure(cwd: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .expect("spawn git");
    }

    fn init_repo(cwd: &Path) {
        git(cwd, &["init", "-b", "main"]);
        git(cwd, &["config", "user.email", "test@test.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        git(cwd, &["config", "commit.gpgsign", "false"]);
        std::fs::write(cwd.join("README.md"), "# test\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "initial commit"]);
    }

    fn commit_at(cwd: &Path, file: &str, contents: &str) {
        std::fs::write(cwd.join(file), contents).unwrap();
        git(cwd, &["add", "."]);
        git_env(
            cwd,
            &["commit", "-m", file],
            &[("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")],
        );
    }

    fn build_large_text(line_count: usize) -> String {
        let mut text = String::new();
        for index in 0..line_count {
            text.push_str(&format!("line {index:05}\n"));
        }
        text
    }

    fn thread_refs(
        name: &str,
        cwd: &Path,
    ) -> (
        String,
        turin_contracts::base_schemas::CheckpointRef,
        turin_contracts::base_schemas::CheckpointRef,
    ) {
        let thread_id = ThreadId::new(name).unwrap();
        (
            cwd.display().to_string(),
            checkpoint_ref_for_thread_turn(&thread_id, 0),
            checkpoint_ref_for_thread_turn(&thread_id, 1),
        )
    }

    struct UnsupportedDriver {
        capabilities: VcsDriverCapabilities,
    }

    #[async_trait]
    impl VcsDriver for UnsupportedDriver {
        fn capabilities(&self) -> &VcsDriverCapabilities {
            &self.capabilities
        }
        async fn execute(&self, _: VcsExecuteInput) -> Result<VcsProcessOutput, VcsError> {
            unimplemented!()
        }
        async fn detect_repository(
            &self,
            _: &str,
        ) -> Result<Option<VcsRepositoryIdentity>, VcsError> {
            unimplemented!()
        }
        async fn is_inside_work_tree(&self, _: &str) -> Result<bool, VcsError> {
            unimplemented!()
        }
        async fn list_workspace_files(
            &self,
            _: &str,
        ) -> Result<VcsListWorkspaceFilesResult, VcsError> {
            unimplemented!()
        }
        async fn list_remotes(&self, _: &str) -> Result<VcsListRemotesResult, VcsError> {
            unimplemented!()
        }
        async fn filter_ignored_paths(
            &self,
            _: &str,
            _: &[String],
        ) -> Result<Vec<String>, VcsError> {
            unimplemented!()
        }
        async fn init_repository(&self, _: &VcsInitInput) -> Result<(), VcsError> {
            unimplemented!()
        }
        fn supports_checkpoints(&self) -> bool {
            false
        }
        async fn capture_checkpoint(&self, _: &VcsCaptureCheckpointInput) -> Result<(), VcsError> {
            unimplemented!()
        }
        async fn has_checkpoint_ref(&self, _: &VcsHasCheckpointInput) -> Result<bool, VcsError> {
            unimplemented!()
        }
        async fn restore_checkpoint(
            &self,
            _: &VcsRestoreCheckpointInput,
        ) -> Result<bool, VcsError> {
            unimplemented!()
        }
        async fn diff_checkpoints(&self, _: &VcsDiffCheckpointsInput) -> Result<String, VcsError> {
            unimplemented!()
        }
        async fn list_authored_paths(
            &self,
            _: &VcsListAuthoredPathsInput,
        ) -> Result<Option<std::collections::HashSet<String>>, VcsError> {
            unimplemented!()
        }
        async fn delete_checkpoint_refs(
            &self,
            _: &VcsDeleteCheckpointRefsInput,
        ) -> Result<(), VcsError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn unsupported_driver_reports_the_checkpoint_operation() {
        let driver = UnsupportedDriver {
            capabilities: VcsDriverCapabilities {
                kind: VcsDriverKind::Jj,
                supports_worktrees: false,
                supports_bookmarks: true,
                supports_atomic_snapshot: false,
                supports_push_default_remote: false,
                ignore_classifier: VcsIgnoreClassifier::GitCompatibleFallback,
            },
        };
        let store = CheckpointStore::new(Arc::new(driver));
        let error = store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: "/tmp".into(),
                checkpoint_ref: turin_contracts::base_schemas::CheckpointRef::new(
                    "refs/t3/checkpoints/x/turn/0",
                )
                .unwrap(),
            })
            .await
            .unwrap_err();
        match error {
            VcsError::UnsupportedOperation(
                VcsUnsupportedOperationError::VcsUnsupportedOperationError {
                    operation,
                    kind,
                    detail,
                },
            ) => {
                assert_eq!(operation, "CheckpointStore.captureCheckpoint");
                assert_eq!(kind, VcsDriverKind::Jj);
                assert_eq!(
                    detail,
                    "jj driver does not implement checkpoint operations."
                );
            }
            other => panic!("expected unsupported operation, got {other}"),
        }
    }

    #[tokio::test]
    async fn is_git_repository_detects_a_repo_and_a_nested_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let store = store();
        let cwd = dir.path().display().to_string();
        assert!(!store.is_git_repository(&cwd).await.unwrap());
        init_repo(dir.path());
        assert!(store.is_git_repository(&cwd).await.unwrap());
        let nested = dir.path().join("packages").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(
            store
                .is_git_repository(&nested.display().to_string())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn diff_checkpoints_returns_full_oversized_patches_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("thread-checkpoint-store", dir.path());
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        std::fs::write(dir.path().join("README.md"), build_large_text(5_000)).unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let diff = store
            .diff_checkpoints(&VcsDiffCheckpointsInput {
                cwd,
                from_checkpoint_ref,
                to_checkpoint_ref,
                fallback_from_to_head: false,
                ignore_whitespace: true,
                format: CheckpointDiffFormat::Patch,
                file_paths: None,
            })
            .await
            .unwrap();
        assert!(diff.contains("diff --git"), "{diff}");
        assert!(!diff.contains("[truncated]"), "{diff}");
        assert!(diff.contains("+line 04999"), "{diff}");
    }

    #[tokio::test]
    async fn diff_checkpoints_keeps_patch_prefixes_when_noprefix_is_set() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        git(dir.path(), &["config", "diff.noprefix", "true"]);
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("thread-checkpoint-store-noprefix", dir.path());
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        std::fs::write(dir.path().join("README.md"), "# changed\n").unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let diff = store
            .diff_checkpoints(&VcsDiffCheckpointsInput {
                cwd,
                from_checkpoint_ref,
                to_checkpoint_ref,
                fallback_from_to_head: false,
                ignore_whitespace: false,
                format: CheckpointDiffFormat::Patch,
                file_paths: None,
            })
            .await
            .unwrap();
        assert!(
            diff.contains("diff --git a/README.md b/README.md"),
            "{diff}"
        );
    }

    #[tokio::test]
    async fn diff_checkpoints_can_hide_indentation_churn() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("thread-checkpoint-store-whitespace", dir.path());
        let component = dir.path().join("Component.tsx");
        std::fs::write(
            &component,
            [
                "export function View() {",
                "  return (",
                "    <section>",
                "      <h1>Title</h1>",
                "      <p>Body</p>",
                "    </section>",
                "  );",
                "}",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        std::fs::write(
            &component,
            [
                "export function View() {",
                "  return (",
                "    <section>",
                "      {isReady ? (",
                "        <div>",
                "          <h1>Title</h1>",
                "          <p>Body</p>",
                "        </div>",
                "      ) : null}",
                "    </section>",
                "  );",
                "}",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();

        let normal_diff = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                false,
                CheckpointDiffFormat::Patch,
                None,
            ))
            .await
            .unwrap();
        let whitespace_ignored_diff = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                true,
                CheckpointDiffFormat::Patch,
                None,
            ))
            .await
            .unwrap();
        assert!(normal_diff.contains("diff --git"));
        assert!(normal_diff.contains("-      <h1>Title</h1>"));
        assert!(normal_diff.contains("+          <h1>Title</h1>"));
        assert!(whitespace_ignored_diff.contains("diff --git"));
        assert!(whitespace_ignored_diff.contains("+      {isReady ? ("));
        assert!(whitespace_ignored_diff.contains("+        <div>"));
        assert!(!whitespace_ignored_diff.contains("-      <h1>Title</h1>"));
        assert!(!whitespace_ignored_diff.contains("+          <h1>Title</h1>"));

        for ignore_whitespace in [false, true] {
            let numstat = store
                .diff_checkpoints(&diff_input(
                    &cwd,
                    &from_checkpoint_ref,
                    &to_checkpoint_ref,
                    ignore_whitespace,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(
                parse_turn_diff_files_from_numstat(&numstat),
                vec![TurnDiffFileSummary {
                    path: "Component.tsx".into(),
                    previous_path: None,
                    additions: if ignore_whitespace { 4 } else { 6 },
                    deletions: if ignore_whitespace { 0 } else { 2 },
                }]
            );
        }
    }

    fn diff_input(
        cwd: &str,
        from_checkpoint_ref: &turin_contracts::base_schemas::CheckpointRef,
        to_checkpoint_ref: &turin_contracts::base_schemas::CheckpointRef,
        ignore_whitespace: bool,
        format: CheckpointDiffFormat,
        file_paths: Option<Vec<String>>,
    ) -> VcsDiffCheckpointsInput {
        VcsDiffCheckpointsInput {
            cwd: cwd.to_owned(),
            from_checkpoint_ref: from_checkpoint_ref.clone(),
            to_checkpoint_ref: to_checkpoint_ref.clone(),
            fallback_from_to_head: false,
            ignore_whitespace,
            format,
            file_paths,
        }
    }

    #[tokio::test]
    async fn numstat_counts_changes_whose_patch_would_be_huge() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("large-checkpoint-summary", dir.path());
        let line_count = 20_000u64;
        let file_path = dir.path().join("README.md");
        std::fs::write(
            &file_path,
            format!("{}\n", "before".repeat(50)).repeat(line_count as usize),
        )
        .unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        std::fs::write(
            &file_path,
            format!("{}\n", "after".repeat(60)).repeat(line_count as usize),
        )
        .unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let numstat = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                false,
                CheckpointDiffFormat::Numstat,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![TurnDiffFileSummary {
                path: "README.md".into(),
                previous_path: None,
                additions: line_count,
                deletions: line_count,
            }]
        );
        assert!(numstat.len() < 100, "{}", numstat.len());
    }

    #[tokio::test]
    async fn summaries_preserve_paths_and_leave_the_user_index_alone() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        git(dir.path(), &["config", "diff.renames", "copies"]);
        let store = store();
        let thread_id = ThreadId::new("checkpoint-summary-paths").unwrap();
        let cwd = dir.path().display().to_string();
        let baseline = checkpoint_ref_for_thread_turn(&thread_id, 0);
        let first_turn = checkpoint_ref_for_thread_turn(&thread_id, 1);
        let second_turn = checkpoint_ref_for_thread_turn(&thread_id, 2);
        let copied_text: String = (0..20)
            .map(|index| format!("copy line {index}\n"))
            .collect();
        let renamed_path = "renamed\tcafé\nname.txt";
        let added_path = "new\tfile\n名.txt";
        for (path, contents) in [
            ("copy-source.txt", copied_text.as_str()),
            ("deleted.txt", "delete me\n"),
            ("rename-old.txt", "before\nkeep one\nkeep two\nkeep three\n"),
            ("binary.bin", "\0before"),
        ] {
            std::fs::write(dir.path().join(path), contents).unwrap();
        }
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: baseline.clone(),
            })
            .await
            .unwrap();

        std::fs::rename(
            dir.path().join("rename-old.txt"),
            dir.path().join(renamed_path),
        )
        .unwrap();
        std::fs::remove_file(dir.path().join("deleted.txt")).unwrap();
        for (path, contents) in [
            ("copy-source.txt", format!("{copied_text}one more\n")),
            ("copied.txt", copied_text.clone()),
            (
                renamed_path,
                "after\nkeep one\nkeep two\nkeep three\n".into(),
            ),
            ("binary.bin", "\0after".into()),
            ("empty.txt", String::new()),
            (added_path, "first\nsecond\n".into()),
        ] {
            std::fs::write(dir.path().join(path), contents).unwrap();
        }
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: first_turn.clone(),
            })
            .await
            .unwrap();
        let user_index = std::fs::read(dir.path().join(".git/index")).unwrap();
        let first_summary = parse_turn_diff_files_from_numstat(
            &store
                .diff_checkpoints(&diff_input(
                    &cwd,
                    &baseline,
                    &first_turn,
                    false,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
                .unwrap(),
        );
        let mut expected_files = vec![
            file("binary.bin", None, 0, 0),
            file("copied.txt", Some("copy-source.txt"), 0, 0),
            file("copy-source.txt", None, 1, 0),
            file("deleted.txt", None, 0, 1),
            file("empty.txt", None, 0, 0),
            file(added_path, None, 2, 0),
            file(renamed_path, Some("rename-old.txt"), 1, 1),
        ];
        expected_files.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(first_summary, expected_files);

        std::fs::remove_file(dir.path().join("empty.txt")).unwrap();
        std::fs::write(dir.path().join("copy-source.txt"), "replacement\n").unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: second_turn.clone(),
            })
            .await
            .unwrap();
        let second_summary = parse_turn_diff_files_from_numstat(
            &store
                .diff_checkpoints(&diff_input(
                    &cwd,
                    &first_turn,
                    &second_turn,
                    false,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
                .unwrap(),
        );
        assert_eq!(
            second_summary,
            vec![
                file("copy-source.txt", None, 1, 21),
                file("empty.txt", None, 0, 0),
            ]
        );

        let inclusive_summary = parse_turn_diff_files_from_numstat(
            &store
                .diff_checkpoints(&diff_input(
                    &cwd,
                    &baseline,
                    &second_turn,
                    false,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
                .unwrap(),
        );
        let expected_inclusive = expected_files
            .into_iter()
            .filter(|file| file.path != "empty.txt")
            .map(|mut file| {
                if file.path == "copy-source.txt" {
                    file.additions = 1;
                    file.deletions = 20;
                }
                file
            })
            .collect::<Vec<_>>();
        assert_eq!(inclusive_summary, expected_inclusive);
        assert_eq!(
            store
                .diff_checkpoints(&diff_input(
                    &cwd,
                    &baseline,
                    &baseline,
                    false,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
                .unwrap(),
            ""
        );
        assert_eq!(
            std::fs::read(dir.path().join(".git/index")).unwrap(),
            user_index
        );
    }

    fn file(
        path: &str,
        previous_path: Option<&str>,
        additions: u64,
        deletions: u64,
    ) -> TurnDiffFileSummary {
        TurnDiffFileSummary {
            path: path.to_owned(),
            previous_path: previous_path.map(str::to_owned),
            additions,
            deletions,
        }
    }

    #[tokio::test]
    async fn missing_baseline_uses_head_only_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("checkpoint-summary-fallback", dir.path());
        std::fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let error = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                false,
                CheckpointDiffFormat::Numstat,
                None,
            ))
            .await
            .unwrap_err();
        assert!(matches!(error, VcsError::Exit(_)), "{error}");
        let mut input = diff_input(
            &cwd,
            &from_checkpoint_ref,
            &to_checkpoint_ref,
            false,
            CheckpointDiffFormat::Numstat,
            None,
        );
        input.fallback_from_to_head = true;
        let numstat = store.diff_checkpoints(&input).await.unwrap();
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![file("README.md", None, 1, 1)]
        );
    }

    #[tokio::test]
    async fn diff_can_be_limited_to_paths_from_a_subdirectory_workspace() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let store = store();
        let thread_id = ThreadId::new("checkpoint-file-paths-subdirectory").unwrap();
        let cwd = app.display().to_string();
        let from_checkpoint_ref = checkpoint_ref_for_thread_turn(&thread_id, 0);
        let to_checkpoint_ref = checkpoint_ref_for_thread_turn(&thread_id, 1);
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        std::fs::write(app.join("kept.txt"), "kept\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# hidden\n").unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let patch = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                false,
                CheckpointDiffFormat::Patch,
                Some(vec!["app/kept.txt".into()]),
            ))
            .await
            .unwrap();
        assert!(patch.contains("+kept"), "{patch}");
        assert!(!patch.contains("hidden"), "{patch}");
    }

    #[tokio::test]
    async fn authored_paths_leave_out_a_merged_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("checkpoint-authored-paths", dir.path());
        git(dir.path(), &["checkout", "-b", "upstream"]);
        commit_at(dir.path(), "upstream.txt", "upstream\n");
        commit_at(dir.path(), "shared.txt", "upstream\n");
        git(dir.path(), &["checkout", "-"]);
        commit_at(dir.path(), "shared.txt", "ours\n");
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let refs = VcsListAuthoredPathsInput {
            cwd: cwd.clone(),
            from_checkpoint_ref: from_checkpoint_ref.clone(),
            to_checkpoint_ref: to_checkpoint_ref.clone(),
        };
        assert!(store.list_authored_paths(&refs).await.unwrap().is_none());

        git_allow_failure(dir.path(), &["merge", "upstream", "--no-edit"]);
        std::fs::write(dir.path().join("shared.txt"), "upstream\n").unwrap();
        git(dir.path(), &["commit", "-am", "merge upstream"]);
        std::fs::write(dir.path().join("README.md"), "# edited\n").unwrap();
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let numstat = store
            .diff_checkpoints(&diff_input(
                &cwd,
                &from_checkpoint_ref,
                &to_checkpoint_ref,
                false,
                CheckpointDiffFormat::Numstat,
                None,
            ))
            .await
            .unwrap();
        let mut paths = parse_turn_diff_files_from_numstat(&numstat)
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "README.md".to_owned(),
                "shared.txt".to_owned(),
                "upstream.txt".to_owned()
            ]
        );
        let authored = store.list_authored_paths(&refs).await.unwrap().unwrap();
        assert_eq!(
            authored,
            ["README.md".to_owned(), "shared.txt".to_owned()]
                .into_iter()
                .collect()
        );
    }

    #[tokio::test]
    async fn authored_paths_leave_out_a_rebase_onto_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let store = store();
        let (cwd, from_checkpoint_ref, to_checkpoint_ref) =
            thread_refs("checkpoint-authored-paths-rebase", dir.path());
        git(dir.path(), &["checkout", "-b", "upstream"]);
        commit_at(dir.path(), "upstream.txt", "upstream.txt\n");
        git(dir.path(), &["checkout", "-"]);
        commit_at(dir.path(), "feature.txt", "feature.txt\n");
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: from_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        git(dir.path(), &["rebase", "upstream"]);
        store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.clone(),
                checkpoint_ref: to_checkpoint_ref.clone(),
            })
            .await
            .unwrap();
        let authored = store
            .list_authored_paths(&VcsListAuthoredPathsInput {
                cwd,
                from_checkpoint_ref,
                to_checkpoint_ref,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(authored, ["feature.txt".to_owned()].into_iter().collect());
    }
}
