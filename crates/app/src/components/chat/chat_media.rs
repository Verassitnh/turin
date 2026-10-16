//! Chat-column overlays for a citation quote and an expanded image.
//!
//! Upstream portals these from the chip and the thumbnail. The markdown flow
//! cannot host a portal, so the open quote and the image dialog live on this
//! session and paint over the chat column.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, Global, IntoElement, ParentElement as _, Styled as _,
    Window, div, rems,
};

use super::assistant_citation_chip::{AssistantCitationChip, CitationPopupSide};
use super::expanded_image::{
    ExpandedImageItem, ExpandedImagePreview, expanded_image_dialog, wrap_expanded_image_index,
};
use super::messages_timeline::MessagesTimeline;
use super::timeline::assistant_citations::{AssistantCitation, serialize_assistant_citation};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_LG, TEXT_SM};

#[derive(Default)]
pub struct ChatMediaSession {
    pub preview: Option<ExpandedImagePreview>,
    pub quote: Option<AssistantCitation>,
}

impl Global for ChatMediaSession {}

pub fn open_quote(citation: AssistantCitation, cx: &mut App) {
    cx.default_global::<ChatMediaSession>().quote = Some(citation);
    cx.refresh_windows();
}

pub fn close_quote(cx: &mut App) {
    cx.default_global::<ChatMediaSession>().quote = None;
    cx.refresh_windows();
}

pub fn open_preview(preview: ExpandedImagePreview, cx: &mut App) {
    cx.default_global::<ChatMediaSession>().preview = Some(preview);
    cx.refresh_windows();
}

pub fn close_preview(cx: &mut App) {
    cx.default_global::<ChatMediaSession>().preview = None;
    cx.refresh_windows();
}

fn step_preview(delta: i64, cx: &mut App) {
    let session = cx.default_global::<ChatMediaSession>();
    if let Some(preview) = session.preview.as_mut() {
        let count = preview.images.len() as i64;
        let next = wrap_expanded_image_index(preview.index as i64 + delta, count);
        preview.index = next as usize;
    }
    cx.refresh_windows();
}

/// The quote panel and image dialog, painted over the chat column.
pub fn chat_media_layer(
    timeline: Entity<MessagesTimeline>,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::AnyElement {
    ensure_demo(cx);
    let session = cx.default_global::<ChatMediaSession>().clone_view();
    let theme = cx.app_theme().clone();
    let mut layer = div().absolute().inset_0();
    if let Some(preview) = session.preview.clone() {
        layer = layer.child(expanded_image_dialog(
            &preview,
            |_, cx| close_preview(cx),
            |delta, _, cx| step_preview(delta, cx),
            window,
            cx,
        ));
    }
    if let Some(citation) = session.quote.clone() {
        let go = citation.clone();
        let timeline_go = timeline.clone();
        layer = layer.child(
            div()
                .absolute()
                .bottom(rems(6.))
                .left(rems(3.))
                .right(rems(3.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(rems(28.))
                        .rounded(RADIUS_LG)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .p(rems(0.75))
                        .text_size(TEXT_SM.size)
                        .child(
                            AssistantCitationChip::new(citation)
                                .comment_open(
                                    true,
                                    super::assistant_citation_chip::CitationPopupSide::Bottom,
                                )
                                .on_go_to_source(move |_, window, cx| {
                                    let citation = go.clone();
                                    timeline_go.update(cx, |timeline, cx| {
                                        timeline.reveal_citation(citation, window, cx);
                                    });
                                    close_quote(cx);
                                }),
                        ),
                ),
        );
    }
    layer.into_any_element()
}

#[derive(Clone)]
struct SessionView {
    preview: Option<ExpandedImagePreview>,
    quote: Option<AssistantCitation>,
}

impl ChatMediaSession {
    fn clone_view(&self) -> SessionView {
        SessionView {
            preview: self.preview.clone(),
            quote: self.quote.clone(),
        }
    }
}

fn ensure_demo(cx: &mut App) {
    let Ok(mode) = std::env::var("TURIN_CHAT_MEDIA_DEMO") else {
        return;
    };
    let session = cx.default_global::<ChatMediaSession>();
    if session.preview.is_some() || session.quote.is_some() {
        return;
    }
    match mode.as_str() {
        "image" => {
            session.preview = Some(ExpandedImagePreview {
                images: vec![ExpandedImageItem {
                    src: None,
                    name: "diagram.png".into(),
                    video: false,
                    original_url: None,
                    bytes: Some(Arc::new(DEMO_PNG.to_vec())),
                    mime_type: Some("image/png".into()),
                    actions_source: None,
                }],
                index: 0,
            });
        }
        "video" => {
            session.preview = Some(ExpandedImagePreview {
                images: vec![ExpandedImageItem {
                    src: None,
                    name: "clip.mp4".into(),
                    video: true,
                    original_url: None,
                    bytes: None,
                    mime_type: Some("video/mp4".into()),
                    actions_source: None,
                }],
                index: 0,
            });
        }
        "citation" => {
            session.quote = Some(AssistantCitation {
                version: 1,
                environment_id: "env".into(),
                thread_id: "thread".into(),
                message_id: "message".into(),
                text: "The cited sentence from the assistant.".into(),
                comment: Some("Why this line?".into()),
                start: 0,
                end: 12,
                prefix: String::new(),
                suffix: String::new(),
            });
        }
        _ => {}
    }
}

/// 64×48 blue PNG, generated without a crate.
const DEMO_PNG: &[u8] = include_bytes!("demo_image.png");

/// Citation chips scanned from a flat prompt. The composer is not a
/// ProseMirror document, so the chips sit with the draft instead of as atoms.
pub fn prompt_citation_chips(prompt: &str) -> Vec<AssistantCitation> {
    super::timeline::assistant_citations::collect_assistant_citations(prompt)
        .into_iter()
        .map(|found| found.citation)
        .collect()
}

pub type CitationEditHandler = Rc<dyn Fn(&AssistantCitation, &mut Window, &mut App)>;

pub fn citation_chip_row(
    citations: &[AssistantCitation],
    editing_key: Option<&str>,
    mut editor: Option<AnyElement>,
    on_edit: CitationEditHandler,
) -> AnyElement {
    let mut row = div().flex().flex_wrap().gap(rems(0.375)).mb(rems(0.5));
    for citation in citations {
        let key = serialize_assistant_citation(citation);
        let open = editing_key == Some(key.as_str());
        let edit_citation = citation.clone();
        let on_edit = on_edit.clone();
        let mut chip = AssistantCitationChip::new(citation.clone())
            .composer(true)
            .on_edit(move |_event: &ClickEvent, window, cx| on_edit(&edit_citation, window, cx));
        if open {
            chip = chip.comment_open(true, CitationPopupSide::Top);
            if let Some(editor) = editor.take() {
                chip = chip.editor(editor);
            }
        }
        row = row.child(chip);
    }
    row.into_any_element()
}
