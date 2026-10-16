//! Port of upstream/apps/server/src/vcs/GitVcsDriver.ts (`makeVcsDriverShape`).
//!
//! Every git invocation here is `git -C <cwd> …` spawned from the process cwd,
//! matching upstream. Checkpoint diffs use the same flags the diff view reads.
//!
//! `VcsError` is the contract union. Boxing it would hide the variant callers match.

#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use turin_contracts::base_schemas::{EffectOption, TrimmedNonEmptyString, UtcDateTime};
use turin_contracts::git::VcsInitInput;
use turin_contracts::vcs::{
    VcsDriverCapabilities, VcsDriverKind, VcsError, VcsFreshness, VcsFreshnessSource,
    VcsIgnoreClassifier, VcsListRemotesResult, VcsListWorkspaceFilesResult, VcsProcessExitError,
    VcsRemote, VcsRepositoryIdentity,
};

use crate::git_vcs_driver_core::split_null_separated_paths;
use crate::process::{OutputMode, StdoutChunkCallback};
use crate::vcs_driver::{
    CheckpointDiffFormat, VcsCaptureCheckpointInput, VcsDeleteCheckpointRefsInput,
    VcsDiffCheckpointsInput, VcsDriver, VcsExecuteInput, VcsHasCheckpointInput,
    VcsListAuthoredPathsInput, VcsRestoreCheckpointInput,
};
use crate::vcs_process::{
    CHECKPOINT_CAPTURE_OPERATION, VcsProcess, VcsProcessInput, VcsProcessOutput,
};

const WORKSPACE_FILES_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const CHECKPOINT_RECOVERY_MAX_CANDIDATES: usize = 64;
const CHECKPOINT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_CHECK_IGNORE_MAX_STDIN_BYTES: usize = 256 * 1024;
const CHECKPOINT_DIFF_MAX_OUTPUT_BYTES: usize = 10_000_000;
const PATCH_RENDER_PREFIX_ARGS: [&str; 2] = ["--src-prefix=a/", "--dst-prefix=b/"];
const HARDENED: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.untrackedCache=false",
];
const DURABLE: [&str; 4] = [
    "-c",
    "core.fsync=objects,reference",
    "-c",
    "core.fsyncMethod=fsync",
];
const INDEX_CONFIG: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "sparse.expectFilesOutsideOfPatterns=false",
];

pub struct GitVcsDriver {
    process: VcsProcess,
    capabilities: VcsDriverCapabilities,
}

impl GitVcsDriver {
    pub fn new(process: VcsProcess) -> Self {
        Self {
            process,
            capabilities: VcsDriverCapabilities {
                kind: VcsDriverKind::Git,
                supports_worktrees: true,
                supports_bookmarks: false,
                supports_atomic_snapshot: false,
                supports_push_default_remote: true,
                ignore_classifier: VcsIgnoreClassifier::Native,
            },
        }
    }

    async fn git(
        &self,
        operation: &str,
        cwd: &str,
        args: &[String],
        options: GitOptions,
    ) -> Result<VcsProcessOutput, VcsError> {
        let mut full = vec!["-C".to_owned(), cwd.to_owned()];
        full.extend(args.iter().cloned());
        self.process
            .run(VcsProcessInput {
                operation: operation.to_owned(),
                command: "git".to_owned(),
                args: full,
                cwd: cwd.to_owned(),
                spawn_cwd: std::env::current_dir()
                    .ok()
                    .map(|path| path.display().to_string()),
                stdin: options.stdin,
                on_stdout_chunk: options.on_stdout_chunk,
                env: options.env,
                clear_env: options.clear_env,
                allow_non_zero_exit: options.allow_non_zero_exit,
                timeout: options.timeout_ms.map(Duration::from_millis),
                max_output_bytes: options.max_output_bytes,
                output_mode: options.output_mode,
                append_truncation_marker: options.append_truncation_marker,
            })
            .await
    }
}

#[derive(Default)]
struct GitOptions {
    stdin: Option<String>,
    env: Option<HashMap<String, String>>,
    clear_env: Vec<String>,
    allow_non_zero_exit: bool,
    timeout_ms: Option<u64>,
    max_output_bytes: Option<usize>,
    output_mode: Option<OutputMode>,
    append_truncation_marker: bool,
    on_stdout_chunk: Option<StdoutChunkCallback>,
}

#[async_trait]
impl VcsDriver for GitVcsDriver {
    fn capabilities(&self) -> &VcsDriverCapabilities {
        &self.capabilities
    }

    async fn execute(&self, input: VcsExecuteInput) -> Result<VcsProcessOutput, VcsError> {
        self.git(
            &input.operation,
            &input.cwd,
            &input.args,
            GitOptions {
                stdin: input.stdin,
                env: input.env,
                clear_env: input.clear_env,
                allow_non_zero_exit: input.allow_non_zero_exit,
                timeout_ms: input.timeout_ms,
                max_output_bytes: input.max_output_bytes,
                output_mode: input.output_mode,
                append_truncation_marker: input.append_truncation_marker,
                on_stdout_chunk: input.on_stdout_chunk,
            },
        )
        .await
    }

    async fn is_inside_work_tree(&self, cwd: &str) -> Result<bool, VcsError> {
        let result = self
            .git(
                "GitVcsDriver.isInsideWorkTree",
                cwd,
                &owned(["rev-parse", "--is-inside-work-tree"]),
                GitOptions {
                    allow_non_zero_exit: true,
                    timeout_ms: Some(5_000),
                    max_output_bytes: Some(4_096),
                    ..GitOptions::default()
                },
            )
            .await?;
        Ok(result.exit_code == 0 && result.stdout.trim() == "true")
    }

