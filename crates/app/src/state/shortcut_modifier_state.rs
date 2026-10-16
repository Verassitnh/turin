//! Port of upstream/apps/web/src/shortcutModifierState.ts (+ test).
//!
//! Which shortcut modifiers are held, for UI that reacts to them (the sidebar's
//! thread jump hints). The sidebar reads gpui's `ModifiersChangedEvent`
//! directly, which is authoritative, so it does not need the key-event
//! tracking: that exists to work around a browser that only reports modifiers
//! through key events and loses a Meta keyup after a dictation tool's
//! synthetic paste. [`ShortcutModifierTracker`] keeps the logic, including the
//! `ignoreEditable` option the sidebar does not use (reset while an editable
//! element is focused; `isEditableFocused` is the caller's), for a consumer
//! that only sees key events.
//!
//! The React hook, its window listeners and its `useState` become the
//! tracker's methods, each returning the new state only when it changed
//! ("even a no-op state dispatch can cost work in the sidebar's large tree").

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    left: &ShortcutModifierState,
    right: &ShortcutModifierState,
) -> bool {
    left == right
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// `keydown` or `keyup`: `event.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardEventKind {
    KeyDown,
    KeyUp,
}

/// The parts of a `KeyboardEvent` the state reads.
#[derive(Debug, Clone, Copy)]
pub struct ShortcutKeyboardEvent<'a> {
    pub kind: KeyboardEventKind,
    pub key: &'a str,
    pub modifiers: ShortcutModifierState,
}

pub fn shortcut_modifier_state_after_keyboard_event(
    current_state: ShortcutModifierState,
    event: &ShortcutKeyboardEvent<'_>,
) -> ShortcutModifierState {
    if let Some(modifier) = normalize_modifier_key(event.key) {
        let pressed = event.kind == KeyboardEventKind::KeyDown;
        let mut next = current_state;
        match modifier {
            ModifierKey::Meta => next.meta_key = pressed,
            ModifierKey::Ctrl => next.ctrl_key = pressed,
            ModifierKey::Alt => next.alt_key = pressed,
            ModifierKey::Shift => next.shift_key = pressed,
        }
        return next;
    }
    // Flags on non-modifier keys may only clear a bit, never set one. After a
    // dictation tool's synthetic ⌘V (Wispr Flow), the browser can keep
    // reporting metaKey=true on real key events (Enter to submit) until the
    // user physically taps ⌘. Trusting that flag would mark ⌘ as held and
    // stick the thread jump hints. Setting a bit requires a real modifier
    // keydown, handled above.
    ShortcutModifierState {
        meta_key: current_state.meta_key && event.modifiers.meta_key,
        ctrl_key: current_state.ctrl_key && event.modifiers.ctrl_key,
        alt_key: current_state.alt_key && event.modifiers.alt_key,
        shift_key: current_state.shift_key && event.modifiers.shift_key,
    }
}

