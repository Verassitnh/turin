//! Port of upstream/apps/server/src/checkpointing/CheckpointDiffQuery.ts
//!
//! Orchestration `getCheckpointContext` is not in this crate. [`CheckpointQueryContext`]
//! is the seam: one lookup returns the thread shell (checkpoint scopes and cwd),
//! checkpoint refs, and the run turn counts those refs belong to.

#![allow(clippy::result_large_err)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use turin_contracts::base_schemas::{
    CheckpointRef, CheckpointScopeId, NonNegativeInt, PositiveInt, RunId, ThreadId,
};
use turin_contracts::checkpoint_diff::{
    OrchestrationGetFullThreadDiffInput, OrchestrationGetFullThreadDiffResult,
    OrchestrationGetTurnDiffInput, OrchestrationGetTurnDiffResult,
};

use crate::checkpointing::checkpoint_store::CheckpointStore;
use crate::checkpointing::diffs::{
    TurnDiffFileSummary, is_git_import, parse_turn_diff_files_from_numstat,
};
use crate::checkpointing::errors::{
    CheckpointDiffOperation, CheckpointDiffResultInvalidError, CheckpointEndpoint,
    CheckpointRefUnavailableError, CheckpointServiceError, CheckpointThreadNotFoundError,
    CheckpointTurnRangeUnavailableError, CheckpointWorkspacePathMissingError,
};
use crate::checkpointing::utils::checkpoint_ref_for_scope_ordinal;
use crate::vcs_driver::{CheckpointDiffFormat, VcsDiffCheckpointsInput, VcsListAuthoredPathsInput};

/// Windows limits a command line to 32,767 characters. Leave room for the rest of git's arguments.
const MAX_PATHSPEC_CHARS: usize = 24_000;

/// Seam for orchestration `ThreadManagementService.getCheckpointContext`.
/// Any error is reported as [`CheckpointThreadNotFoundError`].
#[async_trait]
pub trait CheckpointQueryContext: Send + Sync {
    async fn get_checkpoint_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<CheckpointProjection, CheckpointContextError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("checkpoint context unavailable")]
pub struct CheckpointContextError;

/// Seam: upstream `ProjectionCheckpointContext` (`orchestration-v2/ProjectionStore.ts`).
/// Field names match that slice. Run status, scope kind, and checkpoint status mirror
/// the orchestration enums without importing that module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointProjection {
    pub runs: Vec<CheckpointProjectionRun>,
    pub checkpoint_scopes: Vec<CheckpointProjectionScope>,
    pub checkpoints: Vec<CheckpointProjectionCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointProjectionRun {
    pub id: RunId,
    pub ordinal: NonNegativeInt,
    pub status: CheckpointProjectionRunStatus,
}

/// Mirrors `OrchestrationV2RunStatus` (`snake_case` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointProjectionRunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointProjectionScope {
    pub id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub kind: CheckpointProjectionScopeKind,
    /// Upstream brands cwd as `TrimmedNonEmptyString`.
    pub cwd: String,
}

/// Mirrors `OrchestrationV2CheckpointScopeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointProjectionScopeKind {
    RootRun,
    Subagent,
    Tool,
    ProviderThread,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointProjectionCheckpoint {
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    /// Upstream brands this `PositiveInt`. Null is not a turn.
    pub app_run_ordinal: Option<PositiveInt>,
    pub status: CheckpointProjectionStatus,
    #[serde(rename = "ref")]
    pub ref_: CheckpointRef,
}

/// Mirrors `OrchestrationV2CheckpointStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointProjectionStatus {
    Ready,
    Missing,
    Error,
    Stale,
}

pub struct CheckpointDiffQuery {
    context: Arc<dyn CheckpointQueryContext>,
    store: Arc<CheckpointStore>,
}

impl CheckpointDiffQuery {
    pub fn new(context: Arc<dyn CheckpointQueryContext>, store: Arc<CheckpointStore>) -> Self {
        Self { context, store }
    }