    async fn detect_repository(
        &self,
        cwd: &str,
    ) -> Result<Option<VcsRepositoryIdentity>, VcsError> {
        if !self.is_inside_work_tree(cwd).await? {
            return Ok(None);
        }
        let root = self
            .git(
                "GitVcsDriver.detectRepository.root",
                cwd,
                &owned(["rev-parse", "--show-toplevel"]),
                GitOptions::default(),
            )
            .await?;
        let common = self
            .git(
                "GitVcsDriver.detectRepository.commonDir",
                cwd,
                &owned(["rev-parse", "--git-common-dir"]),
                GitOptions::default(),
            )
            .await
            .ok();
        let metadata = common
            .as_ref()
            .map(|result| result.stdout.trim())
            .filter(|value| !value.is_empty())
            .and_then(|value| TrimmedNonEmptyString::new(value).ok());
        Ok(Some(VcsRepositoryIdentity {
            kind: VcsDriverKind::Git,
            root_path: TrimmedNonEmptyString::new(root.stdout.trim()).map_err(|_| {
                exit_error(
                    "GitVcsDriver.detectRepository.root",
                    "git rev-parse",
                    cwd,
                    0,
                    "git rev-parse --show-toplevel returned an empty path.",
                )
            })?,
            metadata_path: metadata,
            freshness: live_freshness(),
        }))
    }

    async fn list_workspace_files(
        &self,
        cwd: &str,
    ) -> Result<VcsListWorkspaceFilesResult, VcsError> {
        let mut args = HARDENED.map(str::to_owned).to_vec();
        args.extend(owned([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]));
        let result = self
            .git(
                "GitVcsDriver.listWorkspaceFiles",
                cwd,
                &args,
                GitOptions {
                    allow_non_zero_exit: true,
                    timeout_ms: Some(20_000),
                    max_output_bytes: Some(WORKSPACE_FILES_MAX_OUTPUT_BYTES),
                    append_truncation_marker: true,
                    ..GitOptions::default()
                },
            )
            .await?;
        if result.exit_code != 0 {
            return Err(exit_error(
                "GitVcsDriver.listWorkspaceFiles",
                "git ls-files",
                cwd,
                result.exit_code,
                if result.stderr.trim().is_empty() {
                    "git ls-files failed".to_owned()
                } else {
                    result.stderr.trim().to_owned()
                },
            ));
        }
        Ok(VcsListWorkspaceFilesResult {
            paths: split_null_separated_paths(&result.stdout, result.stdout_truncated)
                .into_iter()
                .filter_map(|path| TrimmedNonEmptyString::new(path).ok())
                .collect(),
            truncated: result.stdout_truncated,
            freshness: live_freshness(),
        })
    }

    async fn list_remotes(&self, cwd: &str) -> Result<VcsListRemotesResult, VcsError> {
        let result = self
            .git(
                "GitVcsDriver.listRemotes",
                cwd,
                &owned(["remote", "-v"]),
                GitOptions {
                    allow_non_zero_exit: true,
                    timeout_ms: Some(5_000),
                    max_output_bytes: Some(64 * 1024),
                    ..GitOptions::default()
                },
            )
            .await?;
        if result.exit_code != 0 {
            return Err(exit_error(
                "GitVcsDriver.listRemotes",
                "git remote -v",
                cwd,
                result.exit_code,
                if result.stderr.trim().is_empty() {
                    "git remote -v failed".to_owned()
                } else {
                    result.stderr.trim().to_owned()
                },
            ));
        }
        let mut remotes = Vec::new();
        for (name, remote) in parse_remote_verbose(&result.stdout) {
            let Some(url) = remote.url else {
                continue;
            };
            remotes.push(VcsRemote {
                name: TrimmedNonEmptyString::new(&name).map_err(|_| {
                    exit_error(
                        "GitVcsDriver.listRemotes",
                        "git remote -v",
                        cwd,
                        0,
                        "empty remote",
                    )
                })?,
                url: TrimmedNonEmptyString::new(&url).map_err(|_| {
                    exit_error(
                        "GitVcsDriver.listRemotes",
                        "git remote -v",
                        cwd,
                        0,
                        "empty url",
                    )
                })?,
                push_url: match remote.push_url {
                    Some(push) => {
                        EffectOption::some(TrimmedNonEmptyString::new(push).map_err(|_| {
                            exit_error(
                                "GitVcsDriver.listRemotes",
                                "git remote -v",
                                cwd,
                                0,
                                "empty push url",
                            )
                        })?)
                    }
                    None => EffectOption::None,
                },
                is_primary: name == "origin",
            });
        }
        Ok(VcsListRemotesResult {
            remotes,
            freshness: live_freshness(),
        })
    }

