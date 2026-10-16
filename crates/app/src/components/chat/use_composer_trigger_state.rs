//! Port of upstream/apps/web/src/components/chat/useComposerTriggerState.ts
//!
//! Keep a dismissed suggestion closed until the caret leaves its token.
//! `dismissedTriggerRef` is a plain field here: every method takes `&mut self`
//! so the new value is visible immediately, as the ref is upstream.

use turin_shared::composer_trigger::ComposerTrigger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerTriggerState {
    trigger: Option<ComposerTrigger>,
    dismissed_trigger: Option<ComposerTrigger>,
}

impl ComposerTriggerState {
    /// `useState(initialTrigger)`.
    pub fn new(initial_trigger: impl FnOnce() -> Option<ComposerTrigger>) -> Self {
        Self {
            trigger: initial_trigger(),
            dismissed_trigger: None,
        }
    }

    pub fn trigger(&self) -> Option<&ComposerTrigger> {
        self.trigger.as_ref()
    }

    /// `resolveTrigger`: a candidate on the dismissed token resolves to none.
    pub fn resolve_trigger(&self, candidate: Option<ComposerTrigger>) -> Option<ComposerTrigger> {
        let candidate = candidate?;
        match &self.dismissed_trigger {
            Some(dismissed)
                if candidate.kind == dismissed.kind
                    && candidate.range_start == dismissed.range_start =>
            {
                None
            }
            _ => Some(candidate),
        }
    }

    /// `setTrigger`.
    pub fn set_trigger(&mut self, candidate: Option<ComposerTrigger>) {
        let was_none = candidate.is_none();
        let active = self.resolve_trigger(candidate);
        if was_none || active.is_some() {
            self.dismissed_trigger = None;
        }
        self.trigger = active;
    }

    /// `dismissTrigger`.
    pub fn dismiss_trigger(&mut self, candidate: Option<ComposerTrigger>) {
        self.dismissed_trigger = candidate;
        self.trigger = None;
    }

    /// `resetTrigger`: switching drafts or pending questions clears dismissal.
    pub fn reset_trigger(&mut self, candidate: Option<ComposerTrigger>) {
        self.dismissed_trigger = None;
        self.trigger = candidate;
    }
}

#[cfg(test)]
#[path = "use_composer_trigger_state_tests.rs"]
mod tests;
