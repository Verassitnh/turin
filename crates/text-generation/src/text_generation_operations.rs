//! Port of upstream/packages/provider-core/src/server/textGenerationOperations.ts
//!
//! The operations every provider shares. A provider supplies a [`Runner`] that
//! sends one prompt and returns the reply text; prompts, linked source control
//! context, and output sanitizing live here so providers cannot drift on them.
//!
//! Upstream's `fromRunner` returns a closure typed by each request's Effect
//! schema. Rust has no per-request type parameter on a trait object, so the
//! runner returns the raw reply and [`decode_json_reply`] decodes it into the
//! operation's own output struct.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuideAttention, PullRequestReviewGuideAttentionKind,
    PullRequestReviewGuideExcerpt, PullRequestReviewGuideExcerptSide, PullRequestReviewGuideFile,
    PullRequestReviewGuideNote, PullRequestReviewGuideStop,
};
use turin_contracts::{PositiveInt, TrimmedNonEmptyString, js_trim};
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};

use crate::git_branch_names::{format_generated_branch_name, sanitize_feature_branch_name};
use crate::json_reply::extract_json_object;
use crate::text_generation_policy::TextGenerationPolicy;
use crate::text_generation_prompts::{
    BranchNamePromptInput, CommitMessagePromptInput, PrContentPromptInput, Prompt,
    ThreadTitlePromptInput, build_branch_name_prompt, build_commit_message_prompt,
    build_pr_content_prompt, build_review_guide_prompt, build_thread_title_prompt,
};
use crate::text_generation_utils::{
    sanitize_commit_subject, sanitize_pr_title, sanitize_thread_title,
};

/// Upstream `Operation`: the `ProviderTextGeneration` method names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    GenerateCommitMessage,
    GeneratePrContent,
    GenerateBranchName,
    GenerateThreadTitle,
    GenerateReviewGuide,
}

impl Operation {
    /// The upstream method name, which is also the `operation` on `TextGenerationError`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerateCommitMessage => "generateCommitMessage",
            Self::GeneratePrContent => "generatePrContent",
            Self::GenerateBranchName => "generateBranchName",
            Self::GenerateThreadTitle => "generateThreadTitle",
            Self::GenerateReviewGuide => "generateReviewGuide",
        }
    }
}

/// How long an investigating run may take; a large change needs far longer than a title.
const INVESTIGATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// One prompt for a provider to run.
#[derive(Debug, Clone)]
pub struct Request {
    pub operation: Operation,
    pub cwd: String,
    pub prompt: String,
    /// JSON Schema of the reply. Providers that take a schema (Codex, Claude) pass it through.
    pub output_schema: serde_json::Value,
    pub model_selection: ModelSelection,
    /// The user's attachments, already listed in the prompt. Providers that accept images send them.
    pub attachments: Vec<ChatAttachment>,
    /// Set when the model must investigate `cwd` before answering: it may read files and run
    /// read-only commands there, over as many turns as it needs, and must change nothing. Absent,
    /// the prompt carries everything and the model runs with no tools at all. A runner that cannot
    /// guarantee read-only tools fails rather than answering blind.
    pub investigate: Option<Investigate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Investigate {
    pub timeout: Duration,
}

/// The refusal a runner gives when it has no read-only tool mode to investigate with.
pub fn investigation_unsupported(operation: Operation, provider: &str) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: format!(
            "{provider} cannot investigate a checkout read-only. Choose another review guide model, or source control writer model when none is set."
        ),
        cause: None,
    }
}

/// Sends one request and returns the raw reply text.
///
/// Upstream's `Runner` also decodes the reply. Here decoding is shared, so a runner
/// only transports the prompt. A runner with no read-only tool mode should refuse
/// `request.investigate` with [`investigation_unsupported`].
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError>;
}

/// Decodes the JSON object in a text reply into `T`, which models often wrap in prose.
///
/// `T` plays the role of upstream's `outputSchema`. Missing or mistyped required keys,
/// and a non-object reply, all report the same provider detail.
pub fn decode_json_reply<T: for<'de> Deserialize<'de>>(
    operation: Operation,
    provider: &str,
    text: &str,
) -> Result<T, TextGenerationError> {
    serde_json::from_str::<T>(extract_json_object(text)).map_err(|cause| {
        TextGenerationError::TextGenerationError {
            operation: operation.as_str().to_owned(),
            detail: format!("{provider} returned invalid structured output."),
            cause: Some(serde_json::Value::String(cause.to_string())),
        }
    })
}

fn invalid_output(operation: Operation, provider: &str) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: format!("{provider} returned invalid structured output."),
        cause: None,
    }
}

/// Runs prompts through a [`Runner`] and sanitizes each reply into the trait's result type.
///
/// Upstream `fromRunner(name, run)`. `name` is the provider label in error details, such as
/// "Codex" in "Codex returned invalid structured output."
///
/// The T08 trait's commit and PR inputs have no `policy` field, though upstream's do.
/// The `*_with_policy` methods take it; the [`ProviderTextGeneration`] impl passes `None`.
pub struct RunnerTextGeneration<R: Runner> {
    name: String,
    runner: R,
}

