//! Port of upstream/apps/web/src/components/chat/AssistantCitationCommentEditor.tsx
//!
//! Enter saves, Command/Ctrl+Enter saves and sends, Shift+Enter inserts a
//! line, Escape cancels. A comment past the schema cap stays in the field and
//! the save button asks to shorten it.

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use super::timeline::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant};

fn utf16_len(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

pub fn comment_too_long(comment: &str) -> bool {
    utf16_len(comment) > ASSISTANT_CITATION_MAX_COMMENT_LENGTH
}

/// Grouped decimal, matching `8,000` in the comment-limit message.
pub fn format_comment_limit(limit: usize) -> String {
    let digits = limit.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped.chars().rev().collect()
}

/// What the comment field asks its host to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CitationCommentEvent {
    Submit(String),
    SubmitAndSend(String),
    Cancel,
    Draft(String),
}

/// `AssistantCitationCommentEditor`.
pub struct AssistantCitationCommentEditor {
    input: gpui_kit::Entity<TextareaState>,
}

impl EventEmitter<CitationCommentEvent> for AssistantCitationCommentEditor {}

impl AssistantCitationCommentEditor {
    pub fn new(initial: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Add an optional comment...")
                .default_value(initial.to_owned())
        });
        cx.subscribe(&input, |_this, input, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                cx.emit(CitationCommentEvent::Draft(value));
                cx.notify();
            }
            InputEvent::PressEnter { secondary, shift } => {
                if *shift {
                    return;
                }
                let value = input.read(cx).value().to_string();
                if comment_too_long(&value) {
                    return;
                }
                if *secondary {
                    cx.emit(CitationCommentEvent::SubmitAndSend(value));
                } else {
                    cx.emit(CitationCommentEvent::Submit(value));
                }
            }
            _ => {}
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        Self { input }
    }

    pub fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(CitationCommentEvent::Cancel);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let value = self.value(cx);
        if !comment_too_long(&value) {
            cx.emit(CitationCommentEvent::Submit(value));
        }
    }
}

impl Render for AssistantCitationCommentEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        let value = self.value(cx);
        let too_long = comment_too_long(&value);
        let limit = format_comment_limit(ASSISTANT_CITATION_MAX_COMMENT_LENGTH);
        div()
            .id("citation-comment-editor")
            .w_full()
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(cx);
                }
            }))
            .child(
                div()
                    .id("citation-comment-field")
                    .w_full()
                    .min_h(rems(4.))
                    .max_h(rems(10.))
                    .px(rems(0.25))
                    .py(rems(0.375))
                    .overflow_y_scroll()
                    .text_size(TEXT_SM.size)
                    .child(Textarea::new(&self.input)),
            )
            .when(too_long, |this| {
                this.child(
                    div()
                        .pt(rems(0.25))
                        .text_size(TEXT_XS.size)
                        .text_color(theme.destructive)
                        .child(format!("Comments can contain up to {limit} characters.")),
                )
            })
            .child(
                div()
                    .mt(rems(0.5))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(rems(0.5))
                    .child(
                        Button::new("citation-comment-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .child(
                        Button::new("citation-comment-save")
                            .size(ButtonSize::Xs)
                            .disabled(too_long)
                            .label(if too_long { "Shorten comment" } else { "Save" })
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_comment_limit_with_a_thousands_separator() {
        assert_eq!(format_comment_limit(8000), "8,000");
        assert_eq!(format_comment_limit(999), "999");
        assert_eq!(format_comment_limit(1_000_000), "1,000,000");
    }
}
