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
use turin_contracts::js_trim;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ReviewGuideStopOutput, ThreadTitleGenerationInput, ThreadTitleGenerationResult,
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

    /// `generateReviewGuide` with the output kept as the model wrote it.
    ///
    /// The prompt schema is permissive upstream (`Schema.String`, `Schema.Int`), and the review
    /// guide normalizer drops or clamps what does not fit the change. So empty strings and lines
    /// <= 0 are returned as they are, and only a reply that does not match the schema shape or
    /// the enum values fails.
    pub async fn generate_review_guide_raw(
        &self,
        input: &ReviewGuideGenerationInput,
    ) -> Result<Vec<ReviewGuideStopOutput>, TextGenerationError> {
        let operation = Operation::GenerateReviewGuide;
        let prompt = build_review_guide_prompt(input);
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
        Ok(generated.stops)
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
        // The stops go out as the model wrote them. The review guide normalizer, not the generator,
        // drops and clamps what does not fit the change.
        Ok(ReviewGuideGenerationResult {
            stops: self.generate_review_guide_raw(&input).await?,
        })
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
    stops: Vec<ReviewGuideStopOutput>,
}

#[cfg(test)]
mod review_guide_tests {
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use turin_contracts::model_selection::ModelSelection;
    use turin_contracts::pull_request_review_guide::PullRequestReviewGuideExcerptSide;
    use turin_provider_core::text_generation::{
        ProviderTextGeneration, ReviewGuideGenerationInput,
    };

    use super::*;

    /// Replies with a fixed text and records each request.
    struct FixedReply {
        reply: String,
        requests: Mutex<Vec<Request>>,
    }

    #[async_trait]
    impl Runner for FixedReply {
        async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.reply.clone())
        }
    }

    fn generation(reply: Value) -> RunnerTextGeneration<FixedReply> {
        RunnerTextGeneration::new(
            "Codex",
            FixedReply {
                reply: reply.to_string(),
                requests: Mutex::new(Vec::new()),
            },
        )
    }

    fn input() -> ReviewGuideGenerationInput {
        ReviewGuideGenerationInput {
            cwd: "/tmp/checkout".to_owned(),
            number: 42,
            title: "Add search".to_owned(),
            description: String::new(),
            base_branch: "main".to_owned(),
            base_commit: "abc123".to_owned(),
            settled_review: String::new(),
            instructions: None,
            stack: None,
            model_selection: serde_json::from_value::<ModelSelection>(json!({
                "instanceId": "codex",
                "model": "gpt-5",
            }))
            .expect("selection"),
        }
    }

    fn stop(overrides: Value) -> Value {
        let mut stop = json!({
            "title": "The index moves",
            "summary": "Before and after.",
            "attention": null,
            "excerpts": [],
            "files": [],
            "notes": [],
            "questions": [],
        });
        for (key, value) in overrides.as_object().expect("object") {
            stop[key] = value.clone();
        }
        stop
    }

    #[tokio::test]
    async fn raw_review_guide_keeps_empty_strings_and_out_of_range_lines_for_the_normalizer() {
        let generation = generation(json!({
            "stops": [stop(json!({
                "title": "",
                "attention": { "kind": "risk", "note": "" },
                "excerpts": [{ "path": "", "side": "new", "start": 0, "end": -3, "caption": "" }],
                "files": [{ "path": "", "note": "" }],
                "notes": [{ "path": "", "line": 0, "title": "", "body": "" }],
                "questions": [""],
            }))],
        }));
        let stops = generation
            .generate_review_guide_raw(&input())
            .await
            .expect("permissive stops");
        assert_eq!(stops.len(), 1);
        let stop = &stops[0];
        assert_eq!(stop.title, "");
        assert_eq!(stop.attention.as_ref().expect("attention").note, "");
        assert_eq!(stop.excerpts[0].path, "");
        assert_eq!(stop.excerpts[0].start, 0);
        assert_eq!(stop.excerpts[0].end, -3);
        assert_eq!(stop.files[0].path, "");
        assert_eq!(stop.notes[0].line, 0);
        assert_eq!(stop.notes[0].title, "");
        assert_eq!(stop.questions, vec![String::new()]);
    }

    #[tokio::test]
    async fn raw_review_guide_keeps_negative_note_lines_and_excerpt_ranges() {
        let generation = generation(json!({
            "stops": [stop(json!({
                "excerpts": [{ "path": "a.ts", "side": "old", "start": -1, "end": -1, "caption": "" }],
                "notes": [{ "path": "a.ts", "line": -7, "title": "t", "body": "b" }],
            }))],
        }));
        let stops = generation
            .generate_review_guide_raw(&input())
            .await
            .expect("permissive stops");
        assert_eq!(
            stops[0].excerpts[0].side,
            PullRequestReviewGuideExcerptSide::Old
        );
        assert_eq!(stops[0].excerpts[0].start, -1);
        assert_eq!(stops[0].notes[0].line, -7);
    }

    #[tokio::test]
    async fn raw_review_guide_rejects_an_unknown_attention_kind_and_excerpt_side() {
        for (field, value) in [
            ("attention", json!({ "kind": "urgent", "note": "x" })),
            (
                "excerpts",
                json!([{ "path": "a", "side": "middle", "start": 1, "end": 1, "caption": "" }]),
            ),
        ] {
            let generation = generation(json!({ "stops": [stop(json!({ field: value }))] }));
            let error = generation
                .generate_review_guide_raw(&input())
                .await
                .expect_err(field);
            let TextGenerationError::TextGenerationError { detail, .. } = error;
            assert_eq!(
                detail, "Codex returned invalid structured output.",
                "{field}"
            );
        }
    }

    #[tokio::test]
    async fn raw_review_guide_runs_with_the_fifteen_minute_investigation_budget() {
        let generation = generation(json!({ "stops": [] }));
        generation
            .generate_review_guide_raw(&input())
            .await
            .expect("empty tour");
        let requests = generation.runner.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].investigate,
            Some(Investigate {
                timeout: INVESTIGATION_TIMEOUT,
            })
        );
        assert_eq!(requests[0].cwd, "/tmp/checkout");
    }

    #[tokio::test]
    async fn trait_review_guide_returns_the_model_stops_unchanged() {
        // An empty title and a line of zero are what the model may write; the generator keeps them.
        let generation = generation(json!({
            "stops": [{
                "title": "",
                "summary": "Shape of the change",
                "attention": null,
                "excerpts": [{ "path": "src/a.ts", "side": "new", "start": 0, "end": -1, "caption": "" }],
                "files": [],
                "notes": [],
                "questions": []
            }]
        }));

        let result = ProviderTextGeneration::generate_review_guide(&generation, input())
            .await
            .expect("the generator returns the model's stops");

        assert_eq!(result.stops.len(), 1);
        assert_eq!(result.stops[0].title, "");
        assert_eq!(result.stops[0].excerpts[0].start, 0);
        assert_eq!(result.stops[0].excerpts[0].end, -1);
    }
}
