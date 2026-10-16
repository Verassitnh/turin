//! Port of upstream/apps/web/src/components/chat/threadDetailsCardLayout.ts
//!
//! The floating preview mini player is cut from Turin (see `chat_canvas_layout.rs`), so
//! `frame` and `overlapsDetailsCard` are ignored and height always fills below `topInset`.

use super::chat_canvas_layout::DETAILS_CARD_CLEARANCE;

/// Inset of the card from the canvas edges; the find bar shares it to line up.
pub const THREAD_DETAILS_CARD_GAP: f64 = 12.0;
// Keep in sync with --thread-details-panel-width, which sizes the popover and the find bar.
const THREAD_DETAILS_CARD_WIDTH: f64 = 280.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ThreadDetailsCardDensity {
    Full,
    Compact,
    Essential,
}

#[allow(dead_code)]
pub fn resolve_thread_details_card_density(
    height: f64,
    content: ThreadDetailsCardContentHeights,
) -> ThreadDetailsCardDensity {
    if content.full == 0. || content.full <= height {
        return ThreadDetailsCardDensity::Full;
    }
    if content.compact == 0. || content.compact <= height {
        return ThreadDetailsCardDensity::Compact;
    }
    ThreadDetailsCardDensity::Essential
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub struct ThreadDetailsCardContentHeights {
    pub full: f64,
    pub compact: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadDetailsCardLayoutInput {
    pub container: ThreadDetailsCardContainer,
    pub lane: ThreadDetailsCardLane,
    /// Space taken above the card, such as the open find bar.
    pub top_inset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadDetailsCardContainer {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadDetailsCardLane {
    pub padding: f64,
    pub min_chat_width: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadDetailsCardLayout {
    pub x: f64,
    pub right: f64,
    pub width: f64,
    pub y: f64,
    pub height: f64,
}

/**
 * The card pins to the top right while a readable chat lane fits beside it.
 * The chat canvas decides whether chat moves over to make room.
 */
pub fn resolve_thread_details_card_layout(
    input: ThreadDetailsCardLayoutInput,
) -> Option<ThreadDetailsCardLayout> {
    let gap = THREAD_DETAILS_CARD_GAP;
    let width = THREAD_DETAILS_CARD_WIDTH;
    let x = input.container.width - width - gap;
    if x - DETAILS_CARD_CLEARANCE - input.lane.padding < input.lane.min_chat_width {
        return None;
    }
    let y = gap + input.top_inset;
    let height = input.container.height - y - gap;
    if height < 160. {
        return None;
    }
    Some(ThreadDetailsCardLayout {
        x,
        right: gap,
        width,
        y,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_folds_when_content_exceeds_height() {
        assert_eq!(
            resolve_thread_details_card_density(
                200.,
                ThreadDetailsCardContentHeights {
                    full: 150.,
                    compact: 180.,
                }
            ),
            ThreadDetailsCardDensity::Full
        );
        assert_eq!(
            resolve_thread_details_card_density(
                200.,
                ThreadDetailsCardContentHeights {
                    full: 250.,
                    compact: 180.,
                }
            ),
            ThreadDetailsCardDensity::Compact
        );
        assert_eq!(
            resolve_thread_details_card_density(
                200.,
                ThreadDetailsCardContentHeights {
                    full: 250.,
                    compact: 220.,
                }
            ),
            ThreadDetailsCardDensity::Essential
        );
    }

    fn resolve(width: f64, height: f64, top_inset: f64) -> Option<ThreadDetailsCardLayout> {
        resolve_thread_details_card_layout(ThreadDetailsCardLayoutInput {
            container: ThreadDetailsCardContainer { width, height },
            lane: ThreadDetailsCardLane {
                padding: 48.,
                min_chat_width: 640.,
            },
            top_inset,
        })
    }

    #[test]
    fn pins_to_top_right_at_fixed_width() {
        assert_eq!(
            resolve(1600., 900., 0.),
            Some(ThreadDetailsCardLayout {
                x: 1308.,
                right: 12.,
                y: 12.,
                width: 280.,
                height: 876.,
            })
        );
        assert_eq!(resolve(1344., 900., 0.).unwrap().x, 1052.);
    }

    #[test]
    fn starts_below_open_find_bar() {
        assert_eq!(
            resolve(1600., 900., 48.),
            Some(ThreadDetailsCardLayout {
                x: 1308.,
                right: 12.,
                y: 60.,
                width: 280.,
                height: 828.,
            })
        );
    }

    #[test]
    fn hides_when_readable_chat_lane_cannot_fit() {
        assert_eq!(resolve(1012., 900., 0.).unwrap().x, 720.);
        assert!(resolve(1011., 900., 0.).is_none());
    }
}
