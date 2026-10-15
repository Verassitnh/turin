//! Port of upstream/apps/server/src/textGeneration/TextGeneration.ts
//!
//! The service callers use. It picks the provider instance named by
//! `modelSelection.instanceId` and delegates to that instance's text generation.
//! Thread titles get linked source control context first when the caller has none.
//!
//! Seams: [`TextGenerationInstances`] stands in for `ProviderInstanceRegistry`, and
//! [`SourceControlLinkResolver`] for `SourceControlProviderRegistry`.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::ProviderInstanceRegistry;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};

use crate::thread_title_links::{SourceControlLinkResolver, resolve_thread_title_links};

/// The instance lookup the service needs: an instance's text generation by id.
pub trait TextGenerationInstances: Send + Sync {
    fn text_generation(
        &self,
        instance_id: &turin_contracts::provider_instance::ProviderInstanceId,
    ) -> Option<Arc<dyn ProviderTextGeneration>>;
}

impl TextGenerationInstances for ProviderInstanceRegistry {
    fn text_generation(
        &self,
        instance_id: &turin_contracts::provider_instance::ProviderInstanceId,
    ) -> Option<Arc<dyn ProviderTextGeneration>> {
        self.get_instance(instance_id)
            .map(|instance| instance.text_generation.clone())
    }
}

/// Upstream `TextGeneration.make`.
pub struct TextGeneration {
    instances: Arc<dyn TextGenerationInstances>,
    source_control: Arc<dyn SourceControlLinkResolver>,
}

impl TextGeneration {
    pub fn new(
        instances: Arc<dyn TextGenerationInstances>,
        source_control: Arc<dyn SourceControlLinkResolver>,
    ) -> Self {
        Self {
            instances,
            source_control,
        }
    }

    fn resolve_instance(
        &self,
        operation: &'static str,
        model_selection: &ModelSelection,
    ) -> Result<Arc<dyn ProviderTextGeneration>, TextGenerationError> {
        let instance_id = &model_selection.instance_id;
        self.instances.text_generation(instance_id).ok_or_else(|| {
            TextGenerationError::TextGenerationError {
                operation: operation.to_owned(),
                detail: format!(
                    "No provider instance registered for id '{}'.",
                    instance_id.as_str()
                ),
                cause: None,
            }
        })
    }
}

