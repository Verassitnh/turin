//! Port of the queued-run chords in upstream/apps/web/src/shared/keybindings.ts
//! (`mod+shift+enter` → `thread.steerQueuedMessage`, `alt+arrowup` →
//! `thread.editQueuedMessage`). The composer applies them in `ChatView.tsx`.

use crate::composer::views::prompt_editor::CommandKey;

/// The queued-run chords: `mod+shift+enter` steers the next run and
/// `alt+arrowup` edits the latest one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedRunKey {
    Steer,
    EditLatest,
}

pub fn queued_run_key(key: &CommandKey) -> Option<QueuedRunKey> {
    if key.is_composing {
        return None;
    }
    let is_mod = key.meta || key.ctrl;
    if key.key == "Enter" && is_mod && key.shift && !key.alt {
        return Some(QueuedRunKey::Steer);
    }
    if key.key == "ArrowUp" && key.alt && !is_mod && !key.shift {
        return Some(QueuedRunKey::EditLatest);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(key: &str, shift: bool, alt: bool, meta: bool) -> CommandKey {
        CommandKey {
            key: key.to_owned(),
            shift,
            alt,
            meta,
            ctrl: false,
            is_composing: false,
            repeat: false,
            caret_on_first_line: false,
            caret_on_last_line: false,
        }
    }

    #[test]
    fn mod_shift_enter_steers_and_alt_up_edits_the_queue() {
        assert_eq!(
            queued_run_key(&chord("Enter", true, false, true)),
            Some(QueuedRunKey::Steer)
        );
        assert_eq!(
            queued_run_key(&chord("ArrowUp", false, true, false)),
            Some(QueuedRunKey::EditLatest)
        );
    }

    #[test]
    fn other_chords_keep_their_composer_meaning() {
        // Plain Enter sends; mod+Enter is the alternate send; Shift+Enter is a newline.
        assert_eq!(queued_run_key(&chord("Enter", false, false, true)), None);
        assert_eq!(queued_run_key(&chord("Enter", true, false, false)), None);
        assert_eq!(queued_run_key(&chord("Enter", true, true, true)), None);
        assert_eq!(queued_run_key(&chord("ArrowUp", false, false, false)), None);
        assert_eq!(queued_run_key(&chord("ArrowUp", true, true, false)), None);
    }
}
