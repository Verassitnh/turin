//! Port of upstream/apps/server/src/orchestration-v2/ThreadMessageIntake.ts
//!
//! Claims pending uploads before a message, queued-run edit or question
//! response reaches the orchestrator, and decides afterwards which claimed
//! copies the accepted command still references. A claim is released only
//! when the command is known not to have been accepted (a failure in receipt
//! validation or planning, before `commitCommand`), or when the accepted
//! result does not reference it; an ambiguous failure keeps the copies,
//! because the command may have committed and the provider reads them.
//!
//! `launchThread` is not ported: `ThreadLaunchService` is cut (hooks replace
//! launching into a new worktree).

use super::attachment_claims::{
    AttachmentClaimError, AttachmentClaims, ClaimedAttachments, release_claimed_attachments,
    remap_composer_context_attachments, validate_attachment_limits,
};
use super::attachment_store::resolve_attachment_path;
use super::orchestrator_port::DispatchResult;
use super::thread_management_service::{
    SendInput, SendResult, ThreadManagementError, ThreadManagementService,
};
use super::user_input_attachments::{
    append_user_input_attachment_paths, to_chat_attachment, to_user_input_attachment,
};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Command, OrchestrationV2DomainEvent, OrchestrationV2ServerCommand,
    OrchestrationV2TurnItem,
};
use turin_contracts::provider_policy::UserInputAttachments;

/// The slice of `ThreadManagementService` the intake dispatches through.
#[async_trait]
pub trait IntakeDispatcher: Send + Sync {
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError>;
    async fn send_to_thread(&self, input: SendInput) -> Result<SendResult, ThreadManagementError>;
}

