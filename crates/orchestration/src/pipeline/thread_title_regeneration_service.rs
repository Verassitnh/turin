//! Port of upstream/apps/server/src/orchestration-v2/ThreadTitleRegenerationService.ts
//!
//! Generates a thread title in the background and completes the
//! `titleRegeneration` marker through the `ThreadManagementService`
//! chokepoint. A request whose marker was superseded (or cleared by a manual
//! rename) is dropped without generating anything.
//!
//! Upstream's `Effect.retry` / `catchCause` pair becomes an explicit loop:
//! initial titles get two retries with 2 s then 4 s backoff, regenerations
//! none, and a failure after the last attempt completes without a title.
//! Cancellation is dropping the future.

use super::orchestrator_port::{BoxError, RecordField, RecordFilter};
use super::seams::{ProjectDirectory, ServerSettingsSource, instance_enabled_for_project};
use super::thread_management_service::{ThreadManagementError, ThreadManagementService};
use super::thread_title_context::{
    ThreadTitleMessage, ThreadTitleRole, format_thread_title_context,
};
use std::time::Duration;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Command, OrchestrationV2ConversationMessageRole, OrchestrationV2ServerCommand,
};
use turin_contracts::settings::ServerSettings;
use turin_contracts::{CommandId, MessageId, ProjectId, ThreadId, TrimmedNonEmptyString, js_trim};
use turin_provider_core::text_generation::{ProviderTextGeneration, ThreadTitleGenerationInput};

/// Upstream's `Schedule.exponential("2 seconds")` base.
const RETRY_BASE: Duration = Duration::from_secs(2);
/// Upstream's `Effect.retry({ times: 2 })` for initial titles.
const INITIAL_RETRIES: u32 = 2;
/// The fallback title the model returns when it has nothing to say.
const FALLBACK_TITLE: &str = "New thread";

#[derive(Debug, Clone)]
pub enum TitleRegenerationKind {
    /// Title a thread from its first message.
    Initial { message_id: MessageId },
    /// Replace the title from the conversation so far.
    Regenerate,
}

#[derive(Debug, Clone)]
pub struct TitleRegenerationInput {
    pub thread_id: ThreadId,
    /// The marker's `requestId`; a newer request makes this one stale.
    pub request_id: CommandId,
    pub kind: TitleRegenerationKind,
}

#[derive(Debug)]
enum Outcome {
    Stale,
    Complete {
        title: Option<TrimmedNonEmptyString>,
    },
}

pub struct ThreadTitleRegenerationService<'a> {
    threads: &'a ThreadManagementService,
    projects: &'a dyn ProjectDirectory,
    settings: &'a dyn ServerSettingsSource,
    text_generation: &'a dyn ProviderTextGeneration,
    retry_base: Duration,
}

impl<'a> ThreadTitleRegenerationService<'a> {
    pub fn new(
        threads: &'a ThreadManagementService,
        projects: &'a dyn ProjectDirectory,
        settings: &'a dyn ServerSettingsSource,
        text_generation: &'a dyn ProviderTextGeneration,
    ) -> Self {
        Self {
            threads,
            projects,
            settings,
            text_generation,
            retry_base: RETRY_BASE,
        }
    }

