impl GitVcsDriverCore {
    async fn worktree_list(
        &self,
        operation: &'static str,
        cwd: &str,
        mut git_args: Vec<String>,
        timeout: Option<Duration>,
    ) -> Result<ExecuteGitResult, GitCommandError> {
        git_args.extend(argv(&["worktree", "list", "--porcelain"]));
        let mut nul_args = git_args.clone();
        nul_args.push("-z".to_owned());
        let mut call = GitCall::new(operation, nul_args);
        call.allow_non_zero = true;
        if let Some(timeout) = timeout {
            call.timeout = Some(Some(timeout));
        }
        let result = self.git(cwd, call).await?;
        if result.exit_code == Some(0) {
            return Ok(result);
        }
        let mut line_call = GitCall::new(operation, git_args);
        if let Some(timeout) = timeout {
            line_call.timeout = Some(Some(timeout));
        }
        let mut line_result = self.git(cwd, line_call).await?;
        line_result.stdout = line_result.stdout.replace('\n', "\0");
        Ok(line_result)
    }

    async fn read_empty_tree_hash(&self, cwd: &str) -> Result<String, GitCommandError> {
        let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let stdout = self
            .git_out(
                "GitVcsDriver.review.emptyTree",
                cwd,
                argv(&["hash-object", "-t", "tree", null_path]),
                false,
            )
            .await?;
        Ok(stdout.trim().to_owned())
    }

