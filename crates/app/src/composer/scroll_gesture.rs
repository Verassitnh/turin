//! Port of upstream/apps/web/src/components/chat/composerScrollGesture.ts

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComposerScrollGestureState {
    pub accumulated_delta_px: f32,
    pub collapse_suppressed: bool,
    pub last_event_at: f64,
}

impl Default for ComposerScrollGestureState {
    fn default() -> Self {
        Self {
            accumulated_delta_px: 0.0,
            collapse_suppressed: false,
            last_event_at: f64::NEG_INFINITY,
        }
    }
}

pub struct ScrollKeyInput<'a> {
    pub key: &'a str,
    pub scroll_top: f32,
    pub scroll_height: f32,
    pub client_height: f32,
    pub is_at_logical_end: bool,
}

/// `shouldCollapseComposerForScrollKey`.
pub fn should_collapse_composer_for_scroll_key(input: &ScrollKeyInput<'_>) -> bool {
    match input.key {
        "PageUp" | "Home" => input.scroll_top > 1.0,
        "PageDown" | "End" => {
            !input.is_at_logical_end
                && input.scroll_top < input.scroll_height - input.client_height - 1.0
        }
        _ => false,
    }
}

/// `resetComposerScrollGesture`.
pub fn reset_composer_scroll_gesture(state: &mut ComposerScrollGestureState) {
    *state = ComposerScrollGestureState::default();
}

/// `suppressActiveComposerScrollGesture`.
pub fn suppress_active_composer_scroll_gesture(
    state: &mut ComposerScrollGestureState,
    now: f64,
    gesture_reset_ms: f64,
) {
    if now - state.last_event_at <= gesture_reset_ms {
        state.collapse_suppressed = true;
    }
}

pub struct ScrollGestureEvent {
    pub now: f64,
    pub delta_px: f32,
    pub collapse_threshold_px: f32,
    pub collapse_eligible: bool,
    pub can_scroll_in_gesture_direction: bool,
    pub scrolls_toward_logical_end: bool,
}