    /// Overrides the 2 s retry base (tests).
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    pub async fn execute(
        &self,
        input: TitleRegenerationInput,
    ) -> Result<(), ThreadManagementError> {
        let retries = match input.kind {
            TitleRegenerationKind::Initial { .. } => INITIAL_RETRIES,
            TitleRegenerationKind::Regenerate => 0,
        };
        let mut last_error = None;
        let mut outcome = None;
        for retry in 0..=retries {
            if retry > 0 {
                tokio::time::sleep(self.retry_base * 2u32.pow(retry - 1)).await;
            }
            match self.attempt(&input).await {
                Ok(value) => {
                    outcome = Some(value);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let outcome = match outcome {
            Some(value) => value,
            None => {
                tracing::warn!(
                    thread_id = %input.thread_id,
                    request_id = %input.request_id,
                    error = ?last_error,
                    "Thread title generation failed"
                );
                Outcome::Complete { title: None }
            }
        };
        match outcome {
            Outcome::Stale => Ok(()),
            Outcome::Complete { title } => self
                .threads
                .dispatch(OrchestrationV2ServerCommand::Client(
                    OrchestrationV2Command::ThreadTitleRegenerationComplete {
                        command_id: CommandId::new(format!(
                            "{}:title-complete",
                            input.request_id.as_str()
                        ))
                        .expect("a suffixed command id is non-empty"),
                        thread_id: input.thread_id,
                        request_id: input.request_id,
                        title,
                    },
                ))
                .await
                .map(|_| ()),
        }
    }

    async fn attempt(&self, input: &TitleRegenerationInput) -> Result<Outcome, BoxError> {
        let filter = match &input.kind {
            TitleRegenerationKind::Initial { message_id } => RecordFilter {
                message_ids: Some(vec![message_id.clone()]),
                ..RecordFilter::default()
            },
            TitleRegenerationKind::Regenerate => RecordFilter {
                message_roles: Some(vec![
                    OrchestrationV2ConversationMessageRole::User,
                    OrchestrationV2ConversationMessageRole::Assistant,
                ]),
                ..RecordFilter::default()
            },
        };
        let projection = self
            .threads
            .orchestrator()
            .get_thread_records(&input.thread_id, &[RecordField::Messages], &filter)
            .await?;
        let marker = projection
            .thread
            .title_regeneration
            .as_ref()
            .and_then(Option::as_ref);
        if marker.map(|marker| &marker.request_id) != Some(&input.request_id) {
            return Ok(Outcome::Stale);
        }

        let Some(project) = self.projects.get(&projection.thread.project_id).await? else {
            return Ok(Outcome::Complete { title: None });
        };

        let (message, attachments) = match &input.kind {
            TitleRegenerationKind::Initial { message_id } => projection
                .messages
                .iter()
                .find(|candidate| &candidate.id == message_id && !candidate.streaming)
                .map(|candidate| (candidate.text.clone(), candidate.attachments.clone()))
                .unwrap_or_default(),
            TitleRegenerationKind::Regenerate => {
                let messages: Vec<ThreadTitleMessage<'_>> = projection
                    .messages
                    .iter()
                    .filter(|candidate| !candidate.streaming)
                    .map(|candidate| ThreadTitleMessage {
                        role: title_role(candidate.role),
                        text: &candidate.text,
                        attachments: &candidate.attachments,
                    })
                    .collect();
                let context = format_thread_title_context(&messages);
                (context.message, context.attachments)
            }
        };
        if message.is_empty() && attachments.is_empty() {
            return Ok(Outcome::Complete { title: None });
        }

        let settings = self.settings.get_settings().await?;
        let model_selection = text_generation_selection(&settings, &projection.thread.project_id);
        let previous_title = match input.kind {
            TitleRegenerationKind::Regenerate => Some(projection.thread.title.as_str().to_owned()),
            TitleRegenerationKind::Initial { .. } => None,
        };
        let cwd = projection
            .thread
            .worktree_path
            .as_ref()
            .map_or(project.workspace_root, |path| path.as_str().to_owned());
        let result = self
            .text_generation
            .generate_thread_title(ThreadTitleGenerationInput {
                linked_context: None,
                cwd,
                message,
                previous_title,
                attachments,
                model_selection,
            })
            .await?;

        let generated = js_trim(&result.title);
        let unchanged = generated == FALLBACK_TITLE
            || (matches!(input.kind, TitleRegenerationKind::Regenerate)
                && generated == js_trim(projection.thread.title.as_str()));
        // A blank title is no title: it cannot be dispatched as one.
        let title = if unchanged {
            None
        } else {
            TrimmedNonEmptyString::new(&result.title).ok()
        };
        Ok(Outcome::Complete { title })
    }
}

fn title_role(role: OrchestrationV2ConversationMessageRole) -> ThreadTitleRole {
    match role {
        OrchestrationV2ConversationMessageRole::User => ThreadTitleRole::User,
        OrchestrationV2ConversationMessageRole::Assistant => ThreadTitleRole::Assistant,
        OrchestrationV2ConversationMessageRole::System => ThreadTitleRole::System,
    }
}

/// The text-generation model for a project: its override when the override's
/// instance is enabled for the project, otherwise the environment value.
///
/// Upstream's `resolveProjectSettings` also swaps in a fallback instance when
/// the chosen one is disabled. That fallback is not ported yet, so a disabled
/// selection is passed through.
fn text_generation_selection(settings: &ServerSettings, project_id: &ProjectId) -> ModelSelection {
    settings
        .project_settings_overrides
        .get(project_id)
        .and_then(|overrides| overrides.text_generation_model_selection.clone())
        .filter(|selection| {
            instance_enabled_for_project(settings, project_id, &selection.instance_id)
        })
        .unwrap_or_else(|| settings.text_generation_model_selection.clone())
}

#[cfg(test)]
mod tests;
