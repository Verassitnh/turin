//! Port of upstream/apps/web/src/panelAnimations.ts (+ test).
//!
//! The runtime half of the `panelAnimationDurationMs` setting (the setting and
//! its preview are `settings::panels::appearance`). Upstream's React context
//! and hooks are glue; the state machines are kept as plain structs the view
//! that owns a panel drives:
//!
//! - [`PanelNavigationSuppression`] (`usePanelNavigationSuppression`): panel
//!   motion is off until the first two frames after a route or navigation key
//!   change have painted, so state a route restores is visible immediately.
//! - [`panel_animation_settings`] (`usePanelAnimationSettings`): the active
//!   gate, `duration > 0 && !reduced motion && !suppressed`. The OS
//!   reduced-motion flag (`matchMedia("(prefers-reduced-motion: reduce)")`) is
//!   passed in.
//! - [`PanelPresence`] (`usePanelPresence`): keeps closing panel content and
//!   its value alive for the animation's duration, scoped by a key. The
//!   `setTimeout` becomes a deadline in epoch milliseconds the caller checks
//!   with [`PanelPresence::tick`] (and schedules a repaint at
//!   [`PanelPresence::deadline`]).
//!
//! The sidebar's own width animation is `components::sidebar::sidebar_motion`.

use turin_contracts::settings::PanelAnimationDurationMs;

/// `usePanelNavigationSuppression`. `on_frame` is one painted frame
/// (upstream chains two `requestAnimationFrame`s).
#[derive(Debug, Clone, Default)]
pub struct PanelNavigationSuppression {
    painted_navigation_key: Option<String>,
    tracking_key: Option<String>,
    frames_seen: u8,
}

impl PanelNavigationSuppression {
    pub fn new() -> Self {
        Self::default()
    }

    /// `paintedNavigationKey !== navigationKey`: true for the initial route
    /// and after each navigation, until its frames have painted.
    pub fn is_suppressed(&self, navigation_key: &str) -> bool {
        self.painted_navigation_key.as_deref() != Some(navigation_key)
    }

    /// A frame painted while `navigation_key` is current. Returns whether the
    /// suppression just ended. A key change restarts the count, as the
    /// effect's cleanup cancels the pending frames.
    pub fn on_frame(&mut self, navigation_key: &str) -> bool {
        if !self.is_suppressed(navigation_key) {
            return false;
        }
        if self.tracking_key.as_deref() != Some(navigation_key) {
            self.tracking_key = Some(navigation_key.to_owned());
            self.frames_seen = 0;
        }
        self.frames_seen += 1;
        if self.frames_seen < 2 {
            return false;
        }
        self.painted_navigation_key = Some(navigation_key.to_owned());
        self.tracking_key = None;
        self.frames_seen = 0;
        true
    }
}

/// What `usePanelAnimationSettings` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelAnimationSettings {
    pub active: bool,
    pub duration_ms: PanelAnimationDurationMs,
}

pub fn panel_animation_settings(
    duration_ms: PanelAnimationDurationMs,
    prefers_reduced_motion: bool,
    suppressed: bool,
) -> PanelAnimationSettings {
    PanelAnimationSettings {
        active: duration_ms.get() > 0 && !prefers_reduced_motion && !suppressed,
        duration_ms,
    }
}

/// `{ present, value }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelPresenceState<T> {
    pub present: bool,
    pub value: Option<T>,
}

#[derive(Debug, Clone)]
struct Retained<T> {
    scope_key: Option<String>,
    value: Option<T>,
}

/// `usePanelPresence`.
#[derive(Debug, Clone)]
pub struct PanelPresence<T> {
    present: bool,
    retained: Option<Retained<T>>,
    /// The close timer: when `present` drops. `None` when no timer runs.
    deadline: Option<i64>,
    /// The timer effect's dependencies; it restarts only when they change.
    timer_deps: Option<(bool, bool, i64)>,
}

impl<T: Clone> PanelPresence<T> {
    /// `useState(open)` and `useRef(open ? { scopeKey, value } : null)`.
    pub fn new(open: bool, value: Option<T>, scope_key: Option<&str>) -> Self {
        Self {
            present: open,
            retained: open.then(|| Retained {
                scope_key: scope_key.map(str::to_owned),
                value,
            }),
            deadline: None,
            timer_deps: None,
        }
    }

    /// One render of the hook: returns what to show, then runs the effects
    /// (retain the open value; start, restart or cancel the close timer).
    pub fn update(
        &mut self,
        now_ms: i64,
        open: bool,
        value: Option<T>,
        animated: bool,
        scope_key: Option<&str>,
        duration_ms: PanelAnimationDurationMs,
    ) -> PanelPresenceState<T> {
        let retained_matches = self
            .retained
            .as_ref()
            .is_some_and(|retained| retained.scope_key.as_deref() == scope_key);
        let retained_value = if retained_matches {
            self.retained
                .as_ref()
                .and_then(|retained| retained.value.clone())
        } else {
            None
        };
        let visible = open || (animated && self.present && retained_matches);
        let result = PanelPresenceState {
            present: visible,
            value: if open {
                value.clone()
            } else if visible {
                retained_value
            } else {
                None
            },
        };

        if open {
            self.retained = Some(Retained {
                scope_key: scope_key.map(str::to_owned),
                value,
            });
        }

        let deps = (animated, open, duration_ms.get());
        if open {
            self.present = true;
            self.deadline = None;
        } else if !animated {
            self.present = false;
            self.deadline = None;
        } else if self.timer_deps != Some(deps) {
            self.deadline = Some(now_ms + duration_ms.get());
        }
        self.timer_deps = Some(deps);
        result
    }

