//! Port of upstream/apps/web/src/components/chat/useComposerMenuState.ts
//!
//! React's render-time reset is [`ComposerMenuState::visible`]: hiding the
//! trigger clears a stored open so revealing it cannot reopen the popup.

#[derive(Debug, Default)]
pub struct ComposerMenuState {
    open: bool,
}

impl ComposerMenuState {
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub fn visible(&mut self, hidden: bool) -> bool {
        if hidden && self.open {
            self.open = false;
        }
        self.open && !hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_an_open_popup_when_its_trigger_hides_and_does_not_reopen() {
        let mut menu = ComposerMenuState::default();
        assert!(!menu.visible(false));
        menu.set_open(true);
        assert!(menu.visible(false));
        assert!(!menu.visible(true));
        assert!(!menu.visible(false));
        menu.set_open(true);
        assert!(menu.visible(false));
        menu.set_open(false);
        assert!(!menu.visible(false));
    }

    #[test]
    fn does_not_commit_an_open_popup_requested_while_its_trigger_is_hidden() {
        let mut menu = ComposerMenuState::default();
        assert!(!menu.visible(true));
        menu.set_open(true);
        assert!(!menu.visible(true));
        assert!(!menu.visible(false));
    }
}
