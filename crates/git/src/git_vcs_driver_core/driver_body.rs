impl GitVcsDriverCore {
    async fn limited_stdout(
        &self,
        operation: &'static str,
        cwd: &str,
        args: Vec<String>,
        max_output: usize,
    ) -> Result<String, GitCommandError> {
        let mut call = GitCall::new(operation, args);
        call.max_output = Some(max_output);
        call.marker = true;
        Ok(stdout_text(&self.git(cwd, call).await?))
    }

    async fn branch_exists(&self, cwd: &str, ref_name: &str) -> Result<bool, GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.branchExists",
            argv(&["show-ref", "--verify", "--quiet", &format!("refs/heads/{ref_name}")]),
        );
        call.allow_non_zero = true;
        call.timeout = Some(Some(Duration::from_secs(5)));
        Ok(self.git(cwd, call).await?.exit_code == Some(0))
    }

    async fn resolve_available_branch_name(
        &self,
        cwd: &str,
        desired: &str,
    ) -> Result<String, GitCommandError> {
        if !self.branch_exists(cwd, desired).await? {
            return Ok(desired.to_owned());
        }
        for suffix in 1..=100 {
            let candidate = format!("{desired}-{suffix}");
            if !self.branch_exists(cwd, &candidate).await? {
                return Ok(candidate);
            }
        }
        Err(plain_error(
            "GitVcsDriver.renameBranch",
            cwd,
            &argv(&["branch", "-m", "--", desired]),
            &format!("Could not find an available branch name for '{desired}'."),
        ))
    }

    async fn list_remote_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        let stdout = self
            .git_out("GitVcsDriver.listRemoteNames", cwd, argv(&["remote"]), false)
            .await?;
        Ok(parse_remote_names_in_git_order(&stdout))
    }

    async fn resolve_current_upstream(
        &self,
        cwd: &str,
    ) -> Result<Option<super::ParsedUpstreamRef>, GitCommandError> {
        let stdout = self
            .git_out(
                "GitVcsDriver.resolveCurrentUpstream",
                cwd,
                argv(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]),
                true,
            )
            .await?;
        let upstream_ref = stdout.trim();
        if upstream_ref.is_empty() || upstream_ref == "@{upstream}" {
            return Ok(None);
        }
        let remote_names = match self.list_remote_names(cwd).await {
            Ok(stdout_names) => parse_remote_names(&stdout_names.join("\n")),
            Err(_) => Vec::new(),
        };
        Ok(parse_upstream_ref_with_remote_names(upstream_ref, &remote_names)
            .or_else(|| parse_upstream_ref_by_first_separator(upstream_ref)))
    }

    async fn fetch_remote_for_status(
        &self,
        git_common_dir: &str,
        remote_name: &str,
    ) -> Result<(), GitCommandError> {
        let mut call = GitCall::new(
            "GitVcsDriver.fetchRemoteForStatus",
            argv(&[
                "--git-dir",
                git_common_dir,
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-auto-gc",
                remote_name,
            ]),
        );
        call.env = status_refresh_env();
        call.fallback = Some("Background Git fetch exited with a non-zero status.");
        call.timeout = Some(Some(Duration::from_secs(5)));
        self.git(&fetch_cwd_for(git_common_dir), call).await?;
        Ok(())
    }

    async fn resolve_repository_paths_uncached(
        &self,
        cwd: &str,
    ) -> Result<Option<RepoPaths>, GitCommandError> {
        let args = argv(&["rev-parse", "--git-common-dir"]);
        let mut call = GitCall::new("GitVcsDriver.resolveRepositoryPaths.commonDir", args.clone());
        call.timeout = Some(Some(Duration::from_secs(5)));
        call.allow_non_zero = true;
        call.stable = true;
        let common = match self.git(cwd, call).await {
            Err(error) if is_missing_cwd(&error) => return Ok(None),
            other => other?,
        };
        if common.exit_code != Some(0) {
            if is_non_repository_git_stderr(common.stderr.trim()) {
                return Ok(None);
            }
            return Err(failed(
                "GitVcsDriver.resolveRepositoryPaths.commonDir",
                cwd,
                &args,
                &common,
                "Failed to resolve the Git common directory.",
            ));
        }
        let common_output = common.stdout.trim();
        let resolved = resolve_path(cwd, common_output);
        let git_common_dir = real_or_normalized(&resolved);
        let worktree_args = argv(&["rev-parse", "--show-toplevel"]);
        let branch_args = argv(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let (worktree, branch) = tokio::join!(
            self.git(cwd, {
                let mut call =
                    GitCall::new("GitVcsDriver.resolveRepositoryPaths.worktreeRoot", worktree_args);
                call.timeout = Some(Some(Duration::from_secs(5)));
                call.allow_non_zero = true;
                call
            }),
            self.git(cwd, {
                let mut call =
                    GitCall::new("GitVcsDriver.resolveRepositoryPaths.currentBranch", branch_args);
                call.timeout = Some(Some(Duration::from_secs(5)));
                call.allow_non_zero = true;
                call
            }),
        );
        let worktree = worktree?;
        let branch = branch?;
        let worktree_root = (worktree.exit_code == Some(0))
            .then(|| nonempty(worktree.stdout.trim()))
            .flatten()
            .map(|value| resolve_path(cwd, &value).to_string_lossy().into_owned());
        let current_branch = (branch.exit_code == Some(0))
            .then(|| nonempty(branch.stdout.trim()))
            .flatten();
        Ok(Some(RepoPaths {
            git_common_dir,
            worktree_root,
            current_branch,
        }))
    }

    async fn resolve_repository_paths(
        &self,
        cwd: &str,
        refresh: bool,
    ) -> Result<Option<RepoPaths>, GitCommandError> {
        let key = cache_key(cwd);
        if refresh {
            if let Some(hit) = self.state.paths_refresh.lock().get(&key) {
                return Ok(hit);
            }
            self.state.paths.lock().invalidate(&key);
        } else if let Some(hit) = self.state.paths.lock().get(&key) {
            return Ok(hit);
        }
        let loaded = self.resolve_repository_paths_uncached(cwd).await?;
        let ttl = paths_ttl(&loaded);
        self.state.paths.lock().insert(key.clone(), loaded.clone(), ttl);
        if refresh {
            let refresh_ttl = if loaded.is_none() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(5)
            };
            self.state
                .paths_refresh
                .lock()
                .insert(key, loaded.clone(), refresh_ttl);
        }
        Ok(loaded)
    }

    async fn cached_default_branch(
        &self,
        cwd: &str,
        git_common_dir: &str,
    ) -> Result<Option<String>, GitCommandError> {
        if let Some(hit) = self.state.default_branch.lock().get(git_common_dir) {
            return Ok(hit);
        }
        let fetch_cwd = fetch_cwd_for(git_common_dir);
        let mut call = GitCall::new(
            "GitVcsDriver.statusDetails.defaultBranch",
            argv(&[
                "--git-dir",
                git_common_dir,
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
            ]),
        );
        call.allow_non_zero = true;
        let result = self.git(&fetch_cwd, call).await?;
        let branch = if result.exit_code == Some(0) {
            parse_default_branch_from_remote_head_ref(&result.stdout, "origin")
        } else {
            None
        };
        self.state.default_branch.lock().insert(
            git_common_dir.to_owned(),
            branch.clone(),
            Duration::from_secs(5 * 60),
        );
        let _ = cwd;
        Ok(branch)
    }

    async fn cached_origin_exists(
        &self,
        git_common_dir: &str,
    ) -> Result<bool, GitCommandError> {
        if let Some(hit) = self.state.origin_exists.lock().get(git_common_dir) {
            return Ok(hit);
        }
        let fetch_cwd = fetch_cwd_for(git_common_dir);
        let mut call = GitCall::new(
            "GitVcsDriver.statusDetails.originExists",
            argv(&["--git-dir", git_common_dir, "remote", "get-url", "origin"]),
        );
        call.allow_non_zero = true;
        let exists = self.git(&fetch_cwd, call).await?.exit_code == Some(0);
        self.state.origin_exists.lock().insert(
            git_common_dir.to_owned(),
            exists,
            Duration::from_secs(5 * 60),
        );
        Ok(exists)
    }

    async fn invalidate_status_static(&self, cwd: &str) -> Result<(), GitCommandError> {
        let paths = self.resolve_repository_paths(cwd, false).await.ok().flatten();
        let key = paths
            .as_ref()
            .map(|paths| paths.git_common_dir.clone())
            .unwrap_or_else(|| cache_key(cwd));
        self.state.default_branch.lock().invalidate(&key);
        self.state.origin_exists.lock().invalidate(&key);
        Ok(())
    }

    async fn resolve_git_common_dir(&self, cwd: &str) -> Result<String, GitCommandError> {
        if let Some(paths) = self.resolve_repository_paths(cwd, false).await? {
            return Ok(paths.git_common_dir);
        }
        Err(plain_error(
            "GitVcsDriver.resolveGitCommonDir",
            cwd,
            &argv(&["rev-parse", "--git-common-dir"]),
            "Cannot resolve a Git common directory outside a repository.",
        ))
    }

    async fn refresh_status_upstream_if_stale(&self, cwd: &str) -> Result<(), GitCommandError> {
        let Some(upstream) = self.resolve_current_upstream(cwd).await? else {
            return Ok(());
        };
        let common = self.resolve_git_common_dir(cwd).await?;
        let key = format!("{}\0{}", common, upstream.remote_name);
        if self.state.status_refresh.lock().fresh(&key) {
            return Ok(());
        }
        match self
            .fetch_remote_for_status(&common, &upstream.remote_name)
            .await
        {
            Ok(()) => self.state.status_refresh.lock().succeed(&key),
            Err(error) => {
                tracing::warn!("Background Git fetch failed");
                self.state.status_refresh.lock().fail(&key);
                let _ = error;
            }
        }
        Ok(())
    }

    async fn resolve_publish_branch_name(
        &self,
        cwd: &str,
        branch_name: &str,
    ) -> Result<String, GitCommandError> {
        let remote_names = self.list_remote_names(cwd).await.unwrap_or_default();
        let sorted = parse_remote_names(&remote_names.join("\n"));
        Ok(parse_remote_ref_with_remote_names(branch_name, &sorted)
            .map(|parsed| parsed.branch_name)
            .unwrap_or_else(|| branch_name.to_owned()))
    }

    async fn resolve_push_remote_name(
        &self,
        cwd: &str,
        ref_name: &str,
    ) -> Result<Option<String>, GitCommandError> {
        let branch_remote = self
            .git_out(
                "GitVcsDriver.resolvePushRemoteName.branchPushRemote",
                cwd,
                argv(&["config", "--get", &format!("branch.{ref_name}.pushRemote")]),
                true,
            )
            .await?;
        if let Some(name) = nonempty(branch_remote.trim()) {
            return Ok(Some(name));
        }
        let push_default = self
            .git_out(
                "GitVcsDriver.resolvePushRemoteName.remotePushDefault",
                cwd,
                argv(&["config", "--get", "remote.pushDefault"]),
                true,
            )
            .await?;
        if let Some(name) = nonempty(push_default.trim()) {
            return Ok(Some(name));
        }
        Ok(self.resolve_primary_remote_name(cwd).await.ok())
    }

    async fn resolve_base_branch_for_no_upstream(
        &self,
        cwd: &str,
        ref_name: &str,
        allow_remote_of_current: bool,
    ) -> Result<Option<String>, GitCommandError> {
        let configured = self
            .git_out(
                "GitVcsDriver.resolveBaseBranchForNoUpstream.config",
                cwd,
                argv(&["config", "--get", &format!("branch.{ref_name}.gh-merge-base")]),
                true,
            )
            .await?;
        let primary = self.resolve_primary_remote_name(cwd).await.ok();
        let default_branch = match &primary {
            Some(remote) => self.resolve_default_branch_name(cwd, remote).await?,
            None => None,
        };
        let mut candidates = Vec::new();
        if let Some(name) = nonempty(configured.trim()) {
            candidates.push(name);
        }
        if let Some(name) = default_branch {
            candidates.push(name);
        }
        candidates.push("main".to_owned());
        candidates.push("master".to_owned());
        for candidate in candidates {
            let remote_prefix = primary
                .as_ref()
                .filter(|name| name.as_str() != "origin")
                .map(|name| format!("{name}/"));
            let normalized = if let Some(rest) = candidate.strip_prefix("origin/") {
                rest.to_owned()
            } else if let Some(prefix) = remote_prefix.as_ref().filter(|prefix| candidate.starts_with(prefix.as_str()))
            {
                candidate[prefix.len()..].to_owned()
            } else {
                candidate
            };
            if normalized.is_empty() {
                continue;
            }
            if normalized == ref_name {
                if allow_remote_of_current
                    && let Some(remote) = &primary
                    && self
                        .remote_branch_exists(&GitRemoteBranchExistsInput {
                            cwd: cwd.to_owned(),
                            remote_name: remote.clone(),
                            ref_name: normalized.clone(),
                        })
                        .await?
                {
                    return Ok(Some(format!("{remote}/{normalized}")));
                }
                continue;
            }
            if let Some(remote) = &primary
                && self
                    .remote_branch_exists(&GitRemoteBranchExistsInput {
                        cwd: cwd.to_owned(),
                        remote_name: remote.clone(),
                        ref_name: normalized.clone(),
                    })
                    .await?
            {
                return Ok(Some(format!("{remote}/{normalized}")));
            }
            if self.branch_exists(cwd, &normalized).await? {
                return Ok(Some(normalized));
            }
        }
        Ok(None)
    }

    async fn compute_ahead_count_against_base(
        &self,
        cwd: &str,
        ref_name: &str,
    ) -> Result<u64, GitCommandError> {
        let Some(base_ref) = self
            .resolve_base_branch_for_no_upstream(cwd, ref_name, false)
            .await?
        else {
            return Ok(0);
        };
        let mut call = GitCall::new(
            "GitVcsDriver.computeAheadCountAgainstBase",
            argv(&["rev-list", "--count", &format!("{base_ref}..HEAD")]),
        );
        call.allow_non_zero = true;
        let result = self.git(cwd, call).await?;
        if result.exit_code != Some(0) {
            return Ok(0);
        }
        Ok(parse_count(&result.stdout))
    }

    async fn read_status_details_remote(
        &self,
        cwd: &str,
    ) -> Result<GitRemoteStatusDetails, GitCommandError> {
        let branch_args = argv(&["rev-parse", "--abbrev-ref", "HEAD"]);
        let mut call = GitCall::new("GitVcsDriver.statusDetailsRemote.branch", branch_args.clone());
        call.allow_non_zero = true;
        call.stable = true;
        let branch_result = match self.git(cwd, call).await {
            Err(error) if is_missing_cwd(&error) => return Ok(non_repository_remote()),
            other => other?,
        };
        let branch = if branch_result.exit_code != Some(0) {
            if is_non_repository_git_stderr(&branch_result.stderr) {
                return Ok(non_repository_remote());
            }
            if !is_unborn_head_stderr(&branch_result.stderr) {
                return Err(failed(
                    "GitVcsDriver.statusDetailsRemote.branch",
                    cwd,
                    &branch_args,
                    &branch_result,
                    "Git branch lookup failed.",
                ));
            }
            nonempty(
                self.git_out(
                    "GitVcsDriver.statusDetailsRemote.unbornBranch",
                    cwd,
                    argv(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
                    false,
                )
                .await?
                .trim(),
            )
        } else {
            let value = branch_result.stdout.trim();
            if value.is_empty() || value == "HEAD" {
                None
            } else {
                Some(value.to_owned())
            }
        };
        let upstream = self.resolve_current_upstream(cwd).await?;
        let upstream_ref = upstream.as_ref().map(|parsed| parsed.upstream_ref.clone());
        let (mut ahead_count, mut behind_count) = (0_u64, 0_u64);
        if let Some(upstream_ref) = &upstream_ref {
            let mut call = GitCall::new(
                "GitVcsDriver.statusDetailsRemote.divergence",
                argv(&["rev-list", "--left-right", "--count", &format!("HEAD...{upstream_ref}")]),
            );
            call.allow_non_zero = true;
            let divergence = self.git(cwd, call).await?;
            if divergence.exit_code == Some(0) {
                let mut parts = divergence.stdout.split_whitespace();
                ahead_count = parse_count(parts.next().unwrap_or("0"));
                behind_count = parse_count(parts.next().unwrap_or("0"));
            }
        } else if let Some(branch) = &branch {
            ahead_count = self
                .compute_ahead_count_against_base(cwd, branch)
                .await
                .unwrap_or(0);
        }
        let default_branch = self.resolve_default_branch_name(cwd, "origin").await?;
        let is_default_branch = branch.as_ref().is_some_and(|branch| {
            default_branch.as_ref() == Some(branch)
                || (default_branch.is_none() && (branch == "main" || branch == "master"))
        });
        let ahead_of_default_count = if branch.is_some() && !is_default_branch {
            if upstream_ref.is_none() {
                ahead_count
            } else {
                self.compute_ahead_count_against_base(cwd, branch.as_deref().unwrap_or(""))
                    .await
                    .unwrap_or(0)
            }
        } else {
            0
        };
        Ok(GitRemoteStatusDetails {
            is_repo: true,
            default_branch,
            is_default_branch,
            branch,
            upstream_ref: upstream_ref.clone(),
            has_upstream: upstream_ref.is_some(),
            ahead_count,
            behind_count,
            ahead_of_default_count,
        })
    }

    async fn read_numstat(&self, cwd: &str) -> Result<String, GitCommandError> {
        let args = argv(&["diff-index", "-M", "--numstat", "HEAD", "--"]);
        let mut call = GitCall::new("GitVcsDriver.statusDetails.numstat", args.clone());
        call.allow_non_zero = true;
        call.stable = true;
        let result = self.git(cwd, call).await?;
        if result.exit_code == Some(0) {
            return Ok(result.stdout);
        }
        if is_unborn_head_stderr(&result.stderr) {
            let unstaged = self
                .git_out(
                    "GitVcsDriver.statusDetails.numstat.unborn",
                    cwd,
                    argv(&["diff-files", "--numstat"]),
                    false,
                )
                .await?;
            let staged = self
                .git_out(
                    "GitVcsDriver.statusDetails.numstat.unborn.staged",
                    cwd,
                    argv(&["diff", "--cached", "--numstat"]),
                    false,
                )
                .await?;
            let mut map: HashMap<String, (i64, i64)> = HashMap::new();
            for entry in parse_numstat_entries(&staged)
                .into_iter()
                .chain(parse_numstat_entries(&unstaged))
            {
                let slot = map.entry(entry.path).or_insert((0, 0));
                slot.0 += entry.insertions;
                slot.1 += entry.deletions;
            }
            return Ok(map
                .into_iter()
                .map(|(path, (insertions, deletions))| format!("{insertions}\t{deletions}\t{path}"))
                .collect::<Vec<_>>()
                .join("\n"));
        }
        Err(failed(
            "GitVcsDriver.statusDetails.numstat",
            cwd,
            &args,
            &result,
            "git diff-index HEAD --numstat failed.",
        ))
    }

    async fn read_status_details_local(
        &self,
        cwd: &str,
        options: GitLocalStatusOptions,
    ) -> Result<GitStatusDetails, GitCommandError> {
        let include_divergence = options.include_divergence != Some(false);
        let mut status_args = argv(&["status", "--porcelain=2", "--branch"]);
        if !include_divergence {
            status_args.push("--no-ahead-behind".to_owned());
        }
        let index_args = argv(&["rev-parse", "--git-path", "index"]);
        let mut index_call = GitCall::new("GitVcsDriver.statusDetails.indexPath", index_args);
        index_call.allow_non_zero = true;
        index_call.stable = true;
        let index_result = match self.git(cwd, index_call).await {
            Err(error) if is_missing_cwd(&error) => return Ok(non_repository_status()),
            other => other?,
        };
        if index_result.exit_code == Some(0) {
            let lock_path = format!(
                "{}.lock",
                resolve_path(cwd, index_result.stdout.trim()).display()
            );
            if Path::new(&lock_path).exists() {
                return Err(plain_error(
                    "GitVcsDriver.statusDetails.indexPath",
                    cwd,
                    &[],
                    "Git index is locked. Status will resume when the index lock is removed.",
                ));
            }
        }
        let mut status_call = GitCall::new("GitVcsDriver.statusDetails.status", status_args.clone());
        status_call.allow_non_zero = true;
        status_call.stable = true;
        let status_result = match self.git(cwd, status_call).await {
            Err(error) if is_missing_cwd(&error) => return Ok(non_repository_status()),
            other => other?,
        };
        if status_result.exit_code != Some(0) {
            if is_non_repository_git_stderr(&status_result.stderr) {
                return Ok(non_repository_status());
            }
            return Err(failed(
                "GitVcsDriver.statusDetails.status",
                cwd,
                &status_args,
                &status_result,
                "Git status failed.",
            ));
        }
        let repository_paths = self.resolve_repository_paths(cwd, false).await.ok().flatten();
        let cache_key = repository_paths.as_ref().map(|paths| paths.git_common_dir.clone());
        let (numstat, default_branch, has_primary_remote) = tokio::join!(
            self.read_numstat(cwd),
            async {
                if let Some(key) = &cache_key {
                    self.cached_default_branch(cwd, key).await.unwrap_or(None)
                } else {
                    self.resolve_default_branch_name(cwd, "origin")
                        .await
                        .unwrap_or(None)
                }
            },
            async {
                if let Some(key) = &cache_key {
                    self.cached_origin_exists(key).await.unwrap_or(false)
                } else {
                    self.remote_exists(&GitRemoteExistsInput {
                        cwd: cwd.to_owned(),
                        remote_name: "origin".to_owned(),
                    })
                    .await
                    .unwrap_or(false)
                }
            },
        );
        let numstat_stdout = numstat?;
        let mut ref_name = None;
        let mut upstream_ref = None;
        let mut ahead_count = 0_u64;
        let mut behind_count = 0_u64;
        let mut has_working_tree_changes = false;
        let mut changed_without_numstat = HashSet::new();
        for line in status_result.stdout.split(['\n', '\r']) {
            if let Some(value) = line.strip_prefix("# branch.head ") {
                let value = value.trim();
                ref_name = if value.starts_with('(') {
                    None
                } else {
                    nonempty(value)
                };
                continue;
            }
            if let Some(value) = line.strip_prefix("# branch.upstream ") {
                upstream_ref = nonempty(value.trim());
                continue;
            }
            if include_divergence && let Some(value) = line.strip_prefix("# branch.ab ") {
                let parsed = parse_branch_ab(value.trim());
                ahead_count = parsed.ahead;
                behind_count = parsed.behind;
                continue;
            }
            if !line.trim().is_empty() && !line.starts_with('#') {
                has_working_tree_changes = true;
                if let Some(path) = parse_porcelain_path(line) {
                    changed_without_numstat.insert(path);
                }
            }
        }
        let fallback_ahead = if include_divergence && upstream_ref.is_none() && ref_name.is_some() {
            Some(
                self.compute_ahead_count_against_base(cwd, ref_name.as_deref().unwrap_or(""))
                    .await
                    .unwrap_or(0),
            )
        } else {
            None
        };
        if let Some(fallback) = fallback_ahead {
            ahead_count = fallback;
            behind_count = 0;
        }
        let is_default_branch = ref_name.as_ref().is_some_and(|name| {
            default_branch.as_ref() == Some(name)
                || (default_branch.is_none() && (name == "main" || name == "master"))
        });
        let ahead_of_default_count = if include_divergence && ref_name.is_some() && !is_default_branch {
            if let Some(fallback) = fallback_ahead {
                fallback
            } else {
                self.compute_ahead_count_against_base(cwd, ref_name.as_deref().unwrap_or(""))
                    .await
                    .unwrap_or(0)
            }
        } else {
            0
        };
        let mut file_stats: HashMap<String, (i64, i64)> = HashMap::new();
        for entry in parse_numstat_entries(&numstat_stdout) {
            file_stats.insert(entry.path, (entry.insertions, entry.deletions));
        }
        let mut files: Vec<VcsStatusWorkingTreeFile> = file_stats
            .iter()
            .filter_map(|(path, (insertions, deletions))| {
                Some(VcsStatusWorkingTreeFile {
                    path: file_path(path)?,
                    insertions: (*insertions).max(0) as u64,
                    deletions: (*deletions).max(0) as u64,
                })
            })
            .collect();
        for path in &changed_without_numstat {
            if file_stats.contains_key(path) {
                continue;
            }
            if let Some(path) = file_path(path) {
                files.push(VcsStatusWorkingTreeFile {
                    path,
                    insertions: 0,
                    deletions: 0,
                });
            }
        }
        files.sort_by(|left, right| locale_compare(left.path.as_str(), right.path.as_str()));
        let insertions = files.iter().map(|file| file.insertions).sum();
        let deletions = files.iter().map(|file| file.deletions).sum();
        let branch_changes = if options.include_branch_changes == Some(true) {
            let root = repository_paths
                .as_ref()
                .and_then(|paths| paths.worktree_root.clone())
                .unwrap_or_else(|| cwd.to_owned());
            self.read_branch_change_totals(&root, ref_name.as_deref())
                .await
                .ok()
        } else {
            None
        };
        Ok(GitStatusDetails {
            is_repo: true,
            has_origin_remote: has_primary_remote,
            is_default_branch,
            branch: ref_name,
            upstream_ref: upstream_ref.clone(),
            has_working_tree_changes,
            working_tree: VcsStatusWorkingTree {
                files,
                insertions,
                deletions,
            },
            branch_changes,
            has_upstream: upstream_ref.is_some(),
            ahead_count,
            behind_count,
            ahead_of_default_count,
        })
    }

    async fn stage_commit_changes(
        &self,
        cwd: &str,
        file_paths: Option<&[String]>,
        env: Option<HashMap<String, String>>,
    ) -> Result<(), GitCommandError> {
        let config = if env.is_some() {
            split_index_config()
        } else {
            Vec::new()
        };
        if let Some(file_paths) = file_paths.filter(|paths| !paths.is_empty()) {
            if env.is_some() {
                let mut head = GitCall::new(
                    "GitVcsDriver.prepareCommitContext.head",
                    argv(&["rev-parse", "--verify", "--quiet", "HEAD"]),
                );
                head.allow_non_zero = true;
                let head = self.git(cwd, head).await?;
                let mut args = config.clone();
                args.push("read-tree".to_owned());
                if head.exit_code == Some(0) {
                    args.push("HEAD".to_owned());
                } else {
                    args.push("--empty".to_owned());
                }
                let mut call = GitCall::new("GitVcsDriver.prepareCommitContext.readTree", args);
                call.env = env.clone().unwrap_or_default();
                self.git(cwd, call).await?;
            } else {
                let _ = self
                    .git_void("GitVcsDriver.commit.reset", cwd, argv(&["reset"]), None)
                    .await;
            }
            let mut args = config;
            args.extend(argv(&["--literal-pathspecs", "add", "-A", "--"]));
            args.extend(file_paths.iter().cloned());
            let mut call = GitCall::new("GitVcsDriver.commit.addSelected", args);
            if let Some(env) = env {
                call.env = env;
            }
            self.git(cwd, call).await?;
        } else {
            let mut args = config;
            args.extend(argv(&["add", "-A"]));
            let mut call = GitCall::new("GitVcsDriver.commit.addAll", args);
            if let Some(env) = env {
                call.env = env;
            }
            self.git(cwd, call).await?;
        }
        Ok(())
    }

    async fn commit_inner(
        &self,
        cwd: &str,
        subject: &str,
        body: &str,
        options: GitCommitOptions,
    ) -> Result<GitCommitResult, GitCommandError> {
        if let Some(paths) = &options.stage {
            let file_paths = if paths.is_empty() { None } else { Some(paths.as_slice()) };
            self.stage_commit_changes(cwd, file_paths, None).await?;
        }
        let mut args = argv(&["commit", "-m", subject]);
        let trimmed_body = body.trim();
        if !trimmed_body.is_empty() {
            args.push("-m".to_owned());
            args.push(trimmed_body.to_owned());
        }
        let mut call = GitCall::new("GitVcsDriver.commit.commit", args);
        if let Some(timeout) = options.timeout {
            call.timeout = Some(Some(timeout));
        }
        if let Some(callback) = options.on_output_line {
            call.on_stdout_line = Some(callback.clone());
            call.on_stderr_line = Some(callback);
        }
        self.git(cwd, call).await?;
        let commit_sha = self
            .git_out(
                "GitVcsDriver.commit.revParseHead",
                cwd,
                argv(&["rev-parse", "HEAD"]),
                false,
            )
            .await?
            .trim()
            .to_owned();
        Ok(GitCommitResult { commit_sha })
    }

    async fn push_current_branch_inner(
        &self,
        cwd: &str,
        fallback_branch: Option<&str>,
        options: GitPushOptions,
    ) -> Result<GitPushResult, GitCommandError> {
        let details = self.status_details(cwd).await?;
        let branch = details
            .branch
            .clone()
            .or_else(|| fallback_branch.map(str::to_owned));
        let Some(branch) = branch else {
            return Err(plain_error(
                "GitVcsDriver.pushCurrentBranch",
                cwd,
                &argv(&["push"]),
                "Cannot push from detached HEAD.",
            ));
        };
        let requested = options
            .remote_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if let Some(requested) = requested {
            let publish_branch = self.resolve_publish_branch_name(cwd, &branch).await?;
            self.git_void(
                "GitVcsDriver.pushCurrentBranch.pushWithRequestedRemote",
                cwd,
                argv(&[
                    "push",
                    "-u",
                    requested,
                    &format!("HEAD:refs/heads/{publish_branch}"),
                ]),
                Some(None),
            )
            .await?;
            return Ok(GitPushResult {
                status: GitPushStatus::Pushed,
                branch,
                upstream_branch: Some(format!("{requested}/{publish_branch}")),
                set_upstream: Some(true),
            });
        }
        let has_no_local_delta = details.ahead_count == 0 && details.behind_count == 0;
        if has_no_local_delta {
            if details.has_upstream {
                return Ok(GitPushResult {
                    status: GitPushStatus::SkippedUpToDate,
                    branch,
                    upstream_branch: details.upstream_ref,
                    set_upstream: None,
                });
            }
            if self
                .resolve_base_branch_for_no_upstream(cwd, &branch, false)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                let publish_remote = self.resolve_push_remote_name(cwd, &branch).await.ok().flatten();
                if publish_remote.is_none() {
                    return Ok(GitPushResult {
                        status: GitPushStatus::SkippedUpToDate,
                        branch,
                        upstream_branch: None,
                        set_upstream: None,
                    });
                }
                if let Some(remote) = publish_remote
                    && self
                        .remote_branch_exists(&GitRemoteBranchExistsInput {
                            cwd: cwd.to_owned(),
                            remote_name: remote,
                            ref_name: branch.clone(),
                        })
                        .await
                        .unwrap_or(false)
                {
                    return Ok(GitPushResult {
                        status: GitPushStatus::SkippedUpToDate,
                        branch,
                        upstream_branch: None,
                        set_upstream: None,
                    });
                }
            }
        }
        if !details.has_upstream {
            let Some(publish_remote) = self.resolve_push_remote_name(cwd, &branch).await? else {
                return Err(plain_error(
                    "GitVcsDriver.pushCurrentBranch",
                    cwd,
                    &argv(&["push"]),
                    "Cannot push because no git remote is configured for this repository.",
                ));
            };
            let publish_branch = self.resolve_publish_branch_name(cwd, &branch).await?;
            self.git_void(
                "GitVcsDriver.pushCurrentBranch.pushWithUpstream",
                cwd,
                argv(&[
                    "push",
                    "-u",
                    &publish_remote,
                    &format!("HEAD:refs/heads/{publish_branch}"),
                ]),
                Some(None),
            )
            .await?;
            return Ok(GitPushResult {
                status: GitPushStatus::Pushed,
                branch,
                upstream_branch: Some(format!("{publish_remote}/{publish_branch}")),
                set_upstream: Some(true),
            });
        }
        if let Some(current) = self.resolve_current_upstream(cwd).await.ok().flatten() {
            let is_alias = branch == current.branch_name
                || (branch.ends_with(&format!("/{}", current.branch_name))
                    && current.upstream_ref.ends_with(&format!("/{branch}")));
            if !is_alias {
                let remote_name = self
                    .resolve_push_remote_name(cwd, &branch)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(current.remote_name.clone());
                let publish_branch = self.resolve_publish_branch_name(cwd, &branch).await?;
                let configured = self
                    .git_out(
                        "GitVcsDriver.pushCurrentBranch.readMergeBase",
                        cwd,
                        argv(&["config", "--get", &format!("branch.{branch}.gh-merge-base")]),
                        true,
                    )
                    .await?;
                if configured.trim().is_empty() {
                    self.git_void(
                        "GitVcsDriver.pushCurrentBranch.recordMergeBase",
                        cwd,
                        argv(&[
                            "config",
                            &format!("branch.{branch}.gh-merge-base"),
                            &current.branch_name,
                        ]),
                        None,
                    )
                    .await?;
                }
                self.git_void(
                    "GitVcsDriver.pushCurrentBranch.pushOwnBranch",
                    cwd,
                    argv(&[
                        "push",
                        "-u",
                        &remote_name,
                        &format!("HEAD:refs/heads/{publish_branch}"),
                    ]),
                    Some(None),
                )
                .await?;
                return Ok(GitPushResult {
                    status: GitPushStatus::Pushed,
                    branch,
                    upstream_branch: Some(format!("{remote_name}/{publish_branch}")),
                    set_upstream: Some(true),
                });
            }
            self.git_void(
                "GitVcsDriver.pushCurrentBranch.pushUpstream",
                cwd,
                argv(&[
                    "push",
                    &current.remote_name,
                    &format!("HEAD:refs/heads/{}", current.branch_name),
                ]),
                Some(None),
            )
            .await?;
            return Ok(GitPushResult {
                status: GitPushStatus::Pushed,
                branch,
                upstream_branch: Some(current.upstream_ref),
                set_upstream: Some(false),
            });
        }
        self.git_void(
            "GitVcsDriver.pushCurrentBranch.push",
            cwd,
            argv(&["push"]),
            Some(None),
        )
        .await?;
        Ok(GitPushResult {
            status: GitPushStatus::Pushed,
            branch,
            upstream_branch: details.upstream_ref,
            set_upstream: Some(false),
        })
    }

    async fn pull_current_branch_inner(&self, cwd: &str) -> Result<VcsPullResult, GitCommandError> {
        let details = self.status_details(cwd).await?;
        let Some(ref_name) = details.branch.clone() else {
            return Err(plain_error(
                "GitVcsDriver.pullCurrentBranch",
                cwd,
                &argv(&["pull", "--ff-only"]),
                "Cannot pull from detached HEAD.",
            ));
        };
        if !details.has_upstream {
            return Err(plain_error(
                "GitVcsDriver.pullCurrentBranch",
                cwd,
                &argv(&["pull", "--ff-only"]),
                "Current branch has no upstream configured. Push with upstream first.",
            ));
        }
        let before = self
            .git_out(
                "GitVcsDriver.pullCurrentBranch.beforeSha",
                cwd,
                argv(&["rev-parse", "HEAD"]),
                true,
            )
            .await?;
        let mut call = GitCall::new(
            "GitVcsDriver.pullCurrentBranch.pull",
            argv(&["pull", "--ff-only"]),
        );
        call.timeout = Some(Some(Duration::from_secs(30)));
        call.fallback = Some("git pull failed");
        call.marker = true;
        self.git(cwd, call).await?;
        let after = self
            .git_out(
                "GitVcsDriver.pullCurrentBranch.afterSha",
                cwd,
                argv(&["rev-parse", "HEAD"]),
                true,
            )
            .await?;
        let refreshed = self.status_details(cwd).await?;
        let before = before.trim();
        let after = after.trim();
        Ok(VcsPullResult {
            status: if !before.is_empty() && before == after {
                VcsPullStatus::SkippedUpToDate
            } else {
                VcsPullStatus::Pulled
            },
            ref_name: trimmed_name(&ref_name).unwrap_or_else(|| {
                TrimmedNonEmptyString::new("HEAD").expect("HEAD")
            }),
            upstream_ref: refreshed.upstream_ref.as_deref().and_then(trimmed_name),
        })
    }
}
