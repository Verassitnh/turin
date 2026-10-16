//! Port of upstream/apps/web/src/components/composerSelection.ts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerSelectionRange {
    pub start: usize,
    pub end: usize,
}

/// `didComposerSelectionChangeVisibly`.
pub fn did_composer_selection_change_visibly(
    previous: ComposerSelectionRange,
    next: Option<ComposerSelectionRange>,
) -> bool {
    next.is_some_and(|next| {
        next.start != next.end && (next.start != previous.start || next.end != previous.end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: usize, end: usize) -> ComposerSelectionRange {
        ComposerSelectionRange { start, end }
    }

    #[test]
    fn ignores_selection_neutral_editor_updates() {
        assert!(!did_composer_selection_change_visibly(range(3, 3), None));
        assert!(!did_composer_selection_change_visibly(
            range(3, 3),
            Some(range(3, 3))
        ));
    }

    #[test]
    fn detects_entering_and_extending_a_visible_range_selection() {
        assert!(did_composer_selection_change_visibly(
            range(3, 3),
            Some(range(0, 3))
        ));
        assert!(did_composer_selection_change_visibly(
            range(1, 3),
            Some(range(0, 3))
        ));
    }

    #[test]
    fn does_not_repeatedly_report_the_same_visible_selection() {
        assert!(!did_composer_selection_change_visibly(
            range(0, 3),
            Some(range(0, 3))
        ));
    }
}
