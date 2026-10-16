//! Port of upstream/apps/server/src/pullRequest/PullRequestReviewGuideService.ts
//!
//! Guided reviews: a maintainer-facing brief on a pull request, written by the environment's
//! source-control writer model. See `PullRequestReviewGuide` in contracts for what a guide is for.
//!
//! The model reads the change itself, read-only, from a disposable checkout of the head, so a
//! guide has no size limit. That takes minutes on a large change, so a run belongs to the server
//! rather than to the request that started it: Generate returns as soon as the run is under way
//! and readers watch its state through `get`.
//!
//! Generation is explicit and the result is stored per pull request, so opening a guide never
//! spends a model run. A push leaves the stored guide in place and the reader sees it as stale.
//!
//! # Seams
//!
//! Upstream's collaborators are Effect services; here each is a small port the app implements:
//! [`ReviewGuidePullRequests`] (`PullRequestService`), [`ReviewGuideProjects`]
//! (`ProjectService.getShell`), [`ReviewGuideSettings`] (`ServerSettingsService` +
//! `ProviderRegistry` + `resolveProjectSettings`/`resolveReviewGuideModelSelection`), git through
//! [`SourceControlHost`], and **[`PullRequestReviewGuideGenerator`], the model call
//! (`TextGeneration.generateReviewGuide`), which has no implementation in this crate**.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::FuturesOrdered;
use futures::{FutureExt, StreamExt};
use parking_lot::Mutex;
use tokio::task::JoinSet;
use turin_contracts::base_schemas::{ProjectId, TrimmedNonEmptyString};
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::git::GitCommandError;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::pull_request::{
    PullRequestActivity, PullRequestDetail, PullRequestOperationError, PullRequestRef,
    PullRequestStack, PullRequestStackLayer, PullRequestState,
};
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuide, PullRequestReviewGuideResult, PullRequestReviewGuideRun,
};
use turin_contracts::thread_pull_request::ThreadPullRequestKey;
use turin_persistence::{
    Db, ThreadPullRequestKey as StoredKey, format_utc_millis, get_pull_request_review_guide,
    put_pull_request_review_guide,
};

use super::read_cache::PullRequestReadError;
use super::review_guide_input::{
    ReviewGuideGenerationInput, ReviewGuideGenerationResult, ReviewGuideStackLayer,
    ReviewGuideStackLayerState, review_guide_input_from_pull_request,
};
use super::review_guide_normalize::{
    ReviewGuideChange, normalize_review_guide, parse_review_guide_change,
};
use super::sync_key::pull_request_sync_key;
use crate::shared::source_control::source_control_repository_selector;
use crate::source_control_core::ambient;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::host::{
    SourceControlGitExecuteInput, SourceControlGitOutput, SourceControlHost,
};

/// `PullRequestService.PullRequestError`: unavailable or operation.
pub type PullRequestError = PullRequestReadError;

/// `PATCH_RENDER_PREFIX_ARGS` (vcs/GitVcsDriverCore.ts).
const PATCH_RENDER_PREFIX_ARGS: [&str; 2] = ["--src-prefix=a/", "--dst-prefix=b/"];

const FETCH_TIMEOUT_MS: u64 = 5 * 60_000;
const READ_CHANGE_TIMEOUT_MS: u64 = 60_000;
/// Hunk headers only, but a large change still has many of them.
const READ_CHANGE_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_FAILURE_DETAIL: &str = "The review guide could not be written.";

// ---------------------------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------------------------

/// What the guide needs of `PullRequestService`.
#[async_trait]
pub trait ReviewGuidePullRequests: Send + Sync {
    async fn detail(&self, input: &PullRequestRef) -> Result<PullRequestDetail, PullRequestError>;
    async fn activity(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestActivity, PullRequestError>;
    async fn stack(
        &self,
        input: &PullRequestRef,
    ) -> Result<Option<PullRequestStack>, PullRequestError>;
    /// `invalidate({ reference })`
    async fn invalidate(&self, reference: &PullRequestRef);
}

/// The fields of `OrchestrationProjectShell` the guide reads.
#[derive(Debug, Clone)]
pub struct ReviewGuideProject {
    pub workspace_root: String,
    pub repository_identity: Option<RepositoryIdentity>,
}

#[async_trait]
pub trait ReviewGuideProjects: Send + Sync {
    /// `getShell(projectId)`; `None` for an unknown project or a failed lookup.
    async fn get_shell(&self, project_id: &ProjectId) -> Option<ReviewGuideProject>;
}

/// The project's guide settings, resolved: `resolveProjectSettings(settings, projectId)` and
/// `resolveReviewGuideModelSelection(projectSettings, providers, projectId)`.
#[derive(Debug, Clone)]
pub struct ReviewGuideProjectSettings {
    pub model_selection: ModelSelection,
    /// The maintainer's preferences for guides; `None` for none.
    pub instructions: Option<String>,
}

#[async_trait]
pub trait ReviewGuideSettings: Send + Sync {
    /// `settings.reviewGuidesEnabled`. A beta the server owns, so every client and agent sees the
    /// same answer. The error is the cause to attach to the refusal.
    async fn review_guides_enabled(&self) -> Result<bool, String>;
    async fn for_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<ReviewGuideProjectSettings, String>;
}

/// `TextGenerationError`, as far as the guide reads it: the `detail` shown to the reader.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct ReviewGuideGenerationError {
    pub detail: String,
}

/// The seam to the model: upstream `TextGeneration.generateReviewGuide`. The model reads
/// `input.cwd` (a disposable, read-only checkout of the head) and returns a guide the service then
/// holds to the change. **No implementation lives in this crate**; the app provides one backed by
/// the configured provider instance.
#[async_trait]
pub trait PullRequestReviewGuideGenerator: Send + Sync {
    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, ReviewGuideGenerationError>;
}

// ---------------------------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------------------------

/// Everything [`PullRequestReviewGuideService::new`] needs.
pub struct PullRequestReviewGuideDependencies {
    pub pull_requests: Arc<dyn ReviewGuidePullRequests>,
    pub db: Db,
    pub projects: Arc<dyn ReviewGuideProjects>,
    pub settings: Arc<dyn ReviewGuideSettings>,
    pub generator: Arc<dyn PullRequestReviewGuideGenerator>,
    pub host: Arc<dyn SourceControlHost>,
    pub clock: Arc<dyn Clock>,
    /// `config.worktreesDir`: where each run's checkout is made.
    pub worktrees_dir: PathBuf,
}

/// `PullRequestReviewGuideResult & { started }`
#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestReviewGuideGenerateResult {
    pub guide: Option<PullRequestReviewGuide>,
    pub run: Option<PullRequestReviewGuideRun>,
    pub current_head_sha: Option<TrimmedNonEmptyString>,
    /// Whether this call began the run or joined one already going.
    pub started: bool,
}

struct RunEntry {
    /// Identifies the run: a later claim replaces the entry, and from then on this run's
    /// completion leaves the newer run's state and guide alone.
    claim: u64,
    run: PullRequestReviewGuideRun,
}

struct Inner {
    deps: PullRequestReviewGuideDependencies,
    /// In memory on purpose: a restart drops a run in progress, and a reader presses Generate again.
    runs: Mutex<HashMap<String, RunEntry>>,
    next_claim: AtomicU64,
    /// Names each run's private git refs; unique for the server's lifetime, like the runs map.
    run_counter: AtomicU64,
    /// Claiming a run and storing a finished guide take turns, so a run that still owns its pull
    /// request when it checks is the one whose guide is stored: a newer claim cannot land between.
    ownership: tokio::sync::Mutex<()>,
    /// Runs outlive the request that started them and end with the server.
    tasks: Mutex<JoinSet<()>>,
}

pub struct PullRequestReviewGuideService {
    inner: Arc<Inner>,
}

/// What a step of a run failed with; `detail` is what a reader is told.
#[derive(Debug)]
enum GuideError {
    Operation(PullRequestOperationError),
    Git(GitCommandError),
    Pull(PullRequestError),
    Generation(ReviewGuideGenerationError),
    Other(String),
}

impl GuideError {
    /// `"detail" in error && typeof error.detail === "string"`
    fn detail(&self) -> Option<String> {
        match self {
            Self::Operation(PullRequestOperationError::Operation { detail, .. }) => {
                Some(detail.to_string())
            }
            Self::Git(GitCommandError::GitCommandError { detail, .. }) => Some(detail.clone()),
            Self::Pull(PullRequestReadError::Operation(PullRequestOperationError::Operation {
                detail,
                ..
            })) => Some(detail.to_string()),
            Self::Pull(PullRequestReadError::Unavailable(_)) | Self::Other(_) => None,
            Self::Generation(error) => Some(error.detail.clone()),
        }
    }
}

