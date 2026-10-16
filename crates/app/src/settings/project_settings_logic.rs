//! Port of upstream/apps/web/src/components/settings/ProjectSettingsPanel.logic.ts.

pub fn project_group_title_needs_update(
    member_titles: &[&str],
    next_title: &str,
    was_edited: bool,
) -> bool {
    was_edited && member_titles.iter().any(|title| *title != next_title)
}

pub const PROJECT_TITLE_EMPTY: &str = "Project title cannot be empty";

/// The warning for a rename whose trimmed title is blank
/// (`ProjectSettingsPanel`: "Project title cannot be empty"). The rename is
/// refused, so the stored title is never blanked.
pub fn project_title_warning(next_title: &str) -> Option<&'static str> {
    next_title.trim().is_empty().then_some(PROJECT_TITLE_EMPTY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_only_for_a_blank_project_title() {
        assert_eq!(project_title_warning("  \t "), Some(PROJECT_TITLE_EMPTY));
        assert_eq!(project_title_warning(" Turin "), None);
    }

    #[test]
    fn only_rewrites_titles_after_an_edit_that_differs() {
        assert!(project_group_title_needs_update(&["a", "b"], "b", true));
        assert!(!project_group_title_needs_update(&["b", "b"], "b", true));
        assert!(!project_group_title_needs_update(&["a"], "b", false));
    }
}
