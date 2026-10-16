//! Port of upstream/apps/web/src/components/chat/ChatCanvas.tsx
//!
//! Owns the available conversation space. Cards report where they sit; the
//! canvas decides when chat moves over to make room for them.

use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div, px,
};

use super::chat_canvas_context::ChatCanvasContext;
use super::chat_canvas_layout::{
    ChatCanvasDetailsCard, ChatCanvasLane, ChatCanvasLaneInput, resolve_chat_canvas_lane,
};
use crate::theme::tokens::CHAT_CONTENT_MAX_WIDTH;

#[derive(Clone, Debug, PartialEq)]
pub struct ChatCanvasMeasurements {
    pub width: f64,
    pub height: f64,
    pub padding: f64,
    pub max_chat_width: f64,
    pub min_chat_width: f64,
    pub composer_height: f64,
    pub timeline_gutter: f64,
    pub find_bar_left: f64,
    pub find_bar_right: f64,
    pub find_bar_bottom: f64,
}

impl Default for ChatCanvasMeasurements {
    fn default() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            padding: 48.0,
            max_chat_width: 768.0,
            min_chat_width: 640.0,
            composer_height: 0.0,
            timeline_gutter: 0.0,
            find_bar_left: 0.0,
            find_bar_right: 0.0,
            find_bar_bottom: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatCanvasLayout {
    pub chat: ChatCanvasLane,
    /// Always false without the floating preview mini player (cut in Turin).
    pub overlaps_chat: bool,
}

pub fn resolve_chat_canvas_layout(
    measurements: &ChatCanvasMeasurements,
    details_card: Option<ChatCanvasDetailsCard>,
) -> ChatCanvasLayout {
    let chat = resolve_chat_canvas_lane(ChatCanvasLaneInput {
        container_width: measurements.width,
        padding: measurements.padding,
        max_chat_width: measurements.max_chat_width,
        min_chat_width: measurements.min_chat_width,
        details_card,
    });
    ChatCanvasLayout {
        chat,
        overlaps_chat: false,
    }
}

#[derive(IntoElement)]
pub struct ChatCanvas {
    pub context: ChatCanvasContext,
    pub children: gpui_kit::AnyElement,
}

impl ChatCanvas {
    pub fn from_measurements(
        measurements: ChatCanvasMeasurements,
        details_card: Option<ChatCanvasDetailsCard>,
        children: gpui_kit::AnyElement,
    ) -> Self {
        let layout = resolve_chat_canvas_layout(&measurements, details_card);
        Self {
            context: ChatCanvasContext::new(measurements, layout),
            children,
        }
    }
}

impl RenderOnce for ChatCanvas {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let lane = self.context.layout.chat;
        let inset_start = px(lane.inset_start as f32);
        let inset_end = px(lane.inset_end as f32);
        let _ = CHAT_CONTENT_MAX_WIDTH;
        div()
            .id("chat-canvas")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .pl(inset_start)
            .pr(inset_end)
            .child(self.children)
    }
}