    async fn filter_ignored_paths(
        &self,
        cwd: &str,
        relative_paths: &[String],
    ) -> Result<Vec<String>, VcsError> {
        if relative_paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut ignored = HashSet::new();
        for chunk in chunk_paths_for_check_ignore(relative_paths) {
            let mut args = HARDENED.map(str::to_owned).to_vec();
            args.extend(owned(["check-ignore", "--no-index", "-z", "--stdin"]));
            let result = self
                .git(
                    "GitVcsDriver.filterIgnoredPaths",
                    cwd,
                    &args,
                    GitOptions {
                        stdin: Some(format!("{}\0", chunk.join("\0"))),
                        allow_non_zero_exit: true,
                        timeout_ms: Some(20_000),
                        max_output_bytes: Some(WORKSPACE_FILES_MAX_OUTPUT_BYTES),
                        append_truncation_marker: true,
                        ..GitOptions::default()
                    },
                )
                .await?;
            if result.exit_code != 0 && result.exit_code != 1 {
                return Err(exit_error(
                    "GitVcsDriver.filterIgnoredPaths",
                    "git check-ignore",
                    cwd,
                    result.exit_code,
                    if result.stderr.trim().is_empty() {
                        "git check-ignore failed".to_owned()
                    } else {
                        result.stderr.trim().to_owned()
                    },
                ));
            }
            ignored.extend(split_null_separated_paths(
                &result.stdout,
                result.stdout_truncated,
            ));
        }
        if ignored.is_empty() {
            return Ok(relative_paths.to_vec());
        }
        Ok(relative_paths
            .iter()
            .filter(|path| !ignored.contains(*path))
            .cloned()
            .collect())
    }

    async fn init_repository(&self, input: &VcsInitInput) -> Result<(), VcsError> {
        self.git(
            "GitVcsDriver.initRepository",
            input.cwd.as_str(),
            &owned(["init"]),
            GitOptions {
                timeout_ms: Some(10_000),
                max_output_bytes: Some(64 * 1024),
                ..GitOptions::default()
            },
        )
        .await?;
        Ok(())
    }

    fn supports_checkpoints(&self) -> bool {
        true
    }

    async fn capture_checkpoint(&self, input: &VcsCaptureCheckpointInput) -> Result<(), VcsError> {
        let common = self.resolve_git_common_dir(&input.cwd).await?;
        let index_id = uuid::Uuid::new_v4();
        let temp_index = PathBuf::from(&common).join(format!("t3-checkpoint-index-{index_id}"));
        let _cleanup = TempIndex(temp_index.clone());
        self.capture_with_index(input, &temp_index).await
    }

    async fn has_checkpoint_ref(&self, input: &VcsHasCheckpointInput) -> Result<bool, VcsError> {
        Ok(self
            .resolve_checkpoint_commit(&input.cwd, input.checkpoint_ref.as_str())
            .await?
            .is_some())
    }

