//! Port of upstream/apps/web/src/components/chat/composerTypingGuard.ts
//!
//! Holds a newly arrived approval/question while the user is actively typing
//! in the focused composer, so a keystroke cannot answer it by accident.

use std::collections::BTreeSet;

pub const COMPOSER_TYPING_IDLE_MS: f64 = 1500.0;

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerTypingGuardState {
    pub seen: BTreeSet<String>,
    pub held: BTreeSet<String>,
    pub focused: bool,
    pub last_typed_at: f64,
}

impl Default for ComposerTypingGuardState {
    fn default() -> Self {
        Self {
            seen: BTreeSet::new(),
            held: BTreeSet::new(),
            focused: false,
            last_typed_at: f64::NEG_INFINITY,
        }
    }
}

pub enum TypingGuardEvent<'a> {
    Requests { request_ids: &'a [&'a str], now: f64 },
    Type { now: f64 },
    Focus,
    Blur,
    Send,
    Idle,
}

/// `updateComposerTypingGuard`.
pub fn update_composer_typing_guard(
    state: &ComposerTypingGuardState,
    event: TypingGuardEvent<'_>,
) -> ComposerTypingGuardState {
    let mut next = state.clone();
    match event {
        TypingGuardEvent::Requests { request_ids, now } => {
            next.held.retain(|id| request_ids.contains(&id.as_str()));
            for id in request_ids {
                if !next.seen.contains(*id) && state.focused && now - state.last_typed_at < COMPOSER_TYPING_IDLE_MS {
                    next.held.insert((*id).to_owned());
                }
                next.seen.insert((*id).to_owned());
            }
        }
        TypingGuardEvent::Type { now } => next.last_typed_at = now,
        TypingGuardEvent::Focus => next.focused = true,
        TypingGuardEvent::Blur | TypingGuardEvent::Send | TypingGuardEvent::Idle => {
            next.held.clear();
            next.last_typed_at = f64::NEG_INFINITY;
            if matches!(event, TypingGuardEvent::Blur) {
                next.focused = false;
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing() -> ComposerTypingGuardState {
        let state = update_composer_typing_guard(&ComposerTypingGuardState::default(), TypingGuardEvent::Focus);
        update_composer_typing_guard(&state, TypingGuardEvent::Type { now: 1000.0 })
    }

    fn requests(state: &ComposerTypingGuardState, ids: &[&str], now: f64) -> ComposerTypingGuardState {
        update_composer_typing_guard(state, TypingGuardEvent::Requests { request_ids: ids, now })
    }

    fn held(state: &ComposerTypingGuardState) -> Vec<&str> {
        state.held.iter().map(String::as_str).collect()
    }

    #[test]
    fn holds_new_requests_while_the_focused_draft_was_recently_edited() {
        let state = requests(&typing(), &["approval", "question"], 2499.0);
        assert_eq!(held(&state), ["approval", "question"]);
    }

    #[test]
    fn releases_on_idle_send_blur_and_never_re_holds_the_request() {
        for event in [TypingGuardEvent::Idle, TypingGuardEvent::Send, TypingGuardEvent::Blur] {
            let mut state = requests(&typing(), &["question"], 1200.0);
            state = update_composer_typing_guard(&state, event);
            assert!(held(&state).is_empty());
            state = update_composer_typing_guard(&state, TypingGuardEvent::Focus);
            state = update_composer_typing_guard(&state, TypingGuardEvent::Type { now: 2000.0 });
            state = requests(&state, &[], 2100.0);
            state = requests(&state, &["question", "new-question"], 2200.0);
            assert_eq!(held(&state), ["new-question"]);
        }
    }

    #[test]
    fn shows_requests_immediately_when_idle_or_unfocused() {
        let blurred = update_composer_typing_guard(&typing(), TypingGuardEvent::Blur);
        for state in [ComposerTypingGuardState::default(), typing(), blurred] {
            assert!(held(&requests(&state, &["question"], 2500.0)).is_empty());
        }
    }

    #[test]
    fn does_not_hide_a_request_that_already_took_over_when_typing_starts() {
        let mut state = requests(&ComposerTypingGuardState::default(), &["question"], 0.0);
        state = update_composer_typing_guard(&state, TypingGuardEvent::Focus);
        state = update_composer_typing_guard(&state, TypingGuardEvent::Type { now: 1000.0 });
        state = requests(&state, &["question"], 1100.0);
        assert!(held(&state).is_empty());
    }

    #[test]
    fn drops_a_hold_when_its_request_is_resolved() {
        let state = requests(&typing(), &["question"], 1100.0);
        assert!(held(&requests(&state, &[], 1200.0)).is_empty());
    }
}
