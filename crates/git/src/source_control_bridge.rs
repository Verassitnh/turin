//! The git crate's side of the source-control ports.
//!
//! `turin-source-control` defines [`RepositoryGit`], the four git operations its repository service
//! calls, and this module implements it over [`GitVcsDriverCore`]. [`ChangeRequestHost`] is defined
//! here and implemented over the source-control provider that a `cwd` resolves to. Source-control
//! never names this crate, so the dependency points one way.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tempfile::NamedTempFile;
use turin_contracts::git::{GitCommandError, GitPullRequestState};
use turin_contracts::source_control::{
    ChangeRequest, ChangeRequestState, SourceControlProviderError, SourceControlProviderKind,
};
use turin_source_control::source_control::provider_registry::SourceControlProviderRegistry;
use turin_source_control::source_control::repository_service::{
    GitCommand, PushedBranch, RepositoryGit, redact_url_credentials,
};
use turin_source_control::source_control_core::Cause;
use turin_source_control::source_control_core::source_control_provider::{
    CreateChangeRequestInput, GetChangeRequestInput, ListChangeRequestsInput, SourceControlProvider,
};

use crate::git_manager::{ChangeRequestHost, CreateChangeRequest, HostChangeRequest};
use crate::git_vcs_driver_core::{
    ExecuteGitInput, GitEnsureRemoteInput, GitPushOptions, GitVcsDriverCore,
};

/// Runs git through the driver for [`RepositoryGit`]. A failure reaches the service as a [`Cause`]
/// whose text has no URL credentials, because a clone URL can carry a token.
pub struct GitRepositoryGit {
    driver: Arc<GitVcsDriverCore>,
}

impl GitRepositoryGit {
    pub fn new(driver: Arc<GitVcsDriverCore>) -> Self {
        Self { driver }
    }
}

#[async_trait]
impl RepositoryGit for GitRepositoryGit {
    async fn execute(&self, command: GitCommand) -> Result<(), Cause> {
        let mut input = ExecuteGitInput::new(command.operation, command.cwd, command.args);
        // Both sides read a `None` deadline as "no deadline": the driver's `Some(None)` waits until
        // git exits, and the default 30s applies only when the field is absent.
        input.timeout = Some(command.timeout);
        if !command.env.is_empty() {
            input.env = Some(
                command
                    .env
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
            );
        }
        input.on_stderr_line = command.on_stderr_line;
        self.driver
            .execute(input)
            .await
            .map(|_| ())
            .map_err(git_failure)
    }

    async fn ensure_remote(
        &self,
        cwd: &str,
        preferred_name: &str,
        url: &str,
    ) -> Result<String, Cause> {
        self.driver
            .ensure_remote(&GitEnsureRemoteInput {
                cwd: cwd.to_owned(),
                preferred_name: preferred_name.to_owned(),
                url: url.to_owned(),
            })
            .await
            .map_err(git_failure)
    }

    async fn status_branch(&self, cwd: &str) -> Result<Option<String>, Cause> {
        self.driver
            .status_details(cwd)
            .await
            .map(|details| details.branch)
            .map_err(git_failure)
    }

    async fn push_current_branch(
        &self,
        cwd: &str,
        remote_name: &str,
    ) -> Result<PushedBranch, Cause> {
        let pushed = self
            .driver
            .push_current_branch(
                cwd,
                None,
                GitPushOptions {
                    remote_name: Some(remote_name.to_owned()),
                },
            )
            .await
            .map_err(git_failure)?;
        Ok(PushedBranch {
            branch: pushed.branch,
            upstream_branch: pushed.upstream_branch,
        })
    }
}

/// The git failure as a [`Cause`]: its message with URL credentials removed.
fn git_failure(error: GitCommandError) -> Cause {
    Arc::new(GitFailure(redact_url_credentials(&error.to_string())))
}

#[derive(Debug)]
struct GitFailure(String);

impl fmt::Display for GitFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GitFailure {}

