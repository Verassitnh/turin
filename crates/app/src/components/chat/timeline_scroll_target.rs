//! Port of upstream/apps/web/src/components/chat/timelineScrollTarget.ts
//!
//! A gesture inside the timeline may belong to a nested tool result or code block. Only treat it
//! as timeline navigation if it can chain to the outer list.
//!
//! Upstream walks `parentElement` reading `getComputedStyle`; the caller here hands over the
//! chain of scroll containers between the gesture's target and the timeline instead, innermost
//! first, each with the facts the walk reads.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Auto,
    Scroll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverscrollBehavior {
    Auto,
    Contain,
    None,
}

/// One element between the target and the timeline (the timeline itself excluded).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollContainer {
    pub overflow_y: Overflow,
    pub overscroll_behavior_y: OverscrollBehavior,
    pub scroll_top: f64,
    pub scroll_height: f64,
    pub client_height: f64,
}

impl ScrollContainer {
    /// An element that does not scroll on its own (`overflow-y: visible`, no overscroll rule).
    pub const PLAIN: Self = Self {
        overflow_y: Overflow::Visible,
        overscroll_behavior_y: OverscrollBehavior::Auto,
        scroll_top: 0.0,
        scroll_height: 100.0,
        client_height: 100.0,
    };
}

/// `target` is `None` when the event's target is not an element inside the timeline (a
/// non-`Element` target, or one the timeline does not contain).
pub fn is_timeline_scroll_target(
    target_in_timeline: bool,
    chain: &[ScrollContainer],
    delta_y: f64,
) -> bool {
    if !target_in_timeline || delta_y == 0.0 {
        return false;
    }
    for element in chain {
        if !matches!(element.overflow_y, Overflow::Auto | Overflow::Scroll) {
            continue;
        }
        let can_scroll = if delta_y < 0.0 {
            element.scroll_top > 0.0
        } else {
            element.scroll_top < element.scroll_height - element.client_height
        };
        if can_scroll
            || matches!(
                element.overscroll_behavior_y,
                OverscrollBehavior::Contain | OverscrollBehavior::None
            )
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::scroll_gesture::{
        ComposerScrollGestureState, ScrollGestureEvent, record_composer_scroll_gesture_event,
    };

    fn group() -> ScrollContainer {
        ScrollContainer {
            overflow_y: Overflow::Auto,
            scroll_height: 300.0,
            scroll_top: 80.0,
            ..ScrollContainer::PLAIN
        }
    }

    /// `content` inside `group` inside the timeline: the chain is innermost first.
    fn targets(chain: &[ScrollContainer], delta_y: f64) -> bool {
        is_timeline_scroll_target(true, chain, delta_y)
    }

    #[test]
    fn keeps_a_nested_tool_groups_scroll_out_of_the_timeline() {
        for delta_y in [-30.0, 30.0] {
            let content_chain = [ScrollContainer::PLAIN, group()];
            assert!(!targets(&content_chain, delta_y));
            assert!(!targets(&[group()], delta_y));
        }
    }

    #[test]
    fn allows_chaining_only_past_the_matching_edge() {
        for (scroll_top, delta_y) in [(0.0, -30.0), (200.0, 30.0)] {
            let chain = [
                ScrollContainer::PLAIN,
                ScrollContainer {
                    scroll_top,
                    ..group()
                },
            ];
            assert!(targets(&chain, delta_y));
            assert!(!targets(&chain, -delta_y));
        }
    }

    #[test]
    fn respects_overscroll_y_contain_and_none_at_either_edge() {
        for behavior in [OverscrollBehavior::Contain, OverscrollBehavior::None] {
            let at = |scroll_top: f64, scroll_height: f64| {
                [
                    ScrollContainer::PLAIN,
                    ScrollContainer {
                        overscroll_behavior_y: behavior,
                        scroll_top,
                        scroll_height,
                        ..group()
                    },
                ]
            };
            assert!(!targets(&at(0.0, 300.0), -30.0));
            assert!(!targets(&at(200.0, 300.0), 30.0));
            // A group that cannot scroll at all still contains the gesture.
            assert!(!targets(&at(0.0, 100.0), 30.0));
        }
    }

    #[test]
    fn checks_nested_results_even_when_the_tool_group_cannot_scroll() {
        let idle_group = ScrollContainer {
            scroll_top: 0.0,
            scroll_height: 100.0,
            ..group()
        };
        let result = ScrollContainer {
            overflow_y: Overflow::Scroll,
            scroll_height: 300.0,
            scroll_top: 0.25,
            ..ScrollContainer::PLAIN
        };
        assert!(!targets(
            &[ScrollContainer::PLAIN, result, idle_group],
            -30.0
        ));
        let at_edge = ScrollContainer {
            scroll_top: 0.0,
            ..result
        };
        assert!(targets(&[at_edge, idle_group], -30.0));
    }

    #[test]
    fn checks_an_outer_group_when_an_inner_result_reaches_its_edge() {
        let result = ScrollContainer {
            overflow_y: Overflow::Auto,
            ..ScrollContainer::PLAIN
        };
        assert!(!targets(&[result, group()], -30.0));
        let at_top = ScrollContainer {
            scroll_top: 0.0,
            ..group()
        };
        assert!(targets(&[result, at_top], -30.0));
    }

    #[test]
    fn ignores_overflow_y_visible_hidden_and_clip() {
        for overflow_y in [Overflow::Visible, Overflow::Hidden, Overflow::Clip] {
            let chain = [
                ScrollContainer::PLAIN,
                ScrollContainer {
                    overflow_y,
                    ..group()
                },
            ];
            assert!(targets(&chain, -30.0));
        }
    }

    #[test]
    fn allows_ordinary_message_content_and_the_outer_viewport() {
        assert!(targets(&[ScrollContainer::PLAIN], -30.0));
        assert!(targets(&[], 30.0));
    }

    #[test]
    fn rejects_outside_targets_and_horizontal_only_scrolling() {
        assert!(!is_timeline_scroll_target(false, &[], -30.0));
        assert!(!targets(&[ScrollContainer::PLAIN, group()], 0.0));
    }

    #[test]
    fn does_not_accumulate_nested_scrolling_toward_composer_collapse() {
        let timeline_scroll_top = 1000.0;
        let mut state = ComposerScrollGestureState::default();
        let mut record = |chain: &[ScrollContainer], now: f64, delta_px: f32| {
            record_composer_scroll_gesture_event(
                &mut state,
                &ScrollGestureEvent {
                    now,
                    delta_px,
                    collapse_threshold_px: 24.0,
                    collapse_eligible: targets(chain, f64::from(-delta_px)),
                    can_scroll_in_gesture_direction: timeline_scroll_top > 0.0,
                    scrolls_toward_logical_end: false,
                },
            )
        };
        let nested = [ScrollContainer::PLAIN, group()];
        assert!(!record(&[], 0.0, 20.0));
        assert!(!record(&nested, 20.0, 30.0));
        let at_top = [
            ScrollContainer::PLAIN,
            ScrollContainer {
                scroll_top: 0.0,
                ..group()
            },
        ];
        assert!(!record(&at_top, 40.0, 10.0));
        assert!(record(&at_top, 60.0, 14.0));
    }
}
