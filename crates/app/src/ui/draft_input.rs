//! Port of upstream/apps/web/src/hooks/useCommitOnBlur.ts and
//! upstream/apps/web/src/components/ui/draft-input.tsx.
//!
//! Keystrokes stay in [`CommitOnBlur`] until blur (Enter blurs, unless an IME
//! composition is in progress). An external `value` is shown only while the
//! field is not focused, so a settings push cannot clobber an in-progress edit.
//! The chrome is [`super::text_input_frame`].

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitOnBlur {
    draft: Option<String>,
}

impl CommitOnBlur {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_focused(&self) -> bool {
        self.draft.is_some()
    }

    /// `value: draft ?? value`.
    pub fn value<'a>(&'a self, committed: &'a str) -> &'a str {
        self.draft.as_deref().unwrap_or(committed)
    }

    pub fn on_focus(&mut self, committed: &str) {
        self.draft = Some(committed.to_owned());
    }

    pub fn on_change(&mut self, next: impl Into<String>) {
        self.draft = Some(next.into());
    }

    /// Clears the draft. Returns the text to commit when it differs.
    pub fn on_blur(&mut self, committed: &str) -> Option<String> {
        let next = self.draft.take().unwrap_or_else(|| committed.to_owned());
        if next != committed { Some(next) } else { None }
    }

    /// Enter blurs unless the key event is an IME composition (`keyCode` 229).
    pub fn enter_blurs(composing: bool) -> bool {
        !composing
    }
}

#[cfg(test)]
mod tests {
    use super::CommitOnBlur;

    #[test]
    fn buffers_keystrokes_and_commits_on_blur() {
        let mut field = CommitOnBlur::new();
        assert_eq!(field.value("Work"), "Work");
        field.on_focus("Work");
        field.on_change("Workspace");
        assert_eq!(field.value("Work"), "Workspace");
        // An external push while focused does not replace the draft.
        assert_eq!(field.value("Elsewhere"), "Workspace");
        assert_eq!(field.on_blur("Work").as_deref(), Some("Workspace"));
        assert!(!field.is_focused());
        assert_eq!(field.value("Workspace"), "Workspace");
    }

    #[test]
    fn skips_commit_when_the_text_is_unchanged() {
        let mut field = CommitOnBlur::new();
        field.on_focus("Work");
        assert!(field.on_blur("Work").is_none());
    }

    #[test]
    fn enter_blurs_unless_an_ime_composition_is_active() {
        assert!(CommitOnBlur::enter_blurs(false));
        assert!(!CommitOnBlur::enter_blurs(true));
    }
}
