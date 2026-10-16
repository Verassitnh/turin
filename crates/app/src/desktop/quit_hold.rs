//! Port of upstream/apps/desktop/src/window/QuitHold.ts (+ QuitHold.test.ts).

use turin_contracts::settings::QuitConfirmationMode;

pub const QUIT_HOLD_DURATION_MS: u64 = 1200;
pub const QUIT_DOUBLE_PRESS_MS: u64 = 500;
pub const QUIT_HOLD_RELEASE_GRACE_MS: u64 = 600;
const QUIT_HOLD_REPEAT_CADENCE_MULTIPLIER: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitHoldKeyPhase {
    KeyDown,
    KeyUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuitHoldKeyInput {
    pub phase: QuitHoldKeyPhase,
    pub key: char,
    pub meta: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub is_auto_repeat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitShortcutHint {
    Down { mode: QuitConfirmationMode },
    Up,
}

pub struct QuitShortcutHandler {
    platform_darwin: bool,
    now_ms: u64,
    watchdog_deadline: Option<u64>,
    holding: bool,
    mode: Option<QuitConfirmationMode>,
    notified: bool,
    armed: bool,
    quit_on_release: bool,
    held_since: u64,
    last_press_at: u64,
    last_repeat_at: u64,
    repeat_cadence_ms: u64,
    generation: u64,
}

impl QuitShortcutHandler {
    pub fn new(platform_darwin: bool) -> Self {
        Self {
            platform_darwin,
            now_ms: 0,
            watchdog_deadline: None,
            holding: false,
            mode: None,
            notified: false,
            armed: false,
            quit_on_release: false,
            held_since: 0,
            last_press_at: 0,
            last_repeat_at: 0,
            repeat_cadence_ms: 0,
            generation: 0,
        }
    }

    pub fn set_now_ms(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    pub fn poll_watchdog(
        &mut self,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) -> QuitShortcutEffect {
        if let Some(deadline) = self.watchdog_deadline
            && self.now_ms >= deadline
        {
            self.watchdog_deadline = None;
            return self.quit_now(callbacks);
        }
        QuitShortcutEffect::None
    }

    pub fn handle(
        &mut self,
        input: QuitHoldKeyInput,
        mode: QuitConfirmationMode,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) -> QuitShortcutEffect {
        let key = input.key.to_ascii_lowercase();
        if input.phase == QuitHoldKeyPhase::KeyUp {
            if key == 'q' {
                let should_quit = self.quit_on_release;
                self.release(false, true, callbacks);
                if should_quit {
                    return QuitShortcutEffect::Quit;
                }
            } else if key == 'm' && self.platform_darwin && input.meta
                || key == 'c' && !self.platform_darwin && input.control
            {
                if !self.quit_on_release {
                    self.release(false, true, callbacks);
                } else {
                    self.quit_after_quiet_period(callbacks);
                }
            }
            return QuitShortcutEffect::None;
        }

        let modifier_down = if self.platform_darwin {
            input.meta
        } else {
            input.control
        };
        if input.is_auto_repeat && modifier_down && key == 'q' {
            let now = self.now_ms;
            self.repeat_cadence_ms = now
                - if self.last_repeat_at == 0 {
                    self.held_since
                } else {
                    self.last_repeat_at
                };
            self.last_repeat_at = now;
        }
        if self.quit_on_release {
            if key == 'q' {
                self.quit_after_quiet_period(callbacks);
            }
            return QuitShortcutEffect::PreventDefault;
        }

        if !modifier_down || input.alt || input.shift || key != 'q' {
            if key == if self.platform_darwin { 'm' } else { 'c' } && !input.alt && !input.shift {
                return QuitShortcutEffect::None;
            }
            if !input.is_auto_repeat {
                self.last_press_at = 0;
                self.release(true, false, callbacks);
            }
            return QuitShortcutEffect::None;
        }

        if input.is_auto_repeat {
            if self.mode == Some(QuitConfirmationMode::Hold)
                && self.armed
                && self.now_ms.saturating_sub(self.held_since) >= QUIT_HOLD_DURATION_MS
            {
                self.armed = false;
                self.quit_on_release = true;
                (callbacks.conceal_window)();
                self.quit_after_quiet_period(callbacks);
            }
            return QuitShortcutEffect::PreventDefault;
        }

        let now = self.now_ms;
        let previous_press_at = self.last_press_at;
        self.last_press_at = now;
        if self.holding || self.notified {
            self.release(true, false, callbacks);
        }
        self.generation += 1;
        if previous_press_at != 0 && now.saturating_sub(previous_press_at) <= QUIT_DOUBLE_PRESS_MS {
            return self.quit_now(callbacks);
        }

        self.holding = true;
        self.held_since = now;
        if mode == QuitConfirmationMode::Direct {
            return self.quit_now(callbacks);
        }
        if mode == QuitConfirmationMode::DoubleClick {
            let remaining = QUIT_DOUBLE_PRESS_MS.saturating_sub(now.saturating_sub(now));
            if remaining == 0 {
                self.release(true, false, callbacks);
                return QuitShortcutEffect::None;
            }
            self.mode = Some(mode);
            self.notified = true;
            (callbacks.notify)(QuitShortcutHint::Down { mode });
            self.watchdog_deadline = Some(now + QUIT_DOUBLE_PRESS_MS);
            return QuitShortcutEffect::PreventDefault;
        }

        if !self.holding {
            return QuitShortcutEffect::PreventDefault;
        }
        self.mode = Some(QuitConfirmationMode::Hold);
        self.notified = true;
        (callbacks.notify)(QuitShortcutHint::Down {
            mode: QuitConfirmationMode::Hold,
        });
        self.armed = true;
        self.watchdog_deadline = Some(now + QUIT_HOLD_DURATION_MS + QUIT_HOLD_RELEASE_GRACE_MS);
        QuitShortcutEffect::PreventDefault
    }

    fn quit_after_quiet_period(&mut self, callbacks: &mut QuitShortcutCallbacks<'_>) {
        let quiet = QUIT_HOLD_RELEASE_GRACE_MS.max(
            self.repeat_cadence_ms
                .saturating_mul(QUIT_HOLD_REPEAT_CADENCE_MULTIPLIER),
        );
        self.watchdog_deadline = Some(self.now_ms + quiet);
        let _ = callbacks;
    }

    fn quit_now(&mut self, callbacks: &mut QuitShortcutCallbacks<'_>) -> QuitShortcutEffect {
        self.release(true, false, callbacks);
        self.last_press_at = 0;
        QuitShortcutEffect::Quit
    }

    fn release(
        &mut self,
        cancel_pending_mode: bool,
        keep_double_press_hint: bool,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) {
        if cancel_pending_mode {
            self.generation += 1;
        }
        if !self.holding && !self.notified {
            return;
        }
        let keep_hint = keep_double_press_hint
            && self.mode == Some(QuitConfirmationMode::DoubleClick)
            && self.notified;
        self.holding = false;
        self.armed = false;
        self.quit_on_release = false;
        self.last_repeat_at = 0;
        self.repeat_cadence_ms = 0;
        if keep_hint {
            return;
        }
        self.mode = None;
        self.watchdog_deadline = None;
        if self.notified {
            self.notified = false;
            (callbacks.notify)(QuitShortcutHint::Up);
        }
    }
}

pub struct QuitShortcutCallbacks<'a> {
    pub notify: &'a mut dyn FnMut(QuitShortcutHint),
    pub conceal_window: &'a mut dyn FnMut(),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitShortcutEffect {
    None,
    PreventDefault,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_shows_hint_without_quitting() {
        let mut hints = Vec::new();
        let mut handler = QuitShortcutHandler::new(true);
        handler.set_now_ms(0);
        let mut conceal = false;
        let effect = handler.handle(
            QuitHoldKeyInput {
                phase: QuitHoldKeyPhase::KeyDown,
                key: 'q',
                meta: true,
                control: false,
                alt: false,
                shift: false,
                is_auto_repeat: false,
            },
            QuitConfirmationMode::Hold,
            &mut QuitShortcutCallbacks {
                notify: &mut |hint| hints.push(hint),
                conceal_window: &mut || conceal = true,
            },
        );
        assert_eq!(effect, QuitShortcutEffect::PreventDefault);
        assert_eq!(
            hints,
            vec![QuitShortcutHint::Down {
                mode: QuitConfirmationMode::Hold
            }]
        );
        assert!(!conceal);
    }

    #[test]
    fn double_press_quits_immediately() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut callbacks = QuitShortcutCallbacks {
            notify: &mut |_| {},
            conceal_window: &mut || {},
        };
        handler.set_now_ms(1_000);
        handler.handle(
            QuitHoldKeyInput {
                phase: QuitHoldKeyPhase::KeyDown,
                key: 'q',
                meta: true,
                control: false,
                alt: false,
                shift: false,
                is_auto_repeat: false,
            },
            QuitConfirmationMode::Hold,
            &mut callbacks,
        );
        handler.set_now_ms(1_200);
        let effect = handler.handle(
            QuitHoldKeyInput {
                phase: QuitHoldKeyPhase::KeyDown,
                key: 'q',
                meta: true,
                control: false,
                alt: false,
                shift: false,
                is_auto_repeat: false,
            },
            QuitConfirmationMode::Hold,
            &mut callbacks,
        );
        assert_eq!(effect, QuitShortcutEffect::Quit);
    }
}
