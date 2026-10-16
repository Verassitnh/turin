//! Port of upstream/apps/web/src/components/chat/AssistantCitationChip.tsx
//!
//! A quoted-assistant chip. In a message it opens the quote and a jump back to
//! the source. In the composer it also edits the comment, and keeps an
//! over-length or rejected draft when the source text disappears.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use super::assistant_citation_comment_dismissal::{
    AssistantCitationCommentDismissal, resolve_assistant_citation_comment_dismissal,
};
use super::assistant_citation_comment_editor::comment_too_long;
use super::timeline::assistant_citations::{
    AssistantCitation, assistant_citation_label, serialize_assistant_citation,
};
use crate::theme::color::{MixSpace, color, color_mix, with_alpha};
use crate::theme::tokens::{RADIUS_LG, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// Citation accent (`ContextChip` kind `citation`).
pub fn citation_accent() -> gpui_kit::Hsla {
    color("oklch(0.62 0.16 259)")
}

/// Where the comment popover sits. A live source anchors under the selection;
/// a source that disappeared falls back to the composer trigger (`side="top"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CitationPopupSide {
    Bottom,
    Top,
}

/// The composer chip's comment session. `AssistantCitationChip.test.tsx`.
#[derive(Clone, Debug)]
pub struct CitationCommentSession {
    open: bool,
    draft: Option<String>,
    anchored: bool,
    save_calls: Vec<String>,
}

