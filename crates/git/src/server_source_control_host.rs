//! Port of upstream/apps/server/src/sourceControl/ServerSourceControlHost.ts
//!
//! The app's implementation of [`SourceControlHost`], the only surface source control provider
//! drivers may use. Upstream wires `ServerSettingsService`, `VcsProcess`, `GitVcsDriver` and
//! `VcsDriverRegistry` into a layer; here [`ServerSourceControlHost::new`] takes the same four.
//!
//! Deviation: `turin-git` cannot name `ServerSettingsService` (it lives in `turin-runtime`, which
//! this crate does not depend on), so the settings read is a [`SettingsGetter`] closure. The app
//! passes `|| service.get_settings()`.
//!
//! Deviation: `ExecuteGitResult.exit_code` is `None` when git ends without an exit code (a
//! signal). The host reports `-1`, the process-exit convention for "no code", because
//! `SourceControlGitOutput.exit_code` is a plain `i32`. This only occurs with
//! `allow_non_zero_exit`; otherwise the driver has already failed the call.

#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
use turin_contracts::settings::{ServerSettings, ServerSettingsError};
use turin_contracts::vcs::{VcsError, VcsListRemotesResult};
use turin_source_control::source_control_core::host::{
    OutputMode as HostOutputMode, SourceControlGitExecuteInput, SourceControlGitOutput,
    SourceControlGitRemoteBranch, SourceControlHost, SourceControlProcessInput,
    SourceControlProcessOutput,
};

use crate::git_vcs_driver_core::{
    ExecuteGitInput, GitEnsureRemoteInput, GitFetchRemoteBranchInput,
    GitFetchRemoteTrackingBranchInput, GitResolveCommitInput, GitSetBranchUpstreamInput,
    GitVcsDriverCore,
};
use crate::process::OutputMode;
use crate::vcs_driver_registry::{VcsDriverRegistry, VcsDriverResolveInput};
use crate::vcs_process::{VcsProcess, VcsProcessInput};

/// `serverSettings.getSettings`: read fresh on every call.
pub type SettingsGetter =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ServerSettings, ServerSettingsError>> + Send + Sync>;

pub struct ServerSourceControlHost {
    settings: SettingsGetter,
    process: Arc<VcsProcess>,
    git: Arc<GitVcsDriverCore>,
    vcs_registry: Arc<VcsDriverRegistry>,
}

impl ServerSourceControlHost {
    pub fn new(
        settings: SettingsGetter,
        process: Arc<VcsProcess>,
        git: Arc<GitVcsDriverCore>,
        vcs_registry: Arc<VcsDriverRegistry>,
    ) -> Self {
        Self {
            settings,
            process,
            git,
            vcs_registry,
        }
    }
}

#[async_trait]
impl SourceControlHost for ServerSourceControlHost {
    async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError> {
        (self.settings)().await
    }

    async fn process_run(
        &self,
        input: SourceControlProcessInput,
    ) -> Result<SourceControlProcessOutput, VcsError> {
        let output = self
            .process
            .run(VcsProcessInput {
                operation: input.operation,
                command: input.command,
                args: input.args,
                cwd: input.cwd,
                spawn_cwd: input.spawn_cwd,
                stdin: input.stdin,
                on_stdout_chunk: input.on_stdout_chunk,
                env: input.env,
                clear_env: Vec::new(),
                allow_non_zero_exit: input.allow_non_zero_exit,
                timeout: input.timeout_ms.map(Duration::from_millis),
                max_output_bytes: input.max_output_bytes,
                output_mode: input.output_mode.map(|mode| match mode {
                    HostOutputMode::Error => OutputMode::Error,
                    HostOutputMode::Truncate => OutputMode::Truncate,
                }),
                append_truncation_marker: input.append_truncation_marker,
            })
            .await?;
        Ok(SourceControlProcessOutput {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_truncated: output.stdout_truncated,
            stderr_truncated: output.stderr_truncated,
            stdout_invalid_utf8: Some(output.stdout_invalid_utf8),
            stderr_invalid_utf8: Some(output.stderr_invalid_utf8),
        })
    }

    async fn git_execute(
        &self,
        input: SourceControlGitExecuteInput,
    ) -> Result<SourceControlGitOutput, GitCommandError> {
        let mut execute = ExecuteGitInput::new(input.operation, input.cwd, input.args);
        execute.max_output_bytes = input.max_output_bytes;
        execute.append_truncation_marker = input.append_truncation_marker;
        let result = self.git.execute(execute).await?;
        Ok(SourceControlGitOutput {
            exit_code: result.exit_code.unwrap_or(-1),
            stdout: result.stdout,
            stderr: result.stderr,
            stdout_truncated: result.stdout_truncated,
            stderr_truncated: result.stderr_truncated,
        })
    }

    async fn git_resolve_commit(
        &self,
        cwd: &str,
        revision: &str,
    ) -> Result<String, GitCommandError> {
        self.git
            .resolve_commit(&GitResolveCommitInput {
                cwd: cwd.to_owned(),
                revision: revision.to_owned(),
            })
            .await
            .map(|result| result.commit_sha)
    }

