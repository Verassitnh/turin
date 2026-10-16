//! The `GitHubPullRequestApiError` family of
//! upstream/packages/source-control-github/src/server/GitHubPullRequestApi.ts.

use crate::github::api::GitHubApiError;
use crate::github::stack_actions::GitHubStackActionError;
use crate::source_control_core::Cause;
use crate::source_control_core::rate_limit::SourceControlRateLimitPausedError;

/// Names the read that produced unusable output, so a failure reports the call it came from
/// rather than borrowing another operation's message.
#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub returned an unreadable {operation} response.")]
pub struct GitHubPullRequestReadError {
    pub cwd: String,
    pub operation: String,
    pub cause: Cause,
}

/// Not a decode failure: GitHub answered, the account it answered for just has no login.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("GitHub returned no login for the authenticated account.")]
pub struct GitHubViewerLoginUnavailableError {
    pub cwd: String,
}

/// Not a decode failure: GitHub answered, but the pull request carried no update time.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Pull request {repository}#{number} reported no update time.")]
pub struct GitHubPullRequestUpdatedAtUnavailableError {
    pub cwd: String,
    pub repository: String,
    pub number: u64,
}

/// Not a decode failure: the reader asked to carry on from a cursor this walk never handed out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The diff cursor was not one this pull request handed out.")]
pub struct GitHubDiffCursorError {
    pub cwd: String,
}

/// Not a decode failure: the reader named a commit that is not a sha this repository could hold.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The named commit was not a commit sha.")]
pub struct GitHubDiffCommitError {
    pub cwd: String,
}

/// The revisions read successfully, but cannot name both sides this file needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubDiffRevisionsUnavailableError {
    pub cwd: String,
    pub number: u64,
    pub commit: Option<String>,
}

impl std::fmt::Display for GitHubDiffRevisionsUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.commit {
            None => write!(
                f,
                "Pull request #{} reported no usable base and head revisions.",
                self.number
            ),
            Some(commit) => write!(
                f,
                "Commit {commit} reported no usable revisions for this file."
            ),
        }
    }
}
impl std::error::Error for GitHubDiffRevisionsUnavailableError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubDiffFileContentsUnavailableReason {
    Oversized,
    Binary,
}

/// A blob exists, but expanding it would be unsafe or would not produce text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubDiffFileContentsUnavailableError {
    pub cwd: String,
    pub path: String,
    pub reason: GitHubDiffFileContentsUnavailableReason,
}

impl std::fmt::Display for GitHubDiffFileContentsUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            GitHubDiffFileContentsUnavailableReason::Oversized => write!(
                f,
                "The diff file '{}' exceeds the 1 MB expansion limit.",
                self.path
            ),
            GitHubDiffFileContentsUnavailableReason::Binary => {
                write!(f, "The diff file '{}' is binary.", self.path)
            }
        }
    }
}
impl std::error::Error for GitHubDiffFileContentsUnavailableError {}

/// Not a decode failure: a repository was named that cannot go into a search or into a GraphQL
/// document as itself. Every qualifier and every alias below is composed from `owner/name`, so a
/// name that is not one is refused here rather than escaped into something GitHub might read as a
/// qualifier of its own.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("A repository was named that GitHub cannot address.")]
pub struct GitHubRepositorySelectorError {
    pub cwd: String,
    pub operation: String,
}

/// Not a decode failure: the reader named a subject this pull request never handed out.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The named subject did not belong to the named pull request.")]
pub struct GitHubSubjectScopeError {
    pub cwd: String,
    pub operation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubWorkflowApprovalRefusedReason {
    HeadListTruncated,
    HeadNotUnique,
    RunListTruncated,
}

/// GitHub answered successfully, but approving every returned workflow would be unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubWorkflowApprovalRefusedError {
    pub cwd: String,
    pub number: u64,
    pub reason: GitHubWorkflowApprovalRefusedReason,
    pub observed_count: u64,
    pub limit: u64,
}

