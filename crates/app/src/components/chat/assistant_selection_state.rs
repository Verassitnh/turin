//! Live assistant markdown selection, cited into the composer with mod+L.
//!
//! Upstream floats a "Cite" button at the end of the selection
//! (`AssistantSelectionToolbar.tsx`); Turin drops the button and cites on the shortcut.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use gpui_kit::{App, Focusable as _, Global, Pixels, Point, WeakEntity, Window};

use super::assistant_text_selection::create_assistant_text_selector;
use super::timeline::assistant_citations::{ASSISTANT_CITATION_MAX_TEXT_LENGTH, AssistantCitation};
use crate::composer::views::chat_composer::ChatComposer;
use crate::state::models::parse_scoped_thread_key;

#[derive(Clone, Default)]
pub struct AssistantSelectionCapture {
    pub message_id: Option<String>,
    pub message_text: Option<String>,
    pub selected_text: String,
    pub anchor: Option<Point<Pixels>>,
}

#[derive(Default)]
struct AssistantSelectionGlobal {
    capture: Arc<Mutex<AssistantSelectionCapture>>,
}

impl Global for AssistantSelectionGlobal {}

pub fn assistant_selection_capture(cx: &mut App) -> Arc<Mutex<AssistantSelectionCapture>> {
    cx.default_global::<AssistantSelectionGlobal>()
        .capture
        .clone()
}

pub fn report_flow_selection(
    cx: &mut App,
    message_id: &str,
    message_text: &str,
    flow_text: &str,
    range: Range<usize>,
    anchor: Point<Pixels>,
) {
    let selected = flow_text
        .get(range.clone())
        .unwrap_or("")
        .trim()
        .to_string();
    let shared_capture = assistant_selection_capture(cx);
    let mut capture = shared_capture.lock().expect("selection lock");
    if selected.is_empty() {
        if capture.message_id.as_deref() == Some(message_id) {
            capture.selected_text.clear();
        }
        return;
    }
    if capture.message_id.as_deref() == Some(message_id)
        && capture.message_text.as_deref() == Some(message_text)
        && capture.selected_text == selected
        && capture.anchor == Some(anchor)
    {
        return;
    }
    capture.message_id = Some(message_id.to_owned());
    capture.message_text = Some(message_text.to_owned());
    capture.selected_text = selected;
    capture.anchor = Some(anchor);
    drop(capture);
    cx.refresh_windows();
}

pub fn clear_assistant_selection(cx: &mut App) {
    *assistant_selection_capture(cx)
        .lock()
        .expect("selection lock") = AssistantSelectionCapture::default();
    cx.refresh_windows();
}

/// `text.length > ASSISTANT_CITATION_MAX_TEXT_LENGTH` (JS length: UTF-16 code units).
pub fn assistant_selection_too_long(text: &str) -> bool {
    text.encode_utf16().count() > ASSISTANT_CITATION_MAX_TEXT_LENGTH
}

/// The composer of the open live chat: where mod+L puts the citation.
#[derive(Default)]
struct CitationComposer(Option<WeakEntity<ChatComposer>>);

impl Global for CitationComposer {}

/// The chat column reports its composer on every render: `None` off a live chat. Unchanged
/// targets skip `set_global`, which would notify the global's observers.
pub fn set_citation_composer(composer: Option<WeakEntity<ChatComposer>>, cx: &mut App) {
    let current = cx
        .try_global::<CitationComposer>()
        .and_then(|target| target.0.as_ref())
        .map(|target| target.entity_id());
    if current != composer.as_ref().map(|composer| composer.entity_id()) {
        cx.set_global(CitationComposer(composer));
    }
}

/// mod+L: cites the captured selection into the open chat's composer and focuses it. Does
/// nothing (`false`) without a selection, a live composer, or with a selection over the limit.
pub fn cite_assistant_selection(window: &mut Window, cx: &mut App) -> bool {
    let Some(composer) = cx
        .try_global::<CitationComposer>()
        .and_then(|target| target.0.as_ref())
        .and_then(WeakEntity::upgrade)
    else {
        return false;
    };
    let capture = match assistant_selection_capture(cx).lock() {
        Ok(capture) => capture.clone(),
        Err(_) => return false,
    };
    if capture.selected_text.is_empty() || assistant_selection_too_long(&capture.selected_text) {
        return false;
    }
    let Some((environment_id, thread_id)) = parse_scoped_thread_key(composer.read(cx).thread_key())
    else {
        return false;
    };
    let Some(citation) =
        assistant_citation_from_capture(&capture, environment_id.as_str(), thread_id.as_str())
    else {
        return false;
    };
    composer.update(cx, |composer, cx| {
        composer.insert_assistant_citation_from_selection(citation, window, cx);
    });
    clear_assistant_selection(cx);
    let focus = composer.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    true
}

pub fn assistant_citation_from_capture(
    capture: &AssistantSelectionCapture,
    environment_id: &str,
    thread_id: &str,
) -> Option<AssistantCitation> {
    let message_id = capture.message_id.as_ref()?;
    let message_text = capture.message_text.as_ref()?;
    let selected = capture.selected_text.as_str();
    if selected.is_empty() {
        return None;
    }
    let haystack: Vec<u16> = message_text.encode_utf16().collect();
    let needle: Vec<u16> = selected.encode_utf16().collect();
    let raw_start = haystack
        .windows(needle.len())
        .position(|window| window == needle.as_slice())?;
    let raw_end = raw_start + needle.len();
    let selector = create_assistant_text_selector(message_text, raw_start, raw_end)?;
    Some(AssistantCitation {
        version: 1,
        environment_id: environment_id.to_owned(),
        thread_id: thread_id.to_owned(),
        message_id: message_id.clone(),
        text: selector.text,
        comment: None,
        start: selector.start as u64,
        end: selector.end as u64,
        prefix: selector.prefix,
        suffix: selector.suffix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_counts_utf16_code_units_like_js_length() {
        let limit = "a".repeat(ASSISTANT_CITATION_MAX_TEXT_LENGTH);
        assert!(!assistant_selection_too_long(&limit));
        assert!(assistant_selection_too_long(&format!("{limit}a")));
        // One astral character is two UTF-16 units.
        let almost = "a".repeat(ASSISTANT_CITATION_MAX_TEXT_LENGTH - 1);
        assert!(!assistant_selection_too_long(&almost));
        assert!(assistant_selection_too_long(&format!("{almost}😀")));
    }
}
