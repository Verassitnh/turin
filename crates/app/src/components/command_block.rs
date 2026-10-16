//! Port of upstream/apps/web/src/components/CommandBlock.tsx
//!
//! A shell command to copy and run elsewhere: `$ command` in a bordered mono
//! box with a copy button that flips to a check for 1.5s.

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    ClipboardItem, Context, FontWeight, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_BASE, TEXT_SM};
use crate::ui::icon::IconName;
use crate::ui::{Button, ButtonSize, ButtonVariant, Tooltip};

/// `useCopyToClipboard({ timeout: 1500 })`.
const COPIED_VISIBLE_MS: u64 = 1500;

pub struct CommandBlock {
    command: SharedString,
    prominent: bool,
    copied: bool,
    reset: Option<Task<()>>,
}

impl CommandBlock {
    pub fn new(command: impl Into<SharedString>) -> Self {
        Self {
            command: command.into(),
            prominent: false,
            copied: false,
            reset: None,
        }
    }

    /// `prominent`: larger padding and `text-base`.
    pub fn prominent(mut self, prominent: bool) -> Self {
        self.prominent = prominent;
        self
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.command.to_string()));
        self.copied = true;
        self.reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPIED_VISIBLE_MS))
                .await;
            this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for CommandBlock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let copied = self.copied;
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.border)
            .bg(with_alpha(theme.muted, 0.5))
            .font_family(theme.fonts.mono.clone())
            .font_weight(FontWeight::NORMAL)
            .map(|this| {
                if self.prominent {
                    this.px_4().py(rems(0.875)).text_size(TEXT_BASE.size)
                } else {
                    this.px_3().py(rems(0.625)).text_size(TEXT_SM.size)
                }
            })
            .child(
                div().min_w_0().truncate().child(
                    div()
                        .flex()
                        .child(div().mr_2().text_color(theme.muted_foreground).child("$"))
                        .child(self.command.clone()),
                ),
            )
            .child(
                Button::new("command-block-copy")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconXs)
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .tooltip(Tooltip::new("Copy command"))
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
    }
}