    async fn restore_checkpoint(
        &self,
        input: &VcsRestoreCheckpointInput,
    ) -> Result<bool, VcsError> {
        let operation = "GitVcsDriver.checkpoints.restoreCheckpoint";
        let mut commit = self
            .resolve_checkpoint_commit(&input.cwd, input.checkpoint_ref.as_str())
            .await?;
        if commit.is_none() && input.fallback_to_head {
            commit = self.resolve_head_commit(&input.cwd).await?;
        }
        let Some(commit) = commit else {
            return Ok(false);
        };
        let tracked = self
            .execute(VcsExecuteInput::new(
                operation,
                &input.cwd,
                owned([
                    "ls-files",
                    "--cached",
                    &format!("--with-tree={commit}"),
                    "-z",
                    "--",
                    ".",
                ]),
            ))
            .await?;
        if !tracked.stdout.is_empty() {
            self.execute(VcsExecuteInput::new(
                operation,
                &input.cwd,
                owned([
                    "restore",
                    "--source",
                    &commit,
                    "--worktree",
                    "--staged",
                    "--",
                    ".",
                ]),
            ))
            .await?;
        }
        tokio::fs::create_dir_all(&input.cwd)
            .await
            .map_err(|error| {
                exit_error(
                    operation,
                    "git restore",
                    &input.cwd,
                    0,
                    format!("Could not recreate the checkpoint workspace: {error}"),
                )
            })?;
        let cleaned = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                ..VcsExecuteInput::new(operation, &input.cwd, owned(["clean", "-fd", "--", "."]))
            })
            .await?;
        if cleaned.exit_code != 0 {
            let emptied = cleaned.exit_code == 1
                && FAILED_TO_REMOVE_WORKSPACE_ROOT.is_match(cleaned.stderr.trim())
                && dir_is_empty(&input.cwd).await;
            if !emptied {
                return Err(exit_error(
                    operation,
                    "git clean",
                    &input.cwd,
                    cleaned.exit_code,
                    if cleaned.stderr.trim().is_empty() {
                        "Could not clean the checkpoint workspace.".to_owned()
                    } else {
                        cleaned.stderr.trim().to_owned()
                    },
                ));
            }
        }
        tokio::fs::create_dir_all(&input.cwd)
            .await
            .map_err(|error| {
                exit_error(
                    operation,
                    "git restore",
                    &input.cwd,
                    0,
                    format!("Could not recreate the checkpoint workspace: {error}"),
                )
            })?;
        if self.has_head_commit(&input.cwd, None, Vec::new()).await? {
            self.execute(VcsExecuteInput::new(
                operation,
                &input.cwd,
                owned(["reset", "--quiet", "--", "."]),
            ))
            .await?;
        }
        Ok(true)
    }

    async fn diff_checkpoints(&self, input: &VcsDiffCheckpointsInput) -> Result<String, VcsError> {
        let operation = "GitVcsDriver.checkpoints.diffCheckpoints";
        if input
            .file_paths
            .as_ref()
            .is_some_and(|paths| paths.is_empty())
        {
            return Ok(String::new());
        }
        let mut from_revision = input.from_checkpoint_ref.as_str().to_owned();
        if input.fallback_from_to_head {
            if let Some(resolved) = self
                .resolve_checkpoint_commit(&input.cwd, &from_revision)
                .await?
            {
                from_revision = resolved;
            } else if let Some(head) = self.resolve_head_commit(&input.cwd).await? {
                from_revision = head;
            } else {
                return Err(exit_error(
                    operation,
                    "git diff",
                    &input.cwd,
                    1,
                    "Checkpoint ref is unavailable for diff operation.",
                ));
            }
        }
        let mut args = vec!["diff".to_owned()];
        if input.format == CheckpointDiffFormat::Numstat {
            args.extend(owned(["--numstat", "-z"]));
        } else {
            args.push("--patch".to_owned());
        }
        args.extend(owned(["--no-color", "--no-ext-diff", "--no-textconv"]));
        args.extend(PATCH_RENDER_PREFIX_ARGS.map(str::to_owned));
        if input.ignore_whitespace {
            args.push("--ignore-all-space".to_owned());
        }
        args.push(format!("{from_revision}^{{commit}}"));
        args.push(format!("{}^{{commit}}", input.to_checkpoint_ref.as_str()));
        if let Some(paths) = &input.file_paths {
            args.push("--".to_owned());
            args.extend(paths.iter().map(|file| format!(":(top,literal){file}")));
        }
        let result = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                max_output_bytes: Some(CHECKPOINT_DIFF_MAX_OUTPUT_BYTES),
                output_mode: Some(if input.format == CheckpointDiffFormat::Numstat {
                    OutputMode::Error
                } else {
                    OutputMode::Truncate
                }),
                ..VcsExecuteInput::new(operation, &input.cwd, args)
            })
            .await?;
        if result.exit_code != 0 {
            return Err(exit_error(
                operation,
                "git diff",
                &input.cwd,
                result.exit_code,
                if result.stderr.trim().is_empty() {
                    "Checkpoint ref is unavailable for diff operation.".to_owned()
                } else {
                    result.stderr.trim().to_owned()
                },
            ));
        }
        Ok(result.stdout)
    }

    async fn list_authored_paths(
        &self,
        input: &VcsListAuthoredPathsInput,
    ) -> Result<Option<HashSet<String>>, VcsError> {
        let operation = "GitVcsDriver.checkpoints.listAuthoredPaths";
        let heads = self
            .execute(VcsExecuteInput::new(
                operation,
                &input.cwd,
                owned([
                    "log",
                    "--no-walk=unsorted",
                    "--format=%P %ct",
                    &format!("{}^{{commit}}", input.from_checkpoint_ref.as_str()),
                    &format!("{}^{{commit}}", input.to_checkpoint_ref.as_str()),
                ]),
            ))
            .await?;
        let mut lines = heads.stdout.trim_end().split('\n');
        let from_line = lines.next().unwrap_or("");
        let to_line = lines.next().unwrap_or("");
        let mut from_parts = from_line.split(' ');
        let start_head = from_parts.next().unwrap_or("");
        let captured_at = from_parts.next().unwrap_or("");
        let end_head = to_line.split(' ').next().unwrap_or("");
        if start_head.is_empty() || end_head.is_empty() || start_head == end_head {
            return Ok(None);
        }
        let list = |args: Vec<String>| {
            let cwd = input.cwd.clone();
            async move {
                let mut full = args;
                full.extend(owned([
                    "--name-only",
                    "-z",
                    "--no-renames",
                    "--no-ext-diff",
                ]));
                let result = self
                    .execute(VcsExecuteInput {
                        max_output_bytes: Some(CHECKPOINT_DIFF_MAX_OUTPUT_BYTES),
                        output_mode: Some(OutputMode::Error),
                        ..VcsExecuteInput::new(operation, cwd, full)
                    })
                    .await?;
                Ok::<_, VcsError>(
                    result
                        .stdout
                        .split('\0')
                        .filter(|path| !path.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
            }
        };
        let lists = futures::future::try_join_all([
            Box::pin(list(owned([
                "diff",
                start_head,
                &format!("{}^{{commit}}", input.from_checkpoint_ref.as_str()),
            ])))
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Vec<String>, VcsError>> + Send>,
                >,
            Box::pin(list(owned([
                "diff",
                end_head,
                &format!("{}^{{commit}}", input.to_checkpoint_ref.as_str()),
            ]))),
            Box::pin(list(owned([
                "log",
                "--format=",
                "--diff-merges=remerge",
                &format!("{end_head}..{start_head}"),
            ]))),
            Box::pin(list(owned([
                "log",
                "--format=",
                "--diff-merges=remerge",
                &format!("--since=@{captured_at}"),
                &format!("{start_head}..{end_head}"),
            ]))),
        ])
        .await?;
        Ok(Some(lists.into_iter().flatten().collect()))
    }

    async fn delete_checkpoint_refs(
        &self,
        input: &VcsDeleteCheckpointRefsInput,
    ) -> Result<(), VcsError> {
        for checkpoint_ref in &input.checkpoint_refs {
            self.execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                ..VcsExecuteInput::new(
                    "GitVcsDriver.checkpoints.deleteCheckpointRefs",
                    &input.cwd,
                    owned(["update-ref", "-d", checkpoint_ref.as_str()]),
                )
            })
            .await?;
        }
        Ok(())
    }
}