    /// When the close timer fires, if one is running.
    pub fn deadline(&self) -> Option<i64> {
        self.deadline
    }

    /// The close timer, polled. Returns whether the panel just went away.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        match self.deadline {
            Some(deadline) if now_ms >= deadline => {
                self.deadline = None;
                self.present = false;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duration(ms: i64) -> PanelAnimationDurationMs {
        PanelAnimationDurationMs::new(ms).expect("in range")
    }

    #[test]
    fn suppresses_initial_and_navigated_panel_state_until_each_route_has_painted() {
        let mut suppression = PanelNavigationSuppression::new();
        assert!(suppression.is_suppressed("/thread/one"));

        assert!(!suppression.on_frame("/thread/one"));
        assert!(suppression.is_suppressed("/thread/one"));
        assert!(suppression.on_frame("/thread/one"));
        assert!(!suppression.is_suppressed("/thread/one"));

        assert!(suppression.is_suppressed("/thread/two"));
        assert!(!suppression.on_frame("/thread/two"));
        assert!(suppression.is_suppressed("/thread/two"));
        assert!(suppression.on_frame("/thread/two"));
        assert!(!suppression.is_suppressed("/thread/two"));
    }

    #[test]
    fn a_navigation_mid_flight_restarts_the_frame_count() {
        let mut suppression = PanelNavigationSuppression::new();
        assert!(!suppression.on_frame("/a"));
        assert!(!suppression.on_frame("/b"));
        assert!(suppression.is_suppressed("/b"));
        assert!(suppression.on_frame("/b"));
        assert!(suppression.is_suppressed("/a"));
    }

    #[test]
    fn frames_after_the_route_has_painted_change_nothing() {
        let mut suppression = PanelNavigationSuppression::new();
        suppression.on_frame("/a");
        suppression.on_frame("/a");
        assert!(!suppression.on_frame("/a"));
        assert!(!suppression.is_suppressed("/a"));
    }

    #[test]
    fn the_active_gate_needs_a_duration_and_no_reduced_motion_and_no_suppression() {
        assert!(panel_animation_settings(duration(200), false, false).active);
        assert!(!panel_animation_settings(duration(0), false, false).active);
        assert!(!panel_animation_settings(duration(200), true, false).active);
        assert!(!panel_animation_settings(duration(200), false, true).active);
        assert_eq!(
            panel_animation_settings(duration(120), false, false).duration_ms,
            duration(120)
        );
    }

    #[test]
    fn an_open_panel_shows_its_value() {
        let mut presence = PanelPresence::new(true, Some("a"), Some("scope"));
        assert_eq!(
            presence.update(0, true, Some("a"), true, Some("scope"), duration(200)),
            PanelPresenceState {
                present: true,
                value: Some("a")
            }
        );
    }

    #[test]
    fn a_closing_animated_panel_keeps_its_content_for_the_duration() {
        let mut presence = PanelPresence::new(true, Some("a"), Some("scope"));
        presence.update(0, true, Some("a"), true, Some("scope"), duration(200));

        let closing = presence.update(1_000, false, None, true, Some("scope"), duration(200));
        assert_eq!(
            closing,
            PanelPresenceState {
                present: true,
                value: Some("a")
            }
        );
        assert_eq!(presence.deadline(), Some(1_200));

        assert!(!presence.tick(1_199));
        let still = presence.update(1_199, false, None, true, Some("scope"), duration(200));
        assert!(still.present);
        // A re-render during the close does not restart the timer.
        assert_eq!(presence.deadline(), Some(1_200));

        assert!(presence.tick(1_200));
        assert_eq!(
            presence.update(1_201, false, None, true, Some("scope"), duration(200)),
            PanelPresenceState {
                present: false,
                value: None
            }
        );
    }

    #[test]
    fn a_closing_panel_that_is_not_animated_goes_away_at_once() {
        let mut presence = PanelPresence::new(true, Some("a"), Some("scope"));
        presence.update(0, true, Some("a"), false, Some("scope"), duration(200));
        // The render that closes still sees `present`, but without animation
        // nothing is visible.
        let closing = presence.update(10, false, None, false, Some("scope"), duration(200));
        assert_eq!(
            closing,
            PanelPresenceState {
                present: false,
                value: None
            }
        );
        assert_eq!(presence.deadline(), None);
    }

    #[test]
    fn retained_content_is_scoped_to_its_key() {
        let mut presence = PanelPresence::new(true, Some("a"), Some("thread-1"));
        presence.update(0, true, Some("a"), true, Some("thread-1"), duration(200));
        let other_scope = presence.update(5, false, None, true, Some("thread-2"), duration(200));
        assert_eq!(
            other_scope,
            PanelPresenceState {
                present: false,
                value: None
            }
        );
    }

    #[test]
    fn reopening_cancels_the_close_timer() {
        let mut presence = PanelPresence::new(true, Some("a"), None);
        presence.update(0, true, Some("a"), true, None, duration(200));
        presence.update(10, false, None, true, None, duration(200));
        assert!(presence.deadline().is_some());
        let reopened = presence.update(50, true, Some("b"), true, None, duration(200));
        assert_eq!(
            reopened,
            PanelPresenceState {
                present: true,
                value: Some("b")
            }
        );
        assert_eq!(presence.deadline(), None);
        assert!(!presence.tick(10_000));
    }

    #[test]
    fn a_panel_that_starts_closed_is_absent() {
        let mut presence = PanelPresence::<&str>::new(false, None, None);
        assert_eq!(
            presence.update(0, false, None, true, None, duration(200)),
            PanelPresenceState {
                present: false,
                value: None
            }
        );
    }
}