/// Resolves the provider a `cwd` is bound to. The registry does it for real; tests stand a fixed
/// provider in.
#[async_trait]
pub trait ProviderForCwd: Send + Sync {
    async fn resolve(
        &self,
        cwd: &str,
    ) -> Result<Arc<dyn SourceControlProvider>, SourceControlProviderError>;
}

#[async_trait]
impl ProviderForCwd for Arc<SourceControlProviderRegistry> {
    async fn resolve(
        &self,
        cwd: &str,
    ) -> Result<Arc<dyn SourceControlProvider>, SourceControlProviderError> {
        SourceControlProviderRegistry::resolve(self, cwd).await
    }
}

/// [`ChangeRequestHost`] over the source-control provider for each repository.
pub struct SourceControlChangeRequestHost<P> {
    providers: P,
}

impl<P: ProviderForCwd> SourceControlChangeRequestHost<P> {
    pub fn new(providers: P) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl<P: ProviderForCwd> ChangeRequestHost for SourceControlChangeRequestHost<P> {
    async fn list_for_branch(
        &self,
        cwd: &str,
        head_selectors: &[String],
    ) -> Result<Vec<HostChangeRequest>, SourceControlProviderError> {
        let provider = self.providers.resolve(cwd).await?;
        let mut found = Vec::new();
        for head_selector in head_selectors {
            let listed = provider
                .list_change_requests(ListChangeRequestsInput {
                    cwd: cwd.to_owned(),
                    context: None,
                    source: None,
                    head_selector: head_selector.clone(),
                    state: None,
                    limit: None,
                })
                .await?;
            found.extend(listed.iter().map(host_change_request));
        }
        Ok(found)
    }

    async fn get(
        &self,
        cwd: &str,
        reference: &str,
    ) -> Result<HostChangeRequest, SourceControlProviderError> {
        let provider = self.providers.resolve(cwd).await?;
        let found = provider
            .get_change_request(GetChangeRequestInput {
                cwd: cwd.to_owned(),
                context: None,
                reference: reference.to_owned(),
            })
            .await?;
        Ok(host_change_request(&found))
    }

    async fn create(
        &self,
        cwd: &str,
        input: &CreateChangeRequest,
    ) -> Result<HostChangeRequest, SourceControlProviderError> {
        const OPERATION: &str = "ChangeRequestHost.create";
        let provider = self.providers.resolve(cwd).await?;
        // The provider reads the description from a file, as `gh pr create --body-file` does. The
        // file lives until the host has read it.
        let body_file =
            NamedTempFile::new().map_err(|error| host_error(OPERATION, cwd, &error.to_string()))?;
        tokio::fs::write(body_file.path(), &input.body)
            .await
            .map_err(|error| host_error(OPERATION, cwd, &error.to_string()))?;
        provider
            .create_change_request(CreateChangeRequestInput {
                cwd: cwd.to_owned(),
                context: None,
                source: None,
                target: None,
                base_ref_name: input.base_branch.clone(),
                head_selector: input.head_branch.clone(),
                title: input.title.clone(),
                body_file: body_file.path().to_string_lossy().into_owned(),
            })
            .await?;
        drop(body_file);

        // The host's create reports success, not the change request, so read it back by head.
        let listed = provider
            .list_change_requests(ListChangeRequestsInput {
                cwd: cwd.to_owned(),
                context: None,
                source: None,
                head_selector: input.head_branch.clone(),
                state: Some(ChangeRequestState::Open),
                limit: None,
            })
            .await?;
        listed
            .iter()
            .find(|change_request| {
                change_request.head_ref_name.as_str() == input.head_branch
                    && change_request.base_ref_name.as_str() == input.base_branch
            })
            .map(host_change_request)
            .ok_or_else(|| {
                host_error(
                    OPERATION,
                    cwd,
                    "The created pull request could not be found.",
                )
            })
    }
}

/// The host's shape of a source-control change request. A merged or closed change request keeps
/// its dates; a missing date is absent, never an empty string.
fn host_change_request(change_request: &ChangeRequest) -> HostChangeRequest {
    HostChangeRequest {
        number: change_request.number.get(),
        title: change_request.title.as_str().to_owned(),
        url: change_request.url.clone(),
        base_ref_name: change_request.base_ref_name.as_str().to_owned(),
        head_ref_name: change_request.head_ref_name.as_str().to_owned(),
        state: match change_request.state {
            ChangeRequestState::Open => GitPullRequestState::Open,
            ChangeRequestState::Closed => GitPullRequestState::Closed,
            ChangeRequestState::Merged => GitPullRequestState::Merged,
        },
        is_draft: change_request.is_draft,
        updated_at: change_request
            .updated_at
            .clone()
            .into_option()
            .map(|at| at.to_string()),
        closed_at: change_request.closed_at.clone().flatten(),
        merged_at: change_request.merged_at.clone().flatten(),
        head_sha: change_request
            .head_sha
            .as_ref()
            .map(|sha| sha.as_str().to_owned()),
        is_cross_repository: change_request.is_cross_repository,
        head_repository_name_with_owner: change_request
            .head_repository_name_with_owner
            .clone()
            .flatten()
            .map(|name| name.as_str().to_owned()),
        head_repository_owner_login: change_request
            .head_repository_owner_login
            .clone()
            .flatten()
            .map(|login| login.as_str().to_owned()),
    }
}

/// A host failure the git crate raises itself, shaped like the ones the provider returns.
fn host_error(operation: &str, cwd: &str, detail: &str) -> SourceControlProviderError {
    SourceControlProviderError::SourceControlProviderError {
        provider: SourceControlProviderKind::new("unknown").expect("valid kind"),
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        command: None,
        repository: None,
        reference: None,
        detail: detail.to_owned(),
        cause: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_source_control::source_control_core::source_control_provider::{
        CheckoutChangeRequestInput, CreateRepositoryInput, GetDefaultBranchInput,
        GetRepositoryCloneUrlsInput,
    };

    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.test"])
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository on `main` with one commit, so a branch and a push have something to name.
    fn repository_with_commit(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(dir, &["add", "README.md"]);
        git(dir, &["commit", "-q", "-m", "initial"]);
    }

    fn command(cwd: &Path, args: &[&str]) -> GitCommand {
        GitCommand {
            operation: "test",
            cwd: cwd.to_string_lossy().into_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout: Some(Duration::from_secs(30)),
            env: Vec::new(),
            on_stderr_line: None,
        }
    }

    #[tokio::test]
    async fn runs_git_through_the_driver_and_reports_failures_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        repository_with_commit(dir.path());
        let repo = GitRepositoryGit::new(Arc::new(GitVcsDriverCore::new()));

        repo.execute(command(dir.path(), &["status", "--short"]))
            .await
            .unwrap();
        let failed = repo
            .execute(command(
                dir.path(),
                &["rev-parse", "--verify", "no-such-ref"],
            ))
            .await
            .unwrap_err();
        assert!(!failed.to_string().contains("secret-token"));

        assert_eq!(
            repo.status_branch(&dir.path().to_string_lossy())
                .await
                .unwrap(),
            Some("main".to_owned())
        );
    }

    #[tokio::test]
    async fn adds_a_remote_and_pushes_the_current_branch_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        repository_with_commit(dir.path());
        git(remote.path(), &["init", "-q", "--bare"]);
        let cwd = dir.path().to_string_lossy().into_owned();
        let repo = GitRepositoryGit::new(Arc::new(GitVcsDriverCore::new()));

        let name = repo
            .ensure_remote(&cwd, "origin", &remote.path().to_string_lossy())
            .await
            .unwrap();
        let pushed = repo.push_current_branch(&cwd, &name).await.unwrap();

        assert_eq!(pushed.branch, "main");
        assert_eq!(pushed.upstream_branch, Some(format!("{name}/main")));
    }

