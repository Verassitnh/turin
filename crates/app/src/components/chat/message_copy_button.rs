//! Port of upstream/apps/web/src/components/chat/MessageCopyButton.tsx.
//!
//! Copies `text`, then shows a check in `text-primary` for
//! `ANCHORED_COPY_TOAST_TIMEOUT_MS` while the button is disabled. The
//! anchored "Copied" toast is not ported (no anchored toasts yet); the
//! check carries the confirmation. Also used by code blocks and tables,
//! which upstream wires by hand with the same Copy → Check morph (1200ms).

use std::time::Duration;

use gpui_kit::{
    App, ClipboardItem, ElementId, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Window, rems,
};

use crate::theme::ActiveAppTheme;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `ANCHORED_COPY_TOAST_TIMEOUT_MS`.
pub const ANCHORED_COPY_TOAST_TIMEOUT_MS: u64 = 1000;
/// Code blocks and tables reset their check after 1200ms.
pub const CODE_COPY_TIMEOUT_MS: u64 = 1200;

#[derive(Default)]
struct CopyState {
    copied: bool,
    generation: u64,
}

#[derive(IntoElement)]
pub struct MessageCopyButton {
    id: ElementId,
    text: SharedString,
    label: SharedString,
    icon: IconName,
    size: ButtonSize,
    variant: ButtonVariant,
    timeout_ms: u64,
}

impl MessageCopyButton {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            label: "Copy message".into(),
            icon: IconName::Copy,
            size: ButtonSize::Xs,
            variant: ButtonVariant::Outline,
            timeout_ms: ANCHORED_COPY_TOAST_TIMEOUT_MS,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// `variant="ghost"` maps to `ghost-muted` upstream.
    pub fn ghost(mut self) -> Self {
        self.variant = ButtonVariant::GhostMuted;
        self
    }

    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

impl RenderOnce for MessageCopyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "copy-state".into()),
            cx,
            |_, _| CopyState::default(),
        );
        let copied = state.read(cx).copied;
        let primary = cx.app_theme().primary;
        let text = self.text.clone();
        let timeout = Duration::from_millis(self.timeout_ms);
        let icon = if copied { IconName::Check } else { self.icon };
        let mut glyph = Icon::new(icon).size(rems(0.75));
        if copied {
            glyph = glyph.color(primary);
        }
        Button::new(self.id)
            .variant(self.variant)
            .size(self.size)
            .disabled(copied)
            .tooltip(Tooltip::new(self.label))
            .child(glyph)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                let generation = state.update(cx, |state, cx| {
                    state.copied = true;
                    state.generation += 1;
                    cx.notify();
                    state.generation
                });
                let state = state.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(timeout).await;
                    state.update(cx, |state, cx| {
                        if state.generation == generation {
                            state.copied = false;
                            cx.notify();
                        }
                    });
                })
                .detach();
            })
    }
}
