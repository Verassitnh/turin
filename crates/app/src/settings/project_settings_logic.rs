//! Port of upstream/apps/web/src/components/settings/ProjectSettingsPanel.logic.ts
//! and upstream/apps/web/src/components/settings/ProjectSettingsPanel.logic.test.ts,
//! with the copy `ProjectSettingsPanel.tsx` builds for its icon row and its
//! remove confirmations.

use crate::state::models::{ProjectIcon, ProjectIconColor};

fn color_name(color: ProjectIconColor) -> String {
    serde_json::to_value(color)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The "Project icon" row's description: the icon's kind and color, the
/// favicon path, or "Automatic".
pub fn project_icon_description(icon: Option<&ProjectIcon>, favicon_path: Option<&str>) -> String {
    match icon {
        Some(ProjectIcon::Lucide { name, color }) => format!("{name} · {}", color_name(*color)),
        Some(ProjectIcon::Monogram { text, color }) => format!("{text} · {}", color_name(*color)),
        Some(ProjectIcon::Emoji(emoji)) => emoji.clone(),
        None => favicon_path.unwrap_or("Automatic").to_owned(),
    }
}

/// The danger row's title, description and button for the members in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRemoveCopy {
    pub title: &'static str,
    pub description: String,
    pub button: &'static str,
}

pub fn project_remove_copy(has_other_members: bool, member_count: usize) -> ProjectRemoveCopy {
    if has_other_members {
        ProjectRemoveCopy {
            title: "Remove checkout",
            description: "Deletes the selected machine's checkout entries and their threads. Other machines and files on disk are not touched.".to_owned(),
            button: "Remove checkout",
        }
    } else if member_count > 1 {
        ProjectRemoveCopy {
            title: "Remove this project everywhere",
            description: format!(
                "Deletes all {member_count} checkout entries and their threads on every machine. Files on disk are not touched."
            ),
            button: "Remove all entries",
        }
    } else {
        ProjectRemoveCopy {
            title: "Remove project",
            description:
                "Deletes the project entry and its threads. Files on disk are not touched."
                    .to_owned(),
            button: "Remove project",
        }
    }
}

/// The remove confirmation's lines (`removeMembers`): the question, what is
/// affected, and the reminders. `single` is the one member's path and
/// environment when exactly one is removed.
pub fn project_removal_confirmation(
    target_is_checkout: bool,
    target_label: &str,
    thread_count: usize,
    single: Option<(&str, Option<&str>)>,
    member_count: usize,
    whole_group_only: bool,
) -> Vec<String> {
    let kind = if target_is_checkout {
        "checkout"
    } else {
        "project"
    };
    let mut lines = vec![if thread_count > 0 {
        format!(
            "Remove {kind} \"{target_label}\" and delete its {thread_count} thread{}?",
            if thread_count == 1 { "" } else { "s" }
        )
    } else {
        format!("Remove {kind} \"{target_label}\"?")
    }];
    match single {
        Some((root, environment)) => {
            lines.push(format!("Path: {root}"));
            if let Some(environment) = environment {
                lines.push(format!("Environment: {environment}"));
            }
        }
        None => lines.push(format!(
            "This removes {member_count} grouped project entries."
        )),
    }
    lines.push(if thread_count > 0 {
        "This permanently clears conversation history for those threads and any archived threads."
            .to_owned()
    } else {
        "This permanently clears any archived conversation history.".to_owned()
    });
    lines.push(if whole_group_only {
        "This removes only the project entries, not the files on disk.".to_owned()
    } else {
        "Other entries in this grouped project are unaffected.".to_owned()
    });
    lines.push("This action cannot be undone.".to_owned());
    lines
}

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
    fn describes_the_project_icon() {
        assert_eq!(project_icon_description(None, None), "Automatic");
        assert_eq!(
            project_icon_description(None, Some("assets/logo.png")),
            "assets/logo.png"
        );
        assert_eq!(
            project_icon_description(Some(&ProjectIcon::Emoji("🚀".into())), Some("x")),
            "🚀"
        );
        assert_eq!(
            project_icon_description(
                Some(&ProjectIcon::Lucide {
                    name: "rocket".into(),
                    color: ProjectIconColor::Violet
                }),
                None
            ),
            "rocket · violet"
        );
        assert_eq!(
            project_icon_description(
                Some(&ProjectIcon::Monogram {
                    text: "AB".into(),
                    color: ProjectIconColor::Teal
                }),
                None
            ),
            "AB · teal"
        );
    }

    #[test]
    fn words_the_danger_row_by_what_is_removed() {
        assert_eq!(project_remove_copy(true, 1).button, "Remove checkout");
        assert_eq!(
            project_remove_copy(false, 3).title,
            "Remove this project everywhere"
        );
        assert!(
            project_remove_copy(false, 3)
                .description
                .contains("all 3 checkout entries")
        );
        assert_eq!(project_remove_copy(false, 1).button, "Remove project");
    }

    #[test]
    fn confirms_a_single_checkout_removal_with_its_path_and_environment() {
        let lines = project_removal_confirmation(
            true,
            "turin",
            2,
            Some(("/Users/dev/turin", Some("This device"))),
            1,
            false,
        );
        assert_eq!(
            lines,
            vec![
                "Remove checkout \"turin\" and delete its 2 threads?",
                "Path: /Users/dev/turin",
                "Environment: This device",
                "This permanently clears conversation history for those threads and any archived threads.",
                "Other entries in this grouped project are unaffected.",
                "This action cannot be undone.",
            ]
        );
    }

    #[test]
    fn confirms_removing_a_whole_group() {
        let lines = project_removal_confirmation(false, "turin", 0, None, 3, true);
        assert_eq!(lines[0], "Remove project \"turin\"?");
        assert_eq!(lines[1], "This removes 3 grouped project entries.");
        assert_eq!(
            lines[2],
            "This permanently clears any archived conversation history."
        );
        assert_eq!(
            lines[3],
            "This removes only the project entries, not the files on disk."
        );
        assert_eq!(
            project_removal_confirmation(false, "t", 1, None, 1, true)[0],
            "Remove project \"t\" and delete its 1 thread?"
        );
    }

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