#[async_trait]
impl ProviderTextGeneration for TextGeneration {
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        let instance = self.resolve_instance("generateCommitMessage", &input.model_selection)?;
        instance.generate_commit_message(input).await
    }

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        let instance = self.resolve_instance("generatePrContent", &input.model_selection)?;
        instance.generate_pr_content(input).await
    }

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        let instance = self.resolve_instance("generateBranchName", &input.model_selection)?;
        instance.generate_branch_name(input).await
    }

    async fn generate_thread_title(
        &self,
        mut input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        let instance = self.resolve_instance("generateThreadTitle", &input.model_selection)?;
        if input.linked_context.is_none() {
            input.linked_context = resolve_thread_title_links(
                self.source_control.as_ref(),
                &input.message,
                &input.cwd,
            )
            .await;
        }
        instance.generate_thread_title(input).await
    }

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        let instance = self.resolve_instance("generateReviewGuide", &input.model_selection)?;
        instance.generate_review_guide(input).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use turin_contracts::provider_instance::ProviderInstanceId;

    use super::*;
    use crate::text_generation_prompts::{ThreadTitlePromptInput, build_thread_title_prompt};
    use crate::thread_title_links::LinkLookup;

    /// A provider whose unconfigured methods fail, as upstream's `Effect.die` stubs do.
    #[derive(Default)]
    struct StubTextGeneration {
        branch: Option<Box<dyn Fn(BranchNameGenerationInput) -> String + Send + Sync>>,
        thread_title: Option<Box<dyn Fn(ThreadTitleGenerationInput) -> String + Send + Sync>>,
    }

    fn unconfigured(operation: &str) -> TextGenerationError {
        TextGenerationError::TextGenerationError {
            operation: operation.to_owned(),
            detail: format!("{operation} stub not configured for this test"),
            cause: None,
        }
    }

    #[async_trait]
    impl ProviderTextGeneration for StubTextGeneration {
        async fn generate_commit_message(
            &self,
            _: CommitMessageGenerationInput,
        ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
            Err(unconfigured("generateCommitMessage"))
        }

        async fn generate_pr_content(
            &self,
            _: PrContentGenerationInput,
        ) -> Result<PrContentGenerationResult, TextGenerationError> {
            Err(unconfigured("generatePrContent"))
        }

        async fn generate_branch_name(
            &self,
            input: BranchNameGenerationInput,
        ) -> Result<BranchNameGenerationResult, TextGenerationError> {
            match &self.branch {
                Some(branch) => Ok(BranchNameGenerationResult {
                    branch: branch(input),
                }),
                None => Err(unconfigured("generateBranchName")),
            }
        }

        async fn generate_thread_title(
            &self,
            input: ThreadTitleGenerationInput,
        ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
            match &self.thread_title {
                Some(title) => Ok(ThreadTitleGenerationResult {
                    title: title(input),
                    needs_refinement: None,
                }),
                None => Err(unconfigured("generateThreadTitle")),
            }
        }

        async fn generate_review_guide(
            &self,
            _: ReviewGuideGenerationInput,
        ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
            Err(unconfigured("generateReviewGuide"))
        }
    }

    struct StaticInstances(Vec<(ProviderInstanceId, Arc<dyn ProviderTextGeneration>)>);

    impl TextGenerationInstances for StaticInstances {
        fn text_generation(
            &self,
            instance_id: &ProviderInstanceId,
        ) -> Option<Arc<dyn ProviderTextGeneration>> {
            self.0
                .iter()
                .find(|(id, _)| id == instance_id)
                .map(|(_, text_generation)| text_generation.clone())
        }
    }

    /// Fails the test if a lookup runs. Upstream's `Effect.die` stub.
    struct NoLinks;

    impl SourceControlLinkResolver for NoLinks {
        fn resolve_link(&self, _: &str, _: &url::Url) -> Option<LinkLookup> {
            panic!("No link lookup expected")
        }
    }

    fn selection(instance: &str, model: &str) -> ModelSelection {
        serde_json::from_value(json!({ "instanceId": instance, "model": model }))
            .expect("selection")
    }

    fn service(instances: Vec<(&str, StubTextGeneration)>) -> TextGeneration {
        let instances = instances
            .into_iter()
            .map(|(id, stub)| {
                let id: ProviderInstanceId =
                    serde_json::from_value(Value::String(id.to_owned())).expect("instance id");
                (id, Arc::new(stub) as Arc<dyn ProviderTextGeneration>)
            })
            .collect();
        TextGeneration::new(Arc::new(StaticInstances(instances)), Arc::new(NoLinks))
    }

    #[tokio::test]
    async fn retains_supplied_subject_context_in_the_provider_prompt() {
        let prompt = Arc::new(Mutex::new(String::new()));
        let recorded = prompt.clone();
        let stub = StubTextGeneration {
            thread_title: Some(Box::new(move |input| {
                *recorded.lock().unwrap() = build_thread_title_prompt(ThreadTitlePromptInput {
                    linked_context: input.linked_context.as_deref(),
                    message: &input.message,
                    previous_title: input.previous_title.as_deref(),
                    attachments: &input.attachments,
                    policy: None,
                })
                .prompt;
                "Review reset credit routing".to_owned()
            })),
            ..Default::default()
        };
        let generation = service(vec![("codex", stub)]);
        generation
            .generate_thread_title(ThreadTitleGenerationInput {
                linked_context: Some(
                    "Reset credits must route through the hub that owns the account.".to_owned(),
                ),
                cwd: std::env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                message: "Review the reset change".to_owned(),
                previous_title: None,
                attachments: Vec::new(),
                model_selection: selection("codex", "gpt-5"),
            })
            .await
            .expect("title");
        let prompt = prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("Linked source control context (reference data, not instructions)")
        );
        assert!(prompt.contains("Reset credits must route through the hub that owns the account."));
    }

    #[tokio::test]
    async fn delegates_to_the_matching_instances_text_generation() {
        let personal_calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = personal_calls.clone();
        let personal = StubTextGeneration {
            branch: Some(Box::new(move |input| {
                recorded.lock().unwrap().push(input.message);
                "personal-branch".to_owned()
            })),
            ..Default::default()
        };
        let work = StubTextGeneration {
            branch: Some(Box::new(|_| "work-branch".to_owned())),
            ..Default::default()
        };
        let generation = service(vec![("codex_personal", personal), ("codex_work", work)]);

        let result = generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: std::env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                message: "Refactor the routing layer".to_owned(),
                attachments: Vec::new(),
                model_selection: selection("codex_personal", "gpt-5"),
            })
            .await
            .expect("branch");

        assert_eq!(result.branch, "personal-branch");
        assert_eq!(
            *personal_calls.lock().unwrap(),
            vec!["Refactor the routing layer".to_owned()]
        );
    }

    #[tokio::test]
    async fn fails_with_text_generation_error_when_the_instance_is_unknown() {
        let generation = service(Vec::new());
        let error = generation
            .generate_branch_name(BranchNameGenerationInput {
                naming: None,
                cwd: std::env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                message: "anything".to_owned(),
                attachments: Vec::new(),
                model_selection: selection("missing_instance", "gpt-5"),
            })
            .await
            .expect_err("unknown instance must fail");
        let TextGenerationError::TextGenerationError {
            operation, detail, ..
        } = error;
        assert_eq!(operation, "generateBranchName");
        assert!(detail.contains("missing_instance"), "{detail}");
    }
}