impl GitVcsDriver {
    async fn resolve_head_commit(&self, cwd: &str) -> Result<Option<String>, VcsError> {
        let result = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                ..VcsExecuteInput::new(
                    "GitVcsDriver.checkpoints.resolveHeadCommit",
                    cwd,
                    owned(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
                )
            })
            .await?;
        Ok(non_empty_oid(&result))
    }

    async fn resolve_checkpoint_commit(
        &self,
        cwd: &str,
        checkpoint_ref: &str,
    ) -> Result<Option<String>, VcsError> {
        let result = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                ..VcsExecuteInput::new(
                    "GitVcsDriver.checkpoints.resolveCheckpointCommit",
                    cwd,
                    owned([
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{checkpoint_ref}^{{commit}}"),
                    ]),
                )
            })
            .await?;
        Ok(non_empty_oid(&result))
    }

    async fn has_head_commit(
        &self,
        cwd: &str,
        env: Option<HashMap<String, String>>,
        clear_env: Vec<String>,
    ) -> Result<bool, VcsError> {
        let result = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                env,
                clear_env,
                ..VcsExecuteInput::new(
                    "GitVcsDriver.checkpoints.hasHeadCommit",
                    cwd,
                    owned(["rev-parse", "--verify", "HEAD"]),
                )
            })
            .await?;
        Ok(result.exit_code == 0)
    }

    async fn resolve_git_common_dir(&self, cwd: &str) -> Result<String, VcsError> {
        let result = self
            .execute(VcsExecuteInput::new(
                "GitVcsDriver.checkpoints.resolveGitCommonDir",
                cwd,
                owned(["rev-parse", "--git-common-dir"]),
            ))
            .await?;
        let common = result.stdout.trim();
        let path = Path::new(common);
        Ok(if path.is_absolute() {
            common.to_owned()
        } else {
            Path::new(cwd).join(path).display().to_string()
        })
    }

    async fn capture_with_index(
        &self,
        input: &VcsCaptureCheckpointInput,
        temp_index: &Path,
    ) -> Result<(), VcsError> {
        let operation = CHECKPOINT_CAPTURE_OPERATION;
        let mut commit_env = HashMap::new();
        commit_env.insert(
            "GIT_INDEX_FILE".to_owned(),
            temp_index.display().to_string(),
        );
        commit_env.insert("GIT_AUTHOR_NAME".to_owned(), "T3 Code".to_owned());
        commit_env.insert(
            "GIT_AUTHOR_EMAIL".to_owned(),
            "t3code@users.noreply.github.com".to_owned(),
        );
        commit_env.insert("GIT_COMMITTER_NAME".to_owned(), "T3 Code".to_owned());
        commit_env.insert(
            "GIT_COMMITTER_EMAIL".to_owned(),
            "t3code@users.noreply.github.com".to_owned(),
        );
        let head_exists = self.has_head_commit(&input.cwd, None, Vec::new()).await?;
        let sparse_config = self
            .execute(VcsExecuteInput {
                allow_non_zero_exit: true,
                ..VcsExecuteInput::new(
                    operation,
                    &input.cwd,
                    owned(["config", "--bool", "core.sparseCheckout"]),
                )
            })
            .await?;
        let mut sparse_checkout = sparse_config.stdout.trim() == "true";
        if sparse_checkout {
            let help = self
                .execute(VcsExecuteInput {
                    allow_non_zero_exit: true,
                    ..VcsExecuteInput::new(operation, &input.cwd, owned(["add", "-h"]))
                })
                .await?;
            let help_text = format!("{}{}", help.stdout, help.stderr);
            sparse_checkout = SPARSE_ADD_HELP.is_match(&help_text);
        }
        if head_exists {
            let reused = self
                .try_reuse_index(input, temp_index, &commit_env, sparse_checkout)
                .await
                .unwrap_or(false);
            if !reused {
                if sparse_checkout {
                    let cone = self
                        .execute(VcsExecuteInput {
                            allow_non_zero_exit: true,
                            ..VcsExecuteInput::new(
                                operation,
                                &input.cwd,
                                owned(["config", "--bool", "core.sparseCheckoutCone"]),
                            )
                        })
                        .await?;
                    if cone.stdout.trim() != "true" {
                        return Err(exit_error(
                            operation,
                            "git read-tree",
                            &input.cwd,
                            1,
                            "Cannot rebuild a checkpoint index for non-cone sparse checkout.",
                        ));
                    }
                }
                let _ = tokio::fs::remove_file(temp_index).await;
                let _ =
                    tokio::fs::remove_file(PathBuf::from(format!("{}.lock", temp_index.display())))
                        .await;
                let args = if sparse_checkout {
                    let mut args = INDEX_CONFIG.map(str::to_owned).to_vec();
                    args.extend(owned([
                        "-c",
                        "index.sparse=true",
                        "read-tree",
                        "--reset",
                        "HEAD",
                    ]));
                    args
                } else {
                    owned(["read-tree", "HEAD"])
                };
                self.execute(VcsExecuteInput {
                    env: Some(commit_env.clone()),
                    ..VcsExecuteInput::new(operation, &input.cwd, args)
                })
                .await?;
            }
        }

        let stage = |exclusions: Vec<String>, env: HashMap<String, String>| {
            let cwd = input.cwd.clone();
            async move {
                let mut args = INDEX_CONFIG.map(str::to_owned).to_vec();
                args.extend(DURABLE.map(str::to_owned));
                args.push("add".to_owned());
                if sparse_checkout {
                    args.push("--sparse".to_owned());
                }
                args.extend(owned(["-A", "--", "."]));
                args.extend(exclusions);
                self.execute(VcsExecuteInput {
                    env: Some(env),
                    ..VcsExecuteInput::new(operation, cwd, args)
                })
                .await
                .map(|_| ())
            }
        };
        if let Err(error) = stage(Vec::new(), commit_env.clone()).await {
            let VcsError::Exit(_) = &error else {
                return Err(error);
            };
            // One budget covers discovery, queued Git admission, probes, and the staging retry.
            // Give-up and timeout keep the original `git add` error. A probe or retry failure surfaces.
            let recovered = tokio::time::timeout(
                CHECKPOINT_RECOVERY_TIMEOUT,
                self.stage_excluding_empty_nested(input, &commit_env, stage),
            )
            .await;
            match recovered {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) | Err(_) => return Err(error),
                Ok(Err(probe)) => return Err(probe),
            }
        }

        let write_tree = self
            .execute(VcsExecuteInput {
                env: Some(commit_env.clone()),
                ..VcsExecuteInput::new(operation, &input.cwd, {
                    let mut args = INDEX_CONFIG.map(str::to_owned).to_vec();
                    args.extend(DURABLE.map(str::to_owned));
                    args.push("write-tree".to_owned());
                    args
                })
            })
            .await?;
        let tree_oid = write_tree.stdout.trim();
        if tree_oid.is_empty() {
            return Err(exit_error(
                operation,
                "git write-tree",
                &input.cwd,
                0,
                "git write-tree returned an empty tree oid.",
            ));
        }
        let message = format!("t3 checkpoint ref={}", input.checkpoint_ref.as_str());
        let mut commit_args = DURABLE.map(str::to_owned).to_vec();
        commit_args.extend(owned(["commit-tree", tree_oid]));
        if head_exists {
            commit_args.extend(owned(["-p", "HEAD"]));
        }
        commit_args.extend(owned(["-m", &message]));
        let commit = self
            .execute(VcsExecuteInput {
                env: Some(commit_env),
                ..VcsExecuteInput::new(operation, &input.cwd, commit_args)
            })
            .await?;
        let commit_oid = commit.stdout.trim();
        if commit_oid.is_empty() {
            return Err(exit_error(
                operation,
                "git commit-tree",
                &input.cwd,
                0,
                "git commit-tree returned an empty commit oid.",
            ));
        }
        let mut update = DURABLE.map(str::to_owned).to_vec();
        update.extend(owned([
            "update-ref",
            input.checkpoint_ref.as_str(),
            commit_oid,
        ]));
        self.execute(VcsExecuteInput::new(operation, &input.cwd, update))
            .await?;
        Ok(())
    }

    async fn try_reuse_index(
        &self,
        input: &VcsCaptureCheckpointInput,
        temp_index: &Path,
        commit_env: &HashMap<String, String>,
        sparse_checkout: bool,
    ) -> Result<bool, VcsError> {
        let operation = CHECKPOINT_CAPTURE_OPERATION;
        let index_path = self
            .execute(VcsExecuteInput::new(
                operation,
                &input.cwd,
                owned(["rev-parse", "--path-format=absolute", "--git-path", "index"]),
            ))
            .await?;
        let source = index_path.stdout.trim();
        let modified = tokio::fs::metadata(source)
            .await
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                exit_error(operation, "git rev-parse", &input.cwd, 1, error.to_string())
            })?;
        let millis = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let index_time = (millis.saturating_sub(1) / 1000) as u64;
        if index_time == 0 {
            return Ok(false);
        }
        tokio::fs::copy(source, temp_index).await.map_err(|error| {
            exit_error(operation, "git rev-parse", &input.cwd, 1, error.to_string())
        })?;
        let mut read_tree = INDEX_CONFIG.map(str::to_owned).to_vec();
        read_tree.extend(owned(["read-tree", "--reset", "HEAD"]));
        self.execute(VcsExecuteInput {
            env: Some(commit_env.clone()),
            ..VcsExecuteInput::new(operation, &input.cwd, read_tree)
        })
        .await?;
        // std's `set_times` is a blocking syscall, so the open and the stamp run on the blocking
        // pool rather than a runtime worker.
        let when = UNIX_EPOCH + Duration::from_secs(index_time);
        let stamped_path = temp_index.to_path_buf();
        let stamp = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let file = std::fs::File::options().write(true).open(&stamped_path)?;
            file.set_times(
                std::fs::FileTimes::new()
                    .set_accessed(when)
                    .set_modified(when),
            )
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|stamped| stamped);
        stamp.map_err(|error| {
            exit_error(operation, "git read-tree", &input.cwd, 1, error.to_string())
        })?;

        let scan = Arc::new(Mutex::new(SparseScan::default()));
        let scan_for_chunk = Arc::clone(&scan);
        let mut ls_args = INDEX_CONFIG.map(str::to_owned).to_vec();
        ls_args.extend(owned(["ls-files", "--full-name", "--sparse", "-v", "-z"]));
        self.execute(VcsExecuteInput {
            env: Some(commit_env.clone()),
            max_output_bytes: Some(4_096),
            output_mode: Some(OutputMode::Truncate),
            on_stdout_chunk: Some(Arc::new(move |chunk| {
                if let Ok(mut scan) = scan_for_chunk.lock() {
                    scan.push(chunk, sparse_checkout);
                }
            })),
            ..VcsExecuteInput::new(operation, &input.cwd, ls_args)
        })
        .await?;
        let scan = scan
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut special = scan.special_flags;
        if !scan.skipped_paths.is_empty() && !special {
            let selected = self
                .execute(VcsExecuteInput {
                    stdin: Some(format!("{}\0", scan.skipped_paths.join("\0"))),
                    env: Some(commit_env.clone()),
                    max_output_bytes: Some(1),
                    output_mode: Some(OutputMode::Truncate),
                    ..VcsExecuteInput::new(operation, &input.cwd, {
                        let mut args = INDEX_CONFIG.map(str::to_owned).to_vec();
                        args.extend(owned(["sparse-checkout", "check-rules", "-z"]));
                        args
                    })
                })
                .await?;
            special = !selected.stdout.is_empty() || selected.stdout_truncated;
        }
        Ok(!special)
    }

    /// `Ok(false)` means recovery cannot help, so the caller keeps the original `git add` error.
    async fn stage_excluding_empty_nested<F, Fut>(
        &self,
        input: &VcsCaptureCheckpointInput,
        commit_env: &HashMap<String, String>,
        stage: F,
    ) -> Result<bool, VcsError>
    where
        F: Fn(Vec<String>, HashMap<String, String>) -> Fut,
        Fut: std::future::Future<Output = Result<(), VcsError>>,
    {
        let operation = CHECKPOINT_CAPTURE_OPERATION;
        let untracked = self
            .execute(VcsExecuteInput {
                env: Some(commit_env.clone()),
                max_output_bytes: Some(WORKSPACE_FILES_MAX_OUTPUT_BYTES),
                ..VcsExecuteInput::new(
                    operation,
                    &input.cwd,
                    owned([
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        ".",
                    ]),
                )
            })
            .await?;
        if untracked.stdout_truncated {
            return Ok(false);
        }
        let candidates: Vec<String> = split_null_separated_paths(&untracked.stdout, false)
            .into_iter()
            .filter(|entry| entry.ends_with('/'))
            .collect();
        if candidates.len() > CHECKPOINT_RECOVERY_MAX_CANDIDATES {
            return Ok(false);
        }
        let clear = vec![
            "GIT_DIR".to_owned(),
            "GIT_WORK_TREE".to_owned(),
            "GIT_COMMON_DIR".to_owned(),
            "GIT_INDEX_FILE".to_owned(),
            "GIT_OBJECT_DIRECTORY".to_owned(),
            "GIT_ALTERNATE_OBJECT_DIRECTORIES".to_owned(),
        ];
        let mut exclusions = Vec::new();
        for entry in &candidates {
            let nested = Path::new(&input.cwd).join(entry);
            if tokio::fs::try_exists(nested.join(".git"))
                .await
                .unwrap_or(false)
                && !self
                    .has_head_commit(&nested.display().to_string(), None, clear.clone())
                    .await?
            {
                exclusions.push(format!(":(exclude,literal){entry}"));
            }
        }
        if exclusions.is_empty() {
            return Ok(false);
        }
        stage(exclusions, commit_env.clone()).await?;
        Ok(true)
    }
}

