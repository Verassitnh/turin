//! Port of upstream/apps/web/src/components/chat/ChatCanvasContext.ts

use super::chat_canvas::{ChatCanvasLayout, ChatCanvasMeasurements};

/// Shared layout state for the chat column and the thread details card.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatCanvasContext {
    pub measurements: ChatCanvasMeasurements,
    pub layout: ChatCanvasLayout,
    /// Height reserved above the details card, such as the open find bar.
    pub details_card_top_inset: f64,
}

impl ChatCanvasContext {
    pub fn new(measurements: ChatCanvasMeasurements, layout: ChatCanvasLayout) -> Self {
        Self {
            details_card_top_inset: measurements.find_bar_bottom,
            measurements,
            layout,
        }
    }
}
