//! Port of upstream/apps/web/src/components/QuitHoldOverlay.tsx.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled as _, div, rems};

use crate::desktop::quit_hold::QuitShortcutHint;
use crate::theme::ActiveAppTheme;

const HOLD_HINT_LINGER_MS: u64 = 1200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibleMode {
    Hold,
    DoubleClick,
}

pub struct QuitHoldOverlay {
    visible: Option<VisibleMode>,
    linger_until_ms: u64,
    now_ms: u64,
}

impl QuitHoldOverlay {
    pub fn new() -> Self {
        Self {
            visible: None,
            linger_until_ms: 0,
            now_ms: 0,
        }
    }

    pub fn apply_hint(&mut self, hint: QuitShortcutHint, now_ms: u64, cx: &mut Context<Self>) {
        self.now_ms = now_ms;
        match hint {
            QuitShortcutHint::Down { mode } => {
                self.visible = Some(match mode {
                    turin_contracts::settings::QuitConfirmationMode::DoubleClick => {
                        VisibleMode::DoubleClick
                    }
                    _ => VisibleMode::Hold,
                });
            }
            QuitShortcutHint::Up => {
                if self.visible == Some(VisibleMode::DoubleClick) {
                    self.visible = None;
                } else {
                    self.linger_until_ms = now_ms + HOLD_HINT_LINGER_MS;
                }
            }
        }
        cx.notify();
    }

    pub fn tick(&mut self, now_ms: u64, cx: &mut Context<Self>) {
        self.now_ms = now_ms;
        if self.linger_until_ms != 0 && now_ms >= self.linger_until_ms {
            self.linger_until_ms = 0;
            self.visible = None;
            cx.notify();
        }
    }
}

impl Render for QuitHoldOverlay {
    fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(mode) = self.visible else {
            return div();
        };
        let theme = cx.app_theme();
        let shortcut = "⌘Q";
        let message = match mode {
            VisibleMode::Hold => format!("Hold {shortcut} or press twice to quit"),
            VisibleMode::DoubleClick => format!("Press {shortcut} again to quit"),
        };
        div()
            .absolute()
            .top(rems(5.5))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .px(rems(2.))
                    .py(rems(1.))
                    .rounded_full()
                    .bg(theme.foreground)
                    .text_color(theme.background)
                    .text_size(rems(1.5))
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .child(message),
            )
    }
}
