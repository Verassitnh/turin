//! Port of upstream/apps/web/src/components/diffs/DiffCommentAnnotation.tsx:
//! the inline comment treatment under a diff line (a saved comment, or the
//! draft with its textarea and send button).

use gpui_kit::base::input::{InputEvent, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Subscription, Window};

use super::comment_submit_shortcut::{CommentSubmitShortcutEvent, is_comment_submit_shortcut};
use super::panel::DiffPanel;

/// The draft's textarea and its subscription (Cmd/Ctrl+Enter sends).
pub struct CommentInput {
    pub state: Entity<TextareaState>,
    _subscription: Subscription,
}

impl CommentInput {
    pub fn new(window: &mut Window, cx: &mut Context<DiffPanel>) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Add a comment…")
        });
        let subscription =
            cx.subscribe_in(&state, window, |panel, state, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { secondary, .. } = event {
                    let text = state.read(cx).value().to_string();
                    // `isCommentSubmitShortcut`: Cmd/Ctrl+Enter with text.
                    let shortcut = CommentSubmitShortcutEvent {
                        key: "Enter",
                        meta_key: *secondary,
                        ctrl_key: false,
                    };
                    if is_comment_submit_shortcut(shortcut, &text, false) {
                        panel.try_submit_draft(&text, cx);
                    }
                }
            });
        state.update(cx, |state, cx| state.focus(window, cx));
        Self {
            state,
            _subscription: subscription,
        }
    }
}
