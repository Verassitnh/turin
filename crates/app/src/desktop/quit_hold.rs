//! Port of upstream/apps/desktop/src/window/QuitHold.ts (+ QuitHold.test.ts).

use turin_contracts::settings::QuitConfirmationMode;

pub const QUIT_HOLD_DURATION_MS: u64 = 1200;
pub const QUIT_DOUBLE_PRESS_MS: u64 = 500;
pub const QUIT_HOLD_RELEASE_GRACE_MS: u64 = 600;
const QUIT_HOLD_REPEAT_CADENCE_MULTIPLIER: u64 = 2;
/// Synthesized auto-repeat interval. GPUI does not deliver repeats or the
/// `q` key-up while the platform modifier is held.
pub const QUIT_HOLD_CLOCK_MS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitHoldKeyPhase {
    KeyDown,
    KeyUp,
}

/// `q` is the shortcut character. `Modifier` is the platform modifier
/// (`"meta"` / `"control"` upstream), which is not a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitHoldKey {
    Character(char),
    Modifier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuitHoldKeyInput {
    pub phase: QuitHoldKeyPhase,
    pub key: QuitHoldKey,
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

#[derive(Clone, Copy)]
enum WatchdogKind {
    /// Arming and double-press timers dismiss the hint and do not quit.
    Release,
    /// Quiet period after a completed hold.
    Quit,
}

#[derive(Clone, Copy)]
struct Watchdog {
    kind: WatchdogKind,
    deadline: u64,
}

pub struct QuitShortcutHandler {
    platform_darwin: bool,
    now_ms: u64,
    watchdog: Option<Watchdog>,
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
            watchdog: None,
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

    pub fn next_wake_ms(&self) -> Option<u64> {
        self.watchdog.map(|watchdog| watchdog.deadline)
    }

    /// The shell clock keeps synthesizing `q` repeats while this is true.
    pub fn clock_should_run(&self) -> bool {
        self.holding || self.quit_on_release
    }

    pub fn poll_watchdog(
        &mut self,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) -> QuitShortcutEffect {
        let Some(watchdog) = self.watchdog else {
            return QuitShortcutEffect::None;
        };
        if self.now_ms < watchdog.deadline {
            return QuitShortcutEffect::None;
        }
        self.watchdog = None;
        match watchdog.kind {
            WatchdogKind::Release => {
                self.release(true, false, callbacks);
                QuitShortcutEffect::None
            }
            WatchdogKind::Quit => self.quit_now(callbacks),
        }
    }

    /// One synthesized auto-repeat of `q` while the platform modifier is down.
    pub fn held_key_tick(
        &mut self,
        mode: QuitConfirmationMode,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) -> QuitShortcutEffect {
        if !self.clock_should_run() {
            return QuitShortcutEffect::None;
        }
        self.handle(
            QuitHoldKeyInput {
                phase: QuitHoldKeyPhase::KeyDown,
                key: QuitHoldKey::Character('q'),
                meta: self.platform_darwin,
                control: !self.platform_darwin,
                alt: false,
                shift: false,
                is_auto_repeat: true,
            },
            mode,
            callbacks,
        )
    }

    pub fn handle(
        &mut self,
        input: QuitHoldKeyInput,
        mode: QuitConfirmationMode,
        callbacks: &mut QuitShortcutCallbacks<'_>,
    ) -> QuitShortcutEffect {
        let key = character(input.key);
        if input.phase == QuitHoldKeyPhase::KeyUp {
            if key == Some('q') {
                let should_quit = self.quit_on_release;
                self.release(false, true, callbacks);
                if should_quit {
                    return QuitShortcutEffect::Quit;
                }
            } else if is_platform_modifier(input.key) {
                if !self.quit_on_release {
                    self.release(false, true, callbacks);
                } else {
                    self.quit_after_quiet_period();
                }
            }
            return QuitShortcutEffect::None;
        }

        let modifier_down = if self.platform_darwin {
            input.meta
        } else {
            input.control
        };
        if input.is_auto_repeat && modifier_down && key == Some('q') {
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
            if key == Some('q') {
                self.quit_after_quiet_period();
            }
            return QuitShortcutEffect::PreventDefault;
        }

        if !modifier_down || input.alt || input.shift || key != Some('q') {
            if is_platform_modifier(input.key) && !input.alt && !input.shift {
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
                self.quit_after_quiet_period();
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
            let remaining = QUIT_DOUBLE_PRESS_MS.saturating_sub(self.now_ms.saturating_sub(now));
            if remaining == 0 {
                self.release(true, false, callbacks);
                return QuitShortcutEffect::None;
            }
            self.mode = Some(mode);
            self.notified = true;
            (callbacks.notify)(QuitShortcutHint::Down { mode });
            self.watchdog = Some(Watchdog {
                kind: WatchdogKind::Release,
                deadline: self.now_ms + remaining,
            });
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
        self.watchdog = Some(Watchdog {
            kind: WatchdogKind::Release,
            deadline: now + QUIT_HOLD_DURATION_MS + QUIT_HOLD_RELEASE_GRACE_MS,
        });
        QuitShortcutEffect::PreventDefault
    }

    fn quit_after_quiet_period(&mut self) {
        let quiet = QUIT_HOLD_RELEASE_GRACE_MS.max(
            self.repeat_cadence_ms
                .saturating_mul(QUIT_HOLD_REPEAT_CADENCE_MULTIPLIER),
        );
        self.watchdog = Some(Watchdog {
            kind: WatchdogKind::Quit,
            deadline: self.now_ms + quiet,
        });
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
        self.watchdog = None;
        if self.notified {
            self.notified = false;
            (callbacks.notify)(QuitShortcutHint::Up);
        }
    }
}

fn character(key: QuitHoldKey) -> Option<char> {
    match key {
        QuitHoldKey::Character(ch) => Some(ch.to_ascii_lowercase()),
        QuitHoldKey::Modifier => None,
    }
}

fn is_platform_modifier(key: QuitHoldKey) -> bool {
    matches!(key, QuitHoldKey::Modifier)
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

    fn key(phase: QuitHoldKeyPhase, key: QuitHoldKey, repeat: bool) -> QuitHoldKeyInput {
        QuitHoldKeyInput {
            phase,
            key,
            meta: true,
            control: false,
            alt: false,
            shift: false,
            is_auto_repeat: repeat,
        }
    }

    fn down() -> QuitHoldKeyInput {
        key(
            QuitHoldKeyPhase::KeyDown,
            QuitHoldKey::Character('q'),
            false,
        )
    }

    struct Record {
        hints: Vec<QuitShortcutHint>,
        concealed: bool,
    }

    fn drive(
        handler: &mut QuitShortcutHandler,
        input: QuitHoldKeyInput,
        mode: QuitConfirmationMode,
        record: &mut Record,
    ) -> QuitShortcutEffect {
        handler.handle(
            input,
            mode,
            &mut QuitShortcutCallbacks {
                notify: &mut |hint| record.hints.push(hint),
                conceal_window: &mut || record.concealed = true,
            },
        )
    }

    fn poll(handler: &mut QuitShortcutHandler, record: &mut Record) -> QuitShortcutEffect {
        handler.poll_watchdog(&mut QuitShortcutCallbacks {
            notify: &mut |hint| record.hints.push(hint),
            conceal_window: &mut || record.concealed = true,
        })
    }

    #[test]
    fn tap_deadline_dismisses_the_hint_without_quitting() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(0);
        assert_eq!(
            drive(
                &mut handler,
                down(),
                QuitConfirmationMode::Hold,
                &mut record
            ),
            QuitShortcutEffect::PreventDefault
        );
        handler.set_now_ms(QUIT_HOLD_DURATION_MS + QUIT_HOLD_RELEASE_GRACE_MS);
        assert_eq!(poll(&mut handler, &mut record), QuitShortcutEffect::None);
        assert_eq!(
            record.hints,
            vec![
                QuitShortcutHint::Down {
                    mode: QuitConfirmationMode::Hold
                },
                QuitShortcutHint::Up,
            ]
        );
        assert!(!record.concealed);
    }

    #[test]
    fn completed_hold_quits_on_q_key_up() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(0);
        drive(
            &mut handler,
            down(),
            QuitConfirmationMode::Hold,
            &mut record,
        );
        handler.set_now_ms(QUIT_HOLD_DURATION_MS);
        assert_eq!(
            handler.held_key_tick(
                QuitConfirmationMode::Hold,
                &mut QuitShortcutCallbacks {
                    notify: &mut |hint| record.hints.push(hint),
                    conceal_window: &mut || record.concealed = true,
                },
            ),
            QuitShortcutEffect::PreventDefault
        );
        assert!(record.concealed);
        handler.set_now_ms(QUIT_HOLD_DURATION_MS + 50);
        assert_eq!(
            drive(
                &mut handler,
                key(QuitHoldKeyPhase::KeyUp, QuitHoldKey::Character('q'), false),
                QuitConfirmationMode::Hold,
                &mut record,
            ),
            QuitShortcutEffect::Quit
        );
    }

    #[test]
    fn completed_hold_quits_after_the_quiet_period() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(0);
        drive(
            &mut handler,
            down(),
            QuitConfirmationMode::Hold,
            &mut record,
        );
        handler.set_now_ms(QUIT_HOLD_DURATION_MS);
        handler.held_key_tick(
            QuitConfirmationMode::Hold,
            &mut QuitShortcutCallbacks {
                notify: &mut |_| {},
                conceal_window: &mut || record.concealed = true,
            },
        );
        assert!(record.concealed);
        handler.set_now_ms(QUIT_HOLD_DURATION_MS + 40);
        drive(
            &mut handler,
            key(QuitHoldKeyPhase::KeyUp, QuitHoldKey::Modifier, false),
            QuitConfirmationMode::Hold,
            &mut record,
        );
        let deadline = handler.next_wake_ms().expect("quiet period");
        handler.set_now_ms(deadline);
        assert_eq!(poll(&mut handler, &mut record), QuitShortcutEffect::Quit);
    }

    #[test]
    fn modifier_release_before_the_hold_cancels() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(0);
        drive(
            &mut handler,
            down(),
            QuitConfirmationMode::Hold,
            &mut record,
        );
        handler.set_now_ms(400);
        assert_eq!(
            drive(
                &mut handler,
                key(QuitHoldKeyPhase::KeyUp, QuitHoldKey::Modifier, false),
                QuitConfirmationMode::Hold,
                &mut record,
            ),
            QuitShortcutEffect::None
        );
        assert!(record.hints.ends_with(&[QuitShortcutHint::Up]));
        handler.set_now_ms(400 + QUIT_HOLD_DURATION_MS + QUIT_HOLD_RELEASE_GRACE_MS);
        assert_eq!(poll(&mut handler, &mut record), QuitShortcutEffect::None);
        assert!(!record.concealed);
    }

    #[test]
    fn direct_mode_quits_immediately() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(10);
        assert_eq!(
            drive(
                &mut handler,
                down(),
                QuitConfirmationMode::Direct,
                &mut record
            ),
            QuitShortcutEffect::Quit
        );
    }

    #[test]
    fn double_press_quits_immediately() {
        let mut handler = QuitShortcutHandler::new(true);
        let mut record = Record {
            hints: Vec::new(),
            concealed: false,
        };
        handler.set_now_ms(1_000);
        drive(
            &mut handler,
            down(),
            QuitConfirmationMode::Hold,
            &mut record,
        );
        handler.set_now_ms(1_200);
        assert_eq!(
            drive(
                &mut handler,
                down(),
                QuitConfirmationMode::Hold,
                &mut record
            ),
            QuitShortcutEffect::Quit
        );
    }
}
