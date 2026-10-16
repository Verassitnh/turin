//! Port of upstream/apps/web/src/components/chat/assistantCitationCommentDismissal.ts
//!
//! What closing a citation's comment popover does with the unsaved draft.

use turin_contracts::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH;
use turin_contracts::base_schemas::js_trim;

use crate::composer::js_string::utf16_len;

/// `AssistantCitationCommentDismissal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantCitationCommentDismissal {
    Commit { comment: String },
    Close,
    KeepOpen,
}

/// Why the popover is closing (Base UI `eventDetails.reason`); `"none"` when
/// the editor settles the draft itself.
pub const DISMISS_REASON_ESCAPE_KEY: &str = "escape-key";

/// `resolveAssistantCitationCommentDismissal`.
pub fn resolve_assistant_citation_comment_dismissal(
    reason: &str,
    draft: Option<&str>,
    saved_comment: Option<&str>,
) -> AssistantCitationCommentDismissal {
    let Some(draft) = draft else {
        return AssistantCitationCommentDismissal::Close;
    };
    if reason == DISMISS_REASON_ESCAPE_KEY {
        return AssistantCitationCommentDismissal::Close;
    }
    if js_trim(draft) == js_trim(saved_comment.unwrap_or("")) {
        return AssistantCitationCommentDismissal::Close;
    }
    if utf16_len(draft) > ASSISTANT_CITATION_MAX_COMMENT_LENGTH {
        return AssistantCitationCommentDismissal::KeepOpen;
    }
    AssistantCitationCommentDismissal::Commit {
        comment: draft.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/web/src/components/chat/assistantCitationCommentDismissal.test.ts

    use super::*;

    fn commit(comment: &str) -> AssistantCitationCommentDismissal {
        AssistantCitationCommentDismissal::Commit {
            comment: comment.to_owned(),
        }
    }

    #[test]
    fn commits_typed_text_when_the_popover_is_dismissed_by_clicking_away() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "outside-press",
                Some("needs a retry"),
                None
            ),
            commit("needs a retry")
        );
    }

    #[test]
    fn commits_an_edited_comment_when_focus_leaves_the_popover() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "focus-out",
                Some("second thought"),
                Some("first thought")
            ),
            commit("second thought")
        );
    }

    #[test]
    fn closes_without_saving_when_nothing_changed() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal("outside-press", None, Some("kept")),
            AssistantCitationCommentDismissal::Close
        );
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "outside-press",
                Some("  kept "),
                Some("kept")
            ),
            AssistantCitationCommentDismissal::Close
        );
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "outside-press",
                Some("kept"),
                Some(" kept ")
            ),
            AssistantCitationCommentDismissal::Close
        );
    }

    #[test]
    fn clears_a_comment_when_the_draft_was_emptied() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal("trigger-press", Some(""), Some("old")),
            commit("")
        );
    }

    #[test]
    fn keeps_escape_as_an_explicit_discard() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal("escape-key", Some("unsaved"), None),
            AssistantCitationCommentDismissal::Close
        );
    }

    #[test]
    fn keeps_the_popover_open_instead_of_dropping_an_over_length_draft() {
        let draft = "x".repeat(ASSISTANT_CITATION_MAX_COMMENT_LENGTH + 1);
        assert_eq!(
            resolve_assistant_citation_comment_dismissal("outside-press", Some(&draft), None),
            AssistantCitationCommentDismissal::KeepOpen
        );
    }
}
