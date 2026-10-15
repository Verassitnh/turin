//! Port of upstream/apps/web/src/components/settings/ProjectSettingsPanel.logic.ts.

pub fn project_group_title_needs_update(
    member_titles: &[&str],
    next_title: &str,
    was_edited: bool,
) -> bool {
    was_edited && member_titles.iter().any(|title| *title != next_title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rewrites_titles_after_an_edit_that_differs() {
        assert!(project_group_title_needs_update(&["a", "b"], "b", true));
        assert!(!project_group_title_needs_update(&["b", "b"], "b", true));
        assert!(!project_group_title_needs_update(&["a"], "b", false));
    }
}