    #[test]
    fn a_git_failure_keeps_its_message_but_loses_the_url_credentials() {
        // Git's own stderr can name the remote it could not reach; that URL can carry a token.
        let failure = git_failure(GitCommandError::GitCommandError {
            operation: "pushCurrentBranch".to_owned(),
            command: "git push".to_owned(),
            cwd: "/repo".to_owned(),
            argument_count: None,
            exit_code: Some(128.0),
            stdout_length: None,
            stderr_length: None,
            output_length: None,
            reason: None,
            detail:
                "fatal: unable to access 'https://user:secret-token@example.test/acme/web.git/'"
                    .to_owned(),
            cause: None,
        });

        assert_eq!(
            failure.to_string(),
            "Git command failed in pushCurrentBranch (/repo): fatal: unable to access 'https://example.test/acme/web.git/'"
        );
    }

    /// Stands in for the host: records what it is asked, and keeps the change requests it holds.
    #[derive(Default)]
    struct RecordingProvider {
        held: Mutex<Vec<ChangeRequest>>,
        listed_heads: Mutex<Vec<String>>,
        created_bodies: Mutex<Vec<String>>,
        /// Whether a create shows up in later listings, as a real host's does.
        echo_created: bool,
    }

    fn change_request(number: u64, head: &str, state: &str) -> ChangeRequest {
        serde_json::from_value(json!({
            "provider": "github",
            "number": number,
            "title": format!("Change {number}"),
            "url": format!("https://github.com/acme/web/pull/{number}"),
            "baseRefName": "main",
            "headRefName": head,
            "state": state,
            "isDraft": false,
            "updatedAt": { "_tag": "Some", "value": "2026-07-02T00:00:00Z" },
            "mergedAt": if state == "merged" { json!("2026-07-03T00:00:00Z") } else { json!(null) },
        }))
        .unwrap()
    }