impl std::fmt::Display for GuideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation(error) => write!(f, "{error}"),
            Self::Git(error) => write!(f, "{error}"),
            Self::Pull(error) => write!(f, "{error}"),
            Self::Generation(error) => write!(f, "{error}"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<PullRequestOperationError> for GuideError {
    fn from(error: PullRequestOperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<GitCommandError> for GuideError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}

impl From<PullRequestError> for GuideError {
    fn from(error: PullRequestError) -> Self {
        Self::Pull(error)
    }
}

fn operation_error(operation: &str, detail: impl AsRef<str>) -> PullRequestOperationError {
    operation_error_with_cause(operation, detail, None)
}

fn operation_error_with_cause(
    operation: &str,
    detail: impl AsRef<str>,
    cause: Option<serde_json::Value>,
) -> PullRequestOperationError {
    PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(detail.as_ref()).unwrap_or_else(|_| {
            TrimmedNonEmptyString::new("Unknown failure.").expect("non-blank literal")
        }),
        reason: None,
        cause,
    }
}

fn key_string(key: &ThreadPullRequestKey) -> String {
    format!("{}/{}#{}", key.host, key.repository, key.number)
}

fn stored_key(key: &ThreadPullRequestKey) -> StoredKey {
    StoredKey {
        host: key.host.to_string(),
        repository: key.repository.to_string(),
        number: key.number.get() as i64,
    }
}

fn now_iso(clock: &dyn Clock) -> String {
    format_utc_millis(
        chrono::DateTime::from_timestamp_millis(clock.now_millis()).unwrap_or_default(),
    )
}

fn is_commit_hash(value: &str) -> bool {
    (7..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The key and checkout a request resolves to.
struct Resolved {
    key: ThreadPullRequestKey,
    workspace_root: String,
    identity: Option<RepositoryIdentity>,
}

impl PullRequestReviewGuideService {
    pub fn new(deps: PullRequestReviewGuideDependencies) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                runs: Mutex::new(HashMap::new()),
                next_claim: AtomicU64::new(0),
                run_counter: AtomicU64::new(0),
                ownership: tokio::sync::Mutex::new(()),
                tasks: Mutex::new(JoinSet::new()),
            }),
        }
    }

    /// The stored guide, any run in progress, and the head now. Never runs a model.
    pub async fn get(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestReviewGuideResult, PullRequestError> {
        let inner = &self.inner;
        let Resolved { key, .. } = inner.key_for(input).await?;
        let (guide, detail) = tokio::join!(
            inner.read_stored(&key),
            inner.deps.pull_requests.detail(input)
        );
        let (guide, detail) = (guide?, detail?);
        let run = inner
            .runs
            .lock()
            .get(&key_string(&key))
            .map(|entry| entry.run.clone());
        Ok(PullRequestReviewGuideResult {
            guide,
            run,
            current_head_sha: detail.head_sha,
        })
    }

    /// Start writing a guide for the current head and return once it is under way. Joins a run
    /// already going for that head. A stored guide stays readable until the new one replaces it,
    /// so a reader can rewrite one after changing the guide model or instructions. `started`
    /// says whether this call began the run or joined one already going.
    pub async fn generate(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestReviewGuideGenerateResult, PullRequestError> {
        let inner = &self.inner;
        // A beta the server owns, so every client and agent sees the same answer.
        let enabled = inner
            .deps
            .settings
            .review_guides_enabled()
            .await
            .map_err(|cause| {
                operation_error_with_cause(
                    "generateReviewGuide",
                    "Could not read the server settings.",
                    Some(serde_json::Value::String(cause)),
                )
            })?;
        if !enabled {
            return Err(operation_error(
                "generateReviewGuide",
                "Review guides are off. Turn them on in Settings → Source Control.",
            )
            .into());
        }
        let Resolved {
            key,
            workspace_root,
            identity,
        } = inner.key_for(input).await?;
        inner.require_own_repository(&key, identity.as_ref())?;
        let run_key = key_string(&key);
        // A press asks for the head as it is now, not a read shared from the last minute, so a
        // guide is never started for a commit the pull request has already moved past.
        inner.deps.pull_requests.invalidate(input).await;
        let (detail, stored) = tokio::join!(
            inner.deps.pull_requests.detail(input),
            inner.read_stored(&key)
        );
        let (detail, stored) = (detail?, stored?);
        let current_head_sha = detail.head_sha.clone();
        let Some(current_head) = current_head_sha.clone() else {
            return Err(operation_error(
                "generateReviewGuide",
                "This host did not report the pull request's head commit.",
            )
            .into());
        };
        let running = PullRequestReviewGuideRun::Running {
            head_sha: current_head.clone(),
            started_at: now_iso(inner.deps.clock.as_ref()),
        };
        // Claimed atomically, so two readers pressing Generate start one run between them. The
        // head is read again under the claim: a press that read an older head before a push must
        // not replace a run another press started for the new one, so it asks to be pressed again.
        let claim = inner.next_claim.fetch_add(1, Ordering::SeqCst);
        let claimed = {
            let _ownership = inner.ownership.lock().await;
            let latest = inner.deps.pull_requests.detail(input).await?;
            if latest.head_sha.as_ref() != Some(&current_head) {
                return Err(operation_error(
                    "generateReviewGuide",
                    "The pull request changed while the guide was starting. Try again.",
                )
                .into());
            }
            let mut runs = inner.runs.lock();
            match runs.get(&run_key) {
                Some(RunEntry {
                    run: existing @ PullRequestReviewGuideRun::Running { head_sha, .. },
                    ..
                }) if *head_sha == current_head => Err(existing.clone()),
                _ => {
                    runs.insert(
                        run_key.clone(),
                        RunEntry {
                            claim,
                            run: running.clone(),
                        },
                    );
                    Ok(())
                }
            }
        };
        if let Err(existing) = claimed {
            return Ok(PullRequestReviewGuideGenerateResult {
                guide: stored,
                run: Some(existing),
                current_head_sha,
                started: false,
            });
        }

        let run_id = inner.run_counter.fetch_add(1, Ordering::SeqCst).to_string();
        let task = {
            let inner = inner.clone();
            let input = input.clone();
            let head = current_head.to_string();
            let run_key = run_key.clone();
            async move {
                inner
                    .run(
                        &input,
                        key,
                        workspace_root,
                        detail,
                        head,
                        run_id,
                        run_key,
                        claim,
                    )
                    .await;
            }
        };
        {
            let mut tasks = inner.tasks.lock();
            while tasks.try_join_next().is_some() {}
            tasks.spawn(ambient::capture().run(task));
        }
        Ok(PullRequestReviewGuideGenerateResult {
            guide: stored,
            run: Some(running),
            current_head_sha,
            started: true,
        })
    }
}

impl Inner {
    async fn key_for(&self, input: &PullRequestRef) -> Result<Resolved, PullRequestError> {
        let project = self.deps.projects.get_shell(&input.project_id).await;
        let key = pull_request_sync_key(
            input,
            project
                .as_ref()
                .and_then(|p| p.repository_identity.as_ref()),
        );
        match (key, project) {
            (Some(key), Some(project)) => Ok(Resolved {
                key,
                workspace_root: project.workspace_root,
                identity: project.repository_identity,
            }),
            _ => Err(operation_error(
                "reviewGuide",
                "Could not identify this pull request's host.",
            )
            .into()),
        }
    }

    /// A guide reads the change in a checkout fetched from the project's own remote, so the pull
    /// request has to be in that repository. One from another repository is refused rather than
    /// measured against commits that are not its own.
    fn require_own_repository(
        &self,
        key: &ThreadPullRequestKey,
        identity: Option<&RepositoryIdentity>,
    ) -> Result<(), PullRequestError> {
        let own = identity.and_then(|identity| {
            let repository = source_control_repository_selector(Some(identity))?;
            pull_request_sync_key(
                &PullRequestRef {
                    project_id: ProjectId::new("own").expect("non-blank literal"),
                    host: None,
                    expected_account_id: None,
                    allow_stale: None,
                    repository: TrimmedNonEmptyString::new(repository).ok()?,
                    number: key.number,
                },
                Some(identity),
            )
        });
        match own {
            Some(own) if own.host == key.host && own.repository == key.repository => Ok(()),
            _ => Err(operation_error(
                "generateReviewGuide",
                "Guides are written from this project's checkout, and this pull request is in another repository. Write it from a project that is a checkout of that repository.",
            )
            .into()),
        }
    }

    async fn read_stored(
        &self,
        key: &ThreadPullRequestKey,
    ) -> Result<Option<PullRequestReviewGuide>, PullRequestError> {
        let stored = stored_key(key);
        let value = self
            .deps
            .db
            .read(move |conn| get_pull_request_review_guide(conn, &stored))
            .await
            .map_err(|cause| stored_guide_error(cause.to_string()))?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|cause| stored_guide_error(cause.to_string()))
    }

    /// The forked part of `generate`: write the guide, then forget the run or record why it failed.
    #[allow(clippy::too_many_arguments)]
    async fn run(
        self: &Arc<Self>,
        input: &PullRequestRef,
        key: ThreadPullRequestKey,
        workspace_root: String,
        detail: PullRequestDetail,
        head_sha: String,
        run_id: String,
        run_key: String,
        claim: u64,
    ) {
        let outcome = AssertUnwindSafe(self.write(
            input,
            &key,
            &workspace_root,
            &detail,
            &head_sha,
            &run_id,
            &run_key,
            claim,
        ))
        .catch_unwind()
        .await;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => {
                tracing::warn!(key = %run_key, cause = %error, "Review guide generation failed");
                Some(error.detail())
            }
            Err(_panic) => {
                tracing::warn!(key = %run_key, "Review guide generation failed");
                Some(None)
            }
        };
        match failure {
            None => self.finish(&run_key, claim, None),
            Some(detail) => {
                let head_sha = TrimmedNonEmptyString::new(&head_sha).ok();
                let failed = head_sha.map(|head_sha| PullRequestReviewGuideRun::Failed {
                    head_sha,
                    failed_at: now_iso(self.deps.clock.as_ref()),
                    detail: TrimmedNonEmptyString::new(
                        detail.as_deref().unwrap_or(DEFAULT_FAILURE_DETAIL),
                    )
                    .unwrap_or_else(|_| {
                        TrimmedNonEmptyString::new(DEFAULT_FAILURE_DETAIL).expect("non-blank")
                    }),
                });
                self.finish(&run_key, claim, failed);
            }
        }
    }

    /// Replaces this run's state, unless a newer run has taken the pull request over.
    fn finish(&self, run_key: &str, claim: u64, next: Option<PullRequestReviewGuideRun>) {
        let mut runs = self.runs.lock();
        if runs.get(run_key).map(|entry| entry.claim) != Some(claim) {
            return;
        }
        match next {
            None => {
                runs.remove(run_key);
            }
            Some(run) => {
                runs.insert(run_key.to_owned(), RunEntry { claim, run });
            }
        }
    }

    fn owns(&self, run_key: &str, claim: u64) -> bool {
        self.runs.lock().get(run_key).map(|entry| entry.claim) == Some(claim)
    }

    #[allow(clippy::too_many_arguments)]
    async fn write(
        self: &Arc<Self>,
        input: &PullRequestRef,
        key: &ThreadPullRequestKey,
        workspace_root: &str,
        detail: &PullRequestDetail,
        head_sha: &str,
        run_id: &str,
        run_key: &str,
        claim: u64,
    ) -> Result<(), GuideError> {
        let (activity, settings, stack) = tokio::join!(
            self.deps.pull_requests.activity(input),
            self.deps.settings.for_project(&input.project_id),
            // A guide without its stack is still a guide; the stack only adds seams to point at.
            self.deps.pull_requests.stack(input),
        );
        let activity = activity?;
        let settings = settings.map_err(GuideError::Other)?;
        let stack = stack.unwrap_or(None);

        // The checkout lives only as long as the model is reading it. The change's hunks are read
        // from it too, so the guide can be held to the lines the change actually touched.
        let mut checkout = Checkout::new(self.deps.host.clone(), workspace_root.to_owned());
        let result = async {
            let ready = self
                .checkout_head(&mut checkout, detail, head_sha, stack.as_ref(), run_id)
                .await?;
            let mut prompt =
                review_guide_input_from_pull_request(detail, &activity, &ready.base_commit);
            prompt.instructions = settings.instructions.clone();
            prompt.stack = Some(ready.stack);
            let generation = ReviewGuideGenerationInput::from_prompt(
                prompt,
                ready.path.clone(),
                settings.model_selection.clone(),
            );
            let (generated, change) = tokio::join!(
                self.deps.generator.generate_review_guide(generation),
                self.read_change(&ready.path, &ready.base_commit),
            );
            Ok::<_, GuideError>((generated.map_err(GuideError::Generation)?, change?))
        }
        .await;
        checkout.cleanup().await;
        let (generated, change) = result?;

        let normalized = normalize_review_guide(generated, &change);
        let guide = PullRequestReviewGuide {
            head_sha: TrimmedNonEmptyString::new(head_sha)
                .map_err(|error| GuideError::Other(error.to_string()))?,
            generated_at: now_iso(self.deps.clock.as_ref()),
            model_selection: settings.model_selection,
            stops: normalized,
        };
        // A newer run for this pull request, say after a push, owns its guide now; this one's
        // result is for an older head and is dropped rather than written over it.
        let _ownership = self.ownership.lock().await;
        if self.owns(run_key, claim) {
            let stored = stored_key(key);
            let value = serde_json::to_value(&guide)
                .map_err(|error| GuideError::Other(error.to_string()))?;
            self.deps
                .db
                .write(move |tx| put_pull_request_review_guide(tx, &stored, &value))
                .await
                .map_err(|error| GuideError::Other(error.to_string()))?;
        }
        Ok(())
    }

    /// A detached checkout of the head beside the project, removed when the run ends. Pinned to
    /// the reported commit, so a branch checked out elsewhere works and the checkout is exactly
    /// what the host reported.
    async fn checkout_head(
        &self,
        checkout: &mut Checkout,
        detail: &PullRequestDetail,
        head_sha: &str,
        stack: Option<&PullRequestStack>,
        run_id: &str,
    ) -> Result<CheckedOut, GuideError> {
        let workspace_root = checkout.workspace_root.clone();
        let remote = checkout
            .host
            .git_resolve_primary_remote_name(&workspace_root)
            .await?;
        let base_branch = detail.base_branch.as_str();
        // The base branch is what the change is measured from; the head commit is the change.
        // The host names the base branch. A name git rejects, or one that would change the
        // refspec (a leading `+`, a `:`), is refused before it reaches `git fetch`.
        checkout
            .execute(
                "PullRequestReviewGuideService.checkBaseBranch",
                &workspace_root,
                &["check-ref-format", "--branch", base_branch],
                None,
                None,
            )
            .await?;
        if base_branch.starts_with('+') || base_branch.contains(':') {
            return Err(operation_error(
                "generateReviewGuide",
                "The pull request's base branch name is not one git can fetch.",
            )
            .into());
        }
        // Fetched into a ref of this run's own: FETCH_HEAD is shared by every fetch in the
        // workspace, so a concurrent run could otherwise measure this change from its base.
        let base_ref = format!("refs/t3/review-guide/{run_id}/base");
        checkout
            .fetch(&remote, &format!("+refs/heads/{base_branch}:{base_ref}"))
            .await?;
        checkout.base_ref = Some(base_ref.clone());
        let base_tip = checkout
            .host
            .git_resolve_commit(&workspace_root, &base_ref)
            .await?;
        let resolved_head = checkout
            .fetch_head(
                &remote,
                detail.number.get(),
                head_sha,
                detail.head_branch.as_str(),
            )
            .await?;
        // The other layers of a stack are context the model may read across a seam. A layer that
        // cannot be fetched is still named in the prompt, without a head to read.
        let stack_layers: &[PullRequestStackLayer] = stack
            .map(|stack| stack.layers.as_slice())
            .unwrap_or_default();
        // At most four layers are fetched at a time, answered in stack order.
        let layers: Vec<ReviewGuideStackLayer> = {
            let checkout = &*checkout;
            let remote = remote.as_str();
            let mut remaining = stack_layers.iter();
            let mut in_flight = FuturesOrdered::new();
            let mut layers = Vec::with_capacity(stack_layers.len());
            loop {
                while in_flight.len() < 4 {
                    let Some(layer) = remaining.next() else { break };
                    in_flight.push_back(stack_layer(checkout, remote, detail, head_sha, layer));
                }
                match in_flight.next().await {
                    Some(layer) => layers.push(layer),
                    None => break,
                }
            }
            layers
        };

        let worktrees_dir = self.deps.worktrees_dir.clone();
        let directory = tokio::task::spawn_blocking(move || -> std::io::Result<PathBuf> {
            std::fs::create_dir_all(&worktrees_dir)?;
            Ok(tempfile::Builder::new()
                .prefix("review-guide-")
                .tempdir_in(&worktrees_dir)?
                .keep())
        })
        .await
        .map_err(|error| GuideError::Other(error.to_string()))?
        .map_err(|error| GuideError::Other(error.to_string()))?;
        checkout.directory = Some(directory.clone());
        let path = directory.join("checkout").to_string_lossy().into_owned();
        checkout
            .execute(
                "PullRequestReviewGuideService.addWorktree",
                &workspace_root,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "--quiet",
                    &path,
                    &resolved_head,
                ],
                Some(FETCH_TIMEOUT_MS),
                None,
            )
            .await?;
        checkout.worktree = Some(path.clone());
        // Measured from where the head left the base, so a stacked change is briefed alone rather
        // than with everything below it that the base has not taken yet.
        let merge_base = checkout
            .execute(
                "PullRequestReviewGuideService.mergeBase",
                &path,
                &["merge-base", &base_tip, "HEAD"],
                None,
                None,
            )
            .await?;
        Ok(CheckedOut {
            path,
            base_commit: merge_base.stdout.trim().to_owned(),
            stack: layers,
        })
    }

    /// The changed paths and their hunks, which a generated guide is held to.
    async fn read_change(
        &self,
        cwd: &str,
        base_commit: &str,
    ) -> Result<ReviewGuideChange, GuideError> {
        let diff = |extra: &'static [&'static str]| async move {
            let mut args: Vec<String> = ["-c", "core.quotePath=false", "diff"]
                .map(str::to_owned)
                .to_vec();
            // Fixed prefixes: a user's diff.noPrefix or custom prefixes would hide the headers
            // the parser reads.
            args.extend(PATCH_RENDER_PREFIX_ARGS.map(str::to_owned));
            args.extend(extra.iter().map(|arg| (*arg).to_owned()));
            args.push(format!("{base_commit}...HEAD"));
            execute_git(
                self.deps.host.as_ref(),
                "PullRequestReviewGuideService.readChange",
                cwd,
                args,
                Some(READ_CHANGE_TIMEOUT_MS),
                Some(READ_CHANGE_MAX_OUTPUT_BYTES),
            )
            .await
            .map(|output| output.stdout)
        };
        let (name_only, patch) = tokio::join!(
            diff(&["--name-only", "-z"]),
            diff(&["--unified=0", "--no-color", "--no-ext-diff"]),
        );
        Ok(parse_review_guide_change(&name_only?, &patch?))
    }
}

