//! Port of upstream/apps/web/src/components/chat/timelineMinimapWheel.ts
//!
//! A wheel over the minimap scrolls the timeline instead. Upstream replays a synthetic wheel
//! event on the timeline (so its own listeners can veto) and then `scrollBy`s; the decision and
//! the unit conversion are here, and the view applies them.

/// `WheelEvent.deltaMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelDeltaMode {
    Pixel,
    Line,
    Page,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapWheelEvent {
    pub default_prevented: bool,
    pub ctrl_key: bool,
    pub delta_y: f64,
    pub delta_mode: WheelDeltaMode,
}

/// What forwarding a wheel does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelForward {
    /// Pinch zoom, a horizontal wheel or an already-handled event: the minimap keeps it.
    Ignore,
    /// The minimap's own scroll is cancelled and the timeline sees the replayed wheel; scroll it
    /// by `scroll_by` pixels unless one of its listeners cancels the replay.
    Replay { scroll_by: f64 },
}

pub fn forward_wheel_to_timeline(
    event: &MinimapWheelEvent,
    timeline_client_height: f64,
) -> WheelForward {
    if event.default_prevented || event.ctrl_key || event.delta_y == 0.0 {
        return WheelForward::Ignore;
    }
    let pixels_per_unit = match event.delta_mode {
        WheelDeltaMode::Line => 16.0,
        WheelDeltaMode::Page => timeline_client_height,
        WheelDeltaMode::Pixel => 1.0,
    };
    WheelForward::Replay {
        scroll_by: event.delta_y * pixels_per_unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMELINE_HEIGHT: f64 = 500.0;

    fn wheel(delta_y: f64, delta_mode: WheelDeltaMode) -> MinimapWheelEvent {
        MinimapWheelEvent {
            default_prevented: false,
            ctrl_key: false,
            delta_y,
            delta_mode,
        }
    }

    #[test]
    fn replays_the_wheel_on_the_timeline_and_scrolls_it_instead_of_the_minimap() {
        assert_eq!(
            forward_wheel_to_timeline(&wheel(-120.0, WheelDeltaMode::Pixel), TIMELINE_HEIGHT),
            WheelForward::Replay { scroll_by: -120.0 }
        );
    }

    #[test]
    fn converts_delta_mode_to_pixels() {
        assert_eq!(
            forward_wheel_to_timeline(&wheel(3.0, WheelDeltaMode::Line), TIMELINE_HEIGHT),
            WheelForward::Replay { scroll_by: 48.0 }
        );
        assert_eq!(
            forward_wheel_to_timeline(&wheel(3.0, WheelDeltaMode::Page), TIMELINE_HEIGHT),
            WheelForward::Replay { scroll_by: 1500.0 }
        );
    }

    #[test]
    fn leaves_pinch_zoom_and_horizontal_wheels_alone() {
        let pinch = MinimapWheelEvent {
            ctrl_key: true,
            ..wheel(40.0, WheelDeltaMode::Pixel)
        };
        assert_eq!(
            forward_wheel_to_timeline(&pinch, TIMELINE_HEIGHT),
            WheelForward::Ignore
        );
        // A horizontal-only wheel has no `deltaY`.
        assert_eq!(
            forward_wheel_to_timeline(&wheel(0.0, WheelDeltaMode::Pixel), TIMELINE_HEIGHT),
            WheelForward::Ignore
        );
    }

    #[test]
    fn skips_events_already_handled() {
        let handled = MinimapWheelEvent {
            default_prevented: true,
            ..wheel(40.0, WheelDeltaMode::Pixel)
        };
        assert_eq!(
            forward_wheel_to_timeline(&handled, TIMELINE_HEIGHT),
            WheelForward::Ignore
        );
    }
}