    /// Read the patch diff for a single turn checkpoint transition.
    ///
    /// Verifies checkpoint availability in both projection state and filesystem.
    pub async fn get_turn_diff(
        &self,
        input: &OrchestrationGetTurnDiffInput,
    ) -> Result<OrchestrationGetTurnDiffResult, CheckpointServiceError> {
        const OPERATION: CheckpointDiffOperation = CheckpointDiffOperation::GetTurnDiff;
        let ignore_whitespace = input.ignore_whitespace.unwrap_or(true);
        if input.from_turn_count == input.to_turn_count {
            return finish_turn_diff(OPERATION, turn_diff_result(input, String::new()));
        }

        let projection = self
            .context
            .get_checkpoint_context(&input.thread_id)
            .await
            .map_err(|_| CheckpointThreadNotFoundError {
                operation: OPERATION,
                thread_id: input.thread_id.clone(),
            })?;
        let completed_run_ids = projection
            .runs
            .iter()
            .filter(|run| run.status == CheckpointProjectionRunStatus::Completed)
            .map(|run| run.id.clone())
            .collect::<HashSet<_>>();
        let ready_checkpoints = projection
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.status == CheckpointProjectionStatus::Ready
                    && checkpoint.app_run_ordinal.is_some()
                    && checkpoint
                        .run_id
                        .as_ref()
                        .is_some_and(|run_id| completed_run_ids.contains(run_id))
            })
            .collect::<Vec<_>>();
        let max_turn_count = ready_checkpoints.iter().fold(0, |max, checkpoint| {
            max.max(
                checkpoint
                    .app_run_ordinal
                    .map(PositiveInt::get)
                    .unwrap_or(0),
            )
        });
        if input.to_turn_count > max_turn_count {
            return Err(CheckpointTurnRangeUnavailableError {
                operation: OPERATION,
                thread_id: input.thread_id.clone(),
                requested_turn_count: input.to_turn_count,
                available_turn_count: max_turn_count,
            }
            .into());
        }

        let Some(to_checkpoint) = ready_checkpoints.iter().find(|checkpoint| {
            checkpoint.app_run_ordinal.map(PositiveInt::get) == Some(input.to_turn_count)
        }) else {
            return Err(CheckpointRefUnavailableError {
                operation: OPERATION,
                thread_id: input.thread_id.clone(),
                turn_count: input.to_turn_count,
                checkpoint: CheckpointEndpoint::To,
            }
            .into());
        };
        let Some(to_scope) = projection
            .checkpoint_scopes
            .iter()
            .find(|scope| scope.id == to_checkpoint.scope_id)
        else {
            return Err(CheckpointWorkspacePathMissingError {
                operation: OPERATION,
                thread_id: input.thread_id.clone(),
            }
            .into());
        };

        // The root scope is shared by every run in this thread. Its runId
        // tracks the latest owner, while ordinal zero stays the baseline.
        let from_checkpoint_ref = if input.from_turn_count == 0 {
            let Some(first_scope) = projection
                .checkpoint_scopes
                .iter()
                .find(|scope| scope.kind == CheckpointProjectionScopeKind::RootRun)
            else {
                return Err(missing_ref(OPERATION, input, CheckpointEndpoint::From));
            };
            checkpoint_ref_for_scope_ordinal(&first_scope.id, 0)
        } else {
            let Some(from_checkpoint) = ready_checkpoints.iter().find(|checkpoint| {
                checkpoint.app_run_ordinal.map(PositiveInt::get) == Some(input.from_turn_count)
            }) else {
                return Err(missing_ref(OPERATION, input, CheckpointEndpoint::From));
            };
            from_checkpoint.ref_.clone()
        };

        let diff = self
            .load_filtered_diff(
                &to_scope.cwd,
                &from_checkpoint_ref,
                &to_checkpoint.ref_,
                ignore_whitespace,
            )
            .await?;
        finish_turn_diff(OPERATION, turn_diff_result(input, diff))
    }

    /// Read the full patch diff across a thread range of checkpoints.
    ///
    /// Uses turn-diff semantics with `fromTurnCount = 0`.
    pub async fn get_full_thread_diff(
        &self,
        input: &OrchestrationGetFullThreadDiffInput,
    ) -> Result<OrchestrationGetFullThreadDiffResult, CheckpointServiceError> {
        const OPERATION: CheckpointDiffOperation = CheckpointDiffOperation::GetFullThreadDiff;
        let ignore_whitespace = input.ignore_whitespace.unwrap_or(true);
        if input.to_turn_count == 0 {
            return finish_turn_diff(
                OPERATION,
                turin_contracts::checkpoint_diff::ThreadTurnDiff {
                    thread_id: input.thread_id.clone(),
                    from_turn_count: 0,
                    to_turn_count: 0,
                    diff: String::new(),
                },
            );
        }
        let turn_diff = self
            .get_turn_diff(&OrchestrationGetTurnDiffInput {
                thread_id: input.thread_id.clone(),
                from_turn_count: 0,
                to_turn_count: input.to_turn_count,
                ignore_whitespace: Some(ignore_whitespace),
            })
            .await?;
        finish_turn_diff(OPERATION, turn_diff)
    }

    async fn load_filtered_diff(
        &self,
        cwd: &str,
        from_checkpoint_ref: &CheckpointRef,
        to_checkpoint_ref: &CheckpointRef,
        ignore_whitespace: bool,
    ) -> Result<String, CheckpointServiceError> {
        let paths = VcsListAuthoredPathsInput {
            cwd: cwd.to_owned(),
            from_checkpoint_ref: from_checkpoint_ref.clone(),
            to_checkpoint_ref: to_checkpoint_ref.clone(),
        };
        // Leave out files that a pull, merge, or rebase brought in, matching the turn's file summary.
        // Filtering is optional: when it cannot run, the complete diff still loads.
        let authored_paths = self
            .store
            .list_authored_paths(&paths)
            .await
            .unwrap_or_default();
        let files = if authored_paths.is_none() {
            Vec::new()
        } else {
            match self
                .store
                .diff_checkpoints(&diff_input(
                    cwd,
                    from_checkpoint_ref,
                    to_checkpoint_ref,
                    ignore_whitespace,
                    CheckpointDiffFormat::Numstat,
                    None,
                ))
                .await
            {
                Ok(numstat) => parse_turn_diff_files_from_numstat(&numstat),
                Err(_) => Vec::new(),
            }
        };
        let retained_files = files
            .iter()
            .filter(|file| !is_git_import(file, authored_paths.as_ref()))
            .cloned()
            .collect::<Vec<_>>();
        // Select retained paths before generating a patch, so imported bulk cannot exhaust its output limit.
        if retained_files.len() == files.len() {
            return Ok(self
                .store
                .diff_checkpoints(&diff_input(
                    cwd,
                    from_checkpoint_ref,
                    to_checkpoint_ref,
                    ignore_whitespace,
                    CheckpointDiffFormat::Patch,
                    None,
                ))
                .await?);
        }

        let batches = batch_file_paths(&retained_files);
        let cwd = cwd.to_owned();
        let from_checkpoint_ref = from_checkpoint_ref.clone();
        let to_checkpoint_ref = to_checkpoint_ref.clone();
        let store = Arc::clone(&self.store);
        let mut stream = stream::iter(batches)
            .map(move |file_paths| {
                let store = Arc::clone(&store);
                let cwd = cwd.clone();
                let from_checkpoint_ref = from_checkpoint_ref.clone();
                let to_checkpoint_ref = to_checkpoint_ref.clone();
                async move {
                    store
                        .diff_checkpoints(&diff_input(
                            &cwd,
                            &from_checkpoint_ref,
                            &to_checkpoint_ref,
                            ignore_whitespace,
                            CheckpointDiffFormat::Patch,
                            Some(file_paths),
                        ))
                        .await
                }
            })
            .buffered(4)
            .boxed();
        let mut parts = Vec::new();
        while let Some(patch) = stream.next().await {
            parts.push(patch?);
        }
        Ok(parts.join(""))
    }
}