/// One layer of the stack as the prompt names it, with its head fetched where the host served it.
async fn stack_layer(
    checkout: &Checkout,
    remote: &str,
    detail: &PullRequestDetail,
    head_sha: &str,
    layer: &PullRequestStackLayer,
) -> ReviewGuideStackLayer {
    let state = match layer.state {
        PullRequestState::Open => ReviewGuideStackLayerState::Open,
        PullRequestState::Closed => ReviewGuideStackLayerState::Closed,
        PullRequestState::Merged => ReviewGuideStackLayerState::Merged,
    };
    let described = |head_sha: Option<String>| ReviewGuideStackLayer {
        number: layer.number.get(),
        title: layer.title.clone(),
        head_sha,
        state,
    };
    if layer.number == detail.number {
        return described(Some(head_sha.to_owned()));
    }
    let Some(sha) = layer.head_sha.as_ref() else {
        return described(None);
    };
    match checkout
        .fetch_head(
            remote,
            layer.number.get(),
            sha.as_str(),
            layer.head_branch.as_str(),
        )
        .await
    {
        Ok(commit_sha) => described(Some(commit_sha)),
        Err(error) => {
            tracing::info!(
                layer = layer.number.get(),
                detail = error.detail().unwrap_or_default(),
                "Review guide could not read a stack layer's head"
            );
            described(None)
        }
    }
}