/// Git's "could not remove ./" line from `git clean`, the only stderr that
/// means "every child was removed". Anything longer or multi-line is a real failure.
static FAILED_TO_REMOVE_WORKSPACE_ROOT: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^warning: failed to remove \./: [^\n]+$").unwrap()
    });
static SPARSE_ADD_HELP: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"--(?:\[no-\])?sparse\b").unwrap());

#[derive(Clone)]
struct SparseScan {
    special_flags: bool,
    /// True before the first byte, and after every NUL.
    record_start: bool,
    skipped: bool,
    skipped_record: Vec<u8>,
    skipped_paths: Vec<String>,
}

impl Default for SparseScan {
    fn default() -> Self {
        Self {
            special_flags: false,
            record_start: true,
            skipped: false,
            skipped_record: Vec::new(),
            skipped_paths: Vec::new(),
        }
    }
}

impl SparseScan {
    fn push(&mut self, chunk: &[u8], sparse_checkout: bool) {
        for &byte in chunk {
            if self.record_start {
                self.skipped = byte == b'S';
            }
            if self.skipped && sparse_checkout {
                if byte != 0 {
                    self.skipped_record.push(byte);
                } else if self.skipped_record.last().copied() != Some(b'/') {
                    let name = if self.skipped_record.len() > 2 {
                        &self.skipped_record[2..]
                    } else {
                        &[]
                    };
                    match std::str::from_utf8(name) {
                        Ok(path) => self.skipped_paths.push(path.to_owned()),
                        Err(_) => self.special_flags = true,
                    }
                    self.skipped_record.clear();
                } else {
                    self.skipped_record.clear();
                }
            }
            if self.record_start
                && (byte.is_ascii_lowercase() || (!sparse_checkout && byte == b'S'))
            {
                self.special_flags = true;
            }
            self.record_start = byte == 0;
        }
    }
}