impl CitationCommentSession {
    pub fn open(saved: Option<&str>) -> Self {
        Self {
            open: true,
            draft: saved.map(str::to_owned),
            anchored: true,
            save_calls: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn value(&self) -> &str {
        self.draft.as_deref().unwrap_or("")
    }

    pub fn anchored(&self) -> bool {
        self.anchored
    }

    pub fn popup_side(&self) -> CitationPopupSide {
        if self.anchored {
            CitationPopupSide::Bottom
        } else {
            CitationPopupSide::Top
        }
    }

    pub fn invalid(&self) -> bool {
        comment_too_long(self.value())
    }

    pub fn save_calls(&self) -> &[String] {
        &self.save_calls
    }

    pub fn type_comment(&mut self, value: &str) {
        self.draft = Some(value.to_owned());
    }

    fn settle(&mut self, reason: &str, saved: Option<&str>, save_ok: bool) -> bool {
        match resolve_assistant_citation_comment_dismissal(reason, self.draft.as_deref(), saved) {
            AssistantCitationCommentDismissal::Commit { comment } => {
                self.save_calls.push(comment);
                save_ok
            }
            AssistantCitationCommentDismissal::Close => true,
            AssistantCitationCommentDismissal::KeepOpen => false,
        }
    }

    /// The source range went away (`onSourceUnavailable`).
    pub fn source_unavailable(&mut self, saved: Option<&str>, save_ok: bool) {
        if self.settle("none", saved, save_ok) {
            self.open = false;
        } else {
            self.anchored = false;
        }
    }

    pub fn save(&mut self, saved: Option<&str>, save_ok: bool) {
        let _ = saved;
        if comment_too_long(self.value()) {
            return;
        }
        self.save_calls.push(self.value().to_owned());
        if save_ok {
            self.open = false;
        }
    }

    pub fn cancel(&mut self) {
        self.open = false;
    }
}

/// Replaces one citation marker in `prompt` with the same citation plus `comment`.
pub fn replace_citation_comment(
    prompt: &str,
    citation: &AssistantCitation,
    comment: &str,
) -> String {
    let marker = serialize_assistant_citation(citation);
    let mut next = citation.clone();
    next.comment = if comment.is_empty() {
        None
    } else {
        Some(comment.to_owned())
    };
    prompt.replacen(&marker, &serialize_assistant_citation(&next), 1)
}

pub(crate) type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `AssistantCitationChip`.
#[derive(IntoElement)]
pub struct AssistantCitationChip {
    citation: AssistantCitation,
    composer: bool,
    comment_open: bool,
    popup_side: CitationPopupSide,
    on_open_quote: Option<ClickHandler>,
    on_go_to_source: Option<ClickHandler>,
    on_edit: Option<ClickHandler>,
    editor: Option<AnyElement>,
}

impl AssistantCitationChip {
    pub fn new(citation: AssistantCitation) -> Self {
        Self {
            citation,
            composer: false,
            comment_open: false,
            popup_side: CitationPopupSide::Bottom,
            on_open_quote: None,
            on_go_to_source: None,
            on_edit: None,
            editor: None,
        }
    }

    pub fn composer(mut self, composer: bool) -> Self {
        self.composer = composer;
        self
    }

    pub fn comment_open(mut self, open: bool, side: CitationPopupSide) -> Self {
        self.comment_open = open;
        self.popup_side = side;
        self
    }

    pub fn on_open_quote(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_quote = Some(Rc::new(handler));
        self
    }

    pub fn on_go_to_source(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_go_to_source = Some(Rc::new(handler));
        self
    }

    pub fn on_edit(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_edit = Some(Rc::new(handler));
        self
    }

    pub fn editor(mut self, editor: impl IntoElement) -> Self {
        self.editor = Some(editor.into_any_element());
        self
    }
}

/// `ContextChip` sizes are ems of `text-[0.86em]`, so they track the parent font.
fn chip_em(window: &Window) -> f32 {
    gpui_kit::Rems::from_pixels(
        window.text_style().font_size.to_pixels(window.rem_size()),
        window,
    )
    .0 * 0.86
}

fn chip_shell(theme: &AppTheme, label: SharedString, window: &Window) -> gpui_kit::Div {
    let accent = citation_accent();
    let foreground: gpui_kit::Hsla =
        color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into();
    let border: gpui_kit::Hsla = color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into();
    let em = chip_em(window);
    div()
        .flex()
        .items_center()
        .gap(rems(em * 0.33))
        .h(rems(em * 1.41))
        .max_w(rems(18.))
        .px(rems(em * 0.5))
        .rounded(rems(em * 0.5))
        .border_1()
        .border_color(border)
        .bg(with_alpha(accent, 0.11))
        .text_size(rems(em))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(foreground)
        .child(Icon::new(IconName::Quote).size(rems(em * 1.17)))
        .child(div().min_w_0().truncate().child(label))
}

impl RenderOnce for AssistantCitationChip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let label: SharedString = assistant_citation_label(&self.citation).into();
        let quote = self.citation.text.clone();
        let comment = self.citation.comment.clone();
        if !self.composer {
            let open = self.on_open_quote.clone();
            let go = self.on_go_to_source.clone();
            return div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child({
                    let mut chip = chip_shell(&theme, label.clone(), window)
                        .id("assistant-citation-chip")
                        .cursor_pointer();
                    if let Some(open) = open {
                        chip = chip.on_click(move |event, window, cx| open(event, window, cx));
                    }
                    chip
                })
                .when(self.comment_open, |this| {
                    this.child(quote_panel(&theme, &quote, comment.as_deref(), go))
                })
                .into_any_element();
        }
        let edit = self.on_edit.clone();
        let trigger_id = SharedString::from(format!(
            "citation-comment-{}-{}",
            self.citation.message_id, self.citation.start
        ));
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(rems(0.5))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(chip_shell(&theme, label, window))
                    .when(edit.is_some(), |this| {
                        let edit = edit.clone();
                        this.child(
                            Button::new(trigger_id)
                                .variant(ButtonVariant::GhostMuted)
                                .size(ButtonSize::IconXs)
                                .tooltip(crate::ui::Tooltip::new(if comment.is_some() {
                                    "Edit citation comment"
                                } else {
                                    "Add comment to citation"
                                }))
                                .child(Icon::new(IconName::Pencil).size(rems(0.75)))
                                .on_click(move |event, window, cx| {
                                    if let Some(edit) = &edit {
                                        edit(event, window, cx);
                                    }
                                }),
                        )
                    }),
            )
            .when(self.comment_open, |this| {
                this.child(
                    div()
                        .w(rems(20.))
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .p(rems(0.5))
                        .children(self.editor),
                )
            })
            .into_any_element()
    }
}