    #[async_trait]
    impl SourceControlProvider for RecordingProvider {
        fn kind(&self) -> SourceControlProviderKind {
            SourceControlProviderKind::new("github").unwrap()
        }

        async fn list_change_requests(
            &self,
            input: ListChangeRequestsInput,
        ) -> Result<Vec<ChangeRequest>, SourceControlProviderError> {
            self.listed_heads.lock().push(input.head_selector.clone());
            Ok(self
                .held
                .lock()
                .iter()
                .filter(|change_request| {
                    change_request.head_ref_name.as_str() == input.head_selector
                })
                .cloned()
                .collect())
        }

        async fn get_change_request(
            &self,
            input: GetChangeRequestInput,
        ) -> Result<ChangeRequest, SourceControlProviderError> {
            let number: u64 = input.reference.parse().unwrap();
            self.held
                .lock()
                .iter()
                .find(|change_request| change_request.number.get() == number)
                .cloned()
                .ok_or_else(|| host_error("getChangeRequest", &input.cwd, "not found"))
        }

        async fn create_change_request(
            &self,
            input: CreateChangeRequestInput,
        ) -> Result<(), SourceControlProviderError> {
            let body = std::fs::read_to_string(&input.body_file).unwrap();
            self.created_bodies.lock().push(body);
            if self.echo_created {
                let number = self.held.lock().len() as u64 + 9;
                self.held
                    .lock()
                    .push(change_request(number, &input.head_selector, "open"));
            }
            Ok(())
        }

        async fn get_repository_clone_urls(
            &self,
            input: GetRepositoryCloneUrlsInput,
        ) -> Result<
            turin_contracts::source_control::SourceControlRepositoryCloneUrls,
            SourceControlProviderError,
        > {
            Err(host_error(
                "getRepositoryCloneUrls",
                &format!("{input:?}"),
                "unused",
            ))
        }

        async fn create_repository(
            &self,
            input: CreateRepositoryInput,
        ) -> Result<
            turin_contracts::source_control::SourceControlRepositoryCloneUrls,
            SourceControlProviderError,
        > {
            Err(host_error(
                "createRepository",
                &format!("{input:?}"),
                "unused",
            ))
        }