#[async_trait]
impl IntakeDispatcher for ThreadManagementService {
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        // Inherent methods live on the core this service derefs to; naming
        // `ThreadManagementService::dispatch` here would call this trait method again.
        std::ops::Deref::deref(self).dispatch(command).await
    }

    async fn send_to_thread(&self, input: SendInput) -> Result<SendResult, ThreadManagementError> {
        std::ops::Deref::deref(self).send_to_thread(input).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IntakeError {
    #[error(transparent)]
    Claim(#[from] AttachmentClaimError),
    #[error(transparent)]
    Management(#[from] ThreadManagementError),
}

pub struct ThreadMessageIntake {
    claims: AttachmentClaims,
    threads: Arc<dyn IntakeDispatcher>,
}

impl ThreadMessageIntake {
    pub fn new(claims: AttachmentClaims, threads: Arc<dyn IntakeDispatcher>) -> Self {
        Self { claims, threads }
    }

    /// Keeps only the claimed copies the accepted attachments point at.
    async fn release_unused_claims(&self, claimed_paths: &[PathBuf], accepted: &[ChatAttachment]) {
        if claimed_paths.is_empty() {
            return;
        }
        let retained: HashSet<PathBuf> = accepted
            .iter()
            .filter_map(|attachment| {
                resolve_attachment_path(self.claims.attachments_dir(), attachment)
            })
            .collect();
        let unused: Vec<PathBuf> = claimed_paths
            .iter()
            .filter(|path| !retained.contains(*path))
            .cloned()
            .collect();
        release_claimed_attachments(&unused).await;
    }

    pub async fn dispatch_command(
        &self,
        command: OrchestrationV2Command,
    ) -> Result<DispatchResult, IntakeError> {
        match command {
            OrchestrationV2Command::RuntimeRequestRespond {
                command_id,
                thread_id,
                request_id,
                decision,
                answers,
                attachments_by_question_id: Some(incoming),
            } => {
                let respond = |answers, attachments| {
                    OrchestrationV2ServerCommand::Client(
                        OrchestrationV2Command::RuntimeRequestRespond {
                            command_id: command_id.clone(),
                            thread_id: thread_id.clone(),
                            request_id: request_id.clone(),
                            decision,
                            answers: Some(answers),
                            attachments_by_question_id: Some(attachments),
                        },
                    )
                };
                let all: Vec<ChatAttachment> = incoming
                    .values()
                    .flatten()
                    .filter_map(to_chat_attachment)
                    .collect();
                validate_attachment_limits(&all)?;
                // Claims accumulate across questions, so all of preparation
                // shares one rollback boundary: any failure before dispatch
                // removes every new copy.
                let mut claimed_paths: Vec<PathBuf> = Vec::new();
                let prepared = self
                    .prepare_question_attachments(
                        &thread_id,
                        &incoming,
                        &answers,
                        &mut claimed_paths,
                    )
                    .await;
                let (answers, attachments) = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        release_claimed_attachments(&claimed_paths).await;
                        return Err(error.into());
                    }
                };
                match self.threads.dispatch(respond(answers, attachments)).await {
                    Ok(result) => {
                        // A replayed receipt reports the first attempt's
                        // answer, so this attempt's copies go unreferenced and
                        // are released. The resolved turn item proves the
                        // respond was applied; `questionAnswer` is only
                        // recorded when the accepted command carried
                        // attachments, so its absence means the accepted
                        // answer referenced no copies. With no resolved item
                        // at all the outcome is ambiguous and everything stays.
                        let mut answered = false;
                        let mut accepted: Vec<ChatAttachment> = Vec::new();
                        for stored in &result.stored_events {
                            if let OrchestrationV2DomainEvent::TurnItemUpdated {
                                payload:
                                    OrchestrationV2TurnItem::UserInputRequest {
                                        request_id: item_request,
                                        question_answer,
                                        ..
                                    },
                                ..
                            } = &stored.event
                                && *item_request == request_id
                            {
                                answered = true;
                                if let Some(answer) = question_answer {
                                    accepted.extend(
                                        answer
                                            .attachments_by_question_id
                                            .values()
                                            .flatten()
                                            .filter_map(to_chat_attachment),
                                    );
                                }
                            }
                        }
                        if answered {
                            self.release_unused_claims(&claimed_paths, &accepted).await;
                        }
                        Ok(result)
                    }
                    Err(error) => {
                        if error.was_not_accepted() {
                            release_claimed_attachments(&claimed_paths).await;
                        }
                        Err(error.into())
                    }
                }
            }
            OrchestrationV2Command::MessageDispatch {
                notification,
                created_by,
                creation_source,
                scheduled_task_id,
                sender_thread_id,
                command_id,
                thread_id,
                message_id,
                text,
                context,
                attachments,
                title_seed,
                model_selection,
                source_plan_ref,
                restart_continuation_of_run_id,
                usage_limit_continuation_of_run_id,
                manual_continuation_of_run_id,
                usage_limit_recovery_request_id,
                delivery_intent,
                delegated_completion,
                dispatch_mode,
            } => {
                let claimed = self
                    .claims
                    .claim_pending_attachments(thread_id.as_str(), &attachments)
                    .await?;
                let context = context.map(|context| {
                    remap_composer_context_attachments(&context, &attachments, &claimed.attachments)
                });
                let command = OrchestrationV2Command::MessageDispatch {
                    notification,
                    created_by,
                    creation_source,
                    scheduled_task_id,
                    sender_thread_id,
                    command_id,
                    thread_id,
                    message_id,
                    text,
                    context,
                    attachments: claimed.attachments.clone(),
                    title_seed,
                    model_selection,
                    source_plan_ref,
                    restart_continuation_of_run_id,
                    usage_limit_continuation_of_run_id,
                    manual_continuation_of_run_id,
                    usage_limit_recovery_request_id,
                    delivery_intent,
                    delegated_completion,
                    dispatch_mode,
                };
                self.dispatch_claimed(command, &claimed).await
            }
            OrchestrationV2Command::QueuedRunEdit {
                context,
                command_id,
                thread_id,
                run_id,
                text,
                attachments: Some(attachments),
            } => {
                let claimed = self
                    .claims
                    .claim_pending_attachments(thread_id.as_str(), &attachments)
                    .await?;
                let context = context.map(|context| {
                    remap_composer_context_attachments(&context, &attachments, &claimed.attachments)
                });
                let command = OrchestrationV2Command::QueuedRunEdit {
                    context,
                    command_id,
                    thread_id,
                    run_id,
                    text,
                    attachments: Some(claimed.attachments.clone()),
                };
                self.dispatch_claimed(command, &claimed).await
            }
            other => Ok(self
                .threads
                .dispatch(OrchestrationV2ServerCommand::Client(other))
                .await?),
        }
    }

    /// Dispatches a message-bearing command whose pending uploads were just
    /// claimed, then settles those claims.
    async fn dispatch_claimed(
        &self,
        command: OrchestrationV2Command,
        claimed: &ClaimedAttachments,
    ) -> Result<DispatchResult, IntakeError> {
        match self
            .threads
            .dispatch(OrchestrationV2ServerCommand::Client(command))
            .await
        {
            Ok(result) => {
                let accepted: Vec<ChatAttachment> = result
                    .stored_events
                    .iter()
                    .filter_map(|stored| match &stored.event {
                        OrchestrationV2DomainEvent::MessageUpdated { payload, .. } => {
                            Some(payload.attachments.clone())
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect();
                self.release_unused_claims(&claimed.claimed_paths, &accepted)
                    .await;
                Ok(result)
            }
            Err(error) => {
                if error.was_not_accepted() {
                    release_claimed_attachments(&claimed.claimed_paths).await;
                }
                Err(error.into())
            }
        }
    }

    async fn prepare_question_attachments(
        &self,
        thread_id: &turin_contracts::ThreadId,
        incoming: &UserInputAttachments,
        answers: &Option<turin_contracts::provider_policy::ProviderUserInputAnswers>,
        claimed_paths: &mut Vec<PathBuf>,
    ) -> Result<
        (
            turin_contracts::provider_policy::ProviderUserInputAnswers,
            UserInputAttachments,
        ),
        AttachmentClaimError,
    > {
        let mut by_question = UserInputAttachments::new();
        for (question_id, attachments) in incoming {
            let chat: Vec<ChatAttachment> =
                attachments.iter().filter_map(to_chat_attachment).collect();
            let claimed = self
                .claims
                .claim_pending_attachments(thread_id.as_str(), &chat)
                .await?;
            claimed_paths.extend(claimed.claimed_paths);
            by_question.insert(
                question_id.clone(),
                claimed
                    .attachments
                    .iter()
                    .filter_map(to_user_input_attachment)
                    .collect(),
            );
        }
        let answers = append_user_input_attachment_paths(
            &answers.clone().unwrap_or_default(),
            Some(&by_question),
            self.claims.attachments_dir(),
        )
        .await?;
        Ok((answers, by_question))
    }

    /// `sendToThread`: claims the message's pending uploads, sends, then
    /// settles the claims against the stored message.
    pub async fn send_to_thread(&self, input: SendInput) -> Result<SendResult, IntakeError> {
        let claimed = self
            .claims
            .claim_pending_attachments(input.thread_id.as_str(), &input.attachments)
            .await?;
        let mut sent = input;
        sent.attachments = claimed.attachments.clone();
        match self.threads.send_to_thread(sent).await {
            Ok(result) => {
                self.release_unused_claims(&claimed.claimed_paths, &result.message.attachments)
                    .await;
                Ok(result)
            }
            Err(error) => {
                if error.was_not_accepted() {
                    release_claimed_attachments(&claimed.claimed_paths).await;
                }
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
mod tests;