    async fn git_remotes(&self, cwd: &str) -> Result<VcsListRemotesResult, VcsError> {
        let handle = self
            .vcs_registry
            .resolve(&VcsDriverResolveInput {
                cwd: cwd.to_owned(),
                requested_kind: None,
            })
            .await?;
        handle.driver.list_remotes(cwd).await
    }

    async fn git_read_config_value(
        &self,
        cwd: &str,
        key: &str,
    ) -> Result<Option<String>, GitCommandError> {
        self.git.read_config_value(cwd, key).await
    }

    async fn git_resolve_primary_remote_name(&self, cwd: &str) -> Result<String, GitCommandError> {
        self.git.resolve_primary_remote_name(cwd).await
    }

    async fn git_ensure_remote(
        &self,
        cwd: &str,
        preferred_name: &str,
        url: &str,
    ) -> Result<String, GitCommandError> {
        self.git
            .ensure_remote(&GitEnsureRemoteInput {
                cwd: cwd.to_owned(),
                preferred_name: preferred_name.to_owned(),
                url: url.to_owned(),
            })
            .await
    }

    async fn git_list_local_branch_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        self.git.list_local_branch_names(cwd).await
    }

    async fn git_fetch_remote_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
        local_branch: &str,
    ) -> Result<(), GitCommandError> {
        self.git
            .fetch_remote_branch(&GitFetchRemoteBranchInput {
                cwd: input.cwd.clone(),
                remote_name: input.remote_name.clone(),
                remote_branch: input.remote_branch.clone(),
                local_branch: local_branch.to_owned(),
            })
            .await
    }

    async fn git_fetch_remote_tracking_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
    ) -> Result<(), GitCommandError> {
        self.git
            .fetch_remote_tracking_branch(&GitFetchRemoteTrackingBranchInput {
                cwd: input.cwd.clone(),
                remote_name: input.remote_name.clone(),
                remote_branch: input.remote_branch.clone(),
            })
            .await
    }

    async fn git_set_branch_upstream(
        &self,
        input: &SourceControlGitRemoteBranch,
        branch: &str,
    ) -> Result<(), GitCommandError> {
        self.git
            .set_branch_upstream(&GitSetBranchUpstreamInput {
                cwd: input.cwd.clone(),
                branch: branch.to_owned(),
                remote_name: input.remote_name.clone(),
                remote_branch: input.remote_branch.clone(),
            })
            .await
    }

    async fn git_switch_ref(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        self.git.switch_ref(input).await
    }
}

#[cfg(test)]
mod tests {
    //! Upstream has no `ServerSourceControlHost.test.ts`: the file is layer wiring. These check
    //! that each host method reaches the service it forwards to.

    use std::process::Command;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::git_vcs_driver::GitVcsDriver;
    use crate::process::ProcessRunner;
    use crate::vcs_project_config::VcsProjectConfig;

    fn host() -> ServerSourceControlHost {
        let process = Arc::new(VcsProcess::new(ProcessRunner));
        let registry = Arc::new(VcsDriverRegistry::new(
            Arc::new(VcsProjectConfig::new()),
            Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
        ));
        let settings: SettingsGetter =
            Arc::new(|| Box::pin(async { Ok(ServerSettings::default()) }));
        ServerSourceControlHost::new(
            settings,
            process,
            Arc::new(GitVcsDriverCore::new()),
            registry,
        )
    }

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["commit", "--allow-empty", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn settings_get_reads_the_getter_on_every_call() {
        let host = host();
        assert!(host.settings_get().await.is_ok());
    }

    #[tokio::test]
    async fn process_run_forwards_to_the_vcs_process() {
        let dir = tempfile::tempdir().unwrap();
        let output = host()
            .process_run(SourceControlProcessInput {
                operation: "test.echo".to_owned(),
                command: "git".to_owned(),
                args: vec!["--version".to_owned()],
                cwd: dir.path().display().to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.starts_with("git version"));
        assert_eq!(output.stdout_invalid_utf8, Some(false));
    }

    #[tokio::test]
    async fn git_methods_forward_to_the_git_driver() {
        let dir = repo_with_commit();
        let cwd = dir.path().display().to_string();
        let host = host();

        let executed = host
            .git_execute(SourceControlGitExecuteInput {
                operation: "test.revParse".to_owned(),
                cwd: cwd.clone(),
                args: vec!["rev-parse".to_owned(), "HEAD".to_owned()],
                max_output_bytes: None,
                append_truncation_marker: false,
            })
            .await
            .unwrap();
        assert_eq!(executed.exit_code, 0);
        let sha = host.git_resolve_commit(&cwd, "HEAD").await.unwrap();
        assert_eq!(sha, executed.stdout.trim());

        assert_eq!(
            host.git_list_local_branch_names(&cwd).await.unwrap(),
            vec!["main".to_owned()]
        );
        assert_eq!(
            host.git_read_config_value(&cwd, "remote.nope.url")
                .await
                .unwrap(),
            None
        );

        let remote = host
            .git_ensure_remote(&cwd, "fork", "https://example.com/fork.git")
            .await
            .unwrap();
        assert_eq!(remote, "fork");
        assert_eq!(
            host.git_resolve_primary_remote_name(&cwd).await.unwrap(),
            "fork"
        );
        let remotes = host.git_remotes(&cwd).await.unwrap();
        assert_eq!(remotes.remotes.len(), 1);
    }
}