pub(crate) fn quote_panel(
    theme: &AppTheme,
    quote: &str,
    comment: Option<&str>,
    on_go: Option<ClickHandler>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(rems(0.75))
        .p_1()
        .max_w(rems(24.))
        .text_size(TEXT_SM.size)
        .child(
            div()
                .id("citation-quote-body")
                .w_full()
                .min_h_0()
                .max_h(rems(16.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .child(
                    div()
                        .border_l_2()
                        .border_color(theme.border)
                        .pl(rems(0.75))
                        .text_color(theme.muted_foreground)
                        .child(quote.to_owned()),
                )
                .when_some(comment, |this, comment| this.child(comment.to_owned())),
        )
        .child({
            let mut button = Button::new("citation-go-to-source")
                .variant(ButtonVariant::Outline)
                .size(ButtonSize::Sm)
                .child(Icon::new(IconName::ArrowUpRight).size(rems(0.75)))
                .label("Go to source");
            if let Some(on_go) = on_go {
                button = button.on_click(move |event, window, cx| on_go(event, window, cx));
            }
            button
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Option<&'static str> {
        None
    }

    #[test]
    fn preserves_an_over_length_draft_at_the_composer_until_it_can_be_shortened() {
        let mut session = CitationCommentSession::open(saved());
        let draft = "x".repeat(
            super::super::timeline::assistant_citations::ASSISTANT_CITATION_MAX_COMMENT_LENGTH + 1,
        );
        session.type_comment(&draft);
        session.source_unavailable(saved(), true);
        assert!(session.is_open());
        assert_eq!(session.value(), draft);
        assert!(session.invalid());
        assert_eq!(session.popup_side(), CitationPopupSide::Top);
        assert!(!session.anchored());
        assert!(session.save_calls().is_empty());

        session.type_comment("shortened comment");
        session.save(saved(), true);
        assert_eq!(session.save_calls(), &["shortened comment".to_owned()]);
        assert!(!session.is_open());
    }

    #[test]
    fn preserves_a_rejected_save_and_allows_a_later_retry() {
        let mut session = CitationCommentSession::open(saved());
        session.type_comment("keep this draft");
        session.source_unavailable(saved(), false);
        assert_eq!(session.save_calls(), &["keep this draft".to_owned()]);
        assert!(session.is_open());
        assert!(!session.anchored());
        session.save(saved(), true);
        assert_eq!(
            session.save_calls(),
            &["keep this draft".to_owned(), "keep this draft".to_owned()]
        );
        assert!(!session.is_open());
    }

    #[test]
    fn still_allows_explicit_cancellation_after_source_disappearance() {
        let mut session = CitationCommentSession::open(saved());
        session.type_comment("discard this draft");
        session.source_unavailable(saved(), false);
        let calls = session.save_calls().len();
        session.cancel();
        assert_eq!(session.save_calls().len(), calls);
        assert!(!session.is_open());
    }

    #[test]
    fn saves_and_closes_when_the_source_disappears_with_a_valid_draft() {
        let mut session = CitationCommentSession::open(saved());
        session.type_comment("saved comment");
        session.source_unavailable(saved(), true);
        assert_eq!(session.save_calls(), &["saved comment".to_owned()]);
        assert!(!session.is_open());
    }

    #[test]
    fn comment_replacement_rewrites_only_the_marker() {
        let citation = AssistantCitation {
            version: 1,
            environment_id: "env".into(),
            thread_id: "thread".into(),
            message_id: "message".into(),
            text: "hello".into(),
            comment: None,
            start: 0,
            end: 5,
            prefix: String::new(),
            suffix: String::new(),
        };
        let marker = serialize_assistant_citation(&citation);
        let prompt = format!("see {marker} now");
        let next = replace_citation_comment(&prompt, &citation, "why");
        assert!(next.contains("comment=why") || next.contains("comment=why"));
        assert!(next.starts_with("see "));
        assert!(next.ends_with(" now"));
        assert!(!next.contains(&marker));
    }
}
