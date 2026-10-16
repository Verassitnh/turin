//! Live assistant markdown selection for the cite toolbar.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use gpui_kit::{App, Global, Point, Pixels};

use super::assistant_text_selection::create_assistant_text_selector;
use super::selection_actions::SelectionActionPoint;
use super::timeline::assistant_citations::AssistantCitation;

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

pub fn assistant_selection_capture(cx: &App) -> Arc<Mutex<AssistantSelectionCapture>> {
    cx.default_global::<AssistantSelectionGlobal>()
        .capture
        .clone()
}

pub fn report_flow_selection(
    cx: &App,
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
    let mut capture = assistant_selection_capture(cx).lock().expect("selection lock");
    if selected.is_empty() {
        if capture.message_id.as_deref() == Some(message_id) {
            capture.selected_text.clear();
        }
        return;
    }
    capture.message_id = Some(message_id.to_owned());
    capture.message_text = Some(message_text.to_owned());
    capture.selected_text = selected;
    capture.anchor = Some(anchor);
    cx.refresh_windows();
}

pub fn clear_assistant_selection(cx: &App) {
    *assistant_selection_capture(cx)
        .lock()
        .expect("selection lock") = AssistantSelectionCapture::default();
    cx.refresh_windows();
}

pub fn selection_toolbar_point(
    capture: &AssistantSelectionCapture,
    viewport_width: f64,
    viewport_height: f64,
) -> Option<SelectionActionPoint> {
    let anchor = capture.anchor?;
    Some(SelectionActionPoint {
        x: f64::from(anchor.x).clamp(8.0, viewport_width - 8.0),
        y: f64::from(anchor.y).clamp(8.0, viewport_height - 8.0),
    })
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
