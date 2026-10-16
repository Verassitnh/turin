//! Port of upstream/apps/web/src/shortcutModifierState.ts.
//!
//! The pure state transition plus a tracker that stands in for
//! `useShortcutModifierState`'s window listeners: the host forwards key, paste,
//! blur and focus events and re-renders only when a call reports a change.

// Ported ahead of the views that consume it.
#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShortcutModifierState {
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

pub const EMPTY_SHORTCUT_MODIFIER_STATE: ShortcutModifierState = ShortcutModifierState {
    meta_key: false,
    ctrl_key: false,
    alt_key: false,
    shift_key: false,
};

pub fn are_shortcut_modifier_states_equal(
    left: ShortcutModifierState,
    right: ShortcutModifierState,
) -> bool {
    left == right
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventType {
    KeyDown,
    KeyUp,
}

/// The modifier flags a keyboard event carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardModifierFlags {
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModifierKey {
    Meta,
    Ctrl,
    Alt,
    Shift,
}

fn normalize_modifier_key(key: &str) -> Option<ModifierKey> {
    match key {
        "Meta" | "OS" | "Command" => Some(ModifierKey::Meta),
        "Control" => Some(ModifierKey::Ctrl),
        "Alt" | "Option" => Some(ModifierKey::Alt),
        "Shift" => Some(ModifierKey::Shift),
        _ => None,
    }
}

pub fn shortcut_modifier_state_after_keyboard_event(
    current_state: ShortcutModifierState,
    key: &str,
    event_type: KeyEventType,
    flags: KeyboardModifierFlags,
) -> ShortcutModifierState {
    let next_state = if let Some(modifier) = normalize_modifier_key(key) {
        let down = event_type == KeyEventType::KeyDown;
        let mut next = current_state;
        match modifier {
            ModifierKey::Meta => next.meta_key = down,
            ModifierKey::Ctrl => next.ctrl_key = down,
            ModifierKey::Alt => next.alt_key = down,
            ModifierKey::Shift => next.shift_key = down,
        }
        next
    } else {
        // Flags on non-modifier keys may only clear a bit, never set one. After
        // a dictation tool's synthetic ⌘V (Wispr Flow), the browser can keep
        // reporting metaKey=true on real key events (Enter to submit) until the
        // user physically taps ⌘. Trusting that flag would mark ⌘ as held and
        // stick the thread jump hints. Setting a bit requires a real modifier
        // keydown, handled above.
        ShortcutModifierState {
            meta_key: current_state.meta_key && flags.meta_key,
            ctrl_key: current_state.ctrl_key && flags.ctrl_key,
            alt_key: current_state.alt_key && flags.alt_key,
            shift_key: current_state.shift_key && flags.shift_key,
        }
    };
    if current_state == next_state {
        current_state
    } else {
        next_state
    }
}

/// `useShortcutModifierState`'s listener logic.
#[derive(Debug)]
pub struct ShortcutModifierTracker {
    state: ShortcutModifierState,
    ignore_editable: bool,
}

impl ShortcutModifierTracker {
    pub fn new(ignore_editable: bool) -> Self {
        Self {
            state: EMPTY_SHORTCUT_MODIFIER_STATE,
            ignore_editable,
        }
    }

    pub fn state(&self) -> ShortcutModifierState {
        self.state
    }

    /// Even a no-op state dispatch can cost work in the sidebar's large tree.
    /// Ordinary typing must return before dispatching an update, so this
    /// reports whether the state changed.
    fn update_state(&mut self, next: ShortcutModifierState) -> bool {
        if self.state == next {
            return false;
        }
        self.state = next;
        true
    }

    /// `keydown` / `keyup`. `target_is_editable` is `isEditableFocused(event.target)`.
    pub fn on_keyboard_event(
        &mut self,
        key: &str,
        event_type: KeyEventType,
        flags: KeyboardModifierFlags,
        target_is_editable: bool,
    ) -> bool {
        let next = if self.ignore_editable && target_is_editable {
            EMPTY_SHORTCUT_MODIFIER_STATE
        } else {
            shortcut_modifier_state_after_keyboard_event(self.state, key, event_type, flags)
        };
        self.update_state(next)
    }

    /// Dictation tools (Wispr Flow) paste with a synthetic ⌘V whose Meta keyup
    /// never reaches the page, so the tracked state stays "⌘ held" forever and
    /// the thread jump hints stick on screen. A paste is never jump intent, so
    /// `paste` and `blur` reset. A physically held modifier re-registers on the
    /// next real key event.
    pub fn on_reset_event(&mut self) -> bool {
        self.update_state(EMPTY_SHORTCUT_MODIFIER_STATE)
    }

    /// `focusin`.
    pub fn on_focus_in(&mut self, target_is_editable: bool) -> bool {
        if self.ignore_editable && target_is_editable {
            return self.on_reset_event();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/shortcutModifierState.test.ts.
    use super::*;

    fn press(
        tracker: &mut ShortcutModifierTracker,
        event_type: KeyEventType,
        key: &str,
        shift_key: bool,
    ) -> bool {
        tracker.on_keyboard_event(
            key,
            event_type,
            KeyboardModifierFlags {
                shift_key,
                ..Default::default()
            },
            false,
        )
    }

    #[derive(Clone, Copy, Debug)]
    enum Reset {
        KeyUp,
        Paste,
        Blur,
    }

    #[test]
    fn does_not_render_for_unchanged_modifiers_after_a_reset() {
        for reset in [Reset::KeyUp, Reset::Paste, Reset::Blur] {
            let mut tracker = ShortcutModifierTracker::new(false);
            assert!(press(&mut tracker, KeyEventType::KeyDown, "Shift", true));
            assert_eq!(
                tracker.state(),
                ShortcutModifierState {
                    shift_key: true,
                    ..EMPTY_SHORTCUT_MODIFIER_STATE
                }
            );
            let changed = match reset {
                Reset::KeyUp => press(&mut tracker, KeyEventType::KeyUp, "Shift", false),
                Reset::Paste | Reset::Blur => tracker.on_reset_event(),
            };
            assert!(changed, "{reset:?}");
            assert_eq!(tracker.state(), EMPTY_SHORTCUT_MODIFIER_STATE);

            let mut renders = 0;
            for key in "typing".chars().map(|c| c.to_string()) {
                renders += usize::from(press(&mut tracker, KeyEventType::KeyDown, &key, false));
                renders += usize::from(press(&mut tracker, KeyEventType::KeyUp, &key, false));
            }
            renders += usize::from(tracker.on_reset_event());
            renders += usize::from(tracker.on_reset_event());
            assert_eq!(renders, 0, "{reset:?}");
            assert!(press(&mut tracker, KeyEventType::KeyDown, "Shift", true));
        }
    }

    #[test]
    fn a_stale_meta_flag_on_a_real_key_never_sets_the_bit() {
        let flags = KeyboardModifierFlags {
            meta_key: true,
            ..Default::default()
        };
        let state = shortcut_modifier_state_after_keyboard_event(
            EMPTY_SHORTCUT_MODIFIER_STATE,
            "Enter",
            KeyEventType::KeyDown,
            flags,
        );
        assert_eq!(state, EMPTY_SHORTCUT_MODIFIER_STATE);

        let held = shortcut_modifier_state_after_keyboard_event(
            EMPTY_SHORTCUT_MODIFIER_STATE,
            "Command",
            KeyEventType::KeyDown,
            flags,
        );
        assert!(held.meta_key);
        // A non-modifier key whose flag says the modifier is gone clears it.
        let cleared = shortcut_modifier_state_after_keyboard_event(
            held,
            "a",
            KeyEventType::KeyDown,
            KeyboardModifierFlags::default(),
        );
        assert_eq!(cleared, EMPTY_SHORTCUT_MODIFIER_STATE);
    }

    #[test]
    fn modifier_aliases_map_to_their_bits() {
        let down = |key: &str| {
            shortcut_modifier_state_after_keyboard_event(
                EMPTY_SHORTCUT_MODIFIER_STATE,
                key,
                KeyEventType::KeyDown,
                KeyboardModifierFlags::default(),
            )
        };
        assert!(down("Meta").meta_key && down("OS").meta_key && down("Command").meta_key);
        assert!(down("Control").ctrl_key);
        assert!(down("Alt").alt_key && down("Option").alt_key);
        assert!(down("Shift").shift_key);
        assert!(are_shortcut_modifier_states_equal(
            down("Shift"),
            ShortcutModifierState {
                shift_key: true,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn editable_focus_resets_only_when_ignoring_editables() {
        let mut ignoring = ShortcutModifierTracker::new(true);
        assert!(ignoring.on_keyboard_event(
            "Shift",
            KeyEventType::KeyDown,
            KeyboardModifierFlags::default(),
            false
        ));
        // Typing in an editable clears and ignores modifier state.
        assert!(ignoring.on_keyboard_event(
            "Shift",
            KeyEventType::KeyDown,
            KeyboardModifierFlags::default(),
            true
        ));
        assert_eq!(ignoring.state(), EMPTY_SHORTCUT_MODIFIER_STATE);
        assert!(!ignoring.on_focus_in(true));

        let mut tracking = ShortcutModifierTracker::new(false);
        tracking.on_keyboard_event(
            "Shift",
            KeyEventType::KeyDown,
            KeyboardModifierFlags::default(),
            true,
        );
        assert!(!tracking.on_focus_in(true));
        assert!(tracking.state().shift_key);
    }
}