impl<R: Runner> RunnerTextGeneration<R> {
    pub fn new(name: impl Into<String>, runner: R) -> Self {
        Self {
            name: name.into(),
            runner,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// `generateCommitMessage`, with the commit policy upstream reads from `input.policy`.
    pub async fn generate_commit_message_with_policy(
        &self,
        input: CommitMessageGenerationInput,
        policy: Option<&TextGenerationPolicy>,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        let operation = Operation::GenerateCommitMessage;
        let prompt = build_commit_message_prompt(CommitMessagePromptInput {
            branch: input.branch.as_deref(),
            staged_summary: &input.staged_summary,
            staged_patch: &input.staged_patch,
            include_branch: input.include_branch,
            policy,
        });
        let reply = self
            .send(
                operation,
                &input.cwd,
                &input.model_selection,
                prompt,
                Vec::new(),
                None,
            )
            .await?;
        let generated: CommitMessageReply = decode_json_reply(operation, &self.name, &reply)?;
        let branch = match (input.include_branch, generated.branch) {
            (true, None) => return Err(invalid_output(operation, &self.name)),
            (true, Some(branch)) => Some(sanitize_feature_branch_name(&branch)),
            (false, _) => None,
        };
        Ok(CommitMessageGenerationResult {
            subject: sanitize_commit_subject(&generated.subject),
            body: js_trim(&generated.body).to_owned(),
            branch,
        })
    }

    /// `generatePrContent`, with the change request policy upstream reads from `input.policy`.
    pub async fn generate_pr_content_with_policy(
        &self,
        input: PrContentGenerationInput,
        policy: Option<&TextGenerationPolicy>,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        let operation = Operation::GeneratePrContent;
        let prompt = build_pr_content_prompt(PrContentPromptInput {
            base_branch: &input.base_branch,
            head_branch: &input.head_branch,
            commit_summary: &input.commit_summary,
            diff_summary: &input.diff_summary,
            diff_patch: &input.diff_patch,
            change_request_template: input.change_request_template.as_deref(),
            policy,
        });
        let reply = self
            .send(
                operation,
                &input.cwd,
                &input.model_selection,
                prompt,
                Vec::new(),
                None,
            )
            .await?;
        let generated: PrContentReply = decode_json_reply(operation, &self.name, &reply)?;
        Ok(PrContentGenerationResult {
            title: sanitize_pr_title(&generated.title),
            body: js_trim(&generated.body).to_owned(),
        })
    }

    async fn send(
        &self,
        operation: Operation,
        cwd: &str,
        model_selection: &ModelSelection,
        prompt: Prompt,
        attachments: Vec<ChatAttachment>,
        investigate: Option<Investigate>,
    ) -> Result<String, TextGenerationError> {
        self.runner
            .run(Request {
                operation,
                cwd: cwd.to_owned(),
                prompt: prompt.prompt,
                output_schema: prompt.output_schema,
                model_selection: model_selection.clone(),
                attachments,
                investigate,
            })
            .await
    }
}

#[async_trait]
impl<R: Runner> ProviderTextGeneration for RunnerTextGeneration<R> {
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        self.generate_commit_message_with_policy(input, None).await
    }

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        self.generate_pr_content_with_policy(input, None).await
    }

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        let operation = Operation::GenerateBranchName;
        let prompt = build_branch_name_prompt(BranchNamePromptInput {
            naming: input.naming.as_ref(),
            message: &input.message,
            attachments: &input.attachments,
            policy: None,
        });
        let reply = self
            .send(
                operation,
                &input.cwd,
                &input.model_selection,
                prompt,
                input.attachments.clone(),
                None,
            )
            .await?;
        let generated: BranchNameReply = decode_json_reply(operation, &self.name, &reply)?;
        Ok(BranchNameGenerationResult {
            branch: format_generated_branch_name(&generated.branch, input.naming.as_ref()),
        })
    }

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        let operation = Operation::GenerateThreadTitle;
        let prompt = build_thread_title_prompt(ThreadTitlePromptInput {
            linked_context: input.linked_context.as_deref(),
            message: &input.message,
            previous_title: input.previous_title.as_deref(),
            attachments: &input.attachments,
            policy: None,
        });
        let reply = self
            .send(
                operation,
                &input.cwd,
                &input.model_selection,
                prompt,
                input.attachments.clone(),
                None,
            )
            .await?;
        let generated: ThreadTitleReply = decode_json_reply(operation, &self.name, &reply)?;
        Ok(ThreadTitleGenerationResult {
            title: sanitize_thread_title(&generated.title),
            needs_refinement: generated.needs_refinement.then_some(true),
        })
    }

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        let operation = Operation::GenerateReviewGuide;
        let prompt = build_review_guide_prompt(&input);
        let reply = self
            .send(
                operation,
                &input.cwd,
                &input.model_selection,
                prompt,
                Vec::new(),
                Some(Investigate {
                    timeout: INVESTIGATION_TIMEOUT,
                }),
            )
            .await?;
        let generated: ReviewGuideReply = decode_json_reply(operation, &self.name, &reply)?;
        let stops = generated
            .stops
            .into_iter()
            .map(|stop| review_guide_stop(operation, &self.name, stop))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ReviewGuideGenerationResult { stops })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitMessageReply {
    subject: String,
    body: String,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Deserialize)]
