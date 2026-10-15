//! Port of upstream/apps/web/src/components/chat/OpenInPicker.logic.ts.
//!
//! Upstream also hides the picker for a non-primary environment and shows it
//! for a remote one in deep-link mode. Both branches read `remoteOpen.ts`,
//! which is cut, and Turin has one local environment, so only the project
//! check remains. See PORTING.md Deviations.

/// Whether the "Open in" picker shows for the active thread. It needs an
/// active project, and an empty name counts as none.
pub fn should_show_open_in_picker(active_project_name: Option<&str>) -> bool {
    active_project_name.is_some_and(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_the_picker_for_projects() {
        assert!(should_show_open_in_picker(Some("codething-mvp")));
    }

    #[test]
    fn hides_the_picker_when_there_is_no_active_project() {
        assert!(!should_show_open_in_picker(None));
        assert!(!should_show_open_in_picker(Some("")));
    }
}