fn non_empty_oid(result: &VcsProcessOutput) -> Option<String> {
    if result.exit_code != 0 {
        return None;
    }
    let commit = result.stdout.trim();
    if commit.is_empty() {
        None
    } else {
        Some(commit.to_owned())
    }
}

/// Whether `path` is an empty directory. An unreadable one counts as not empty, so a failed clean
/// still fails the restore.
async fn dir_is_empty(path: &str) -> bool {
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => matches!(entries.next_entry().await, Ok(None)),
        Err(_) => false,
    }
}

fn owned<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.into_iter().map(str::to_owned).collect()
}

fn live_freshness() -> VcsFreshness {
    VcsFreshness {
        source: VcsFreshnessSource::LiveLocal,
        observed_at: UtcDateTime::now(),
        expires_at: EffectOption::None,
    }
}

fn exit_error(
    operation: &str,
    command: &str,
    cwd: &str,
    exit_code: i32,
    detail: impl Into<String>,
) -> VcsError {
    VcsError::Exit(VcsProcessExitError::VcsProcessExitError {
        operation: operation.to_owned(),
        command: command.to_owned(),
        cwd: cwd.to_owned(),
        argument_count: None,
        exit_code: f64::from(exit_code),
        detail: detail.into(),
        failure_kind: None,
        retryable: None,
        stderr_length: None,
        stderr_truncated: None,
    })
}