/// `useShortcutModifierState(ignoreEditable)`.
#[derive(Debug, Clone, Copy, Default)]
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

    /// `updateState`: returns the new state only when it changed.
    fn update(&mut self, next: ShortcutModifierState) -> Option<ShortcutModifierState> {
        if are_shortcut_modifier_states_equal(&self.state, &next) {
            return None;
        }
        self.state = next;
        Some(next)
    }

    /// A capturing `keydown` / `keyup` listener. `target_is_editable` is
    /// `isEditableFocused(event.target)`.
    pub fn on_keyboard_event(
        &mut self,
        event: &ShortcutKeyboardEvent<'_>,
        target_is_editable: bool,
    ) -> Option<ShortcutModifierState> {
        let next = if self.ignore_editable && target_is_editable {
            EMPTY_SHORTCUT_MODIFIER_STATE
        } else {
            shortcut_modifier_state_after_keyboard_event(self.state, event)
        };
        self.update(next)
    }

    /// `paste` and `blur`. Dictation tools (Wispr Flow) paste with a synthetic
    /// ⌘V whose Meta keyup never reaches the page, so the tracked state stays
    /// "⌘ held" forever and the thread jump hints stick on screen. A paste is
    /// never jump intent, so treat it like a blur and reset. A physically held
    /// modifier re-registers on the next real key event.
    pub fn on_reset(&mut self) -> Option<ShortcutModifierState> {
        self.update(EMPTY_SHORTCUT_MODIFIER_STATE)
    }

    /// `focusin`: focus landing in an editable element resets when
    /// `ignore_editable` is set.
    pub fn on_focus_in(&mut self, target_is_editable: bool) -> Option<ShortcutModifierState> {
        if self.ignore_editable && target_is_editable {
            return self.on_reset();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: KeyboardEventKind, key: &str, shift_key: bool) -> ShortcutKeyboardEvent<'_> {
        ShortcutKeyboardEvent {
            kind,
            key,
            modifiers: ShortcutModifierState {
                shift_key,
                ..EMPTY_SHORTCUT_MODIFIER_STATE
            },
        }
    }

    fn shift_only() -> ShortcutModifierState {
        ShortcutModifierState {
            shift_key: true,
            ..EMPTY_SHORTCUT_MODIFIER_STATE
        }
    }

    #[test]
    fn does_not_notify_for_unchanged_modifiers_after_a_reset() {
        for reset in ["keyup", "paste", "blur"] {
            let mut tracker = ShortcutModifierTracker::new(false);

            assert_eq!(
                tracker.on_keyboard_event(&key(KeyboardEventKind::KeyDown, "Shift", true), false),
                Some(shift_only()),
                "{reset}"
            );
            let after_reset = if reset == "keyup" {
                tracker.on_keyboard_event(&key(KeyboardEventKind::KeyUp, "Shift", false), false)
            } else {
                tracker.on_reset()
            };
            assert_eq!(after_reset, Some(EMPTY_SHORTCUT_MODIFIER_STATE), "{reset}");

            for typed in "typing".chars() {
                let typed = typed.to_string();
                let typed = typed.as_str();
                assert_eq!(
                    tracker
                        .on_keyboard_event(&key(KeyboardEventKind::KeyDown, typed, false), false),
                    None
                );
                assert_eq!(
                    tracker.on_keyboard_event(&key(KeyboardEventKind::KeyUp, typed, false), false),
                    None
                );
            }
            assert_eq!(tracker.on_reset(), None);
            assert_eq!(tracker.on_reset(), None);
            assert_eq!(
                tracker.on_keyboard_event(&key(KeyboardEventKind::KeyDown, "Shift", true), false),
                Some(shift_only()),
                "{reset}"
            );
        }
    }

    #[test]
    fn key_aliases_map_onto_the_four_modifiers() {
        let down = |name: &str| {
            shortcut_modifier_state_after_keyboard_event(
                EMPTY_SHORTCUT_MODIFIER_STATE,
                &key(KeyboardEventKind::KeyDown, name, false),
            )
        };
        for name in ["Meta", "OS", "Command"] {
            assert!(down(name).meta_key, "{name}");
        }
        assert!(down("Control").ctrl_key);
        for name in ["Alt", "Option"] {
            assert!(down(name).alt_key, "{name}");
        }
        assert!(down("Shift").shift_key);
        assert_eq!(down("a"), EMPTY_SHORTCUT_MODIFIER_STATE);
    }

    #[test]
    fn tracks_bare_modifier_keydown_and_keyup_events_explicitly() {
        let event = |kind, key, meta_key, shift_key| ShortcutKeyboardEvent {
            kind,
            key,
            modifiers: ShortcutModifierState {
                meta_key,
                shift_key,
                ..EMPTY_SHORTCUT_MODIFIER_STATE
            },
        };
        let state = |meta_key, shift_key| ShortcutModifierState {
            meta_key,
            shift_key,
            ..EMPTY_SHORTCUT_MODIFIER_STATE
        };
        let mut current = EMPTY_SHORTCUT_MODIFIER_STATE;

        current = shortcut_modifier_state_after_keyboard_event(
            current,
            &event(KeyboardEventKind::KeyDown, "Meta", false, false),
        );
        assert_eq!(current, state(true, false));

        current = shortcut_modifier_state_after_keyboard_event(
            current,
            &event(KeyboardEventKind::KeyDown, "Shift", true, false),
        );
        assert_eq!(current, state(true, true));

        // Meta comes up while Shift stays held.
        current = shortcut_modifier_state_after_keyboard_event(
            current,
            &event(KeyboardEventKind::KeyUp, "Meta", true, true),
        );
        assert_eq!(current, state(false, true));

        current = shortcut_modifier_state_after_keyboard_event(
            current,
            &event(KeyboardEventKind::KeyUp, "Shift", false, true),
        );
        assert_eq!(current, EMPTY_SHORTCUT_MODIFIER_STATE);
    }

    #[test]
    fn a_non_modifier_key_can_only_clear_a_bit_never_set_one() {
        let meta_held = ShortcutModifierState {
            meta_key: true,
            ..EMPTY_SHORTCUT_MODIFIER_STATE
        };
        // A stuck `metaKey: true` on Enter does not mark ⌘ as held.
        let stuck = ShortcutKeyboardEvent {
            kind: KeyboardEventKind::KeyDown,
            key: "Enter",
            modifiers: meta_held,
        };
        assert_eq!(
            shortcut_modifier_state_after_keyboard_event(EMPTY_SHORTCUT_MODIFIER_STATE, &stuck),
            EMPTY_SHORTCUT_MODIFIER_STATE
        );
        // A held ⌘ stays while the event still reports it, and clears when it does not.
        assert_eq!(
            shortcut_modifier_state_after_keyboard_event(meta_held, &stuck),
            meta_held
        );
        assert_eq!(
            shortcut_modifier_state_after_keyboard_event(
                meta_held,
                &key(KeyboardEventKind::KeyDown, "Enter", false)
            ),
            EMPTY_SHORTCUT_MODIFIER_STATE
        );
    }

    #[test]
    fn ignore_editable_resets_while_an_editable_element_has_the_keyboard() {
        let mut tracker = ShortcutModifierTracker::new(true);
        assert_eq!(
            tracker.on_keyboard_event(&key(KeyboardEventKind::KeyDown, "Shift", true), false),
            Some(shift_only())
        );
        assert_eq!(tracker.on_focus_in(false), None);
        assert_eq!(
            tracker.on_focus_in(true),
            Some(EMPTY_SHORTCUT_MODIFIER_STATE)
        );
        assert_eq!(
            tracker.on_keyboard_event(&key(KeyboardEventKind::KeyDown, "Shift", true), true),
            None
        );
        assert_eq!(tracker.state(), EMPTY_SHORTCUT_MODIFIER_STATE);
    }

    #[test]
    fn without_ignore_editable_an_editable_target_changes_nothing() {
        let mut tracker = ShortcutModifierTracker::new(false);
        assert_eq!(tracker.on_focus_in(true), None);
        assert_eq!(
            tracker.on_keyboard_event(&key(KeyboardEventKind::KeyDown, "Shift", true), true),
            Some(shift_only())
        );
    }
}
