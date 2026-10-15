//! Port of upstream/packages/source-control-github/src/server/githubStackRebase.ts

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::api::GitHubApi;
use crate::source_control_core::Cause;
use crate::source_control_core::host::{SourceControlHost, SourceControlProcessInput};

/// One open layer of a stack, bottom to top, with the head the reader reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeLayer {
    pub number: u64,
    pub head_branch: String,
    pub head_sha: String,
}

/// A layer's rebase hit a conflict; the layers below it are already rebased on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "PR #{number} conflicts with the layer below it. {completed} layers were rebased; resolve #{number} with `gh stack rebase` and push."
)]
pub struct GitHubStackRebaseConflictError {
    pub number: u64,
    pub completed: u64,
}

/// Git failed for a reason other than a conflict: the fetch, or a push GitHub refused.
#[derive(Debug, Clone, thiserror::Error)]
#[error("Stack rebase stopped at PR #{number} while {step}. {completed} layers were rebased.")]
pub struct GitHubStackRebaseGitError {
    pub step: String,
    pub number: u64,
    pub completed: u64,
    pub cause: Cause,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubStackRebaseError {
    #[error(transparent)]
    Conflict(#[from] GitHubStackRebaseConflictError),
    #[error(transparent)]
    Git(#[from] GitHubStackRebaseGitError),
}

#[derive(Debug, Clone)]
pub struct CascadeRebaseInput {
    pub host: String,
    pub repository: String,
    pub base: String,
    pub layers: Vec<CascadeLayer>,
    /// Where to fetch and push; the repository's own URL on its host unless a test points elsewhere.
    pub remote: Option<String>,
}

/// The git URL of a repository on a GitHub host, which is also where its token is good.
fn remote_url(host: &str, repository: &str) -> String {
    format!("https://{}/{repository}.git", host.trim().to_lowercase())
}

/// GitHub's own "Rebase stack": every open layer, bottom to top, onto the new head of the layer
/// below it (the bottom one onto the stack's base). Each layer moves only its own commits —
/// `git rebase --onto <new parent> <old parent>` — so a rebased lower layer is never replayed into
/// the one above it, which is what GitHub's per-PR "update branch" does and why it conflicts.
///
/// It works in a throwaway clone, so the environment's checkout never moves, and pushes each
/// layer with a lease on the head that was reviewed, so a push that landed meanwhile is refused
/// rather than overwritten. The stacks API has no rebase endpoint to call instead.
///
/// Returns how many layers were rebased.
pub async fn cascade_rebase_stack(
    api: &dyn GitHubApi,
    host: &dyn SourceControlHost,
    input: &CascadeRebaseInput,
) -> Result<u64, GitHubStackRebaseError> {
    let Some(first) = input.layers.first() else { return Ok(0) };
    let credential = api.credential(&input.host).await.map_err(|error| {
        // Unlike upstream (where a credential failure escapes untyped), keep it a typed git error.
        GitHubStackRebaseGitError {
            step: "preparing".into(),
            number: first.number,
            completed: 0,
            cause: std::sync::Arc::new(error),
        }
    })?;
    let directory = tempfile::Builder::new()
        .prefix("t3-stack-rebase-")
        .tempdir()
        .map_err(|error| GitHubStackRebaseGitError {
            step: "preparing".into(),
            number: first.number,
            completed: 0,
            cause: std::sync::Arc::new(error),
        })?;
    let result = cascade_in_directory(
        host,
        input,
        credential.token.value(),
        &directory.path().to_string_lossy(),
    )
    .await;
    // Scoped cleanup (upstream `Effect.scoped`), off the async thread.
    let _ = tokio::task::spawn_blocking(move || drop(directory)).await;
    result
}

async fn cascade_in_directory(
    host: &dyn SourceControlHost,
    input: &CascadeRebaseInput,
    token: &str,
    directory: &str,
) -> Result<u64, GitHubStackRebaseError> {
    let first = &input.layers[0];
    // The token rides in a header for this process only: never in the URL, argv, or git config.
    let remote = input
        .remote
        .clone()
        .unwrap_or_else(|| remote_url(&input.host, &input.repository));
    let authorization = format!(
        "AUTHORIZATION: basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    );
    let env: HashMap<String, String> = [
        ("GIT_TERMINAL_PROMPT", "0".to_owned()),
        ("GIT_CONFIG_COUNT", "3".to_owned()),
        ("GIT_CONFIG_KEY_0", format!("http.{remote}.extraheader")),
        ("GIT_CONFIG_VALUE_0", authorization),
        ("GIT_CONFIG_KEY_1", "user.name".to_owned()),
        ("GIT_CONFIG_VALUE_1", "T3 Code".to_owned()),
        ("GIT_CONFIG_KEY_2", "user.email".to_owned()),
        ("GIT_CONFIG_VALUE_2", "noreply@t3.codes".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();

    let git = |args: Vec<String>, allow_non_zero_exit: bool| {
        let env = env.clone();
        async move {
            host.process_run(SourceControlProcessInput {
                operation: "cascadeRebaseStack".into(),
                command: "git".into(),
                args,
                cwd: directory.to_owned(),
                env: Some(env),
                timeout_ms: Some(120_000),
                allow_non_zero_exit,
                ..Default::default()
            })
            .await
        }
    };
    let failed = |step: &str, number: u64, completed: u64| {
        let step = step.to_owned();
        move |cause: turin_contracts::vcs::VcsError| GitHubStackRebaseGitError {
            step,
            number,
            completed,
            cause: std::sync::Arc::new(cause),
        }
    };
    let strings = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();

    git(strings(&["init", "--quiet"]), false)
        .await
        .map_err(failed("preparing", first.number, 0))?;
    git(strings(&["remote", "add", "origin", &remote]), false)
        .await
        .map_err(failed("preparing", first.number, 0))?;
    let mut fetch = strings(&[
        "fetch",
        "--quiet",
        "--no-tags",
        "origin",
        &format!("+refs/heads/{0}:refs/remotes/origin/{0}", input.base),
    ]);
    fetch.extend(
        input
            .layers
            .iter()
            .map(|l| format!("+refs/heads/{0}:refs/remotes/origin/{0}", l.head_branch)),
    );
    git(fetch, false).await.map_err(failed("fetching", first.number, 0))?;

    let mut parent_old = format!("origin/{}", input.base);
    let mut parent_new = format!("origin/{}", input.base);
    for (index, layer) in input.layers.iter().enumerate() {
        let completed = index as u64;
        // The bottom layer's old parent is where it forked from the base; above that it is the
        // reviewed head of the layer below, which is exactly the commits this layer must not replay.
        let upstream = if index == 0 {
            git(strings(&["merge-base", &parent_old, &layer.head_sha]), false)
                .await
                .map_err(failed("reading the fork point", layer.number, completed))?
                .stdout
                .trim()
                .to_owned()
        } else {
            parent_old.clone()
        };
        git(strings(&["checkout", "--quiet", "--detach", &layer.head_sha]), false)
            .await
            .map_err(failed("checking out", layer.number, completed))?;
        let rebase = git(
            strings(&["rebase", "--quiet", "--onto", &parent_new, &upstream]),
            true,
        )
        .await
        .map_err(failed("rebasing", layer.number, completed))?;
        if rebase.exit_code != 0 {
            let _ = git(strings(&["rebase", "--abort"]), true).await;
            return Err(GitHubStackRebaseConflictError { number: layer.number, completed }.into());
        }
        let rebased = git(strings(&["rev-parse", "HEAD"]), false)
            .await
            .map_err(failed("rebasing", layer.number, completed))?
            .stdout
            .trim()
            .to_owned();
        if rebased != layer.head_sha {
            git(
                strings(&[
                    "push",
                    "--quiet",
                    &format!(
                        "--force-with-lease=refs/heads/{}:{}",
                        layer.head_branch, layer.head_sha
                    ),
                    "origin",
                    &format!("{rebased}:refs/heads/{}", layer.head_branch),
                ]),
                false,
            )
            .await
            .map_err(failed("pushing", layer.number, completed))?;
        }
        parent_old = layer.head_sha.clone();
        parent_new = rebased;
    }
    Ok(input.layers.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::api::{
        GitHubApiError, GitHubGraphQlInput, GitHubRequestCredential, GitHubRestInput,
        GitHubRestResponse,
    };
    use crate::source_control_core::host::{
        SourceControlGitExecuteInput, SourceControlGitOutput, SourceControlGitRemoteBranch,
        SourceControlProcessOutput,
    };
    use crate::source_control_core::redacted::Redacted;
    use crate::source_control_core::test_process;
    use async_trait::async_trait;
    use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
    use turin_contracts::settings::{ServerSettings, ServerSettingsError};
    use turin_contracts::vcs::{VcsError, VcsListRemotesResult};

    struct FakeApi;
    #[async_trait]
    impl GitHubApi for FakeApi {
        async fn graphql(&self, _: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
            unimplemented!()
        }
        async fn rest(&self, _: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
            unimplemented!()
        }
        async fn credential(&self, _: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
            Ok(GitHubRequestCredential {
                token: Redacted::new("token".into()),
                fingerprint: "fp".into(),
            })
        }
    }

    /// Runs real git; the cascade never asks the host for anything else.
    struct RealGitHost;
    #[async_trait]
    impl SourceControlHost for RealGitHost {
        async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError> {
            unimplemented!()
        }
        async fn process_run(
            &self,
            input: SourceControlProcessInput,
        ) -> Result<SourceControlProcessOutput, VcsError> {
            test_process::run(input).await
        }
        async fn git_execute(
            &self,
            _: SourceControlGitExecuteInput,
        ) -> Result<SourceControlGitOutput, GitCommandError> {
            unimplemented!()
        }
        async fn git_resolve_commit(&self, _: &str, _: &str) -> Result<String, GitCommandError> {
            unimplemented!()
        }
        async fn git_remotes(&self, _: &str) -> Result<VcsListRemotesResult, VcsError> {
            unimplemented!()
        }
        async fn git_read_config_value(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<String>, GitCommandError> {
            unimplemented!()
        }
        async fn git_resolve_primary_remote_name(&self, _: &str) -> Result<String, GitCommandError> {
            unimplemented!()
        }
        async fn git_ensure_remote(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<String, GitCommandError> {
            unimplemented!()
        }
        async fn git_list_local_branch_names(&self, _: &str) -> Result<Vec<String>, GitCommandError> {
            unimplemented!()
        }
        async fn git_fetch_remote_branch(
            &self,
            _: &SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_fetch_remote_tracking_branch(
            &self,
            _: &SourceControlGitRemoteBranch,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_set_branch_upstream(
            &self,
            _: &SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_switch_ref(
            &self,
            _: &VcsSwitchRefInput,
        ) -> Result<VcsSwitchRefResult, GitCommandError> {
            unimplemented!()
        }
    }

    fn author_env() -> HashMap<String, String> {
        [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    async fn git(cwd: &str, args: &[&str]) -> String {
        test_process::run(SourceControlProcessInput {
            operation: "test".into(),
            command: "git".into(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            cwd: cwd.into(),
            env: Some(author_env()),
            ..Default::default()
        })
        .await
        .unwrap()
        .stdout
        .trim()
        .to_owned()
    }

    struct Repo {
        _root: tempfile::TempDir,
        remote: String,
        layers: Vec<CascadeLayer>,
    }

    impl Repo {
        async fn head(&self, branch: &str) -> String {
            git(&self.remote, &["rev-parse", &format!("refs/heads/{branch}")]).await
        }
    }

    /// A bare "GitHub" holding the live-run stack: `main` moved ahead after the stack was cut,
    /// and a second layer sits on the first.
    async fn setup() -> Repo {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_string_lossy().into_owned();
        let remote = format!("{base}/remote.git");
        let work = format!("{base}/work");
        let write = |path: &str, text: &str| std::fs::write(format!("{work}/{path}"), text).unwrap();
        git(&base, &["init", "--quiet", "--bare", "-b", "main", &remote]).await;
        git(&base, &["clone", "--quiet", &remote, &work]).await;
        write("index.ts", "export const answer = 41;\n");
        git(&work, &["add", "-A"]).await;
        git(&work, &["commit", "--quiet", "-m", "Initial"]).await;
        git(&work, &["switch", "--quiet", "-c", "feat/answer-42"]).await;
        write("index.ts", "export const answer = 42;\n");
        git(&work, &["commit", "--quiet", "-am", "A: set the answer to 42"]).await;
        git(&work, &["switch", "--quiet", "-c", "feat/answer-doc"]).await;
        write("index.ts", "/** The answer. */\nexport const answer = 42;\n");
        git(&work, &["commit", "--quiet", "-am", "B: document the answer"]).await;
        git(&work, &["switch", "--quiet", "main"]).await;
        write("README.md", "main moved ahead\n");
        git(&work, &["add", "-A"]).await;
        git(&work, &["commit", "--quiet", "-m", "Docs"]).await;
        git(&work, &["push", "--quiet", "origin", "main", "feat/answer-42", "feat/answer-doc"])
            .await;
        let mut repo = Repo { _root: root, remote, layers: vec![] };
        repo.layers = vec![
            CascadeLayer {
                number: 1,
                head_branch: "feat/answer-42".into(),
                head_sha: repo.head("feat/answer-42").await,
            },
            CascadeLayer {
                number: 2,
                head_branch: "feat/answer-doc".into(),
                head_sha: repo.head("feat/answer-doc").await,
            },
        ];
        repo
    }

    fn input(repo: &Repo, layers: Vec<CascadeLayer>) -> CascadeRebaseInput {
        CascadeRebaseInput {
            host: "github.com".into(),
            repository: "acme/web".into(),
            base: "main".into(),
            layers,
            remote: Some(repo.remote.clone()),
        }
    }

    #[tokio::test]
    async fn moves_each_layers_own_commits_onto_the_rebased_layer_below_it() {
        let repo = setup().await;
        let completed =
            cascade_rebase_stack(&FakeApi, &RealGitHost, &input(&repo, repo.layers.clone()))
                .await
                .unwrap();
        assert_eq!(completed, 2);
        let main = repo.head("main").await;
        let bottom = repo.head("feat/answer-42").await;
        let top = repo.head("feat/answer-doc").await;
        // Bottom sits on the new main; top sits on the new bottom with only its own commit.
        assert_eq!(git(&repo.remote, &["rev-parse", &format!("{bottom}~1")]).await, main);
        assert_eq!(git(&repo.remote, &["rev-parse", &format!("{top}~1")]).await, bottom);
        assert_eq!(
            git(&repo.remote, &["log", "--format=%s", &format!("{bottom}..{top}")]).await,
            "B: document the answer"
        );
    }

    #[tokio::test]
    async fn refuses_to_overwrite_a_layer_pushed_after_it_was_reviewed() {
        let repo = setup().await;
        // Reviewed at the bottom layer's head, but the branch is really at its own commit.
        let stale = vec![
            repo.layers[0].clone(),
            CascadeLayer { head_sha: repo.layers[0].head_sha.clone(), ..repo.layers[1].clone() },
        ];
        let error = cascade_rebase_stack(&FakeApi, &RealGitHost, &input(&repo, stale))
            .await
            .unwrap_err();
        match error {
            GitHubStackRebaseError::Git(e) => assert_eq!((e.number, e.completed), (2, 1)),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(repo.head("feat/answer-doc").await, repo.layers[1].head_sha);
    }

    #[tokio::test]
    async fn stops_at_a_conflicting_layer_and_leaves_it_untouched() {
        let repo = setup().await;
        // Make main conflict with the bottom layer's change to the same line.
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().to_string_lossy().into_owned();
        git(&dir, &["clone", "--quiet", &repo.remote, "."]).await;
        std::fs::write(format!("{dir}/index.ts"), "export const answer = 43;\n").unwrap();
        git(&dir, &["commit", "--quiet", "-am", "Conflict"]).await;
        git(&dir, &["push", "--quiet", "origin", "main"]).await;

        let error =
            cascade_rebase_stack(&FakeApi, &RealGitHost, &input(&repo, repo.layers.clone()))
                .await
                .unwrap_err();
        match error {
            GitHubStackRebaseError::Conflict(e) => assert_eq!((e.number, e.completed), (1, 0)),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(repo.head("feat/answer-42").await, repo.layers[0].head_sha);
    }
}
