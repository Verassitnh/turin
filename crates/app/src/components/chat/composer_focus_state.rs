//! Port of upstream/apps/web/src/components/chat/useComposerFocusState.ts

use crate::composer::footer_layout::{RestingLayoutInput, should_use_resting_composer_layout};

#[derive(Debug, Default)]
pub struct ComposerFocusState {
    pub is_composer_focused: bool,
    pub is_composer_scroll_collapsed: bool,
}

impl ComposerFocusState {
    pub fn restore_after_timeline_reached_end(&mut self) {
        self.is_composer_scroll_collapsed = false;
    }

    pub fn is_resting(&self) -> bool {
        should_use_resting_composer_layout(&RestingLayoutInput {
            is_existing_thread: true,
            is_mobile_viewport: false,
            is_scroll_collapsed: self.is_composer_scroll_collapsed,
            has_expanded_chrome: false,
            has_multiline_prompt: false,
            timeline_overflows: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_expanded_when_the_composer_loses_focus() {
        let mut state = ComposerFocusState {
            is_composer_focused: true,
            ..ComposerFocusState::default()
        };
        assert!(!state.is_resting());
        state.is_composer_focused = false;
        assert!(!state.is_resting());
    }

    #[test]
    fn can_collapse_again_on_the_next_scroll_after_returning_to_the_end() {
        let mut state = ComposerFocusState {
            is_composer_scroll_collapsed: true,
            ..ComposerFocusState::default()
        };
        assert!(state.is_resting());
        state.restore_after_timeline_reached_end();
        assert!(!state.is_resting());
        state.is_composer_scroll_collapsed = true;
        assert!(state.is_resting());
    }

    #[test]
    fn does_not_move_focus_into_the_composer_when_the_timeline_reaches_the_end() {
        let mut state = ComposerFocusState {
            is_composer_scroll_collapsed: true,
            ..ComposerFocusState::default()
        };
        state.restore_after_timeline_reached_end();
        assert!(!state.is_resting());
        assert!(!state.is_composer_focused);
    }
}
