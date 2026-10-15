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
