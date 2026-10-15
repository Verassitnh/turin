//! Port of upstream/apps/web/src/components/pullRequest/PullRequestMarkdownEditor.tsx,
//! PullRequestCommentForm.tsx and PullRequestComposer.tsx: the box a body is rewritten in, the
//! comment half of the floating composer, and the composer itself.
//!
//! Not ported: the TipTap rich editor of the full upstream editor is out of scope (the upstream
//! file is a plain `Textarea` with a Write / Preview switch, which is what [`MarkdownEditor`]
//! is); the auth write scope check (one process: the snapshot's `can_write` stands in for
//! `AuthSourceControlWriteScope`); toasts for failed posts (commands return immediately and the
//! source reports failures through the snapshot's `action_error`).

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, px,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Toggle, ToggleSize, ToggleVariant};

use super::comment_body::pull_request_markdown;

/// `ToggleGroup variant="segmented"`: `flex w-fit gap-0.5 rounded-lg bg-input/40 p-0.5`.
pub fn segmented_group(cx: &App) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .gap(px(2.))
        .p(px(2.))
        .rounded(RADIUS_LG)
        .bg(with_alpha(cx.app_theme().input, 0.4))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownEditorEvent {
    Save(String),
    Cancel,
}

/// `PullRequestMarkdownEditor`: the box a body is rewritten in. It owns the draft and nothing
/// else: the caller sends the request and says whether it is still in flight ([`set_saving`]),
/// so the same box serves every mutation without knowing which one it is.
///
/// Preview renders through the same component the saved body will be read through, which is the
/// only way to see what a host's markdown will actually become before it is sent.
///
/// [`set_saving`]: MarkdownEditor::set_saving
pub struct MarkdownEditor {
    id: ElementId,
    input: Entity<TextareaState>,
    preview: bool,
    saving: bool,
    /// A description may be cleared, which is how one is removed; a remark may not be emptied.
    allow_empty: bool,
    can_write: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MarkdownEditorEvent> for MarkdownEditor {}

impl MarkdownEditor {
    pub fn new(
        id: impl Into<ElementId>,
        value: &str,
        placeholder: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = value.to_owned();
        let placeholder = placeholder.map(str::to_owned);
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).rows(6).default_value(value);
            if let Some(placeholder) = placeholder {
                state = state.placeholder(placeholder);
            }
            state
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event {
                this.save(cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            id: id.into(),
            input,
            preview: false,
            saving: false,
            allow_empty: false,
            can_write: true,
            _subscriptions: vec![subscription],
        }
    }

    pub fn allow_empty(mut self, allow: bool) -> Self {
        self.allow_empty = allow;
        self
    }

    pub fn set_saving(&mut self, saving: bool, cx: &mut Context<Self>) {
        self.saving = saving;
        cx.notify();
    }

    pub fn set_can_write(&mut self, can_write: bool, cx: &mut Context<Self>) {
        self.can_write = can_write;
        cx.notify();
    }

    pub fn draft(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn save_disabled(&self, cx: &App) -> bool {
        let empty = self.draft(cx).trim().is_empty();
        !self.can_write || self.saving || (empty && !self.allow_empty)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.save_disabled(cx) {
            cx.emit(MarkdownEditorEvent::Save(self.draft(cx)));
        }
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let draft = self.draft(cx);
        let empty = draft.trim().is_empty();
        let save_disabled = self.save_disabled(cx);
        let saving = self.saving;
        let preview = self.preview;
        let id = self.id.clone();
        let toggle = |label: &'static str, pressed: bool, to_preview: bool| {
            Toggle::new(SharedString::from(format!("{id:?}-mode-{label}")))
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Segmented)
                .pressed(pressed)
                .disabled(saving)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.preview = to_preview;
                    cx.notify();
                }))
        };
        div()
            .id(SharedString::from(format!("{:?}-editor", self.id)))
            .flex()
            .flex_col()
            .gap_2()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !this.saving {
                    cx.emit(MarkdownEditorEvent::Cancel);
                    cx.stop_propagation();
                }
            }))
            .child(
                segmented_group(cx)
                    .child(toggle("Write", !preview, false))
                    .child(toggle("Preview", preview, true)),
            )
            .child(if preview {
                div()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .px_3()
                    .py_2()
                    .child(if empty {
                        div()
                            .text_size(TEXT_XS.size)
                            .line_height(TEXT_XS.line_height)
                            .text_color(theme.muted_foreground)
                            .child("Nothing to preview.")
                            .into_any_element()
                    } else {
                        pull_request_markdown(
                            SharedString::from(format!("{:?}-preview", self.id)),
                            draft,
                        )
                        .into_any_element()
                    })
                    .into_any_element()
            } else {
                Textarea::new(&self.input).disabled(saving).into_any_element()
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("{:?}-cancel", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .disabled(saving)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(MarkdownEditorEvent::Cancel);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{:?}-save", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .disabled(save_disabled)
                            .label(if saving { "Saving..." } else { "Save" })
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
