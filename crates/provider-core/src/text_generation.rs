//! Port of upstream/packages/provider-core/src/server/textGeneration.ts.
//!
//! Every [`crate::driver::ProviderInstance`] carries one of these, even when
//! the driver reports `supportsTextGeneration: false`.

use async_trait::async_trait;
use turin_contracts::{
    chat_attachment::ChatAttachment,
    git::TextGenerationError,
    model_selection::ModelSelection,
    pull_request_review_guide::PullRequestReviewGuideStop,
    settings::BranchNamingOptions,
};

#[derive(Debug, Clone)]
pub struct CommitMessageGenerationInput {
    pub cwd: String,
    pub branch: Option<String>,
    pub staged_summary: String,
    pub staged_patch: String,
    /// When true, the model also returns a semantic branch name for the change.
    pub include_branch: bool,
    pub model_selection: ModelSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMessageGenerationResult {
    pub subject: String,
    pub body: String,
    /// Only present when `include_branch` was set on the input.
    pub branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrContentGenerationInput {
    pub cwd: String,
    pub base_branch: String,
    pub head_branch: String,
    pub commit_summary: String,
    pub diff_summary: String,
    pub diff_patch: String,
    pub change_request_template: Option<String>,
    pub model_selection: ModelSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrContentGenerationResult {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct BranchNameGenerationInput {
    pub naming: Option<BranchNamingOptions>,
    pub cwd: String,
    pub message: String,
    pub attachments: Vec<ChatAttachment>,
    pub model_selection: ModelSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchNameGenerationResult {
    pub branch: String,
}

#[derive(Debug, Clone)]
pub struct ThreadTitleGenerationInput {
    pub linked_context: Option<String>,
    pub cwd: String,
    pub message: String,
    /// Present when replacing an existing title from the current thread history.
    pub previous_title: Option<String>,
    pub attachments: Vec<ChatAttachment>,
    pub model_selection: ModelSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadTitleGenerationResult {
    pub title: String,
    pub needs_refinement: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewGuideStackState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone)]
pub struct ReviewGuideStackLayer {
    pub number: u64,
    pub title: Option<String>,
    /// The layer's head in the checkout, null where it could not be fetched.
    pub head_sha: Option<String>,
    pub state: ReviewGuideStackState,
}

#[derive(Debug, Clone)]
pub struct ReviewGuideGenerationInput {
    /// A disposable checkout of the change's head. The model investigates it read-only.
    pub cwd: String,
    pub number: u64,
    pub title: String,
    /// The author's own description, which states intent the diff cannot.
    pub description: String,
    pub base_branch: String,
    /// The commit the change is measured from, present in `cwd`.
    pub base_commit: String,
    /// What review already settled. The guide leaves these alone.
    pub settled_review: String,
    /// The maintainer's preferences for guides. Empty for none.
    pub instructions: Option<String>,
    /// The stack the change is a layer of, bottom first, when it is in one.
    pub stack: Option<Vec<ReviewGuideStackLayer>>,
    pub model_selection: ModelSelection,
}

/// A guide as the model wrote it, before the server holds it to the change.
///
/// Upstream `Omit<PullRequestReviewGuide, "headSha" | "generatedAt" | "modelSelection">`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewGuideGenerationResult {
    pub stops: Vec<PullRequestReviewGuideStop>,
}

#[async_trait]
pub trait ProviderTextGeneration: Send + Sync {
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError>;

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError>;

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError>;

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError>;

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError>;
}
