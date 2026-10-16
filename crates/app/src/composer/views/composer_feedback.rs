//! Port of upstream/apps/web/src/components/chat/ComposerFeedback.tsx
//!
//! The `/feedback` upload as a composer notice. It stacks under warnings and
//! dismisses like one; an upload in flight is an activity and cannot be
//! dismissed, and a sent report offers `Copy ID` for the feedback thread id.

use crate::composer::view_model::{BannerCopyAction, BannerItem, BannerPriority, BannerVariant};
use crate::state::thread_feedback::{
    CodexFeedbackStatus, CodexFeedbackSubmission, codex_feedback_notice,
};
use crate::ui::IconName;

/// `feedbackBannerItem`; `None` when the submission has no notice (an
/// interrupted upload).
pub fn feedback_banner_item(submission: &CodexFeedbackSubmission) -> Option<BannerItem> {
    let notice = codex_feedback_notice(submission)?;
    let uploading = submission.status == CodexFeedbackStatus::Uploading;
    Some(BannerItem {
        id: format!("feedback:{}", submission.details.id.as_str()),
        variant: match submission.status {
            CodexFeedbackStatus::Failed { .. } => BannerVariant::Error,
            CodexFeedbackStatus::Sent { .. } => BannerVariant::Success,
            _ => BannerVariant::Info,
        },
        priority: Some(if uploading {
            BannerPriority::Activity
        } else {
            BannerPriority::Notice
        }),
        icon: IconName::MessageSquare,
        title: notice.title,
        description: notice.description,
        account: None,
        dismissible: !uploading,
        dismiss_label: (!uploading).then(|| "Dismiss feedback notice".to_owned()),
        actions: Vec::new(),
        copy_action: match &submission.status {
            CodexFeedbackStatus::Sent { feedback_id } => Some(BannerCopyAction {
                label: "Copy ID".to_owned(),
                text: feedback_id.clone(),
            }),
            _ => None,
        },
        body: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::MessageId;
    use crate::state::thread_feedback::CodexFeedbackSubmissionDetails;

    fn submission(status: CodexFeedbackStatus) -> CodexFeedbackSubmission {
        CodexFeedbackSubmission {
            details: CodexFeedbackSubmissionDetails {
                id: MessageId::new("feedback-1").unwrap(),
                command: "/feedback".to_owned(),
                created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            },
            status,
        }
    }

    #[test]
    fn an_upload_in_flight_is_an_undismissable_activity() {
        let item = feedback_banner_item(&submission(CodexFeedbackStatus::Uploading)).unwrap();
        assert_eq!(item.id, "feedback:feedback-1");
        assert_eq!(item.variant, BannerVariant::Info);
        assert_eq!(item.priority, Some(BannerPriority::Activity));
        assert!(!item.dismissible);
        assert_eq!(item.dismiss_label, None);
        assert_eq!(item.title, "Sending feedback to OpenAI...");
        assert_eq!(item.copy_action, None);
    }

    #[test]
    fn a_sent_report_offers_the_thread_id() {
        let item = feedback_banner_item(&submission(CodexFeedbackStatus::Sent {
            feedback_id: "thread-9".to_owned(),
        }))
        .unwrap();
        assert_eq!(item.variant, BannerVariant::Success);
        assert_eq!(item.priority, Some(BannerPriority::Notice));
        assert!(item.dismissible);
        assert_eq!(
            item.dismiss_label.as_deref(),
            Some("Dismiss feedback notice")
        );
        assert_eq!(item.description.as_deref(), Some("Thread ID: thread-9"));
        assert_eq!(
            item.copy_action,
            Some(BannerCopyAction {
                label: "Copy ID".to_owned(),
                text: "thread-9".to_owned(),
            })
        );
    }

    #[test]
    fn a_failed_upload_is_an_error_notice() {
        let item = feedback_banner_item(&submission(CodexFeedbackStatus::Failed {
            error_message: "offline".to_owned(),
        }))
        .unwrap();
        assert_eq!(item.variant, BannerVariant::Error);
        assert_eq!(item.description.as_deref(), Some("offline"));
        assert_eq!(item.copy_action, None);
    }

    #[test]
    fn an_interrupted_upload_has_no_notice() {
        assert!(feedback_banner_item(&submission(CodexFeedbackStatus::Interrupted)).is_none());
    }
}
