//! Port of upstream/packages/client-runtime/src/state/threadFeedback.ts

use std::collections::HashSet;
use std::future::Future;
use std::sync::LazyLock;

use regex::Regex;

use crate::state::command_result::{CommandFailure, CommandResult};
use crate::state::contracts::{MessageId, ProviderUploadFeedbackResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexFeedbackSubmissionDetails {
    pub id: MessageId,
    pub command: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexFeedbackStatus {
    Uploading,
    Interrupted,
    Sent { feedback_id: String },
    Failed { error_message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexFeedbackSubmission {
    pub details: CodexFeedbackSubmissionDetails,
    pub status: CodexFeedbackStatus,
}

/// `{ reason?: string }`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexFeedbackCommand {
    pub reason: Option<String>,
}

pub fn parse_codex_feedback_command(text: &str) -> Option<CodexFeedbackCommand> {
    static COMMAND: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?iu)^/feedback(?:\s+([\s\S]*))?$").expect("static regex"));
    let trimmed = crate::state::contracts::js_trim(text);
    let captures = COMMAND.captures(trimmed)?;
    let reason = captures
        .get(1)
        .map(|reason| crate::state::contracts::js_trim(reason.as_str()))
        .filter(|reason| !reason.is_empty());
    Some(CodexFeedbackCommand {
        reason: reason.map(str::to_owned),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexFeedbackNotice {
    pub title: String,
    pub description: Option<String>,
}

pub fn codex_feedback_notice(submission: &CodexFeedbackSubmission) -> Option<CodexFeedbackNotice> {
    match &submission.status {
        CodexFeedbackStatus::Interrupted => None,
        CodexFeedbackStatus::Uploading => Some(CodexFeedbackNotice {
            title: "Sending feedback to OpenAI...".to_owned(),
            description: None,
        }),
        CodexFeedbackStatus::Sent { feedback_id } => Some(CodexFeedbackNotice {
            title: "Feedback sent to OpenAI".to_owned(),
            description: Some(format!("Thread ID: {feedback_id}")),
        }),
        CodexFeedbackStatus::Failed { error_message } => Some(CodexFeedbackNotice {
            title: "Could not send feedback to OpenAI".to_owned(),
            description: Some(error_message.clone()),
        }),
    }
}

/// Claims the thread's upload slot. Returns `false` while the thread already has an upload in
/// flight; the caller frees the slot by removing `thread_key` from the set once the upload settles.
pub fn begin_codex_feedback_submission(
    submissions_in_flight: &mut HashSet<String>,
    thread_key: &str,
) -> bool {
    submissions_in_flight.insert(thread_key.to_owned())
}

/// A chat row that exists only on this client, such as a feedback exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalChatMessage {
    pub id: MessageId,
    pub role: LocalChatRole,
    pub text: String,
    pub streaming: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalChatRole {
    User,
    Assistant,
}

pub fn codex_feedback_message(
    submission: &CodexFeedbackSubmission,
    role: LocalChatRole,
) -> LocalChatMessage {
    let text = match role {
        LocalChatRole::User => submission.details.command.clone(),
        LocalChatRole::Assistant => match &submission.status {
            CodexFeedbackStatus::Sent { feedback_id } => {
                format!("Feedback sent to OpenAI.\n\nThread ID: `{feedback_id}`")
            }
            CodexFeedbackStatus::Failed { error_message } => {
                format!("Could not send feedback to OpenAI.\n\n{error_message}")
            }
            _ => "Sending feedback to OpenAI...".to_owned(),
        },
    };
    LocalChatMessage {
        id: match role {
            LocalChatRole::User => submission.details.id.clone(),
            LocalChatRole::Assistant => {
                MessageId::new(format!("{}:feedback", submission.details.id))
                    .expect("non-blank id with suffix")
            }
        },
        role,
        text,
        streaming: false,
        created_at: submission.details.created_at.clone(),
        updated_at: submission.details.created_at.clone(),
    }
}

pub async fn submit_codex_feedback<E: std::fmt::Display, Fut>(
    submission: CodexFeedbackSubmissionDetails,
    clear_draft: impl FnOnce(),
    mut on_update: impl FnMut(CodexFeedbackSubmission),
    upload: impl FnOnce() -> Fut,
) -> CommandResult<ProviderUploadFeedbackResult, E>
where
    Fut: Future<Output = CommandResult<ProviderUploadFeedbackResult, E>>,
{
    let update = |status| CodexFeedbackSubmission {
        details: submission.clone(),
        status,
    };
    on_update(update(CodexFeedbackStatus::Uploading));
    clear_draft();

    let result = upload().await;
    match &result {
        Ok(value) => on_update(update(CodexFeedbackStatus::Sent {
            feedback_id: value.feedback_id.as_str().to_owned(),
        })),
        Err(CommandFailure::Interrupted) => on_update(update(CodexFeedbackStatus::Interrupted)),
        Err(CommandFailure::Failed(error)) => on_update(update(CodexFeedbackStatus::Failed {
            error_message: error.to_string(),
        })),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tokio::sync::oneshot;

    #[test]
    fn allows_only_one_upload_per_thread_until_the_active_upload_releases_its_guard() {
        let mut in_flight = HashSet::new();
        assert!(begin_codex_feedback_submission(
            &mut in_flight,
            "environment:thread"
        ));
        assert!(!begin_codex_feedback_submission(
            &mut in_flight,
            "environment:thread"
        ));
        assert!(begin_codex_feedback_submission(
            &mut in_flight,
            "environment:other-thread"
        ));
        in_flight.remove("environment:thread");
        assert!(begin_codex_feedback_submission(
            &mut in_flight,
            "environment:thread"
        ));
    }

    #[test]
    fn parses_feedback_commands() {
        assert_eq!(
            parse_codex_feedback_command(" /feedback "),
            Some(CodexFeedbackCommand::default())
        );
        assert_eq!(
            parse_codex_feedback_command("/feedback The agent stopped early."),
            Some(CodexFeedbackCommand {
                reason: Some("The agent stopped early.".into())
            })
        );
        assert_eq!(
            parse_codex_feedback_command("/Feedback Retry failed."),
            Some(CodexFeedbackCommand {
                reason: Some("Retry failed.".into())
            })
        );
        assert_eq!(parse_codex_feedback_command("/feedback-status"), None);
        assert_eq!(parse_codex_feedback_command("Please send /feedback"), None);
    }

    fn details(id: &str) -> CodexFeedbackSubmissionDetails {
        CodexFeedbackSubmissionDetails {
            id: MessageId::new(id).unwrap(),
            command: "/feedback The agent stopped early.".into(),
            created_at: "2026-08-23T00:00:00.000Z".into(),
        }
    }

    fn result(feedback_id: &str) -> ProviderUploadFeedbackResult {
        serde_json::from_value(serde_json::json!({ "feedbackId": feedback_id })).unwrap()
    }

    #[tokio::test]
    async fn reports_upload_progress_and_clears_the_draft_before_the_upload_finishes() {
        let draft = RefCell::new(details("m").command);
        let states = RefCell::new(Vec::new());
        let (finish, finished) = oneshot::channel::<()>();
        let submission = submit_codex_feedback::<String, _>(
            details("feedback-message-1"),
            || draft.borrow_mut().clear(),
            |state| states.borrow_mut().push(state),
            || {
                assert_eq!(*draft.borrow(), "");
                async move {
                    let _ = finished.await;
                    Ok(result("codex-thread-1"))
                }
            },
        );
        tokio::pin!(submission);
        // Poll once: the upload is pending.
        assert!(futures::poll!(submission.as_mut()).is_pending());
        assert_eq!(*draft.borrow(), "");
        assert_eq!(states.borrow().len(), 1);
        assert_eq!(states.borrow()[0].status, CodexFeedbackStatus::Uploading);
        assert_eq!(
            codex_feedback_notice(&states.borrow()[0]),
            Some(CodexFeedbackNotice {
                title: "Sending feedback to OpenAI...".into(),
                description: None
            })
        );

        draft.borrow_mut().push_str("Keep this newer message.");
        finish.send(()).unwrap();
        submission.await.unwrap();

        assert_eq!(*draft.borrow(), "Keep this newer message.");
        let last = states.borrow().last().cloned().unwrap();
        assert_eq!(
            last.status,
            CodexFeedbackStatus::Sent {
                feedback_id: "codex-thread-1".into()
            }
        );
        assert!(
            codex_feedback_notice(&last)
                .unwrap()
                .description
                .unwrap()
                .contains("codex-thread-1")
        );
    }

    #[tokio::test]
    async fn records_a_failed_upload_without_losing_its_user_facing_error() {
        let states = RefCell::new(Vec::new());
        let _ = submit_codex_feedback::<String, _>(
            details("feedback-message-1"),
            || {},
            |state| states.borrow_mut().push(state),
            || async { Err(CommandFailure::Failed("Upload rejected.".to_owned())) },
        )
        .await;
        let last = states.borrow().last().cloned().unwrap();
        assert_eq!(
            last.status,
            CodexFeedbackStatus::Failed {
                error_message: "Upload rejected.".into()
            }
        );
        assert_eq!(
            codex_feedback_notice(&last).unwrap().description.as_deref(),
            Some("Upload rejected.")
        );
    }

    #[tokio::test]
    async fn lets_another_feedback_submission_finish_while_the_first_remains_in_flight() {
        let first_states = RefCell::new(Vec::new());
        let second_states = RefCell::new(Vec::new());
        let (release_first, first_released) = oneshot::channel::<()>();
        let first = submit_codex_feedback::<String, _>(
            details("feedback-message-1"),
            || {},
            |state| first_states.borrow_mut().push(state),
            || async move {
                let _ = first_released.await;
                Ok(result("codex-thread-1"))
            },
        );
        let second = async {
            let outcome = submit_codex_feedback::<String, _>(
                details("feedback-message-2"),
                || {},
                |state| second_states.borrow_mut().push(state),
                || async { Ok(result("codex-thread-2")) },
            )
            .await;
            // The first upload has not settled when the second one has.
            assert_eq!(
                first_states.borrow().last().map(|state| &state.status),
                Some(&CodexFeedbackStatus::Uploading)
            );
            release_first.send(()).unwrap();
            outcome
        };
        let (first_outcome, second_outcome) = futures::join!(first, second);
        first_outcome.unwrap();
        second_outcome.unwrap();

        assert_eq!(
            second_states
                .borrow()
                .last()
                .map(|state| state.status.clone()),
            Some(CodexFeedbackStatus::Sent {
                feedback_id: "codex-thread-2".into()
            })
        );
        assert_eq!(
            first_states
                .borrow()
                .last()
                .map(|state| state.status.clone()),
            Some(CodexFeedbackStatus::Sent {
                feedback_id: "codex-thread-1".into()
            })
        );
    }

    #[tokio::test]
    async fn marks_interruptions_without_reporting_them_as_upload_failures() {
        let states = RefCell::new(Vec::new());
        let _ = submit_codex_feedback::<String, _>(
            details("feedback-message-1"),
            || {},
            |state| states.borrow_mut().push(state),
            || async { Err(CommandFailure::Interrupted) },
        )
        .await;
        let last = states.borrow().last().cloned().unwrap();
        assert_eq!(last.status, CodexFeedbackStatus::Interrupted);
        assert_eq!(codex_feedback_notice(&last), None);
    }

    #[test]
    fn builds_local_chat_rows() {
        let sent = CodexFeedbackSubmission {
            details: details("feedback-message-1"),
            status: CodexFeedbackStatus::Sent {
                feedback_id: "abc".into(),
            },
        };
        let user = codex_feedback_message(&sent, LocalChatRole::User);
        assert_eq!(user.id.as_str(), "feedback-message-1");
        assert_eq!(user.text, "/feedback The agent stopped early.");
        let assistant = codex_feedback_message(&sent, LocalChatRole::Assistant);
        assert_eq!(assistant.id.as_str(), "feedback-message-1:feedback");
        assert_eq!(
            assistant.text,
            "Feedback sent to OpenAI.\n\nThread ID: `abc`"
        );
    }
}