fn stored_guide_error(cause: String) -> PullRequestError {
    operation_error_with_cause(
        "reviewGuide",
        "Could not read the stored review guide.",
        Some(serde_json::Value::String(cause)),
    )
    .into()
}

/// `git.execute` with the timeout upstream passes as `timeoutMs`. `SourceControlHost` has no
/// timeout of its own, so it is enforced here; dropping the call stops the git process.
async fn execute_git(
    host: &dyn SourceControlHost,
    operation: &str,
    cwd: &str,
    args: Vec<String>,
    timeout_ms: Option<u64>,
    max_output_bytes: Option<usize>,
) -> Result<SourceControlGitOutput, GitCommandError> {
    let call = host.git_execute(SourceControlGitExecuteInput {
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        args: args.clone(),
        max_output_bytes,
        append_truncation_marker: false,
    });
    let Some(timeout_ms) = timeout_ms else {
        return call.await;
    };
    match tokio::time::timeout(Duration::from_millis(timeout_ms), call).await {
        Ok(result) => result,
        Err(_) => Err(GitCommandError::GitCommandError {
            operation: operation.to_owned(),
            command: "git".to_owned(),
            cwd: cwd.to_owned(),
            argument_count: Some(args.len() as f64),
            exit_code: None,
            stdout_length: None,
            stderr_length: None,
            output_length: None,
            reason: None,
            detail: format!("git timed out after {timeout_ms}ms."),
            cause: None,
        }),
    }
}

struct CheckedOut {
    path: String,
    base_commit: String,
    stack: Vec<ReviewGuideStackLayer>,
}

/// A run's private git state: its base ref, the temporary directory holding the worktree, and the
/// worktree itself. Released in reverse order of creation, on success, failure or cancellation.
struct Checkout {
    host: Arc<dyn SourceControlHost>,
    workspace_root: String,
    base_ref: Option<String>,
    directory: Option<PathBuf>,
    worktree: Option<String>,
}

impl Checkout {
    fn new(host: Arc<dyn SourceControlHost>, workspace_root: String) -> Self {
        Self {
            host,
            workspace_root,
            base_ref: None,
            directory: None,
            worktree: None,
        }
    }

    async fn execute(
        &self,
        operation: &str,
        cwd: &str,
        args: &[&str],
        timeout_ms: Option<u64>,
        max_output_bytes: Option<usize>,
    ) -> Result<SourceControlGitOutput, GitCommandError> {
        execute_git(
            self.host.as_ref(),
            operation,
            cwd,
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout_ms,
            max_output_bytes,
        )
        .await
    }

    async fn fetch(&self, remote: &str, refspec: &str) -> Result<(), GitCommandError> {
        self.execute(
            "PullRequestReviewGuideService.fetch",
            &self.workspace_root,
            &["fetch", "--quiet", "--no-tags", remote, refspec],
            Some(FETCH_TIMEOUT_MS),
            None,
        )
        .await
        .map(|_| ())
    }

    /// Not redundant: rev-parse prefers a branch or tag named like a short hash over the commit.
    async fn resolve_head(&self, sha: &str) -> Result<String, GuideError> {
        let commit_sha = self
            .host
            .git_resolve_commit(&self.workspace_root, sha)
            .await?;
        if !commit_sha.starts_with(sha) {
            return Err(operation_error(
                "generateReviewGuide",
                format!("{sha} names {commit_sha} here, a branch or tag rather than the commit."),
            )
            .into());
        }
        Ok(commit_sha)
    }

    async fn fetch_and_resolve(
        &self,
        remote: &str,
        sha: &str,
        refspec: &str,
    ) -> Result<String, GuideError> {
        self.fetch(remote, refspec).await?;
        self.resolve_head(sha).await
    }

    /// Finds `reported_sha` in the checkout, fetching it from the likeliest places in turn.
    async fn fetch_head(
        &self,
        remote: &str,
        number: u64,
        reported_sha: &str,
        branch: &str,
    ) -> Result<String, GuideError> {
        let sha = reported_sha.to_lowercase();
        if !is_commit_hash(&sha) {
            return Err(operation_error(
                "generateReviewGuide",
                format!("The host reported a head for #{number} that is not a commit hash."),
            )
            .into());
        }
        let mut failures: Vec<String> = Vec::new();
        let mut note = |source: &str, error: GuideError| {
            failures.push(format!(
                "{source}: {}",
                error.detail().unwrap_or_else(|| error.to_string())
            ));
        };
        match self.resolve_head(&sha).await {
            Ok(commit) => return Ok(commit),
            Err(error) => note("local objects", error),
        }
        if sha.len() == 40 || sha.len() == 64 {
            match self.fetch_and_resolve(remote, &sha, &sha).await {
                Ok(commit) => return Ok(commit),
                Err(error) => note("the commit", error),
            }
        }
        let branch_ref = format!("refs/heads/{branch}");
        let by_branch = async {
            self.execute(
                "PullRequestReviewGuideService.checkHeadBranch",
                &self.workspace_root,
                &["check-ref-format", &branch_ref],
                None,
                None,
            )
            .await?;
            self.fetch_and_resolve(remote, &sha, &branch_ref).await
        };
        match by_branch.await {
            Ok(commit) => return Ok(commit),
            Err(error) => note(&format!("branch {branch}"), error),
        }
        for source in [
            format!("refs/pull/{number}/head"),
            format!("refs/merge-requests/{number}/head"),
        ] {
            match self.fetch_and_resolve(remote, &sha, &source).await {
                Ok(commit) => return Ok(commit),
                Err(error) => note(&source, error),
            }
        }
        Err(operation_error(
            "generateReviewGuide",
            format!(
                "Could not find head {sha} of #{number}. {}",
                failures.join(" ")
            ),
        )
        .into())
    }

    /// Removes the worktree, then its directory, then the base ref; failures are logged and ignored.
    async fn cleanup(&mut self) {
        let host = self.host.clone();
        let workspace_root = self.workspace_root.clone();
        let (worktree, directory, base_ref) = (
            self.worktree.take(),
            self.directory.take(),
            self.base_ref.take(),
        );
        release(
            host.as_ref(),
            &workspace_root,
            worktree,
            directory,
            base_ref,
        )
        .await;
    }
}

async fn release(
    host: &dyn SourceControlHost,
    workspace_root: &str,
    worktree: Option<String>,
    directory: Option<PathBuf>,
    base_ref: Option<String>,
) {
    if let Some(path) = worktree {
        let result = execute_git(
            host,
            "GitVcsDriver.removeWorktree",
            workspace_root,
            ["worktree", "remove", "--force", path.as_str()]
                .map(str::to_owned)
                .to_vec(),
            None,
            None,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "Could not remove the review guide checkout");
        }
    }
    if let Some(directory) = directory {
        remove_directory(&directory).await;
    }
    if let Some(base_ref) = base_ref {
        let result = execute_git(
            host,
            "PullRequestReviewGuideService.dropBaseRef",
            workspace_root,
            ["update-ref", "-d", base_ref.as_str()]
                .map(str::to_owned)
                .to_vec(),
            None,
            None,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "Could not drop the review guide base ref");
        }
    }
}

async fn remove_directory(directory: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(directory).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "Could not remove the review guide directory");
    }
}