struct PrContentReply {
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct BranchNameReply {
    branch: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadTitleReply {
    title: String,
    #[serde(default)]
    needs_refinement: bool,
}

#[derive(Deserialize)]
struct ReviewGuideReply {
    stops: Vec<ReviewGuideStopReply>,
}

#[derive(Deserialize)]
struct ReviewGuideStopReply {
    title: String,
    summary: String,
    attention: Option<ReviewGuideAttentionReply>,
    excerpts: Vec<ReviewGuideExcerptReply>,
    files: Vec<ReviewGuideFileReply>,
    notes: Vec<ReviewGuideNoteReply>,
    questions: Vec<String>,
}

#[derive(Deserialize)]
struct ReviewGuideAttentionReply {
    kind: ReviewGuideAttentionKindReply,
    note: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum ReviewGuideAttentionKindReply {
    Tradeoff,
    Contract,
    Product,
    Risk,
    Accessibility,
}

#[derive(Deserialize)]
struct ReviewGuideExcerptReply {
    path: String,
    side: ReviewGuideSideReply,
    start: i64,
    end: i64,
    caption: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum ReviewGuideSideReply {
    New,
    Old,
}

#[derive(Deserialize)]
struct ReviewGuideFileReply {
    path: String,
    note: String,
}

#[derive(Deserialize)]
struct ReviewGuideNoteReply {
    path: String,
    line: i64,
    title: String,
    body: String,
}

/// Holds a model-written stop to the contract types. Upstream's loose prompt schema
/// accepts any `Int` and empty strings; the contract requires positive lines and
/// non-empty titles, so those cases fail as invalid output.
fn review_guide_stop(
    operation: Operation,
    provider: &str,
    stop: ReviewGuideStopReply,
) -> Result<PullRequestReviewGuideStop, TextGenerationError> {
    let invalid = || invalid_output(operation, provider);
    let text = |value: &str| TrimmedNonEmptyString::new(value).map_err(|_| invalid());
    let line = |value: i64| {
        u64::try_from(value)
            .ok()
            .and_then(PositiveInt::new)
            .ok_or_else(invalid)
    };
    Ok(PullRequestReviewGuideStop {
        title: text(&stop.title)?,
        summary: stop.summary,
        attention: stop
            .attention
            .map(|attention| {
                Ok::<_, TextGenerationError>(PullRequestReviewGuideAttention {
                    kind: match attention.kind {
                        ReviewGuideAttentionKindReply::Tradeoff => {
                            PullRequestReviewGuideAttentionKind::Tradeoff
                        }
                        ReviewGuideAttentionKindReply::Contract => {
                            PullRequestReviewGuideAttentionKind::Contract
                        }
                        ReviewGuideAttentionKindReply::Product => {
                            PullRequestReviewGuideAttentionKind::Product
                        }
                        ReviewGuideAttentionKindReply::Risk => {
                            PullRequestReviewGuideAttentionKind::Risk
                        }
                        ReviewGuideAttentionKindReply::Accessibility => {
                            PullRequestReviewGuideAttentionKind::Accessibility
                        }
                    },
                    note: text(&attention.note)?,
                })
            })
            .transpose()?,
        excerpts: stop
            .excerpts
            .into_iter()
            .map(|excerpt| {
                Ok(PullRequestReviewGuideExcerpt {
                    path: text(&excerpt.path)?,
                    side: match excerpt.side {
                        ReviewGuideSideReply::New => PullRequestReviewGuideExcerptSide::New,
                        ReviewGuideSideReply::Old => PullRequestReviewGuideExcerptSide::Old,
                    },
                    start: line(excerpt.start)?,
                    end: line(excerpt.end)?,
                    caption: excerpt.caption,
                })
            })
            .collect::<Result<_, TextGenerationError>>()?,
        files: stop
            .files
            .into_iter()
            .map(|file| {
                Ok(PullRequestReviewGuideFile {
                    path: text(&file.path)?,
                    note: Some(file.note),
                })
            })
            .collect::<Result<_, TextGenerationError>>()?,
        notes: stop
            .notes
            .into_iter()
            .map(|note| {
                Ok(PullRequestReviewGuideNote {
                    path: text(&note.path)?,
                    line: line(note.line)?,
                    title: text(&note.title)?,
                    body: note.body,
                })
            })
            .collect::<Result<_, TextGenerationError>>()?,
        questions: stop
            .questions
            .iter()
            .map(|question| text(question))
            .collect::<Result<_, _>>()?,
    })
}