/// `recordComposerScrollGestureEvent`: true when the gesture collapses the
/// composer.
pub fn record_composer_scroll_gesture_event(
    state: &mut ComposerScrollGestureState,
    input: &ScrollGestureEvent,
) -> bool {
    state.last_event_at = input.now;
    if state.collapse_suppressed
        || !input.collapse_eligible
        || !input.can_scroll_in_gesture_direction
        || input.scrolls_toward_logical_end
    {
        state.accumulated_delta_px = 0.0;
        return false;
    }
    state.accumulated_delta_px += input.delta_px;
    if state.accumulated_delta_px < input.collapse_threshold_px {
        return false;
    }
    state.accumulated_delta_px = 0.0;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESET_MS: f64 = 120.0;
    const THRESHOLD_PX: f32 = 24.0;

    fn middle(key: &str) -> ScrollKeyInput<'_> {
        ScrollKeyInput {
            key,
            scroll_top: 500.0,
            scroll_height: 1500.0,
            client_height: 500.0,
            is_at_logical_end: false,
        }
    }

    #[test]
    fn collapses_on_page_and_home_end_keys_in_the_timeline() {
        for key in ["PageUp", "PageDown", "Home", "End"] {
            assert!(should_collapse_composer_for_scroll_key(&middle(key)));
        }
    }

    #[test]
    fn does_not_collapse_at_the_top_or_the_logical_end() {
        for key in ["PageUp", "Home"] {
            let mut input = middle(key);
            input.scroll_top = 0.0;
            assert!(!should_collapse_composer_for_scroll_key(&input));
        }
        for key in ["PageDown", "End"] {
            let mut input = middle(key);
            input.is_at_logical_end = true;
            assert!(!should_collapse_composer_for_scroll_key(&input));
            let mut input = middle(key);
            input.scroll_top = 1000.0;
            assert!(!should_collapse_composer_for_scroll_key(&input));
        }
        assert!(!should_collapse_composer_for_scroll_key(&middle("Enter")));
    }

    struct Opts {
        delta_px: f32,
        collapse_eligible: bool,
        can_scroll: bool,
        toward_end: bool,
    }

    impl Default for Opts {
        fn default() -> Self {
            Self {
                delta_px: 30.0,
                collapse_eligible: true,
                can_scroll: true,
                toward_end: false,
            }
        }
    }

    fn record(state: &mut ComposerScrollGestureState, now: f64, o: Opts) -> bool {
        record_composer_scroll_gesture_event(
            state,
            &ScrollGestureEvent {
                now,
                delta_px: o.delta_px,
                collapse_threshold_px: THRESHOLD_PX,
                collapse_eligible: o.collapse_eligible,
                can_scroll_in_gesture_direction: o.can_scroll,
                scrolls_toward_logical_end: o.toward_end,
            },
        )
    }

    #[test]
    fn lets_an_editor_change_win_over_the_rest_of_the_active_gesture() {
        let mut state = ComposerScrollGestureState::default();
        assert!(record(&mut state, 0.0, Opts::default()));
        suppress_active_composer_scroll_gesture(&mut state, 20.0, RESET_MS);
        for now in [40.0, 80.0, 160.0] {
            assert!(!record(&mut state, now, Opts::default()));
        }
    }

    #[test]
    fn preserves_suppression_while_collapse_is_temporarily_ineligible() {
        let mut state = ComposerScrollGestureState::default();
        record(&mut state, 0.0, Opts::default());
        suppress_active_composer_scroll_gesture(&mut state, 20.0, RESET_MS);
        assert!(!record(
            &mut state,
            40.0,
            Opts {
                collapse_eligible: false,
                ..Opts::default()
            }
        ));
        assert!(!record(&mut state, 80.0, Opts::default()));
    }

    #[test]
    fn keeps_a_boundary_momentum_tail_in_the_same_suppressed_gesture() {
        let mut state = ComposerScrollGestureState::default();
        record(&mut state, 0.0, Opts::default());
        suppress_active_composer_scroll_gesture(&mut state, 20.0, RESET_MS);
        assert!(!record(
            &mut state,
            80.0,
            Opts {
                can_scroll: false,
                ..Opts::default()
            }
        ));
        assert_eq!(state.last_event_at, 80.0);
        assert!(!record(&mut state, 160.0, Opts::default()));
    }

    #[test]
    fn does_not_carry_gesture_state_into_a_new_thread_after_reset() {
        let mut state = ComposerScrollGestureState::default();
        record(&mut state, 0.0, Opts::default());
        suppress_active_composer_scroll_gesture(&mut state, 20.0, RESET_MS);
        record(&mut state, 80.0, Opts::default());
        reset_composer_scroll_gesture(&mut state);
        assert!(record(&mut state, 240.0, Opts::default()));
    }

    #[test]
    fn accumulates_small_deltas_only_while_collapse_remains_eligible_and_scrollable() {
        let mut state = ComposerScrollGestureState::default();
        assert!(!record(
            &mut state,
            0.0,
            Opts {
                delta_px: 10.0,
                ..Opts::default()
            }
        ));
        assert!(!record(
            &mut state,
            20.0,
            Opts {
                delta_px: 10.0,
                ..Opts::default()
            }
        ));
        assert!(record(
            &mut state,
            40.0,
            Opts {
                delta_px: 4.0,
                ..Opts::default()
            }
        ));
        assert!(!record(
            &mut state,
            60.0,
            Opts {
                delta_px: 10.0,
                collapse_eligible: false,
                ..Opts::default()
            }
        ));
        assert!(!record(
            &mut state,
            80.0,
            Opts {
                delta_px: 14.0,
                ..Opts::default()
            }
        ));
    }

    #[test]
    fn does_not_collapse_while_scrolling_down_through_composer_footer_space() {
        let mut state = ComposerScrollGestureState::default();
        assert!(!record(
            &mut state,
            0.0,
            Opts {
                delta_px: 20.0,
                ..Opts::default()
            }
        ));
        assert!(!record(
            &mut state,
            20.0,
            Opts {
                toward_end: true,
                ..Opts::default()
            }
        ));
        assert!(!record(
            &mut state,
            40.0,
            Opts {
                delta_px: 10.0,
                ..Opts::default()
            }
        ));
    }
}