/// A run that is cancelled mid-flight (its task aborted with the service) still gives back its
/// worktree, directory and base ref.
impl Drop for Checkout {
    fn drop(&mut self) {
        let (worktree, directory, base_ref) = (
            self.worktree.take(),
            self.directory.take(),
            self.base_ref.take(),
        );
        if worktree.is_none() && directory.is_none() && base_ref.is_none() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let host = self.host.clone();
        let workspace_root = self.workspace_root.clone();
        // Detached on purpose: the owner is already gone, and this is bounded cleanup work.
        drop(runtime.spawn(async move {
            release(
                host.as_ref(),
                &workspace_root,
                worktree,
                directory,
                base_ref,
            )
            .await;
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_control_core::clock::ManualClock;
    use futures::future::BoxFuture;
    use pretty_assertions::assert_eq;
    use turin_contracts::base_schemas::PositiveInt;
    use turin_contracts::pull_request_review_guide::{
        PullRequestReviewGuideExcerpt, PullRequestReviewGuideExcerptSide,
        PullRequestReviewGuideFile,
    };
    use turin_provider_core::text_generation::{
        ReviewGuideExcerptOutput, ReviewGuideNoteOutput, ReviewGuideStopOutput,
    };

    const START: i64 = 1_790_812_800_000;

    // ---- git fixtures (real local git, no network) ------------------------------------------

    async fn git(cwd: &Path, args: &[&str]) -> String {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    struct Repositories {
        root: tempfile::TempDir,
        workspace: PathBuf,
        head_sha: String,
        lower_sha: String,
        upper_sha: String,
    }

    impl Repositories {
        fn remote(&self) -> PathBuf {
            self.root.path().join("remote")
        }
    }

    /// A remote whose `main` has moved on past where the change's base branch left it, with the
    /// change stacked on that base branch. A guide for the change must see only its own commit.
    async fn make_repositories() -> Repositories {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--quiet", "--initial-branch=main"]).await;
        git(&remote, &["config", "user.email", "test@example.com"]).await;
        git(&remote, &["config", "user.name", "Test"]).await;
        let commit = |path: &'static str, message: &'static str, contents: String| {
            let remote = remote.clone();
            async move {
                std::fs::write(remote.join(path), contents).unwrap();
                git(&remote, &["add", path]).await;
                git(&remote, &["commit", "--quiet", "-m", message]).await;
                git(&remote, &["rev-parse", "HEAD"]).await
            }
        };
        commit("base.txt", "base", "base".into()).await;
        git(&remote, &["checkout", "--quiet", "-b", "stack-base"]).await;
        let lower_sha = commit("lower.txt", "lower layer", "lower layer".into()).await;
        git(&remote, &["checkout", "--quiet", "-b", "feature"]).await;
        // Ten lines, so an excerpt reaching past them has something to be clamped to.
        let contents: String = (1..=10).map(|index| format!("line {index}\n")).collect();
        let head_sha = commit("feature.txt", "the change", contents).await;
        // A layer above the change, which uses what the change adds.
        git(&remote, &["checkout", "--quiet", "-b", "upper"]).await;
        let upper_sha = commit("upper.txt", "upper layer", "upper layer".into()).await;
        git(&remote, &["checkout", "--quiet", "main"]).await;
        commit("unrelated.txt", "main moved on", "main moved on".into()).await;
        git(
            root.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                workspace.to_str().unwrap(),
            ],
        )
        .await;
        Repositories {
            root,
            workspace,
            head_sha,
            lower_sha,
            upper_sha,
        }
    }

    // ---- a SourceControlHost over the real git binary ------------------------------------------

    type GitHook =
        Arc<dyn Fn(&SourceControlGitExecuteInput) -> Option<GitCommandError> + Send + Sync>;

    struct RealGitHost {
        before_execute: Option<GitHook>,
    }

    fn git_error(
        operation: &str,
        cwd: &str,
        detail: String,
        exit_code: Option<i32>,
    ) -> GitCommandError {
        GitCommandError::GitCommandError {
            operation: operation.to_owned(),
            command: "git".to_owned(),
            cwd: cwd.to_owned(),
            argument_count: None,
            exit_code: exit_code.map(f64::from),
            stdout_length: None,
            stderr_length: None,
            output_length: None,
            reason: None,
            detail,
            cause: None,
        }
    }

    impl RealGitHost {
        async fn run(
            &self,
            operation: &str,
            cwd: &str,
            args: &[String],
        ) -> Result<SourceControlGitOutput, GitCommandError> {
            let output = tokio::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| git_error(operation, cwd, error.to_string(), None))?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if !output.status.success() {
                let detail = stderr.trim();
                let detail = if detail.is_empty() {
                    "git failed"
                } else {
                    detail
                };
                return Err(git_error(
                    operation,
                    cwd,
                    detail.to_owned(),
                    output.status.code(),
                ));
            }
            Ok(SourceControlGitOutput {
                exit_code: 0,
                stdout,
                stderr,
                stdout_truncated: false,
                stderr_truncated: false,
            })
        }
    }

    #[async_trait]
    impl SourceControlHost for RealGitHost {
        async fn settings_get(
            &self,
        ) -> Result<
            turin_contracts::settings::ServerSettings,
            turin_contracts::settings::ServerSettingsError,
        > {
            unimplemented!()
        }
        async fn process_run(
            &self,
            _: crate::source_control_core::host::SourceControlProcessInput,
        ) -> Result<
            crate::source_control_core::host::SourceControlProcessOutput,
            turin_contracts::vcs::VcsError,
        > {
            unimplemented!()
        }
        async fn git_execute(
            &self,
            input: SourceControlGitExecuteInput,
        ) -> Result<SourceControlGitOutput, GitCommandError> {
            if let Some(hook) = &self.before_execute
                && let Some(error) = hook(&input)
            {
                return Err(error);
            }
            self.run(&input.operation, &input.cwd, &input.args).await
        }
        async fn git_resolve_commit(
            &self,
            cwd: &str,
            revision: &str,
        ) -> Result<String, GitCommandError> {
            let args =
                ["rev-parse", "--verify", &format!("{revision}^{{commit}}")].map(str::to_owned);
            Ok(self
                .run("GitVcsDriver.resolveCommit", cwd, &args)
                .await?
                .stdout
                .trim()
                .to_owned())
        }
        async fn git_resolve_primary_remote_name(
            &self,
            cwd: &str,
        ) -> Result<String, GitCommandError> {
            let remotes = self
                .run(
                    "GitVcsDriver.resolvePrimaryRemoteName",
                    cwd,
                    &["remote".to_owned()],
                )
                .await?
                .stdout;
            let names: Vec<&str> = remotes.lines().collect();
            if names.contains(&"origin") {
                return Ok("origin".to_owned());
            }
            names.first().map(|name| (*name).to_owned()).ok_or_else(|| {
                git_error(
                    "GitVcsDriver.resolvePrimaryRemoteName",
                    cwd,
                    "No git remote is configured for this repository.".to_owned(),
                    None,
                )
            })
        }
        async fn git_remotes(
            &self,
            _: &str,
        ) -> Result<turin_contracts::vcs::VcsListRemotesResult, turin_contracts::vcs::VcsError>
        {
            unimplemented!()
        }
        async fn git_read_config_value(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<String>, GitCommandError> {
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
        async fn git_list_local_branch_names(
            &self,
            _: &str,
        ) -> Result<Vec<String>, GitCommandError> {
            unimplemented!()
        }
        async fn git_fetch_remote_branch(
            &self,
            _: &crate::source_control_core::host::SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_fetch_remote_tracking_branch(
            &self,
            _: &crate::source_control_core::host::SourceControlGitRemoteBranch,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_set_branch_upstream(
            &self,
            _: &crate::source_control_core::host::SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            unimplemented!()
        }
        async fn git_switch_ref(
            &self,
            _: &turin_contracts::git::VcsSwitchRefInput,
        ) -> Result<turin_contracts::git::VcsSwitchRefResult, GitCommandError> {
            unimplemented!()
        }
    }

    // ---- fakes for the other ports ---------------------------------------------------------------

    fn capabilities() -> serde_json::Value {
        serde_json::json!({
            "diff": false, "comment": false, "actions": [], "mergeMethods": [], "search": false,
            "review": { "inlineComment": false, "reply": false, "resolve": false, "verdicts": [] },
            "reviewers": { "request": false, "listCandidates": false },
        })
    }

    fn detail_for(workspace: &Path, head_sha: &str) -> PullRequestDetail {
        serde_json::from_value(serde_json::json!({
            "provider": "github",
            "capabilities": capabilities(),
            "viewerPermissions": {
                "actions": [], "comment": false, "resolve": false, "verdicts": [],
                "requestReviewers": false,
            },
            "projectId": "project-1",
            "projectTitle": "Project",
            "workspaceRoot": workspace.to_str().unwrap(),
            "repository": "acme/app",
            "number": 7,
            "title": "Add the feature",
            "body": "Adds the feature.",
            "url": "https://github.com/acme/app/pull/7",
            "author": null,
            "state": "open",
            "isDraft": false,
            "mergeability": "mergeable",
            "additions": 1,
            "deletions": 0,
            "changedFiles": 1,
            "headBranch": "feature",
            "baseBranch": "stack-base",
            "headSha": head_sha,
            "createdAt": "2026-10-01T00:00:00.000Z",
            "updatedAt": "2026-10-01T00:00:00.000Z",
            "mergedAt": null,
            "closedAt": null,
            "reviewers": [],
            "labels": [],
            "checks": [],
            "mergeCapabilities": { "merge": true, "squash": true, "rebase": true },
        }))
        .unwrap()
    }

    fn empty_activity() -> PullRequestActivity {
        serde_json::from_value(serde_json::json!({
            "comments": [], "commentCount": 0, "commentsTruncated": false,
            "reviewThreads": [], "commits": [],
        }))
        .unwrap()
    }

    /// The change as the middle layer of three; the top layer's head is not on any fetched branch.
    fn stack_for(repositories: &Repositories) -> PullRequestStack {
        serde_json::from_value(serde_json::json!({
            "kind": "native",
            "id": "stack",
            "number": 1,
            "url": "https://example.com/stack/1",
            "base": "main",
            "layers": [
                { "number": 6, "title": "Lower", "headSha": repositories.lower_sha,
                  "headBranch": "stack-base", "state": "open" },
                { "number": 7, "title": "Add the feature", "headSha": repositories.head_sha,
                  "headBranch": "feature", "state": "open" },
                { "number": 8, "title": "Upper", "headSha": &repositories.upper_sha[..12],
                  "headBranch": "upper", "state": "open" },
                { "number": 9, "title": "Unpushed", "headSha": "f".repeat(40),
                  "headBranch": "gone", "state": "open" },
            ],
        }))
        .unwrap()
    }

    struct FakePullRequests {
        workspace: PathBuf,
        head: Arc<Mutex<String>>,
        stack: PullRequestStack,
    }

    #[async_trait]
    impl ReviewGuidePullRequests for FakePullRequests {
        async fn detail(&self, _: &PullRequestRef) -> Result<PullRequestDetail, PullRequestError> {
            Ok(detail_for(&self.workspace, &self.head.lock().clone()))
        }
        async fn activity(
            &self,
            _: &PullRequestRef,
        ) -> Result<PullRequestActivity, PullRequestError> {
            Ok(empty_activity())
        }
        async fn stack(
            &self,
            _: &PullRequestRef,
        ) -> Result<Option<PullRequestStack>, PullRequestError> {
            Ok(Some(self.stack.clone()))
        }
        async fn invalidate(&self, _: &PullRequestRef) {}
    }

    struct FakeProjects {
        workspace: PathBuf,
        identity: RepositoryIdentity,
    }

    #[async_trait]
    impl ReviewGuideProjects for FakeProjects {
        async fn get_shell(&self, _: &ProjectId) -> Option<ReviewGuideProject> {
            Some(ReviewGuideProject {
                workspace_root: self.workspace.to_string_lossy().into_owned(),
                repository_identity: Some(self.identity.clone()),
            })
        }
    }

    struct FakeSettings {
        enabled: bool,
    }

    #[async_trait]
    impl ReviewGuideSettings for FakeSettings {
        async fn review_guides_enabled(&self) -> Result<bool, String> {
            Ok(self.enabled)
        }
        async fn for_project(&self, _: &ProjectId) -> Result<ReviewGuideProjectSettings, String> {
            Ok(ReviewGuideProjectSettings {
                model_selection: model_selection(),
                instructions: Some("Be brief.".to_owned()),
            })
        }
    }

    fn model_selection() -> ModelSelection {
        serde_json::from_value(serde_json::json!({ "instanceId": "codex", "model": "gpt-5.4" }))
            .unwrap()
    }

    type GenerateFn = Arc<
        dyn Fn(
                ReviewGuideGenerationInput,
            ) -> BoxFuture<
                'static,
                Result<ReviewGuideGenerationResult, ReviewGuideGenerationError>,
            > + Send
            + Sync,
    >;

    struct FakeGenerator(GenerateFn);

    #[async_trait]
    impl PullRequestReviewGuideGenerator for FakeGenerator {
        async fn generate_review_guide(
            &self,
            input: ReviewGuideGenerationInput,
        ) -> Result<ReviewGuideGenerationResult, ReviewGuideGenerationError> {
            (self.0)(input).await
        }
    }

    fn generator(
        f: impl Fn(ReviewGuideGenerationInput) -> BoxFuture<'static, ReviewGuideGenerationResult>
        + Send
        + Sync
        + 'static,
    ) -> GenerateFn {
        Arc::new(move |input| {
            let future = f(input);
            async move { Ok(future.await) }.boxed()
        })
    }

    fn acme_identity(name: &str) -> RepositoryIdentity {
        serde_json::from_value(serde_json::json!({
            "canonicalKey": format!("github.com/acme/{name}"),
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": format!("https://github.com/acme/{name}.git"),
            },
            "provider": "github",
            "displayName": format!("acme/{name}"),
            "owner": "acme",
            "name": name,
        }))
        .unwrap()
    }

    fn reference(repository: &str) -> PullRequestRef {
        PullRequestRef {
            project_id: ProjectId::new("project-1").unwrap(),
            host: Some(TrimmedNonEmptyString::new("github.com").unwrap()),
            expected_account_id: None,
            allow_stale: None,
            repository: TrimmedNonEmptyString::new(repository).unwrap(),
            number: PositiveInt::new(7).unwrap(),
        }
    }

    struct Setup<'a> {
        repositories: &'a Repositories,
        workspace: PathBuf,
        head: Arc<Mutex<String>>,
        identity: RepositoryIdentity,
        enabled: bool,
        generate: GenerateFn,
        before_execute: Option<GitHook>,
    }

    async fn make_service(setup: Setup<'_>) -> PullRequestReviewGuideService {
        PullRequestReviewGuideService::new(PullRequestReviewGuideDependencies {
            pull_requests: Arc::new(FakePullRequests {
                workspace: setup.workspace.clone(),
                head: setup.head,
                stack: stack_for(setup.repositories),
            }),
            db: Db::open_in_memory().await.unwrap(),
            projects: Arc::new(FakeProjects {
                workspace: setup.workspace,
                identity: setup.identity,
            }),
            settings: Arc::new(FakeSettings {
                enabled: setup.enabled,
            }),
            generator: Arc::new(FakeGenerator(setup.generate)),
            host: Arc::new(RealGitHost {
                before_execute: setup.before_execute,
            }),
            clock: ManualClock::new(START),
            worktrees_dir: setup.repositories.root.path().join("worktrees"),
        })
    }

    /// Runs drive real git, so waits use real time.
    async fn eventually<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
        for _ in 0..1500 {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting");
    }

    /// The run's failure detail, panicking with it if a guide is written instead.
    async fn run_failure(
        service: &PullRequestReviewGuideService,
        input: &PullRequestRef,
    ) -> String {
        eventually(async || {
            let result = service.get(input).await.unwrap();
            match result.run {
                Some(PullRequestReviewGuideRun::Failed { detail, .. }) => Some(detail.to_string()),
                _ => {
                    assert!(result.guide.is_none(), "a guide was written");
                    None
                }
            }
        })
        .await
    }

    /// The stored guide once a run has written it, panicking if the run failed instead.
    async fn stored_guide(
        service: &PullRequestReviewGuideService,
        input: &PullRequestRef,
    ) -> PullRequestReviewGuide {
        eventually(async || {
            let result = service.get(input).await.unwrap();
            if let Some(PullRequestReviewGuideRun::Failed { detail, .. }) = &result.run {
                panic!("run failed: {detail}");
            }
            result.guide
        })
        .await
    }

    // ---- tests -------------------------------------------------------------------------------------

    struct SingleBranchRun {
        repositories: Repositories,
        reported_head: String,
        service: PullRequestReviewGuideService,
        reference: PullRequestRef,
        fetched_refs: Arc<Mutex<Vec<String>>>,
        observed: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    /// A workspace cloned with only `main`, so the change's head is not there until fetched.
    async fn start_single_branch_run(
        repository_name: &str,
        reported_head: impl FnOnce(&Repositories) -> String,
        refuse_commit_fetch: bool,
        prepare: impl AsyncFnOnce(&Repositories, &Path),
        on_branch_fetch: Option<BranchFetchHook>,
    ) -> SingleBranchRun {
        let repositories = make_repositories().await;
        let workspace = repositories.root.path().join("single-branch");
        git(
            repositories.root.path(),
            &[
                "clone",
                "--quiet",
                "--no-local",
                "--single-branch",
                "--branch",
                "main",
                repositories.remote().to_str().unwrap(),
                workspace.to_str().unwrap(),
            ],
        )
        .await;
        prepare(&repositories, &workspace).await;
        let reported_head = reported_head(&repositories);
        let fetched_refs: Arc<Mutex<Vec<String>>> = Arc::default();
        let observed: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let remote = repositories.remote();
        let head_sha = repositories.head_sha.clone();
        let hook: GitHook = {
            let fetched_refs = fetched_refs.clone();
            Arc::new(move |call| {
                let reference = call.args.last()?;
                if call.args.first().map(String::as_str) != Some("fetch") {
                    return None;
                }
                fetched_refs.lock().push(reference.clone());
                if refuse_commit_fetch && *reference == head_sha {
                    return Some(git_error(
                        &call.operation,
                        &call.cwd,
                        "The test remote refuses fetches by commit.".to_owned(),
                        Some(128),
                    ));
                }
                if reference == "refs/heads/feature"
                    && let Some(on_branch_fetch) = &on_branch_fetch
                {
                    on_branch_fetch(&Repositories_ {
                        remote: remote.clone(),
                    });
                }
                None
            })
        };
        let generate = {
            let observed = observed.clone();
            Arc::new(move |input: ReviewGuideGenerationInput| {
                let observed = observed.clone();
                async move {
                    let cwd = PathBuf::from(&input.cwd);
                    let head = git(&cwd, &["rev-parse", "HEAD"]).await;
                    let changed = git(
                        &cwd,
                        &[
                            "diff",
                            "--name-only",
                            &format!("{}...HEAD", input.base_commit),
                        ],
                    )
                    .await;
                    observed.lock().push((input.cwd.clone(), head, changed));
                    Ok(ReviewGuideGenerationResult { stops: vec![] })
                }
                .boxed()
            }) as GenerateFn
        };
        let name = repository_name.to_owned();
        let service = make_service(Setup {
            repositories: &repositories,
            workspace,
            head: Arc::new(Mutex::new(reported_head.clone())),
            identity: acme_identity(&name),
            enabled: true,
            generate,
            before_execute: Some(hook),
        })
        .await;
        let reference = reference(&format!("acme/{repository_name}"));
        service.generate(&reference).await.unwrap();
        SingleBranchRun {
            repositories,
            reported_head,
            service,
            reference,
            fetched_refs,
            observed,
        }
    }

    type BranchFetchHook = Arc<dyn Fn(&Repositories_) + Send + Sync>;

    /// Handle given to a push-during-fetch hook.
    struct Repositories_ {
        remote: PathBuf,
    }

    fn git_blocking(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct WriteCase {
        name: &'static str,
        short_hash: bool,
        prepare: &'static str,
        push_during_fetch: bool,
        refuse_commit_fetch: bool,
        expected_fetches: &'static [&'static str],
    }

    #[tokio::test]
    async fn writes_a_guide_for_the_reported_commit() {
        let cases = [
            WriteCase {
                name: "a short head already fetched",
                short_hash: true,
                prepare: "fetch-feature",
                push_during_fetch: false,
                refuse_commit_fetch: true,
                expected_fetches: &[],
            },
            WriteCase {
                name: "a short head on its branch",
                short_hash: true,
                prepare: "none",
                push_during_fetch: false,
                refuse_commit_fetch: true,
                expected_fetches: &["refs/heads/feature"],
            },
            WriteCase {
                name: "a short head whose branch moves during the fetch",
                short_hash: true,
                prepare: "none",
                push_during_fetch: true,
                refuse_commit_fetch: true,
                expected_fetches: &["refs/heads/feature"],
            },
            WriteCase {
                name: "a full head served by commit",
                short_hash: false,
                prepare: "none",
                push_during_fetch: false,
                refuse_commit_fetch: false,
                expected_fetches: &["<head>"],
            },
            WriteCase {
                name: "a full head on its branch",
                short_hash: false,
                prepare: "none",
                push_during_fetch: false,
                refuse_commit_fetch: true,
                expected_fetches: &["<head>", "refs/heads/feature"],
            },
            WriteCase {
                name: "a full head only on its pull ref",
                short_hash: false,
                prepare: "refs/pull/7/head",
                push_during_fetch: false,
                refuse_commit_fetch: true,
                expected_fetches: &["<head>", "refs/heads/feature", "refs/pull/7/head"],
            },
            WriteCase {
                name: "a full head only on its merge request ref",
                short_hash: false,
                prepare: "refs/merge-requests/7/head",
                push_during_fetch: false,
                refuse_commit_fetch: true,
                expected_fetches: &[
                    "<head>",
                    "refs/heads/feature",
                    "refs/pull/7/head",
                    "refs/merge-requests/7/head",
                ],
            },
        ];
        for case in cases {
            let on_branch_fetch: Option<BranchFetchHook> = case.push_during_fetch.then(|| {
                Arc::new(|repositories: &Repositories_| {
                    git_blocking(&repositories.remote, &["checkout", "--quiet", "feature"]);
                    git_blocking(
                        &repositories.remote,
                        &[
                            "commit",
                            "--quiet",
                            "--allow-empty",
                            "-m",
                            "push during fetch",
                        ],
                    );
                }) as BranchFetchHook
            });
            let short_hash = case.short_hash;
            let prepare_kind = case.prepare;
            let run = start_single_branch_run(
                "fetched-head",
                move |repositories| {
                    if short_hash {
                        repositories.head_sha[..12].to_owned()
                    } else {
                        repositories.head_sha.clone()
                    }
                },
                case.refuse_commit_fetch,
                async move |repositories: &Repositories, workspace: &Path| {
                    let remote = repositories.remote();
                    match prepare_kind {
                        "fetch-feature" => {
                            git(workspace, &["fetch", "--quiet", "origin", "feature"]).await;
                        }
                        "none" => {}
                        reference => {
                            git(&remote, &["update-ref", reference, &repositories.head_sha]).await;
                            git(
                                &remote,
                                &["update-ref", "refs/heads/feature", &repositories.lower_sha],
                            )
                            .await;
                        }
                    }
                },
                on_branch_fetch,
            )
            .await;
            let guide = stored_guide(&run.service, &run.reference).await;
            let head_sha = run.repositories.head_sha.clone();
            assert_eq!(guide.head_sha.as_str(), run.reported_head, "{}", case.name);
            assert_eq!(
                guide.stops[0].files,
                vec![PullRequestReviewGuideFile {
                    path: TrimmedNonEmptyString::new("feature.txt").unwrap(),
                    note: None
                }],
                "{}",
                case.name
            );
            let observed = run.observed.lock().clone();
            assert_eq!(observed.len(), 1, "{}", case.name);
            assert_eq!(observed[0].1, head_sha, "{}", case.name);
            assert_eq!(observed[0].2, "feature.txt", "{}", case.name);
            let head_refs = [
                head_sha.clone(),
                run.reported_head.clone(),
                "refs/heads/feature".to_owned(),
                "refs/pull/7/head".to_owned(),
                "refs/merge-requests/7/head".to_owned(),
            ];
            let fetched: Vec<String> = run
                .fetched_refs
                .lock()
                .iter()
                .filter(|reference| head_refs.contains(reference))
                .cloned()
                .collect();
            let expected: Vec<String> = case
                .expected_fetches
                .iter()
                .map(|reference| {
                    if *reference == "<head>" {
                        head_sha.clone()
                    } else {
                        (*reference).to_owned()
                    }
                })
                .collect();
            assert_eq!(fetched, expected, "{}", case.name);
            assert!(!Path::new(&observed[0].0).exists(), "{}", case.name);
        }
    }

    #[tokio::test]
    async fn refuses_to_write_a_guide_for_an_unusable_head() {
        struct Case {
            name: &'static str,
            reported: fn(&Repositories) -> String,
            shadow: &'static str,
            expected: &'static [&'static str],
        }
        let cases = [
            Case {
                name: "a branch named like the short head",
                reported: |r| r.head_sha[..12].to_owned(),
                shadow: "branch",
                expected: &["a branch or tag rather than the commit"],
            },
            Case {
                name: "a tag named like the short head",
                reported: |r| r.head_sha[..12].to_owned(),
                shadow: "tag",
                expected: &["a branch or tag rather than the commit"],
            },
            Case {
                name: "a head that is not a commit hash",
                reported: |r| format!("--upload-pack=touch {}/injected", r.root.path().display()),
                shadow: "none",
                expected: &["not a commit hash"],
            },
            Case {
                name: "a head no source has",
                reported: |_| "e".repeat(40),
                shadow: "none",
                expected: &[
                    "local objects:",
                    "the commit:",
                    "branch feature:",
                    "refs/pull/7/head:",
                    "refs/merge-requests/7/head:",
                ],
            },
        ];
        for case in cases {
            let shadow = case.shadow;
            let run = start_single_branch_run(
                "refused-head",
                case.reported,
                false,
                async move |repositories: &Repositories, workspace: &Path| {
                    git(workspace, &["fetch", "--quiet", "origin", "feature"]).await;
                    if shadow != "none" {
                        git(workspace, &[shadow, &repositories.head_sha[..12], "HEAD"]).await;
                    }
                },
                None,
            )
            .await;
            let detail = run_failure(&run.service, &run.reference).await;
            for part in case.expected {
                assert!(detail.contains(part), "{}: {detail}", case.name);
            }
            assert!(run.observed.lock().is_empty(), "{}", case.name);
            assert!(
                !run.fetched_refs
                    .lock()
                    .iter()
                    .any(|reference| reference.starts_with('-')),
                "{}",
                case.name
            );
            assert!(
                !run.repositories.root.path().join("injected").exists(),
                "{}",
                case.name
            );
        }
    }

    fn answer(title: &str) -> ReviewGuideGenerationResult {
        ReviewGuideGenerationResult {
            stops: vec![ReviewGuideStopOutput {
                title: title.to_owned(),
                summary: String::new(),
                attention: None,
                excerpts: vec![],
                files: vec![],
                notes: vec![],
                questions: vec![],
            }],
        }
    }

    struct Seen {
        input: ReviewGuideGenerationInput,
        changed: String,
        head: String,
        upper: String,
    }

    #[tokio::test]
    async fn briefs_the_change_against_its_own_base_in_a_checkout_it_then_removes() {
        let repositories = make_repositories().await;
        let seen: Arc<Mutex<Option<Seen>>> = Arc::default();
        let finish = Arc::new(tokio::sync::Notify::new());
        let upper_sha = repositories.upper_sha.clone();
        let generate: GenerateFn = {
            let (seen, finish) = (seen.clone(), finish.clone());
            generator(move |input| {
                let (seen, finish, upper_sha) = (seen.clone(), finish.clone(), upper_sha.clone());
                async move {
                    let cwd = PathBuf::from(&input.cwd);
                    let changed = git(
                        &cwd,
                        &[
                            "diff",
                            "--name-only",
                            &format!("{}...HEAD", input.base_commit),
                        ],
                    )
                    .await;
                    let head = git(&cwd, &["rev-parse", "HEAD"]).await;
                    // Another layer's head is readable from the checkout, to check a seam.
                    let upper = git(&cwd, &["show", &format!("{upper_sha}:upper.txt")]).await;
                    *seen.lock() = Some(Seen {
                        input,
                        changed,
                        head,
                        upper,
                    });
                    finish.notified().await;
                    // Written the way a model gets it wrong: an excerpt past the end of the file, a
                    // note outside what is left of it, and a changed file left off the tour.
                    ReviewGuideGenerationResult {
                        stops: vec![ReviewGuideStopOutput {
                            title: "The feature".to_owned(),
                            summary: "The change.".to_owned(),
                            attention: None,
                            excerpts: vec![ReviewGuideExcerptOutput {
                                path: "feature.txt".to_owned(),
                                side: PullRequestReviewGuideExcerptSide::New,
                                start: 4,
                                end: 40,
                                caption: String::new(),
                            }],
                            files: vec![],
                            notes: vec![
                                ReviewGuideNoteOutput {
                                    path: "feature.txt".to_owned(),
                                    line: 5,
                                    title: "Kept".to_owned(),
                                    body: "Inside.".to_owned(),
                                },
                                ReviewGuideNoteOutput {
                                    path: "feature.txt".to_owned(),
                                    line: 30,
                                    title: "Dropped".to_owned(),
                                    body: "Outside.".to_owned(),
                                },
                            ],
                            questions: vec![],
                        }],
                    }
                }
                .boxed()
            })
        };
        let service = make_service(Setup {
            repositories: &repositories,
            workspace: repositories.workspace.clone(),
            head: Arc::new(Mutex::new(repositories.head_sha.clone())),
            identity: acme_identity("app"),
            enabled: true,
            generate,
            before_execute: None,
        })
        .await;
        let reference = reference("acme/app");

        // Generate returns while the run is still reading the change.
        let started = service.generate(&reference).await.unwrap();
        assert!(matches!(
            started.run,
            Some(PullRequestReviewGuideRun::Running { .. })
        ));
        assert_eq!(started.guide, None);
        assert!(started.started);

        // A run that fails before reaching the model reports why rather than leaving this waiting.
        let observed = eventually(async || {
            let result = service.get(&reference).await.unwrap();
            if let Some(PullRequestReviewGuideRun::Failed { detail, .. }) = &result.run {
                panic!("run failed: {detail}");
            }
            seen.lock().take()
        })
        .await;
        assert_eq!(observed.head, repositories.head_sha);
        assert_eq!(observed.changed, "feature.txt");
        assert_eq!(observed.input.base_branch, "stack-base");
        // The stack comes along, bottom first; a layer whose head could not be fetched is named
        // without one.
        let layer = |number: u64, title: &str, head_sha: Option<&str>| ReviewGuideStackLayer {
            number,
            title: Some(title.to_owned()),
            head_sha: head_sha.map(str::to_owned),
            state: ReviewGuideStackLayerState::Open,
        };
        assert_eq!(
            observed.input.stack,
            Some(vec![
                layer(6, "Lower", Some(&repositories.lower_sha)),
                layer(7, "Add the feature", Some(&repositories.head_sha)),
                layer(8, "Upper", Some(&repositories.upper_sha)),
                layer(9, "Unpushed", None),
            ])
        );
        assert_eq!(observed.upper, "upper layer");
        assert_eq!(observed.input.instructions.as_deref(), Some("Be brief."));

        // A second press while running joins the run instead of starting another.
        let joined = service.generate(&reference).await.unwrap();
        assert_eq!(joined.run, started.run);
        assert!(!joined.started);

        finish.notify_one();
        let guide = stored_guide(&service, &reference).await;
        assert_eq!(guide.head_sha.as_str(), repositories.head_sha);
        let story = &guide.stops[0];
        let t = |value: &str| TrimmedNonEmptyString::new(value).unwrap();
        assert_eq!(
            story.excerpts,
            vec![PullRequestReviewGuideExcerpt {
                path: t("feature.txt"),
                side: PullRequestReviewGuideExcerptSide::New,
                start: PositiveInt::new(4).unwrap(),
                end: PositiveInt::new(10).unwrap(),
                caption: String::new(),
            }]
        );
        let titles: Vec<&str> = story.notes.iter().map(|note| note.title.as_str()).collect();
        assert_eq!(titles, vec!["Kept"]);
        // lower.txt is below the change's base, so it is neither in the change nor on the tour.
        assert_eq!(guide.stops.len(), 1);

        assert!(!Path::new(&observed.input.cwd).exists());
        let worktrees = git(
            &repositories.workspace,
            &["worktree", "list", "--porcelain"],
        )
        .await;
        assert!(!worktrees.contains(&observed.input.cwd), "{worktrees}");
        let refs = git(&repositories.workspace, &["for-each-ref", "refs/t3"]).await;
        assert_eq!(refs, "");
    }

    #[tokio::test]
    async fn refuses_to_write_a_guide_while_the_server_has_review_guides_off() {
        let repositories = make_repositories().await;
        let service = make_service(Setup {
            repositories: &repositories,
            workspace: repositories.workspace.clone(),
            head: Arc::new(Mutex::new(repositories.head_sha.clone())),
            identity: acme_identity("app"),
            enabled: false,
            generate: Arc::new(|_| -> BoxFuture<'static, _> {
                panic!("a guide must not be written while the beta is off")
            }),
            before_execute: None,
        })
        .await;
        let error = service.generate(&reference("acme/app")).await.unwrap_err();
        assert!(
            error.to_string().contains("Review guides are off"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn keeps_a_newer_runs_guide_when_an_older_run_for_the_same_pull_request_finishes_late() {
        let repositories = make_repositories().await;
        let current_head = Arc::new(Mutex::new(repositories.head_sha.clone()));
        // The old run waits on this; the new one answers at once.
        let release_old = Arc::new(tokio::sync::Notify::new());
        let old_started = Arc::new(tokio::sync::Notify::new());
        let generate = {
            let (current_head, release_old, old_started) = (
                current_head.clone(),
                release_old.clone(),
                old_started.clone(),
            );
            let old_head = repositories.head_sha.clone();
            generator(move |_| {
                let head = current_head.lock().clone();
                let (release_old, old_started, old_head) =
                    (release_old.clone(), old_started.clone(), old_head.clone());
                async move {
                    if head == old_head {
                        old_started.notify_one();
                        release_old.notified().await;
                        answer("old head")
                    } else {
                        answer("new head")
                    }
                }
                .boxed()
            })
        };
        let service = make_service(Setup {
            repositories: &repositories,
            workspace: repositories.workspace.clone(),
            head: current_head.clone(),
            identity: acme_identity("app"),
            enabled: true,
            generate,
            before_execute: None,
        })
        .await;
        let reference = reference("acme/app");

        service.generate(&reference).await.unwrap();
        tokio::time::timeout(Duration::from_secs(60), old_started.notified())
            .await
            .unwrap();
        // A push: the host reports the upper layer's commit as the head now, and a new run starts.
        *current_head.lock() = repositories.upper_sha.clone();
        let second = service.generate(&reference).await.unwrap();
        let Some(PullRequestReviewGuideRun::Running { head_sha, .. }) = &second.run else {
            panic!("expected a running run, got {:?}", second.run);
        };
        assert_eq!(head_sha.as_str(), repositories.upper_sha);
        let stored = stored_guide(&service, &reference).await;
        assert_eq!(stored.head_sha.as_str(), repositories.upper_sha);

        // The old run finishes last. Its guide is for a head nobody is looking at any more.
        release_old.notify_one();
        eventually(async || {
            let result = service.get(&reference).await.unwrap();
            result.run.is_none().then_some(())
        })
        .await;
        // Give the late run every chance to write over the newer guide; it must not.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after = service.get(&reference).await.unwrap();
        assert!(after.run.is_none());
        let guide = after.guide.unwrap();
        assert_eq!(guide.head_sha.as_str(), repositories.upper_sha);
        assert_eq!(guide.stops[0].title.as_str(), "new head");
    }

    #[tokio::test]
    async fn refuses_a_pull_request_from_a_repository_the_project_is_not_a_checkout_of() {
        let repositories = make_repositories().await;
        let service = make_service(Setup {
            repositories: &repositories,
            workspace: repositories.workspace.clone(),
            head: Arc::new(Mutex::new(repositories.head_sha.clone())),
            identity: acme_identity("other"),
            enabled: true,
            generate: Arc::new(|_| -> BoxFuture<'static, _> {
                panic!("a guide must not be written from another repository's checkout")
            }),
            before_execute: None,
        })
        .await;
        let error = service.generate(&reference("acme/app")).await.unwrap_err();
        assert!(
            error.to_string().contains("in another repository"),
            "{error}"
        );
    }
}
