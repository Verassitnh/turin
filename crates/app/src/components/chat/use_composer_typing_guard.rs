//! Port of upstream/apps/web/src/components/chat/useComposerTypingGuard.ts
//!
//! The React hook keeps the guard state in a snapshot keyed by scope and the
//! JSON of the request ids, and re-runs the `requests` event when either
//! changes during render. [`ComposerTypingGuardHook::sync`] is that render-time
//! check. The idle `setTimeout` is the caller's: [`idle_delay_ms`] says how
//! long to wait, and the timer calls [`on_idle`].
//!
//! [`idle_delay_ms`]: ComposerTypingGuardHook::idle_delay_ms
//! [`on_idle`]: ComposerTypingGuardHook::on_idle

use std::collections::BTreeSet;

use crate::composer::typing_guard::{
    COMPOSER_TYPING_IDLE_MS, ComposerTypingGuardState, TypingGuardEvent,
    update_composer_typing_guard,
};

#[derive(Debug, Clone)]
pub struct ComposerTypingGuardHook {
    scope: String,
    requests_key: String,
    state: ComposerTypingGuardState,
}

/// `JSON.stringify(requestIds)`.
fn requests_key(request_ids: &[&str]) -> String {
    serde_json::to_string(request_ids).unwrap_or_default()
}

impl ComposerTypingGuardHook {
    /// `useState(() => ...)`: the first snapshot.
    pub fn new(scope: &str, request_ids: &[&str], now: f64) -> Self {
        Self {
            scope: scope.to_owned(),
            requests_key: requests_key(request_ids),
            state: update_composer_typing_guard(
                &ComposerTypingGuardState::default(),
                TypingGuardEvent::Requests { request_ids, now },
            ),
        }
    }

    /// The render-time `setSnapshot` when scope or requests changed. A new
    /// scope starts a fresh state but keeps `focused`.
    pub fn sync(&mut self, scope: &str, request_ids: &[&str], now: f64) {
        let key = requests_key(request_ids);
        if self.scope == scope && self.requests_key == key {
            return;
        }
        let base = if self.scope == scope {
            self.state.clone()
        } else {
            ComposerTypingGuardState {
                focused: self.state.focused,
                ..ComposerTypingGuardState::default()
            }
        };
        self.state =
            update_composer_typing_guard(&base, TypingGuardEvent::Requests { request_ids, now });
        self.scope = scope.to_owned();
        self.requests_key = key;
    }

    fn dispatch(&mut self, event: TypingGuardEvent<'_>) {
        self.state = update_composer_typing_guard(&self.state, event);
    }

    pub fn held_request_ids(&self) -> &BTreeSet<String> {
        &self.state.held
    }

    pub fn on_draft_change(&mut self, now: f64) {
        self.dispatch(TypingGuardEvent::Type { now });
    }

    pub fn on_focus(&mut self) {
        self.dispatch(TypingGuardEvent::Focus);
    }

    pub fn on_blur(&mut self) {
        self.dispatch(TypingGuardEvent::Blur);
    }

    pub fn on_send(&mut self) {
        self.dispatch(TypingGuardEvent::Send);
    }

    pub fn on_idle(&mut self) {
        self.dispatch(TypingGuardEvent::Idle);
    }

    /// The effect's timeout: `None` when nothing is held (no timer), else the
    /// milliseconds until the typing pause ends.
    pub fn idle_delay_ms(&self, now: f64) -> Option<f64> {
        if self.state.held.is_empty() {
            return None;
        }
        Some((self.state.last_typed_at + COMPOSER_TYPING_IDLE_MS - now).max(0.0))
    }
}

#[cfg(test)]
#[path = "use_composer_typing_guard_tests.rs"]
mod tests;