fn chunk_paths_for_check_ignore(relative_paths: &[String]) -> Vec<Vec<String>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    let mut chunk_bytes = 0;
    for relative_path in relative_paths {
        let relative_bytes = relative_path.len() + 1;
        if !chunk.is_empty() && chunk_bytes + relative_bytes > GIT_CHECK_IGNORE_MAX_STDIN_BYTES {
            chunks.push(std::mem::take(&mut chunk));
            chunk_bytes = 0;
        }
        chunk.push(relative_path.clone());
        chunk_bytes += relative_bytes;
        if chunk_bytes >= GIT_CHECK_IGNORE_MAX_STDIN_BYTES {
            chunks.push(std::mem::take(&mut chunk));
            chunk_bytes = 0;
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

struct RemoteVerbose {
    url: Option<String>,
    push_url: Option<String>,
}

fn parse_remote_verbose(output: &str) -> Vec<(String, RemoteVerbose)> {
    let mut order = Vec::new();
    let mut remotes: HashMap<String, RemoteVerbose> = HashMap::new();
    for line in output.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(url) = parts.next() else { continue };
        let Some(direction) = parts.next() else {
            continue;
        };
        if !direction.starts_with('(') {
            continue;
        }
        if !remotes.contains_key(name) {
            order.push(name.to_owned());
        }
        let remote = remotes.entry(name.to_owned()).or_insert(RemoteVerbose {
            url: None,
            push_url: None,
        });
        if direction.contains("fetch") {
            remote.url = Some(url.to_owned());
        } else {
            remote.push_url = Some(url.to_owned());
        }
    }
    order
        .into_iter()
        .filter_map(|name| remotes.remove(&name).map(|remote| (name, remote)))
        .collect()
}

fn remove_force(path: impl AsRef<Path>) {
    let _ = std::fs::remove_file(path);
}

/// Removes a checkpoint temp index and its `.lock` when dropped. Upstream's
/// `cleanupTempIndex` finalizer runs on interruption too, so a capture future
/// dropped mid-run must not leave them in `.git/`.
struct TempIndex(PathBuf);

impl Drop for TempIndex {
    fn drop(&mut self) {
        remove_force(&self.0);
        remove_force(PathBuf::from(format!("{}.lock", self.0.display())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessRunner;
    use turin_contracts::base_schemas::CheckpointRef;

    fn driver() -> GitVcsDriver {
        GitVcsDriver::new(VcsProcess::new(ProcessRunner))
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn checkpoint_patch_matches_git_diff_flags() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        git(cwd, &["init", "-b", "main"]);
        git(cwd, &["config", "user.email", "test@test.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        std::fs::write(cwd.join("note.txt"), "one\n").unwrap();
        git(cwd, &["add", "note.txt"]);
        git(cwd, &["commit", "-m", "initial"]);

        let driver = driver();
        let cwd_s = cwd.display().to_string();
        let first = CheckpointRef::new("refs/t3/checkpoints/turn/1").unwrap();
        driver
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd_s.clone(),
                checkpoint_ref: first.clone(),
            })
            .await
            .expect("capture 1");
        std::fs::write(cwd.join("note.txt"), "one\ntwo\n").unwrap();
        let second = CheckpointRef::new("refs/t3/checkpoints/turn/2").unwrap();
        driver
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd_s.clone(),
                checkpoint_ref: second.clone(),
            })
            .await
            .expect("capture 2");

        let patch = driver
            .diff_checkpoints(&VcsDiffCheckpointsInput {
                cwd: cwd_s,
                from_checkpoint_ref: first,
                to_checkpoint_ref: second,
                fallback_from_to_head: false,
                ignore_whitespace: false,
                format: CheckpointDiffFormat::Patch,
                file_paths: None,
            })
            .await
            .expect("diff");
        assert!(
            patch.contains("diff --git a/note.txt b/note.txt"),
            "{patch}"
        );
        assert!(patch.contains("+two"), "{patch}");
        assert!(!patch.contains("\u{1b}["), "color leaked into {patch}");
    }

    #[tokio::test]
    async fn an_empty_directory_is_told_apart_from_one_with_entries_or_none() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(dir_is_empty(empty.to_str().unwrap()).await);

        std::fs::write(empty.join("note.txt"), "one\n").unwrap();
        assert!(!dir_is_empty(empty.to_str().unwrap()).await);

        let missing = dir.path().join("missing");
        assert!(!dir_is_empty(missing.to_str().unwrap()).await);
    }

    #[test]
    fn chunks_check_ignore_stdin_on_the_byte_budget() {
        let paths = vec!["a".repeat(100), "b".repeat(100)];
        let chunks = chunk_paths_for_check_ignore(&paths);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn dropping_the_temp_index_guard_removes_the_index_and_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("t3-checkpoint-index-test");
        let lock = PathBuf::from(format!("{}.lock", index.display()));
        std::fs::write(&index, "index").unwrap();
        std::fs::write(&lock, "lock").unwrap();
        drop(TempIndex(index.clone()));
        assert!(!index.exists());
        assert!(!lock.exists());
    }

    #[test]
    fn only_a_single_line_root_removal_warning_counts_as_emptied() {
        let root = &FAILED_TO_REMOVE_WORKSPACE_ROOT;
        assert!(root.is_match("warning: failed to remove ./: Directory not empty"));
        assert!(!root.is_match("warning: failed to remove ./:"));
        assert!(!root.is_match("warning: failed to remove ./: "));
        assert!(
            !root.is_match(
                "warning: failed to remove ./: Directory not empty\nfatal: something else"
            )
        );
        assert!(!root.is_match("fatal: warning: failed to remove ./: detail"));
    }
}
