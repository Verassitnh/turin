//! Port of upstream/apps/web/src/components/chat/assistantCitationCommentDismissal.ts
//!
//! Decides what a citation comment popover does when it is dismissed: commit
//! the draft, close without saving, or stay open because the draft is too long.

use turin_contracts::base_schemas::js_trim;

use super::timeline::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantCitationCommentDismissal {
    Commit { comment: String },
    Close,
    KeepOpen,
}

fn utf16_len(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

/// `resolveAssistantCitationCommentDismissal`.
pub fn resolve_assistant_citation_comment_dismissal(
    reason: &str,
    draft: Option<&str>,
    saved_comment: Option<&str>,
) -> AssistantCitationCommentDismissal {
    if reason == "escape-key" || draft.is_none() {
        return AssistantCitationCommentDismissal::Close;
    }
    let draft = draft.unwrap_or("");
    let saved = saved_comment.unwrap_or("");
    if js_trim(draft) == js_trim(saved) {
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
    use super::*;

    #[test]
    fn commits_typed_text_when_the_popover_is_dismissed_by_clicking_away() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "outside-press",
                Some("needs a retry"),
                None
            ),
            AssistantCitationCommentDismissal::Commit {
                comment: "needs a retry".into(),
            }
        );
    }

    #[test]
    fn commits_an_edited_comment_when_focus_leaves_the_popover() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "focus-out",
                Some("second thought"),
                Some("first thought"),
            ),
            AssistantCitationCommentDismissal::Commit {
                comment: "second thought".into(),
            }
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
                Some("kept"),
            ),
            AssistantCitationCommentDismissal::Close
        );
        assert_eq!(
            resolve_assistant_citation_comment_dismissal(
                "outside-press",
                Some("kept"),
                Some(" kept "),
            ),
            AssistantCitationCommentDismissal::Close
        );
    }

    #[test]
    fn clears_a_comment_when_the_draft_was_emptied() {
        assert_eq!(
            resolve_assistant_citation_comment_dismissal("trigger-press", Some(""), Some("old")),
            AssistantCitationCommentDismissal::Commit {
                comment: String::new()
            }
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