        async fn get_default_branch(
            &self,
            input: GetDefaultBranchInput,
        ) -> Result<Option<String>, SourceControlProviderError> {
            Err(host_error(
                "getDefaultBranch",
                &format!("{input:?}"),
                "unused",
            ))
        }

        async fn checkout_change_request(
            &self,
            input: CheckoutChangeRequestInput,
        ) -> Result<(), SourceControlProviderError> {
            Err(host_error(
                "checkoutChangeRequest",
                &format!("{input:?}"),
                "unused",
            ))
        }
    }

    struct Fixed(Arc<RecordingProvider>);

    #[async_trait]
    impl ProviderForCwd for Fixed {
        async fn resolve(
            &self,
            _cwd: &str,
        ) -> Result<Arc<dyn SourceControlProvider>, SourceControlProviderError> {
            Ok(self.0.clone())
        }
    }

    fn host_over(
        provider: RecordingProvider,
    ) -> (
        SourceControlChangeRequestHost<Fixed>,
        Arc<RecordingProvider>,
    ) {
        let provider = Arc::new(provider);
        (
            SourceControlChangeRequestHost::new(Fixed(provider.clone())),
            provider,
        )
    }

    #[tokio::test]
    async fn lists_a_branch_through_the_provider_and_maps_each_change_request() {
        let (host, provider) = host_over(RecordingProvider {
            held: Mutex::new(vec![
                change_request(7, "feature/x", "merged"),
                change_request(8, "feature/y", "open"),
            ]),
            ..RecordingProvider::default()
        });

        let listed = host
            .list_for_branch("/repo", &["feature/x".to_owned(), "feature/z".to_owned()])
            .await
            .unwrap();

        assert_eq!(
            *provider.listed_heads.lock(),
            vec!["feature/x", "feature/z"]
        );
        assert_eq!(listed.len(), 1);
        let found = &listed[0];
        assert_eq!(found.number, 7);
        assert_eq!(found.head_ref_name, "feature/x");
        assert_eq!(found.base_ref_name, "main");
        assert_eq!(found.state, GitPullRequestState::Merged);
        assert_eq!(found.merged_at.as_deref(), Some("2026-07-03T00:00:00Z"));
        assert_eq!(found.closed_at, None);
        assert!(found.updated_at.is_some());
    }

    #[tokio::test]
    async fn gets_a_change_request_by_the_reference_the_caller_gave() {
        let (host, _) = host_over(RecordingProvider {
            held: Mutex::new(vec![change_request(7, "feature/x", "open")]),
            ..RecordingProvider::default()
        });

        let found = host.get("/repo", "7").await.unwrap();

        assert_eq!(found.number, 7);
        assert_eq!(found.state, GitPullRequestState::Open);
        assert!(host.get("/repo", "99").await.is_err());
    }

    #[tokio::test]
    async fn creates_a_change_request_with_its_body_in_a_file_and_reads_it_back() {
        let (host, provider) = host_over(RecordingProvider {
            echo_created: true,
            ..RecordingProvider::default()
        });

        let created = host
            .create(
                "/repo",
                &CreateChangeRequest {
                    cwd: "/repo".to_owned(),
                    title: "Ship it".to_owned(),
                    body: "Body text".to_owned(),
                    base_branch: "main".to_owned(),
                    head_branch: "feature/x".to_owned(),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            *provider.created_bodies.lock(),
            vec!["Body text".to_owned()]
        );
        assert_eq!(created.head_ref_name, "feature/x");
        assert_eq!(created.base_ref_name, "main");
        assert_eq!(created.state, GitPullRequestState::Open);
    }

    #[tokio::test]
    async fn a_create_the_host_does_not_show_back_is_an_error() {
        let (host, _) = host_over(RecordingProvider::default());

        let error = host
            .create(
                "/repo",
                &CreateChangeRequest {
                    cwd: "/repo".to_owned(),
                    title: "Ship it".to_owned(),
                    body: String::new(),
                    base_branch: "main".to_owned(),
                    head_branch: "feature/x".to_owned(),
                },
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("could not be found"));
    }
}
