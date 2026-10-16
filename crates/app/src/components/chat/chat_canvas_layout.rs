//! Port of upstream/apps/web/src/components/chat/chatCanvasLayout.ts (the chat lane).
//!
//! Chat stays centered in the canvas. An inline workspace card docked to the
//! top-right corner moves chat left, and only as far as it must; chat narrows
//! only after it reaches the left padding.
//!
//! The floating preview mini player is cut from Turin (see PORTING.md, cut list), so
//! its frame, drag clamping and composer lift are not ported. With no preview, upstream
//! returns the chat lane below and `frame`, `overlapsChat` and `overlapsDetailsCard`
//! are always null or false. Upstream's `composerHeight`, `findBar` and `preview` inputs
//! only affect that frame and are dropped with it.

/// Minimum space between chat and the workspace card. Chat stays centered while
/// the card fits beside it with this much room.
pub const DETAILS_CARD_CLEARANCE: f64 = 32.0;

/// The inline workspace card docked to the top-right corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatCanvasDetailsCard {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
}

/// The chat column inside the canvas. `inset_start` and `inset_end` are the empty space
/// before and after it, as the canvas lays them out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatCanvasLane {
    pub left: f64,
    pub width: f64,
    pub inset_start: f64,
    pub inset_end: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatCanvasLaneInput {
    pub container_width: f64,
    pub padding: f64,
    pub max_chat_width: f64,
    pub min_chat_width: f64,
    pub details_card: Option<ChatCanvasDetailsCard>,
}

impl Default for ChatCanvasLaneInput {
    fn default() -> Self {
        Self {
            container_width: 0.0,
            padding: 48.0,
            max_chat_width: 768.0,
            min_chat_width: 640.0,
            details_card: None,
        }
    }
}

/// Pure geometry for the conversation column beside the workspace card.
pub fn resolve_chat_canvas_lane(input: ChatCanvasLaneInput) -> ChatCanvasLane {
    let ChatCanvasLaneInput {
        container_width,
        padding,
        max_chat_width,
        details_card,
        ..
    } = input;
    let centered_width = (max_chat_width.min(container_width - padding * 2.0)).max(0.0);
    // A workspace card that does not fit beside the centered chat first moves
    // chat left, only as far as it needs. Chat narrows only after it reaches the
    // left padding.
    let lane_right = match details_card {
        Some(card) => card.left - DETAILS_CARD_CLEARANCE,
        None => container_width - padding,
    };
    let normal_width = (centered_width.min(lane_right - padding)).max(0.0);
    let normal_left =
        padding.max(((container_width - normal_width) / 2.0).min(lane_right - normal_width));
    ChatCanvasLane {
        left: normal_left,
        width: normal_width,
        inset_start: 0.0,
        inset_end: (container_width - normal_left * 2.0 - normal_width).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(
        container_width: f64,
        padding: f64,
        max_chat_width: f64,
        details_card: Option<ChatCanvasDetailsCard>,
    ) -> ChatCanvasLane {
        resolve_chat_canvas_lane(ChatCanvasLaneInput {
            container_width,
            padding,
            max_chat_width,
            details_card,
            ..Default::default()
        })
    }

    /// The workspace card as the canvas places it: 292 px wide, 12 px from the right edge.
    fn with_card(width: f64, max_chat_width: f64) -> ChatCanvasLane {
        lane(
            width,
            48.0,
            max_chat_width,
            Some(ChatCanvasDetailsCard {
                left: width - 292.0,
                right: width - 12.0,
                bottom: 400.0,
            }),
        )
    }

    #[test]
    fn centers_chat_in_the_whole_container_without_a_card() {
        assert_eq!(
            resolve_chat_canvas_lane(ChatCanvasLaneInput {
                container_width: 1344.0,
                ..Default::default()
            }),
            ChatCanvasLane {
                left: 288.0,
                width: 768.0,
                inset_start: 0.0,
                inset_end: 0.0
            }
        );
        assert_eq!(
            resolve_chat_canvas_lane(ChatCanvasLaneInput {
                container_width: 390.0,
                padding: 12.0,
                ..Default::default()
            }),
            ChatCanvasLane {
                left: 12.0,
                width: 366.0,
                inset_start: 0.0,
                inset_end: 0.0
            }
        );
    }

    #[test]
    fn keeps_chat_centered_while_the_card_fits_beside_it() {
        assert_eq!(
            with_card(1384.0, 736.0),
            ChatCanvasLane {
                left: 324.0,
                width: 736.0,
                inset_start: 0.0,
                inset_end: 0.0
            }
        );
    }

    #[test]
    fn moves_chat_left_only_as_far_as_the_card_requires() {
        assert_eq!(
            with_card(1383.0, 736.0),
            ChatCanvasLane {
                left: 323.0,
                width: 736.0,
                inset_start: 0.0,
                inset_end: 1.0
            }
        );
        assert_eq!(
            with_card(1147.0, 736.0),
            ChatCanvasLane {
                left: 87.0,
                width: 736.0,
                inset_start: 0.0,
                inset_end: 237.0
            }
        );
    }

    #[test]
    fn narrows_chat_only_after_it_reaches_the_left_padding() {
        assert_eq!(with_card(1108.0, 736.0).left, 48.0);
        assert_eq!(with_card(1108.0, 736.0).width, 736.0);
        assert_eq!(
            with_card(1028.0, 736.0),
            ChatCanvasLane {
                left: 48.0,
                width: 656.0,
                inset_start: 0.0,
                inset_end: 276.0
            }
        );
        assert_eq!(with_card(1012.0, 736.0).width, 640.0);
    }

    #[test]
    fn keeps_a_full_width_chat_clear_of_the_card() {
        assert_eq!(
            lane(
                1147.0,
                48.0,
                10_000.0,
                Some(ChatCanvasDetailsCard {
                    left: 855.0,
                    right: 1135.0,
                    bottom: 400.0
                }),
            ),
            ChatCanvasLane {
                left: 48.0,
                width: 775.0,
                inset_start: 0.0,
                inset_end: 276.0
            }
        );
    }
}