    async fn prepare_review_index(
        &self,
        cwd: &str,
        untracked_paths: &[String],
    ) -> Result<Option<HashMap<String, String>>, GitCommandError> {
        let (staged, index_value) = tokio::join!(
            self.limited_stdout(
                "GitVcsDriver.readUnifiedWorkingTreeReviewDiff.stagedDeletions",
                cwd,
                argv(&[
                    "diff",
                    "--cached",
                    "--name-only",
                    "--diff-filter=D",
                    "-z",
                    "HEAD",
                    "--"
                ]),
                REVIEW_METADATA_MAX,
            ),
            self.git_out(
                "GitVcsDriver.readUnifiedWorkingTreeReviewDiff.indexPath",
                cwd,
                argv(&["rev-parse", "--git-path", "index"]),
                false,
            ),
        );
        let staged_deletions: HashSet<String> = staged?
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        let paths_to_add: Vec<&String> = untracked_paths
            .iter()
            .filter(|path| !staged_deletions.contains(path.as_str()))
            .collect();
        if paths_to_add.is_empty() {
            return Ok(None);
        }
        let index_raw = index_value?.trim().to_owned();
        let index_path = if Path::new(&index_raw).is_absolute() {
            PathBuf::from(&index_raw)
        } else {
            resolve_path(cwd, &index_raw)
        };
        let temp_index = std::env::temp_dir().join(format!(
            "t3code-review-index-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _guard = TempPath(temp_index.clone());
        if index_path.exists() {
            std::fs::copy(&index_path, &temp_index).map_err(|error| {
                plain_error(
                    "GitVcsDriver.prepareReviewIndex",
                    cwd,
                    &argv(&["git", "diff"]),
                    "Could not prepare the review index.",
                )
                .with_cause(error.to_string())
            })?;
            set_index_times(&temp_index, index_mtime_secs(&index_path));
        }
        let mut env = HashMap::new();
        env.insert(
            "GIT_INDEX_FILE".to_owned(),
            temp_index.to_string_lossy().into_owned(),
        );
        if !index_path.exists() {
            let mut call = GitCall::new("GitVcsDriver.review.emptyIndex", argv(&["read-tree", "--empty"]));
            call.env = env.clone();
            self.git(cwd, call).await?;
        }
        let mut expand = split_index_config();
        expand.extend(argv(&["update-index", "--no-split-index"]));
        let mut call = GitCall::new(
            "GitVcsDriver.readUnifiedWorkingTreeReviewDiff.expandSplitIndex",
            expand,
        );
        call.env = env.clone();
        self.git(cwd, call).await?;
        let mut add = split_index_config();
        add.extend(argv(&[
            "--literal-pathspecs",
            "add",
            "--intent-to-add",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ]));
        let mut call = GitCall::new(
            "GitVcsDriver.readUnifiedWorkingTreeReviewDiff.addUntracked",
            add,
        );
        call.env = env.clone();
        call.stdin = Some(format!("{}\0", paths_to_add.iter().map(|path| path.as_str()).collect::<Vec<_>>().join("\0")));
        self.git(cwd, call).await?;
        // The temporary index must outlive the diff. Leak it until process exit via forget of the guard
        // after the caller finishes. Callers run the diff before this function returns only if we
        // keep the file. The diff is run by the caller after this returns, so keep the file.
        std::mem::forget(_guard);
        Ok(Some(env))
    }

    async fn prepare_untracked_review_index(
        &self,
        cwd: &str,
        path_args: &[String],
        only_path: Option<&str>,
    ) -> Result<Option<Option<HashMap<String, String>>>, GitCommandError> {
        let mut args = argv(&["ls-files", "--others", "--exclude-standard", "-z", "--"]);
        args.extend(path_args.iter().cloned());
        let mut call = GitCall::new("GitVcsDriver.review.listUntracked", args);
        call.max_output = Some(REVIEW_METADATA_MAX);
        let untracked = match self.git(cwd, call).await {
            Err(error) => {
                let GitCommandError::GitCommandError { output_length, .. } = &error;
                if output_length.is_none() {
                    return Ok(None);
                }
                return Err(error);
            }
            Ok(result) => result,
        };
        let paths = split_null_separated_paths(&untracked.stdout, untracked.stdout_truncated)
            .into_iter()
            .filter(|candidate| only_path.is_none_or(|only| candidate == only))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(Some(None));
        }
        if paths.len() > REVIEW_UNTRACKED_MAX_FILES {
            return Ok(None);
        }
        let env = self.prepare_review_index(cwd, &paths).await.map_err(|error| {
            let GitCommandError::GitCommandError { cause, .. } = &error;
            plain_error(
                "GitVcsDriver.prepareReviewIndex",
                cwd,
                &argv(&["git", "diff"]),
                "Could not prepare the review index.",
            )
            .with_cause(
                cause
                    .as_ref()
                    .and_then(|value| value.get("message").and_then(|message| message.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| error.to_string()),
            )
        })?;
        Ok(Some(env))
    }

    async fn resolve_review_merge_base(
        &self,
        cwd: &str,
        branch: Option<&str>,
        explicit_base_ref: Option<&str>,
    ) -> Result<(Option<String>, String), GitCommandError> {
        let base_ref = if let Some(explicit) = explicit_base_ref {
            Some(explicit.to_owned())
        } else if let Some(branch) = branch {
            self.resolve_base_branch_for_no_upstream(cwd, branch, true)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let Some(base_ref) = base_ref else {
            return Ok((None, "HEAD".to_owned()));
        };
        let args = argv(&["merge-base", &base_ref, "HEAD"]);
        let mut call = GitCall::new("GitVcsDriver.resolveReviewMergeBase", args.clone());
        call.allow_non_zero = true;
        let result = self.git(cwd, call).await?;
        let merge_base = result.stdout.trim().to_owned();
        if result.exit_code != Some(0) || merge_base.is_empty() {
            let mut head = GitCall::new(
                "GitVcsDriver.resolveReviewMergeBase.head",
                argv(&["rev-parse", "--verify", "--quiet", "HEAD"]),
            );
            head.allow_non_zero = true;
            let head = self.git(cwd, head).await?;
            if explicit_base_ref.is_none() && head.exit_code != Some(0) {
                return Ok((None, "HEAD".to_owned()));
            }
            return Err(failed(
                "GitVcsDriver.resolveReviewMergeBase",
                cwd,
                &args,
                &result,
                &format!("Could not find a common commit between '{base_ref}' and HEAD."),
            ));
        }
        Ok((Some(base_ref), merge_base))
    }

    async fn read_branch_change_totals(
        &self,
        cwd: &str,
        branch: Option<&str>,
    ) -> Result<VcsStatusBranchChanges, GitCommandError> {
        let (base_ref, merge_base) = self.resolve_review_merge_base(cwd, branch, None).await?;
        let untracked = self.prepare_untracked_review_index(cwd, &[], None).await?;
        let Some(untracked_env) = untracked else {
            return Err(plain_error(
                "GitVcsDriver.readBranchChangeTotals",
                cwd,
                &argv(&["ls-files"]),
                "Too many untracked files to count.",
            ));
        };
        let read_numstat = |reference: String, env: Option<HashMap<String, String>>| async move {
            let mut args = review_diff_args();
            args.extend(argv(&["--numstat", "-z", &reference, "--"]));
            let mut call = GitCall::new("GitVcsDriver.readBranchChangeTotals", args);
            call.allow_non_zero = true;
            call.max_output = Some(REVIEW_METADATA_MAX);
            if let Some(env) = env {
                call.env = env;
            }
            self.git(cwd, call).await
        };
        let mut result = read_numstat(merge_base.clone(), untracked_env.clone()).await?;
        if result.exit_code != Some(0) && merge_base == "HEAD" && is_unborn_head_stderr(&result.stderr) {
            result = read_numstat(self.read_empty_tree_hash(cwd).await?, untracked_env).await?;
        }
        if result.exit_code != Some(0) {
            return Err(failed(
                "GitVcsDriver.readBranchChangeTotals",
                cwd,
                &argv(&["diff", "--numstat"]),
                &result,
                "Could not read Changes totals.",
            ));
        }
        let mut insertions = 0_f64;
        let mut deletions = 0_f64;
        for file in parse_review_numstat(&result.stdout) {
            insertions += file.additions;
            deletions += file.deletions;
        }
        Ok(VcsStatusBranchChanges {
            base_ref: base_ref.as_deref().and_then(trimmed_name),
            insertions: insertions.max(0.0) as u64,
            deletions: deletions.max(0.0) as u64,
        })
    }

    async fn read_review_tracked(
        &self,
        cwd: &str,
        diff_args: &[String],
        reference: Option<String>,
        env: Option<HashMap<String, String>>,
        path_args: &[String],
        patch_limit: usize,
    ) -> Result<(String, bool, Option<Vec<ReviewDiffFileStat>>), GitCommandError> {
        let Some(reference) = reference else {
            return Ok((String::new(), false, Some(Vec::new())));
        };
        let mut args = diff_args.to_vec();
        args.extend(argv(&["--numstat", "-z", &reference, "--"]));
        args.extend(path_args.iter().cloned());
        let mut call = GitCall::new("GitVcsDriver.getReviewDiffPreview.stat", args.clone());
        call.allow_non_zero = true;
        call.max_output = Some(REVIEW_METADATA_MAX);
        if let Some(env) = env.clone() {
            call.env = env;
        }
        let result = self.git(cwd, call).await?;
        let (resolved, files) = if result.exit_code == Some(0) {
            (reference, parse_review_numstat(&result.stdout))
        } else if reference == "HEAD" && is_unborn_head_stderr(&result.stderr) {
            let empty = self.read_empty_tree_hash(cwd).await?;
            let mut unborn = diff_args.to_vec();
            unborn.extend(argv(&["--numstat", "-z", &empty, "--"]));
            unborn.extend(path_args.iter().cloned());
            let stdout = self
                .limited_stdout(
                    "GitVcsDriver.getReviewDiffPreview.unbornStat",
                    cwd,
                    unborn,
                    REVIEW_METADATA_MAX,
                )
                .await?;
            (empty, parse_review_numstat(&stdout))
        } else {
            return Err(failed(
                "GitVcsDriver.getReviewDiffPreview.stat",
                cwd,
                &args,
                &result,
                "Could not read complete diff statistics.",
            ));
        };
        if files.is_empty() {
            return Ok((String::new(), false, Some(files)));
        }
        let mut patch_args = diff_args.to_vec();
        patch_args.extend(argv(&["--patch", &resolved, "--"]));
        patch_args.extend(path_args.iter().cloned());
        let mut call = GitCall::new("GitVcsDriver.getReviewDiffPreview.patch", patch_args);
        call.max_output = Some(patch_limit);
        call.marker = true;
        if let Some(env) = env {
            call.env = env;
        }
        let patch = self.git(cwd, call).await?;
        Ok((stdout_text(&patch), patch.stdout_truncated, Some(files)))
    }

    async fn review_diff_preview(
        &self,
        input: &ReviewDiffPreviewInput,
    ) -> Result<ReviewDiffPreviewResult, GitCommandError> {
        let cwd_input = input.cwd.as_str();
        let path_args = if let Some(file) = &input.file {
            let mut paths = vec![format!(":(top,literal){}", file.path)];
            if let Some(previous) = &file.previous_path {
                paths.push(format!(":(top,literal){previous}"));
            }
            paths
        } else {
            Vec::new()
        };
        let patch_limit = if input.file.is_some() {
            REVIEW_DIFF_FILE_MAX
        } else {
            REVIEW_DIFF_PATCH_MAX
        };
        let repository = match self.resolve_repository_paths_uncached(cwd_input).await {
            Err(error) if is_missing_cwd(&error) => None,
            other => other?,
        };
        let Some(repository) = repository.as_ref().filter(|paths| paths.worktree_root.is_some()) else {
            return Ok(ReviewDiffPreviewResult {
                cwd: input.cwd.clone(),
                generated_at: UtcDateTime::now(),
                sources: Vec::new(),
            });
        };
        let cwd = repository.worktree_root.clone().unwrap_or_else(|| cwd_input.to_owned());
        let dirty_ref = if input.file.as_ref().is_some_and(|file| {
            file.source_kind == ReviewDiffPreviewSourceKind::BranchRange
        }) {
            None
        } else {
            Some("HEAD".to_owned())
        };
        let (base_ref, merge_base) = if input.file.as_ref().is_some_and(|file| {
            file.source_kind == ReviewDiffPreviewSourceKind::WorkingTree
        }) {
            (input.base_ref.as_ref().map(|value| value.as_str().to_owned()), None)
        } else {
            let (base, merge) = self
                .resolve_review_merge_base(
                    &cwd,
                    repository.current_branch.as_deref(),
                    input.base_ref.as_ref().map(|value| value.as_str()),
                )
                .await?;
            (base, Some(merge))
        };
        let mut diff_args = review_diff_args();
        if input.ignore_whitespace == Some(true) {
            diff_args.push("--ignore-all-space".to_owned());
        }
        let untracked = self
            .prepare_untracked_review_index(
                &cwd,
                &path_args,
                input.file.as_ref().map(|file| file.path.as_str()),
            )
            .await?;
        let env = untracked.as_ref().and_then(|value| value.clone());
        let (mut dirty, mut base) = if merge_base.as_deref() == dirty_ref.as_deref() {
            let shared = self
                .read_review_tracked(&cwd, &diff_args, dirty_ref.clone(), env, &path_args, patch_limit)
                .await?;
            (shared.clone(), shared)
        } else {
            let dirty = self
                .read_review_tracked(
                    &cwd,
                    &diff_args,
                    dirty_ref.clone(),
                    env.clone(),
                    &path_args,
                    patch_limit,
                )
                .await?;
            let base = self
                .read_review_tracked(&cwd, &diff_args, merge_base.clone(), env, &path_args, patch_limit)
                .await?;
            (dirty, base)
        };
        if untracked.is_none() {
            if dirty_ref.is_some() {
                dirty.1 = true;
                dirty.2 = None;
            }
            if merge_base.is_some() {
                base.1 = true;
                base.2 = None;
            }
        }
        let dirty_files = dirty.2.clone().unwrap_or_default();
        let base_files = base.2.clone().unwrap_or_default();
        let dirty_hash = trimmed_name(&review_hash(&dirty.0, &dirty_files))
            .unwrap_or_else(|| TrimmedNonEmptyString::new("0").expect("hash"));
        let base_hash = trimmed_name(&review_hash(&base.0, &base_files))
            .unwrap_or_else(|| TrimmedNonEmptyString::new("0").expect("hash"));
        let title = if let Some(base_ref) = &base_ref {
            format!("Changes vs {base_ref}")
        } else {
            "Changes".to_owned()
        };
        let sources = vec![
            ReviewDiffPreviewSource {
                id: trimmed_name("working-tree").expect("id"),
                kind: ReviewDiffPreviewSourceKind::WorkingTree,
                title: trimmed_name("Uncommitted").expect("title"),
                base_ref: trimmed_name("HEAD"),
                head_ref: None,
                diff: dirty.0,
                diff_hash: dirty_hash,
                truncated: dirty.1,
                files: dirty.2,
            },
            ReviewDiffPreviewSource {
                id: trimmed_name("branch-range").expect("id"),
                kind: ReviewDiffPreviewSourceKind::BranchRange,
                title: trimmed_name(&title).unwrap_or_else(|| trimmed_name("Changes").expect("title")),
                base_ref: base_ref.as_deref().and_then(trimmed_name),
                head_ref: trimmed_name(repository.current_branch.as_deref().unwrap_or("HEAD")),
                diff: base.0,
                diff_hash: base_hash,
                truncated: base.1,
                files: base.2,
            },
        ];
        Ok(ReviewDiffPreviewResult {
            cwd: input.cwd.clone(),
            generated_at: UtcDateTime::now(),
            sources,
        })
    }

    async fn review_diff_file_contents(
        &self,
        input: &ReviewDiffFileContentsInput,
    ) -> Result<ReviewDiffFileContentsResult, GitCommandError> {
        let root = self
            .git_out(
                "GitVcsDriver.getReviewDiffFileContents.repositoryRoot",
                input.cwd.as_str(),
                argv(&["rev-parse", "--show-toplevel"]),
                false,
            )
            .await?;
        let root = root.trim();
        if root.is_empty() {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents",
                input.cwd.as_str(),
                &[],
                "Could not resolve the Git repository root.",
            ));
        }
        let old_revision = if input.source_kind == ReviewDiffPreviewSourceKind::WorkingTree {
            input
                .base_ref
                .as_ref()
                .map(|value| value.as_str().to_owned())
                .unwrap_or_else(|| "HEAD".to_owned())
        } else {
            self.resolve_review_merge_base(
                input.cwd.as_str(),
                None,
                input.base_ref.as_ref().map(|value| value.as_str()),
            )
            .await?
            .1
        };
        let old_contents = if input.change_type == ReviewDiffFileChangeType::New {
            String::new()
        } else {
            let mut call = GitCall::new(
                "GitVcsDriver.getReviewDiffFileContents.revision",
                argv(&["show", &format!("{old_revision}:{}", input.old_path.as_str())]),
            );
            call.max_output = Some(REVIEW_DIFF_FILE_MAX);
            let result = self.git(input.cwd.as_str(), call).await?;
            if result.stdout.contains('\0') {
                return Err(plain_error(
                    "GitVcsDriver.getReviewDiffFileContents",
                    input.cwd.as_str(),
                    &[],
                    &format!("Cannot expand binary file '{}'.", input.old_path.as_str()),
                ));
            }
            result.stdout
        };
        let new_contents = if input.change_type == ReviewDiffFileChangeType::Deleted {
            String::new()
        } else {
            self.read_working_tree_review_file(input, root).await?
        };
        Ok(ReviewDiffFileContentsResult {
            old_contents,
            new_contents,
        })
    }

    async fn read_working_tree_review_file(
        &self,
        input: &ReviewDiffFileContentsInput,
        repository_root: &str,
    ) -> Result<String, GitCommandError> {
        let requested = resolve_path(repository_root, input.new_path.as_str());
        let root = Path::new(repository_root);
        if !within_root(root, &requested) {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.path.resolve",
                input.cwd.as_str(),
                &[],
                &format!(
                    "Diff file '{}' resolves outside the review workspace.",
                    input.new_path.as_str()
                ),
            ));
        }
        let real_root = std::fs::canonicalize(root).map_err(|error| {
            plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.realPath",
                input.cwd.as_str(),
                &[],
                &format!("Could not resolve diff file '{}'.", input.new_path.as_str()),
            )
            .with_cause(error.to_string())
        })?;
        let real_target = std::fs::canonicalize(&requested).map_err(|error| {
            plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.realPath",
                input.cwd.as_str(),
                &[],
                &format!("Could not resolve diff file '{}'.", input.new_path.as_str()),
            )
            .with_cause(error.to_string())
        })?;
        if !within_root(&real_root, &real_target) {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.realPath",
                input.cwd.as_str(),
                &[],
                &format!(
                    "Diff file '{}' resolves outside the review workspace.",
                    input.new_path.as_str()
                ),
            ));
        }
        let info = std::fs::metadata(&real_target).map_err(|error| {
            plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.stat",
                input.cwd.as_str(),
                &[],
                &format!("Could not inspect diff file '{}'.", input.new_path.as_str()),
            )
            .with_cause(error.to_string())
        })?;
        if !info.is_file() {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.stat",
                input.cwd.as_str(),
                &[],
                &format!("Diff path '{}' is not a file.", input.new_path.as_str()),
            ));
        }
        if info.len() > REVIEW_DIFF_FILE_MAX as u64 {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.stat",
                input.cwd.as_str(),
                &[],
                &format!(
                    "Diff file '{}' exceeds the 1 MB expansion limit.",
                    input.new_path.as_str()
                ),
            ));
        }
        let bytes = std::fs::read(&real_target).map_err(|error| {
            plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.readFile",
                input.cwd.as_str(),
                &[],
                &format!("Could not read diff file '{}'.", input.new_path.as_str()),
            )
            .with_cause(error.to_string())
        })?;
        if bytes.contains(&0) {
            return Err(plain_error(
                "GitVcsDriver.getReviewDiffFileContents.workingTree.fs.readFile",
                input.cwd.as_str(),
                &[],
                &format!("Cannot expand binary file '{}'.", input.new_path.as_str()),
            ));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn read_refs_snapshot(&self, git_common_dir: &str) -> Result<RefsSnapshot, GitCommandError> {
        let fetch_cwd = fetch_cwd_for(git_common_dir);
        let git_dir = argv(&["--git-dir", git_common_dir]);
        let refs_args = {
            let mut args = git_dir.clone();
            args.extend(argv(&[
                "for-each-ref",
                "--format=%(refname)%09%(committerdate:unix)%09%(symref)",
                "refs/heads",
                "refs/remotes",
            ]));
            args
        };
        let default_args = {
            let mut args = git_dir.clone();
            args.extend(argv(&["symbolic-ref", "refs/remotes/origin/HEAD"]));
            args
        };
        let remote_args = {
            let mut args = git_dir.clone();
            args.extend(argv(&["remote"]));
            args
        };
        let (refs_result, default_ref, worktrees, remote_names_result) = tokio::join!(
            async {
                let mut call = GitCall::new("GitVcsDriver.listRefs.snapshotRefs", refs_args);
                call.stable = true;
                call.timeout = Some(Some(Duration::from_secs(30)));
                call.max_output = Some(16 * 1024 * 1024);
                call.fallback = Some("Git ref snapshot enumeration failed.");
                self.git(&fetch_cwd, call).await
            },
            async {
                let mut call = GitCall::new("GitVcsDriver.listRefs.defaultRef", default_args);
                call.timeout = Some(Some(Duration::from_secs(5)));
                call.allow_non_zero = true;
                self.git(&fetch_cwd, call).await
            },
            self.worktree_list(
                "GitVcsDriver.listRefs.worktreeList",
                &fetch_cwd,
                git_dir.clone(),
                Some(Duration::from_secs(30)),
            ),
            async {
                let mut call = GitCall::new("GitVcsDriver.listRefs.remoteNames", remote_args);
                call.timeout = Some(Some(Duration::from_secs(5)));
                call.allow_non_zero = true;
                self.git(&fetch_cwd, call).await
            },
        );
        let refs_result = refs_result?;
        let default_ref = default_ref?;
        let remote_names_result = remote_names_result?;
        let remote_names = if remote_names_result.exit_code == Some(0) {
            parse_remote_names(&remote_names_result.stdout)
        } else {
            if !remote_names_result.stderr.trim().is_empty() {
                tracing::warn!(
                    "GitVcsDriver.listRefs: remote name lookup returned code {:?} for {git_common_dir}",
                    remote_names_result.exit_code
                );
            }
            Vec::new()
        };
        let default_branch = if default_ref.exit_code == Some(0) {
            let prefix = "refs/remotes/origin/";
            let trimmed = default_ref.stdout.trim();
            trimmed.strip_prefix(prefix).map(str::to_owned)
        } else {
            None
        };
        let worktrees = worktrees?;
        let worktree_map = if worktrees.exit_code == Some(0) {
            parse_worktree_branch_paths(&worktrees.stdout)
                .into_iter()
                .filter(|(_, path)| Path::new(path).exists())
                .map(|(branch, path)| (branch, resolve_path(&fetch_cwd, &path).to_string_lossy().into_owned()))
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
        let mut local = Vec::new();
        let mut remote = Vec::new();
        for line in refs_result.stdout.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let Some(full_ref) = parts.next() else { continue };
            let last_commit_raw = parts.next().unwrap_or("0");
            let symbolic = parts.next().unwrap_or("");
            if !symbolic.is_empty() {
                continue;
            }
            let last_commit = last_commit_raw.parse::<i64>().unwrap_or(0);
            if let Some(name) = full_ref.strip_prefix("refs/heads/") {
                let Some(name_value) = trimmed_name(name) else { continue };
                local.push((
                    VcsRef {
                        name: name_value,
                        is_remote: Some(false),
                        remote_name: None,
                        current: false,
                        is_default: default_branch.as_deref() == Some(name),
                        worktree_path: worktree_map.get(name).and_then(|path| trimmed_name(path)),
                    },
                    last_commit,
                ));
            } else if let Some(name) = full_ref.strip_prefix("refs/remotes/") {
                let parsed = parse_remote_ref_with_remote_names(name, &remote_names);
                let Some(name_value) = trimmed_name(name) else { continue };
                remote.push((
                    VcsRef {
                        name: name_value,
                        is_remote: Some(true),
                        remote_name: parsed
                            .as_ref()
                            .and_then(|parsed| trimmed_name(&parsed.remote_name)),
                        current: false,
                        is_default: default_branch.as_ref().is_some_and(|branch| {
                            parsed.as_ref().is_some_and(|parsed| {
                                parsed.remote_name == "origin" && parsed.branch_name == *branch
                            })
                        }),
                        worktree_path: None,
                    },
                    last_commit,
                ));
            }
        }
        let sort_refs = |mut refs: Vec<(VcsRef, i64)>| {
            refs.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| left.0.name.as_str().cmp(right.0.name.as_str()))
            });
            refs.into_iter().map(|(reference, _)| reference).collect()
        };
        Ok(RefsSnapshot {
            local_branches: sort_refs(local),
            remote_branches: sort_refs(remote),
            has_primary_remote: remote_names.iter().any(|name| name == "origin"),
        })
    }

    async fn resolve_list_refs_snapshot(
        &self,
        git_common_dir: &str,
        refresh: bool,
    ) -> Result<RefsSnapshot, GitCommandError> {
        loop {
            let generation = self.state.refs.lock().generation(git_common_dir);
            let epoch = self.state.refs.lock().epoch(git_common_dir);
            let snapshot = if refresh || epoch.is_none() {
                let refresh_key = format!("{git_common_dir}\0{generation}");
                if let Some(hit) = self.state.refs.lock().refresh.get(&refresh_key) {
                    hit
                } else {
                    let epoch = self.state.refs.lock().bump_epoch(git_common_dir);
                    let loaded = self.read_refs_snapshot(git_common_dir).await?;
                    let snapshot_key = format!("{git_common_dir}\0{epoch}");
                    self.state.refs.lock().snapshots.insert(
                        snapshot_key,
                        loaded.clone(),
                        Duration::from_secs(2 * 60),
                    );
                    self.state.refs.lock().refresh.insert(
                        refresh_key,
                        loaded.clone(),
                        Duration::from_secs(5),
                    );
                    loaded
                }
            } else {
                let epoch = epoch.unwrap_or(0);
                let snapshot_key = format!("{git_common_dir}\0{epoch}");
                if let Some(hit) = self.state.refs.lock().snapshots.get(&snapshot_key) {
                    hit
                } else {
                    let loaded = self.read_refs_snapshot(git_common_dir).await?;
                    self.state.refs.lock().snapshots.insert(
                        snapshot_key,
                        loaded.clone(),
                        Duration::from_secs(2 * 60),
                    );
                    loaded
                }
            };
            if self.state.refs.lock().generation(git_common_dir) == generation {
                return Ok(snapshot);
            }
        }
    }

    async fn invalidate_list_refs(&self, cwd: &str) -> Result<(), GitCommandError> {
        let key = cache_key(cwd);
        let cached = self.state.paths.lock().get(&key).flatten();
        let Some(paths) = cached.or(self.resolve_repository_paths(cwd, false).await?) else {
            return Ok(());
        };
        let previous = self.state.refs.lock().generation(&paths.git_common_dir);
        self.state.refs.lock().bump_generation(&paths.git_common_dir);
        self.state.refs.lock().bump_epoch(&paths.git_common_dir);
        let refresh_key = format!("{}\0{previous}", paths.git_common_dir);
        self.state.refs.lock().refresh.invalidate(&refresh_key);
        self.state.paths_refresh.lock().invalidate(&key);
        self.state.paths.lock().invalidate(&key);
        Ok(())
    }

    async fn list_refs_inner(
        &self,
        input: &VcsListRefsInput,
    ) -> Result<VcsListRefsResult, GitCommandError> {
        let paths = match self
            .resolve_repository_paths(input.cwd.as_str(), input.refresh == Some(true))
            .await
        {
            Err(error) if is_missing_cwd(&error) => None,
            other => other?,
        };
        let Some(paths) = paths else {
            return Ok(VcsListRefsResult {
                refs: Vec::new(),
                is_repo: false,
                has_primary_remote: false,
                next_cursor: None,
                total_count: 0,
            });
        };
        let snapshot = self
            .resolve_list_refs_snapshot(&paths.git_common_dir, input.refresh == Some(true))
            .await?;
        let has_current = paths.worktree_root.as_ref().is_some_and(|root| {
            snapshot
                .local_branches
                .iter()
                .any(|reference| reference.worktree_path.as_ref().map(|path| path.as_str()) == Some(root.as_str()))
        });
        let local: Vec<VcsRef> = snapshot
            .local_branches
            .into_iter()
            .map(|mut reference| {
                reference.current = if has_current {
                    paths.worktree_root.as_ref().is_some_and(|root| {
                        reference.worktree_path.as_ref().map(|path| path.as_str()) == Some(root.as_str())
                    })
                } else {
                    paths.current_branch.as_deref() == Some(reference.name.as_str())
                };
                reference
            })
            .collect();
        let mut combined = local;
        combined.extend(snapshot.remote_branches);
        if input.include_matching_remote_refs != Some(true) {
            combined = dedupe_remote_branches_with_local_matches(&combined);
        }
        combined.sort_by(|left, right| {
            let priority = |reference: &VcsRef| {
                if reference.current {
                    0
                } else if reference.is_default {
                    1
                } else {
                    2
                }
            };
            priority(left).cmp(&priority(right))
        });
        let filtered = match input.ref_kind {
            Some(VcsListRefsRefKind::Local) => combined
                .into_iter()
                .filter(|reference| reference.is_remote != Some(true))
                .collect(),
            Some(VcsListRefsRefKind::Remote) => combined
                .into_iter()
                .filter(|reference| reference.is_remote == Some(true))
                .collect(),
            _ => combined,
        };
        let queried = filter_branches_for_list_query(
            &filtered,
            input.query.as_ref().map(|query| query.as_str()),
        );
        let page = paginate_branches(
            &queried,
            input.cursor.map(|cursor| cursor as usize),
            input.limit.map(|limit| limit.get() as usize),
        );
        Ok(VcsListRefsResult {
            refs: page.refs,
            is_repo: true,
            has_primary_remote: snapshot.has_primary_remote,
            next_cursor: page.next_cursor.map(|cursor| cursor as u64),
            total_count: page.total_count as u64,
        })
    }

    async fn create_worktree_inner(
        &self,
        input: &VcsCreateWorktreeInput,
        options: CreateWorktreeOptions,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        let target_branch = input
            .new_ref_name
            .as_ref()
            .map(|name| name.as_str().to_owned())
            .unwrap_or_else(|| input.ref_name.as_str().to_owned());
        let worktree_path = if let Some(path) = &input.path {
            path.as_str().to_owned()
        } else {
            let setting = options.worktrees_directory.as_deref().unwrap_or("");
            let Some(parent) = resolve_worktrees_directory(setting) else {
                return Err(plain_error(
                    "GitVcsDriver.createWorktree",
                    input.cwd.as_str(),
                    &argv(&["worktree", "add"]),
                    &format!(
                        "The worktree location \"{}\" must be an absolute folder on this machine, not a drive root. Change it in Settings → Storage.",
                        options.worktrees_directory.as_deref().unwrap_or("")
                    ),
                ));
            };
            let repo_name = Path::new(input.cwd.as_str())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".to_owned());
            let sanitized = target_branch.replace('/', "-");
            parent
                .join(repo_name)
                .join(sanitized)
                .to_string_lossy()
                .into_owned()
        };
        let mut args = if let Some(new_ref) = &input.new_ref_name {
            argv(&[
                "worktree",
                "add",
                "-b",
                new_ref.as_str(),
                &worktree_path,
                input.ref_name.as_str(),
            ])
        } else {
            argv(&["worktree", "add", &worktree_path, input.ref_name.as_str()])
        };
        let workers = self
            .read_config_value(input.cwd.as_str(), "checkout.workers")
            .await?
            .unwrap_or_else(|| "0".to_owned());
        let mut full = argv(&["-c", &format!("checkout.workers={workers}")]);
        full.append(&mut args);
        let mut call = GitCall::new("GitVcsDriver.createWorktree", full);
        call.fallback = Some("git worktree add failed");
        call.timeout = Some(Some(Duration::from_secs(300)));
        if options.on_checkout_progress.is_some() {
            call.env.insert("GIT_PROGRESS_DELAY".to_owned(), "0".to_owned());
            call.env.insert("LC_ALL".to_owned(), "C".to_owned());
            if let Some(progress) = options.on_checkout_progress.clone() {
                call.on_stderr_line = Some(Arc::new(move |line| {
                    if let Some(parsed) = parse_git_checkout_progress_line(line) {
                        progress(parsed);
                    }
                }));
            }
        }
        self.git(input.cwd.as_str(), call).await?;
        if let Some(claimed) = &options.on_worktree_claimed {
            claimed(&worktree_path);
        }
        let has_submodules = Path::new(&worktree_path).join(".gitmodules").exists();
        let (mode, source) = if !has_submodules {
            (WorktreeSubmodules::None, "environment")
        } else if let Some(mode) = options.submodules {
            (mode, "environment")
        } else {
            let file = Path::new(&worktree_path).join("t3.json");
            let parsed = std::fs::read_to_string(&file).ok().and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("worktreeSubmodules")
                            .and_then(|item| item.as_str())
                            .and_then(|mode| match mode {
                                "recursive" => Some(WorktreeSubmodules::Recursive),
                                "top-level" => Some(WorktreeSubmodules::TopLevel),
                                "none" => Some(WorktreeSubmodules::None),
                                _ => None,
                            })
                    })
            });
            match parsed {
                Some(mode) => (mode, "t3.json"),
                None => (WorktreeSubmodules::Recursive, "environment"),
            }
        };
        if has_submodules && mode == WorktreeSubmodules::None
            && let Some(disabled) = &options.on_submodules_disabled
        {
            disabled(if source == "t3.json" { "t3.json" } else { "settings" });
        }
        if mode != WorktreeSubmodules::None {
            if let Some(started) = &options.on_submodules_started {
                started();
            }
            let submodule_args = if mode == WorktreeSubmodules::Recursive {
                argv(&["submodule", "update", "--init", "--recursive"])
            } else {
                argv(&["submodule", "update", "--init"])
            };
            let mut call = GitCall::new("GitVcsDriver.createWorktree.updateSubmodules", submodule_args);
            if options.on_submodule_line.is_some() {
                call.env.insert("LC_ALL".to_owned(), "C".to_owned());
                call.on_stdout_line = options.on_submodule_line.clone();
                call.on_stderr_line = options.on_submodule_line.clone();
            }
            match self.git(&worktree_path, call).await {
                Err(error) => {
                    tracing::warn!("worktree submodule checkout failed; submodule paths are empty");
                    if let Some(finished) = &options.on_submodules_finished {
                        finished(false, Some(error.to_string()));
                    }
                }
                Ok(_) => {
                    if let Some(finished) = &options.on_submodules_finished {
                        finished(true, None);
                    }
                }
            }
        }
        if let (Some(new_ref), Some(base_ref)) = (&input.new_ref_name, &input.base_ref_name)
            && let Some(base_branch) = self
                .resolve_branch_name(input.cwd.as_str(), base_ref.as_str())
                .await?
        {
            self.git_void(
                "GitVcsDriver.createWorktree.configureBaseRef",
                input.cwd.as_str(),
                argv(&[
                    "config",
                    &format!("branch.{}.gh-merge-base", new_ref.as_str()),
                    &base_branch,
                ]),
                None,
            )
            .await?;
        }
        Ok(VcsCreateWorktreeResult {
            worktree: VcsWorktree {
                path: trimmed_name(&worktree_path).unwrap_or_else(|| trimmed_name(".").expect("path")),
                ref_name: trimmed_name(&target_branch).unwrap_or_else(|| input.ref_name.clone()),
            },
        })
    }

    async fn fetch_pull_request_branch_inner(
        &self,
        input: &GitFetchPullRequestBranchInput,
    ) -> Result<(), GitCommandError> {
        let remote_name = self.resolve_primary_remote_name(&input.cwd).await?;
        let mut call = GitCall::new(
            "GitVcsDriver.fetchPullRequestBranch",
            argv(&[
                "fetch",
                "--quiet",
                "--no-tags",
                &remote_name,
                &format!(
                    "+refs/pull/{}/head:refs/heads/{}",
                    input.pr_number, input.branch
                ),
            ]),
        );
        call.fallback = Some("git fetch pull request branch failed");
        self.git(&input.cwd, call).await?;
        Ok(())
    }

    async fn refresh_checked_out_branch_inner(
        &self,
        input: &GitRefreshCheckedOutBranchInput,
    ) -> Result<GitRefreshCheckedOutBranchResult, GitCommandError> {
        let head = self
            .resolve_commit(&GitResolveCommitInput {
                cwd: input.cwd.clone(),
                revision: "HEAD".to_owned(),
            })
            .await?
            .commit_sha;
        if head == input.target_commit {
            return Ok(GitRefreshCheckedOutBranchResult {
                head_commit: head,
                moved: false,
                on_target: true,
            });
        }
        let changes = self
            .git_out(
                "GitVcsDriver.refreshCheckedOutBranch.status",
                &input.cwd,
                argv(&["status", "--porcelain"]),
                false,
            )
            .await?;
        if !changes.trim().is_empty() {
            return Ok(GitRefreshCheckedOutBranchResult {
                head_commit: head,
                moved: false,
                on_target: false,
            });
        }
        let mut ancestor = GitCall::new(
            "GitVcsDriver.refreshCheckedOutBranch.isAncestor",
            argv(&["merge-base", "--is-ancestor", &head, &input.target_commit]),
        );
        ancestor.allow_non_zero = true;
        let is_ancestor = self.git(&input.cwd, ancestor).await?.exit_code == Some(0);
        if !is_ancestor && Some(head.as_str()) != input.reset_when_head_commit.as_deref() {
            return Ok(GitRefreshCheckedOutBranchResult {
                head_commit: head,
                moved: false,
                on_target: false,
            });
        }
        if !is_ancestor {
            let mut keep = GitCall::new(
                "GitVcsDriver.refreshCheckedOutBranch.keepPrevious",
                argv(&["update-ref", "refs/t3code/pre-refresh", &head]),
            );
            keep.fallback = Some("git failed to record the previous checkout commit");
            self.git(&input.cwd, keep).await?;
        }
        let move_args = if is_ancestor {
            argv(&["merge", "--ff-only", &input.target_commit])
        } else {
            argv(&["reset", "--merge", &input.target_commit])
        };
        let mut call = GitCall::new("GitVcsDriver.refreshCheckedOutBranch.move", move_args);
        call.timeout = Some(Some(Duration::from_secs(30)));
        call.fallback = Some("git failed to move the checkout onto the pull request head");
        self.git(&input.cwd, call).await?;
        Ok(GitRefreshCheckedOutBranchResult {
            head_commit: input.target_commit.clone(),
            moved: true,
            on_target: true,
        })
    }

    async fn ensure_remote_inner(
        &self,
        input: &GitEnsureRemoteInput,
    ) -> Result<String, GitCommandError> {
        let preferred = sanitize_remote_name(&input.preferred_name);
        let target = normalize_git_remote_url(&input.url);
        let stdout = self
            .git_out(
                "GitVcsDriver.ensureRemote.listRemoteUrls",
                &input.cwd,
                argv(&["remote", "-v"]),
                false,
            )
            .await?;
        let remotes = parse_remote_fetch_urls(&stdout);
        for (name, url) in &remotes {
            if normalize_git_remote_url(url) == target {
                return Ok(name.clone());
            }
        }
        let mut remote_name = preferred.clone();
        let mut suffix = 1_u32;
        while remotes.contains_key(&remote_name) {
            remote_name = format!("{preferred}-{suffix}");
            suffix += 1;
        }
        self.git_void(
            "GitVcsDriver.ensureRemote.add",
            &input.cwd,
            argv(&["remote", "add", &remote_name, &input.url]),
            None,
        )
        .await?;
        Ok(remote_name)
    }

    async fn run_fetch(
        &self,
        cwd: &str,
        args: Vec<String>,
    ) -> Result<ExecuteGitResult, GitCommandError> {
        let mut call = GitCall::new("GitVcsDriver.fetchRemote", args);
        call.env = status_refresh_env();
        call.allow_non_zero = true;
        call.stable = true;
        let result = self.git(cwd, call.clone_call()).await?;
        if result.exit_code != Some(0) && is_ref_lock_failure(&result.stderr) {
            return self.git(cwd, call).await;
        }
        Ok(result)
    }

    async fn fetch_remote_inner(&self, input: &GitFetchRemoteInput) -> Result<(), GitCommandError> {
        let args = argv(&["fetch", "--quiet", &input.remote_name]);
        let fallback = format!("git fetch {} failed", input.remote_name);
        if input.ref_name.is_none() {
            let result = self.run_fetch(&input.cwd, args.clone()).await?;
            if result.exit_code == Some(0) {
                return Ok(());
            }
            return Err(failed(
                "GitVcsDriver.fetchRemote",
                &input.cwd,
                &args,
                &result,
                fetch_failure_detail(&result.stderr).unwrap_or(fallback.as_str()),
            ));
        }
        let branch = parse_remote_ref_with_remote_names(
            input.ref_name.as_deref().unwrap_or(""),
            std::slice::from_ref(&input.remote_name),
        )
        .map(|parsed| parsed.branch_name)
        .unwrap_or_else(|| input.ref_name.clone().unwrap_or_default());
        let mut scoped = args.clone();
        scoped.push(format!(
            "+refs/heads/{branch}:refs/remotes/{}/{branch}",
            input.remote_name
        ));
        let result = self.run_fetch(&input.cwd, scoped.clone()).await?;
        if result.exit_code == Some(0) {
            return Ok(());
        }
        let missing = format!("fatal: couldn't find remote ref refs/heads/{branch}");
        if result.stderr.split(['\r', '\n']).any(|line| line == missing) {
            let all = self.run_fetch(&input.cwd, args.clone()).await?;
            if all.exit_code == Some(0) {
                return Ok(());
            }
            return Err(failed(
                "GitVcsDriver.fetchRemote",
                &input.cwd,
                &args,
                &all,
                fetch_failure_detail(&all.stderr).unwrap_or(fallback.as_str()),
            ));
        }
        Err(failed(
            "GitVcsDriver.fetchRemote",
            &input.cwd,
            &scoped,
            &result,
            fetch_failure_detail(&result.stderr).unwrap_or(fallback.as_str()),
        ))
    }

    async fn fetch_remote_branch_inner(
        &self,
        input: &GitFetchRemoteBranchInput,
    ) -> Result<(), GitCommandError> {
        self.git_void(
            "GitVcsDriver.fetchRemoteBranch.fetch",
            &input.cwd,
            argv(&[
                "fetch",
                "--quiet",
                "--no-tags",
                &input.remote_name,
                &format!(
                    "+refs/heads/{0}:refs/remotes/{1}/{0}",
                    input.remote_branch, input.remote_name
                ),
            ]),
            None,
        )
        .await?;
        let exists = self.branch_exists(&input.cwd, &input.local_branch).await?;
        let target = format!("{}/{}", input.remote_name, input.remote_branch);
        let args = if exists {
            argv(&["branch", "--force", &input.local_branch, &target])
        } else {
            argv(&["branch", &input.local_branch, &target])
        };
        self.git_void("GitVcsDriver.fetchRemoteBranch.materialize", &input.cwd, args, None)
            .await
    }

    async fn remove_worktree_inner(
        &self,
        input: &VcsRemoveWorktreeInput,
    ) -> Result<(), GitCommandError> {
        let mut args = argv(&[
            "-c",
            "status.showUntrackedFiles=normal",
            "worktree",
            "remove",
        ]);
        if input.force == Some(true) {
            args.push("--force".to_owned());
        }
        args.push(input.path.as_str().to_owned());
        let mut call = GitCall::new("GitVcsDriver.removeWorktree", args.clone());
        call.timeout = Some(Some(Duration::from_secs(5 * 60)));
        call.allow_non_zero = true;
        call.stable = true;
        let result = self.git(input.cwd.as_str(), call).await?;
        if result.exit_code == Some(0) {
            return Ok(());
        }
        let already_gone = is_missing_worktree_stderr(&result.stderr)
            && !Path::new(input.path.as_str()).exists();
        if already_gone {
            let mut prune = GitCall::new("GitVcsDriver.pruneWorktrees", argv(&["worktree", "prune"]));
            prune.timeout = Some(Some(Duration::from_secs(15)));
            prune.fallback = Some("git worktree prune failed");
            self.git(input.cwd.as_str(), prune).await?;
            return Ok(());
        }
        tracing::warn!(
            "GitVcsDriver.removeWorktree: git worktree remove exited with code {:?} for {} (stderr length {}).",
            result.exit_code,
            input.path.as_str(),
            result.stderr.len()
        );
        Err(failed(
            "GitVcsDriver.removeWorktree",
            input.cwd.as_str(),
            &args,
            &result,
            "git worktree remove failed",
        ))
    }

    async fn rename_branch_inner(
        &self,
        input: &GitRenameBranchInput,
    ) -> Result<GitRenameBranchResult, GitCommandError> {
        if input.old_branch == input.new_branch {
            return Ok(GitRenameBranchResult {
                branch: input.new_branch.clone(),
            });
        }
        let target = if input.exact_name {
            input.new_branch.clone()
        } else {
            self.resolve_available_branch_name(&input.cwd, &input.new_branch)
                .await?
        };
        let mut call = GitCall::new(
            "GitVcsDriver.renameBranch",
            argv(&["branch", "-m", "--", &input.old_branch, &target]),
        );
        call.timeout = Some(Some(Duration::from_secs(10)));
        call.fallback = Some("git branch rename failed");
        self.git(&input.cwd, call).await?;
        Ok(GitRenameBranchResult { branch: target })
    }

    async fn create_ref_inner(
        &self,
        input: &VcsCreateRefInput,
    ) -> Result<VcsCreateRefResult, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.createRef",
            argv(&["branch", input.ref_name.as_str()]),
        );
        call.timeout = Some(Some(Duration::from_secs(10)));
        call.fallback = Some("git branch create failed");
        self.git(input.cwd.as_str(), call).await?;
        if input.switch_ref == Some(true) {
            self.switch_ref_inner(&VcsSwitchRefInput {
                cwd: input.cwd.clone(),
                ref_name: input.ref_name.clone(),
            })
            .await?;
        }
        Ok(VcsCreateRefResult {
            ref_name: input.ref_name.clone(),
        })
    }

    async fn show_ref_ok(&self, operation: &'static str, cwd: &str, reference: &str) -> Result<bool, GitCommandError> {
        let mut call = GitCall::new(operation, argv(&["show-ref", "--verify", "--quiet", reference]));
        call.timeout = Some(Some(Duration::from_secs(5)));
        call.allow_non_zero = true;
        Ok(self.git(cwd, call).await?.exit_code == Some(0))
    }

    async fn switch_ref_inner(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        let cwd = input.cwd.as_str();
        let ref_name = input.ref_name.as_str();
        let local_ref = format!("refs/heads/{ref_name}");
        let remote_ref = format!("refs/remotes/{ref_name}");
        let (local_exists, remote_exists) = tokio::join!(
            self.show_ref_ok("GitVcsDriver.switchRef.localInputExists", cwd, &local_ref),
            self.show_ref_ok("GitVcsDriver.switchRef.remoteExists", cwd, &remote_ref),
        );
        let local_exists = local_exists?;
        let remote_exists = remote_exists?;
        let local_tracking = if remote_exists {
            let mut call = GitCall::new(
                "GitVcsDriver.switchRef.localTrackingBranch",
                argv(&[
                    "for-each-ref",
                    "--format=%(refname:short)\t%(upstream:short)",
                    "refs/heads",
                ]),
            );
            call.timeout = Some(Some(Duration::from_secs(5)));
            call.allow_non_zero = true;
            let result = self.git(cwd, call).await?;
            if result.exit_code == Some(0) {
                parse_tracking_branch_by_upstream_ref(&result.stdout, ref_name)
            } else {
                None
            }
        } else {
            None
        };
        let local_candidate = super::derive_local_branch_name_from_remote_ref(ref_name);
        let local_target_exists = if remote_exists
            && let Some(candidate) = &local_candidate
        {
            self.show_ref_ok(
                "GitVcsDriver.switchRef.localTrackedBranchTargetExists",
                cwd,
                &format!("refs/heads/{candidate}"),
            )
            .await?
        } else {
            false
        };
        let checkout = if local_exists
            || (remote_exists && local_tracking.is_none() && local_target_exists)
        {
            argv(&["checkout", ref_name])
        } else if remote_exists && local_tracking.is_none() {
            argv(&["checkout", "--track", ref_name])
        } else if remote_exists && let Some(tracking) = &local_tracking {
            argv(&["checkout", tracking])
        } else {
            argv(&["checkout", ref_name])
        };
        let mut args = checkout;
        args.push("--".to_owned());
        let mut call = GitCall::new("GitVcsDriver.switchRef.checkout", args);
        call.timeout = Some(Some(Duration::from_secs(10)));
        call.fallback = Some("git checkout failed");
        self.git(cwd, call).await?;
        let current = self
            .git_out(
                "GitVcsDriver.switchRef.currentBranch",
                cwd,
                argv(&["branch", "--show-current"]),
                false,
            )
            .await?;
        Ok(VcsSwitchRefResult {
            ref_name: nonempty(current.trim()).and_then(|name| trimmed_name(&name)),
        })
    }
}

impl GitCall {
    fn clone_call(&self) -> Self {
        Self {
            operation: self.operation,
            args: self.args.clone(),
            allow_non_zero: self.allow_non_zero,
            timeout: self.timeout,
            max_output: self.max_output,
            marker: self.marker,
            stable: self.stable,
            env: self.env.clone(),
            stdin: self.stdin.clone(),
            fallback: self.fallback,
            on_stdout_line: self.on_stdout_line.clone(),
            on_stderr_line: self.on_stderr_line.clone(),
        }
    }
}
