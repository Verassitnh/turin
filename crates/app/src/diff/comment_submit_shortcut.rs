//! Port of upstream/apps/web/src/components/diffs/commentSubmitShortcut.ts (+ test).

/// The modifier/key state of a key event.
#[derive(Clone, Copy, Debug)]
pub struct CommentSubmitShortcutEvent<'a> {
    pub key: &'a str,
    pub meta_key: bool,
    pub ctrl_key: bool,
}

/// Shared guard for inline comment composers that submit on Command/Ctrl+Enter.
pub fn is_comment_submit_shortcut(
    event: CommentSubmitShortcutEvent<'_>,
    value: &str,
    pending: bool,
) -> bool {
    !pending
        && (event.meta_key || event.ctrl_key)
        && event.key == "Enter"
        && !value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTER_META: CommentSubmitShortcutEvent = CommentSubmitShortcutEvent {
        key: "Enter",
        meta_key: true,
        ctrl_key: false,
    };

    #[test]
    fn accepts_command_or_ctrl_enter_only_while_an_eligible_comment_is_idle() {
        assert!(is_comment_submit_shortcut(ENTER_META, "Looks good", false));
        assert!(is_comment_submit_shortcut(
            CommentSubmitShortcutEvent {
                key: "Enter",
                meta_key: false,
                ctrl_key: true
            },
            "Looks good",
            false
        ));
        assert!(!is_comment_submit_shortcut(ENTER_META, "Looks good", true));
    }

    #[test]
    fn rejects_empty_comments_and_unrelated_key_presses() {
        assert!(!is_comment_submit_shortcut(ENTER_META, "   ", false));
        assert!(!is_comment_submit_shortcut(
            CommentSubmitShortcutEvent {
                key: "K",
                meta_key: true,
                ctrl_key: false
            },
            "Looks good",
            false
        ));
    }
}