impl std::fmt::Display for GitHubWorkflowApprovalRefusedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            GitHubWorkflowApprovalRefusedReason::HeadListTruncated => write!(
                f,
                "GitHub returned more than {} pull requests for this head branch.",
                self.limit
            ),
            GitHubWorkflowApprovalRefusedReason::HeadNotUnique => write!(
                f,
                "The head revision matched {} pull requests instead of uniquely matching #{}.",
                self.observed_count, self.number
            ),
            GitHubWorkflowApprovalRefusedReason::RunListTruncated => write!(
                f,
                "GitHub returned more than {} workflow runs awaiting approval.",
                self.limit
            ),
        }
    }
}
impl std::error::Error for GitHubWorkflowApprovalRefusedError {}

/// GitHub omitted the immutable head identity needed to scope an approval safely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("GitHub did not report a complete head revision for #{number}.")]
pub struct GitHubWorkflowApprovalHeadUnavailableError {
    pub cwd: String,
    pub number: u64,
}

/// The pull request moved after its approval candidates were read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The head revision of #{number} changed before its workflows could be approved.")]
pub struct GitHubWorkflowApprovalHeadChangedError {
    pub cwd: String,
    pub number: u64,
}

/// Everything a `GitHubPullRequestApi` method can fail with. The rate-limit pause is carried by
/// [`GitHubApiError::Paused`] (upstream lists `SourceControlRateLimitPausedError` beside
/// `GitHubApiError`; here the transport error already contains it).
#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubPullRequestApiError {
    #[error(transparent)]
    Stack(#[from] GitHubStackActionError),
    #[error(transparent)]
    Api(#[from] GitHubApiError),
    #[error(transparent)]
    Read(#[from] GitHubPullRequestReadError),
    #[error(transparent)]
    DiffCursor(#[from] GitHubDiffCursorError),
    #[error(transparent)]
    DiffCommit(#[from] GitHubDiffCommitError),
    #[error(transparent)]
    DiffRevisionsUnavailable(#[from] GitHubDiffRevisionsUnavailableError),
    #[error(transparent)]
    DiffFileContentsUnavailable(#[from] GitHubDiffFileContentsUnavailableError),
    #[error(transparent)]
    RepositorySelector(#[from] GitHubRepositorySelectorError),
    #[error(transparent)]
    SubjectScope(#[from] GitHubSubjectScopeError),
    #[error(transparent)]
    WorkflowApprovalRefused(#[from] GitHubWorkflowApprovalRefusedError),
    #[error(transparent)]
    WorkflowApprovalHeadUnavailable(#[from] GitHubWorkflowApprovalHeadUnavailableError),
    #[error(transparent)]
    WorkflowApprovalHeadChanged(#[from] GitHubWorkflowApprovalHeadChangedError),
    #[error(transparent)]
    ViewerLoginUnavailable(#[from] GitHubViewerLoginUnavailableError),
    #[error(transparent)]
    UpdatedAtUnavailable(#[from] GitHubPullRequestUpdatedAtUnavailableError),
}

impl From<SourceControlRateLimitPausedError> for GitHubPullRequestApiError {
    fn from(error: SourceControlRateLimitPausedError) -> Self {
        Self::Api(GitHubApiError::Paused(error))
    }
}

impl GitHubPullRequestApiError {
    /// The transport error, when this failure is one.
    pub fn api(&self) -> Option<&GitHubApiError> {
        match self {
            Self::Api(error) => Some(error),
            _ => None,
        }
    }

    /// `_tag === "SourceControlRateLimitPausedError"`.
    pub fn is_rate_limit_paused(&self) -> bool {
        matches!(self, Self::Api(GitHubApiError::Paused(_)))
    }

    /// `_tag === "GitHubApiNotFoundError"`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Api(GitHubApiError::NotFound(_)))
    }
}