fn diff_input(
    cwd: &str,
    from_checkpoint_ref: &CheckpointRef,
    to_checkpoint_ref: &CheckpointRef,
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

fn turn_diff_result(
    input: &OrchestrationGetTurnDiffInput,
    diff: String,
) -> OrchestrationGetTurnDiffResult {
    turin_contracts::checkpoint_diff::ThreadTurnDiff {
        thread_id: input.thread_id.clone(),
        from_turn_count: input.from_turn_count,
        to_turn_count: input.to_turn_count,
        diff,
    }
}

fn finish_turn_diff(
    operation: CheckpointDiffOperation,
    diff: OrchestrationGetTurnDiffResult,
) -> Result<OrchestrationGetTurnDiffResult, CheckpointServiceError> {
    if diff.from_turn_count <= diff.to_turn_count {
        Ok(diff)
    } else {
        Err(CheckpointDiffResultInvalidError {
            operation,
            thread_id: diff.thread_id,
        }
        .into())
    }
}

fn missing_ref(
    operation: CheckpointDiffOperation,
    input: &OrchestrationGetTurnDiffInput,
    checkpoint: CheckpointEndpoint,
) -> CheckpointServiceError {
    CheckpointRefUnavailableError {
        operation,
        thread_id: input.thread_id.clone(),
        turn_count: input.from_turn_count,
        checkpoint,
    }
    .into()
}

/// Splits files into pathspec lists that each fit on one git command line. Renames keep both paths together.
fn batch_file_paths(files: &[TurnDiffFileSummary]) -> Vec<Vec<String>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut length = 0usize;
    for file in files {
        let paths = match &file.previous_path {
            None => vec![file.path.clone()],
            Some(previous) => vec![previous.clone(), file.path.clone()],
        };
        // Each pathspec also carries a `:(top,literal)` prefix and a separator.
        // JavaScript `path.length` counts UTF-16 code units.
        let size = paths
            .iter()
            .map(|path| js_string_len(path) + 16)
            .sum::<usize>();
        if !batch.is_empty() && length + size > MAX_PATHSPEC_CHARS {
            batches.push(std::mem::take(&mut batch));
            length = 0;
        }
        batch.extend(paths);
        length += size;
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

fn js_string_len(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::vcs_driver::{
        VcsCaptureCheckpointInput, VcsDeleteCheckpointRefsInput, VcsDriver, VcsExecuteInput,
        VcsHasCheckpointInput, VcsRestoreCheckpointInput,
    };
    use crate::vcs_process::VcsProcessOutput;
    use turin_contracts::git::VcsInitInput;
    use turin_contracts::vcs::{
        VcsDriverCapabilities, VcsDriverKind, VcsError, VcsIgnoreClassifier, VcsListRemotesResult,
        VcsListWorkspaceFilesResult, VcsRepositoryIdentity,
    };

    fn summary(path: &str, previous: Option<&str>) -> TurnDiffFileSummary {
        TurnDiffFileSummary {
            path: path.to_owned(),
            previous_path: previous.map(str::to_owned),
            additions: 0,
            deletions: 0,
        }
    }

    #[test]
    fn batch_file_paths_keeps_renames_together_and_counts_utf16() {
        let big = "b".repeat(23_990);
        let files = vec![summary("a", None), summary(&big, Some(&big))];
        let batches = batch_file_paths(&files);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec!["a".to_owned()]);
        assert_eq!(batches[1], vec![big.clone(), big]);

        // "名" is 3 bytes and 1 UTF-16 code unit. JS `path.length` is 1, so
        // each path costs 1_516 and 15 fit under 24_000.
        let name = "名".repeat(1_500);
        let files = (0..16).map(|_| summary(&name, None)).collect::<Vec<_>>();
        let batches = batch_file_paths(&files);
        assert_eq!(batches[0].len(), 15);
        assert_eq!(batches[1].len(), 1);
    }

    #[test]
    fn projection_json_uses_the_upstream_context_keys() {
        let projection = sample_projection();
        let json = serde_json::to_value(&projection).unwrap();
        assert!(json.get("checkpointScopes").is_some());
        assert_eq!(json["checkpointScopes"][0]["kind"], "root_run");
        assert_eq!(json["runs"][0]["status"], "completed");
        assert_eq!(json["checkpoints"][0]["status"], "ready");
        assert_eq!(json["checkpoints"][0]["appRunOrdinal"], 2);
        assert!(json["checkpoints"][0].get("ref").is_some());
        assert!(json["checkpoints"][0].get("ref_").is_none());
    }

    fn thread_id() -> ThreadId {
        ThreadId::new("thread:checkpoint-diff-v2").unwrap()
    }
    fn first_run() -> RunId {
        RunId::new("run:checkpoint-diff-v2:1").unwrap()
    }
    fn second_run() -> RunId {
        RunId::new("run:checkpoint-diff-v2:2").unwrap()
    }
    fn first_scope() -> CheckpointScopeId {
        CheckpointScopeId::new("scope:checkpoint-diff-v2:1").unwrap()
    }
    fn second_scope() -> CheckpointScopeId {
        CheckpointScopeId::new("scope:checkpoint-diff-v2:2").unwrap()
    }
    fn second_ref() -> CheckpointRef {
        CheckpointRef::new("refs/t3/test/second").unwrap()
    }

    fn sample_projection() -> CheckpointProjection {
        CheckpointProjection {
            runs: vec![
                CheckpointProjectionRun {
                    id: first_run(),
                    ordinal: 1,
                    status: CheckpointProjectionRunStatus::Completed,
                },
                CheckpointProjectionRun {
                    id: second_run(),
                    ordinal: 2,
                    status: CheckpointProjectionRunStatus::Completed,
                },
            ],
            checkpoint_scopes: vec![
                CheckpointProjectionScope {
                    id: first_scope(),
                    run_id: Some(first_run()),
                    kind: CheckpointProjectionScopeKind::RootRun,
                    cwd: "/repo".into(),
                },
                CheckpointProjectionScope {
                    id: second_scope(),
                    run_id: Some(second_run()),
                    kind: CheckpointProjectionScopeKind::RootRun,
                    cwd: "/repo".into(),
                },
            ],
            checkpoints: vec![CheckpointProjectionCheckpoint {
                scope_id: second_scope(),
                run_id: Some(second_run()),
                app_run_ordinal: PositiveInt::new(2),
                status: CheckpointProjectionStatus::Ready,
                ref_: second_ref(),
            }],
        }
    }

    struct StaticContext {
        result: Mutex<Result<CheckpointProjection, ()>>,
    }

    #[async_trait]
    impl CheckpointQueryContext for StaticContext {
        async fn get_checkpoint_context(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<CheckpointProjection, CheckpointContextError> {
            let result = self
                .result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            result.map_err(|_| CheckpointContextError)
        }
    }

    struct DiffCall {
        cwd: String,
        from_checkpoint_ref: CheckpointRef,
        to_checkpoint_ref: CheckpointRef,
        fallback_from_to_head: bool,
        ignore_whitespace: bool,
        format: CheckpointDiffFormat,
        file_paths: Option<Vec<String>>,
    }

    struct FakeDriver {
        capabilities: VcsDriverCapabilities,
        calls: Mutex<Vec<DiffCall>>,
        authored: Option<HashSet<String>>,
        script: Box<dyn Fn(&VcsDiffCheckpointsInput) -> String + Send + Sync>,
    }

    #[async_trait]
    impl VcsDriver for FakeDriver {
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
            true
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
        async fn diff_checkpoints(
            &self,
            input: &VcsDiffCheckpointsInput,
        ) -> Result<String, VcsError> {
            let output = (self.script)(input);
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(DiffCall {
                    cwd: input.cwd.clone(),
                    from_checkpoint_ref: input.from_checkpoint_ref.clone(),
                    to_checkpoint_ref: input.to_checkpoint_ref.clone(),
                    fallback_from_to_head: input.fallback_from_to_head,
                    ignore_whitespace: input.ignore_whitespace,
                    format: input.format,
                    file_paths: input.file_paths.clone(),
                });
            Ok(output)
        }
        async fn list_authored_paths(
            &self,
            _: &VcsListAuthoredPathsInput,
        ) -> Result<Option<HashSet<String>>, VcsError> {
            Ok(self.authored.clone())
        }
        async fn delete_checkpoint_refs(
            &self,
            _: &VcsDeleteCheckpointRefsInput,
        ) -> Result<(), VcsError> {
            unimplemented!()
        }
    }

    fn capabilities() -> VcsDriverCapabilities {
        VcsDriverCapabilities {
            kind: VcsDriverKind::Git,
            supports_worktrees: true,
            supports_bookmarks: false,
            supports_atomic_snapshot: false,
            supports_push_default_remote: true,
            ignore_classifier: VcsIgnoreClassifier::Native,
        }
    }

    fn query_with(
        projection: Result<CheckpointProjection, ()>,
        authored: Option<HashSet<String>>,
        script: impl Fn(&VcsDiffCheckpointsInput) -> String + Send + Sync + 'static,
    ) -> (CheckpointDiffQuery, Arc<FakeDriver>) {
        let driver = Arc::new(FakeDriver {
            capabilities: capabilities(),
            calls: Mutex::new(Vec::new()),
            authored,
            script: Box::new(script),
        });
        let store = Arc::new(CheckpointStore::new(driver.clone()));
        let context = Arc::new(StaticContext {
            result: Mutex::new(projection),
        });
        (CheckpointDiffQuery::new(context, store), driver)
    }

    fn turn_input(from: u64, to: u64) -> OrchestrationGetTurnDiffInput {
        OrchestrationGetTurnDiffInput {
            thread_id: thread_id(),
            from_turn_count: from,
            to_turn_count: to,
            ignore_whitespace: None,
        }
    }

    #[tokio::test]
    async fn computes_v2_run_diffs_from_projected_checkpoint_scopes() {
        let (query, driver) = query_with(Ok(sample_projection()), None, |_| {
            "diff --git a/file b/file".into()
        });
        let result = query
            .get_full_thread_diff(&OrchestrationGetFullThreadDiffInput {
                thread_id: thread_id(),
                to_turn_count: 2,
                ignore_whitespace: None,
            })
            .await
            .unwrap();
        assert_eq!(
            result,
            turin_contracts::checkpoint_diff::ThreadTurnDiff {
                thread_id: thread_id(),
                from_turn_count: 0,
                to_turn_count: 2,
                diff: "diff --git a/file b/file".into(),
            }
        );
        let calls = driver
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cwd, "/repo");
        assert_eq!(
            calls[0].from_checkpoint_ref,
            checkpoint_ref_for_scope_ordinal(&first_scope(), 0)
        );
        assert_eq!(calls[0].to_checkpoint_ref, second_ref());
        assert!(!calls[0].fallback_from_to_head);
        assert!(calls[0].ignore_whitespace);
        assert_eq!(calls[0].format, CheckpointDiffFormat::Patch);
        assert!(calls[0].file_paths.is_none());
    }

    #[tokio::test]
    async fn leaves_git_imports_out_of_large_turn_diffs() {
        let own_paths: Vec<String> = (0..1_000)
            .map(|index| format!("src/module-{index}/file.ts"))
            .collect();
        let numstat = {
            let mut out = String::new();
            for path in own_paths
                .iter()
                .chain(std::iter::once(&"upstream.ts".to_owned()))
            {
                out.push_str(&format!("1\t0\t{path}\0"));
            }
            out
        };
        let (query, driver) = query_with(
            Ok(sample_projection()),
            Some(own_paths.iter().cloned().collect()),
            move |input| {
                if input.format == CheckpointDiffFormat::Numstat {
                    numstat.clone()
                } else {
                    input
                        .file_paths
                        .clone()
                        .unwrap_or_else(|| vec!["all".into()])
                        .join("\n")
                }
            },
        );
        query
            .get_full_thread_diff(&OrchestrationGetFullThreadDiffInput {
                thread_id: thread_id(),
                to_turn_count: 2,
                ignore_whitespace: None,
            })
            .await
            .unwrap();
        let calls = driver
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let patch_paths = calls
            .iter()
            .filter(|call| call.format != CheckpointDiffFormat::Numstat)
            .map(|call| {
                call.file_paths
                    .clone()
                    .unwrap_or_else(|| vec!["all".into()])
            })
            .collect::<Vec<_>>();
        assert!(patch_paths.len() > 1);
        let flat = patch_paths.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(flat, own_paths);
    }

    #[tokio::test]
    async fn preserves_the_typed_missing_thread_error() {
        let (query, _) = query_with(Err(()), None, |_| "diff".into());
        let error = query.get_turn_diff(&turn_input(0, 1)).await.unwrap_err();
        let CheckpointServiceError::ThreadNotFound(error) = error else {
            panic!("expected thread not found");
        };
        assert_eq!(error.operation, CheckpointDiffOperation::GetTurnDiff);
        assert_eq!(error.thread_id, thread_id());
    }

    #[tokio::test]
    async fn preserves_the_typed_unavailable_range_error() {
        let (query, _) = query_with(Ok(sample_projection()), None, |_| "diff".into());
        let error = query.get_turn_diff(&turn_input(0, 3)).await.unwrap_err();
        let CheckpointServiceError::TurnRangeUnavailable(error) = error else {
            panic!("expected range error");
        };
        assert_eq!(error.requested_turn_count, 3);
        assert_eq!(error.available_turn_count, 2);
    }

    #[tokio::test]
    async fn excludes_ready_checkpoints_from_rolled_back_runs() {
        let mut projection = sample_projection();
        for run in &mut projection.runs {
            if run.id == second_run() {
                run.status = CheckpointProjectionRunStatus::RolledBack;
            }
        }
        projection.checkpoints.insert(
            0,
            CheckpointProjectionCheckpoint {
                scope_id: first_scope(),
                run_id: Some(first_run()),
                app_run_ordinal: PositiveInt::new(1),
                status: CheckpointProjectionStatus::Ready,
                ref_: CheckpointRef::new("refs/t3/test/first").unwrap(),
            },
        );
        let (query, _) = query_with(Ok(projection), None, |_| "diff".into());
        let error = query.get_turn_diff(&turn_input(0, 2)).await.unwrap_err();
        let CheckpointServiceError::TurnRangeUnavailable(error) = error else {
            panic!("expected range error");
        };
        assert_eq!(error.requested_turn_count, 2);
        assert_eq!(error.available_turn_count, 1);
    }

    #[tokio::test]
    async fn preserves_the_typed_missing_baseline_ref_error() {
        let mut projection = sample_projection();
        for scope in &mut projection.checkpoint_scopes {
            scope.kind = CheckpointProjectionScopeKind::Tool;
        }
        let (query, _) = query_with(Ok(projection), None, |_| "diff".into());
        let error = query.get_turn_diff(&turn_input(0, 2)).await.unwrap_err();
        let CheckpointServiceError::RefUnavailable(error) = error else {
            panic!("expected missing ref");
        };
        assert_eq!(error.checkpoint, CheckpointEndpoint::From);
        assert_eq!(error.turn_count, 0);
    }

    #[tokio::test]
    async fn empty_ranges_skip_the_projection() {
        let (query, driver) = query_with(Err(()), None, |_| "diff".into());
        let full = query
            .get_full_thread_diff(&OrchestrationGetFullThreadDiffInput {
                thread_id: thread_id(),
                to_turn_count: 0,
                ignore_whitespace: None,
            })
            .await
            .unwrap();
        assert_eq!(full.diff, "");
        assert_eq!(full.from_turn_count, 0);
        let turn = query.get_turn_diff(&turn_input(2, 2)).await.unwrap();
        assert_eq!(turn.diff, "");
        assert!(
            driver
                .calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn inverted_turn_range_fails_the_contract_check() {
        let mut projection = sample_projection();
        projection.checkpoints.push(CheckpointProjectionCheckpoint {
            scope_id: second_scope(),
            run_id: Some(second_run()),
            app_run_ordinal: PositiveInt::new(5),
            status: CheckpointProjectionStatus::Ready,
            ref_: CheckpointRef::new("refs/t3/test/fifth").unwrap(),
        });
        let (query, _) = query_with(Ok(projection), None, |_| "diff".into());
        let error = query.get_turn_diff(&turn_input(5, 2)).await.unwrap_err();
        assert!(matches!(error, CheckpointServiceError::ResultInvalid(_)));
    }
}
