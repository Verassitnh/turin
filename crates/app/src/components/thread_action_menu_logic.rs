//! Port of upstream/apps/web/src/components/threadActionMenu.logic.ts
//!
//! The per-thread action menu shared by the sidebar row's right-click menu
//! and the chat header menu: labels, ordering and capability gating.

use super::sidebar::runtime::thread_settled::SnoozePreset;
use super::sidebar::sidebar_logic::ContextMenuItem;

/// Right-click menu for an unsent draft row.
pub fn build_draft_action_menu_items(
    has_path: bool,
    has_branch: bool,
    has_project: bool,
) -> Vec<ContextMenuItem> {
    let mut copy_children = Vec::new();
    if has_path {
        copy_children.push(ContextMenuItem::new("copy-path", "Path").icon("folder"));
    }
    if has_branch {
        copy_children.push(ContextMenuItem::new("copy-branch", "Branch").icon("git-branch"));
    }
    let mut items = vec![
        ContextMenuItem::new("copy", "Copy")
            .icon("copy")
            .disabled(!has_path && !has_branch)
            .children(copy_children),
    ];
    if has_project {
        items.push(ContextMenuItem::new("project-settings", "Project settings").icon("settings"));
    }
    items.push(
        ContextMenuItem::new("discard", "Discard draft")
            .icon("trash")
            .destructive()
            .separator_before(),
    );
    items
}

/// Project scoping for the thread list; `None` on surfaces with no scoped
/// list behind the menu (the chat header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFilterState {
    pub label: String,
    /// True when the list is already scoped to this thread's project.
    pub is_active: bool,
}

/// Which lifecycle commands the thread's server understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadActionSupports {
    pub settlement: bool,
    /// Server understands `thread.auto-settle.set`.
    pub auto_settle_opt_out: bool,
    pub snooze: bool,
    pub pinning: bool,
    pub title_regeneration: bool,
}

impl ThreadActionSupports {
    pub const ALL: Self = Self {
        settlement: true,
        auto_settle_opt_out: true,
        snooze: true,
        pinning: true,
        title_regeneration: true,
    };
}

/// `ThreadActionMenuState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadActionMenuState {
    pub can_operate: bool,
    pub branch: Option<String>,
    pub project_filter: Option<ProjectFilterState>,
    pub is_pinned: bool,
    pub is_settled: bool,
    /// False while the user has turned automatic settlement off.
    pub auto_settle_enabled: bool,
    pub is_snoozed: bool,
    pub can_snooze_now: bool,
    pub is_regenerating_title: bool,
    /// Archive rejects a thread with an attached provider.
    pub is_running: bool,
    pub supports: ThreadActionSupports,
    pub snooze_presets: Vec<SnoozePreset>,
}

/// `threadActionRequiresOperate`: local navigation, read markers, and
/// copying stay available to read-only clients.
pub fn thread_action_requires_operate(action: &str) -> bool {
    !matches!(
        action,
        "new-thread-on-branch"
            | "project-settings"
            | "mark-unread"
            | "copy"
            | "copy-path"
            | "copy-branch"
            | "copy-thread-id"
    )
}

/// `buildThreadActionMenuItems`.
pub fn build_thread_action_menu_items(state: &ThreadActionMenuState) -> Vec<ContextMenuItem> {
    let mut items = Vec::new();
    if let Some(branch) = &state.branch {
        items.push(
            ContextMenuItem::new("new-thread-on-branch", format!("New thread on {branch}"))
                .icon("message-square-plus"),
        );
    }
    if state.supports.pinning {
        items.push(if state.is_pinned {
            ContextMenuItem::new("unpin", "Unpin thread").icon("pin-off")
        } else {
            ContextMenuItem::new("pin", "Pin thread").icon("pin")
        });
    }
    // Both lifecycle actions stay available on pinned threads: settling
    // clears the pin, and snoozing hides the card with the pin intact.
    if state.supports.settlement {
        items.push(if state.is_settled {
            ContextMenuItem::new("unsettle", "Un-settle thread").icon("circle-check")
        } else {
            ContextMenuItem::new("settle", "Settle thread").icon("circle-check")
        });
    }
    if state.supports.snooze {
        items.push(if state.is_snoozed {
            ContextMenuItem::new("unsnooze", "Wake thread").icon("clock")
        } else {
            let mut children: Vec<ContextMenuItem> = state
                .snooze_presets
                .iter()
                .map(|preset| {
                    ContextMenuItem::new(
                        format!("snooze:{}", preset.id.as_str()),
                        format!("{} ({})", preset.label, preset.when_label),
                    )
                })
                .collect();
            children.push(ContextMenuItem::new("snooze:custom", "Custom…").separator_before());
            ContextMenuItem::new("snooze", "Snooze")
                .icon("clock")
                .disabled(!state.can_snooze_now)
                .children(children)
        });
    }
    items.push(
        ContextMenuItem::new("rename", "Rename thread")
            .icon("pencil")
            .separator_before(),
    );
    if state.supports.title_regeneration {
        items.push(
            ContextMenuItem::new(
                "regenerate-title",
                if state.is_regenerating_title {
                    "Regenerating…"
                } else {
                    "Regenerate title"
                },
            )
            .icon("refresh-cw")
            .disabled(state.is_regenerating_title),
        );
    }
    items.push(ContextMenuItem::new("mark-unread", "Mark unread").icon("mail-open"));
    if let Some(filter) = &state.project_filter {
        items.push(
            ContextMenuItem::new(
                "filter-by-project",
                if filter.is_active {
                    "Show all projects".to_string()
                } else {
                    format!("Filter by {}", filter.label)
                },
            )
            .icon("folder-tree"),
        );
    }
    // A submenu with the current option checked: a setting, sitting with
    // the other per-thread settings rather than the lifecycle verbs.
    if state.supports.auto_settle_opt_out {
        items.push(
            ContextMenuItem::new("auto-settle", "Auto-settle behavior")
                .icon("timer")
                .children(vec![
                    ContextMenuItem::new("auto-settle:enabled", "Enabled")
                        .checked(state.auto_settle_enabled),
                    ContextMenuItem::new("auto-settle:disabled", "Disabled")
                        .checked(!state.auto_settle_enabled),
                ]),
        );
    }
    let mut copy_children = vec![ContextMenuItem::new("copy-path", "Path").icon("folder")];
    if state.branch.is_some() {
        copy_children.push(ContextMenuItem::new("copy-branch", "Branch").icon("git-branch"));
    }
    copy_children.push(ContextMenuItem::new("copy-thread-id", "Thread ID").icon("hash"));
    items.push(
        ContextMenuItem::new("copy", "Copy")
            .icon("copy")
            .separator_before()
            .children(copy_children),
    );
    items.push(ContextMenuItem::new("project-settings", "Project settings").icon("settings"));
    // Archive keeps the conversation under Settings > Archived threads:
    // distinct from Settle and Delete, so not destructive-styled.
    items.push(
        ContextMenuItem::new("archive", "Archive thread")
            .icon("archive")
            .disabled(state.is_running)
            .separator_before(),
    );
    items.push(
        ContextMenuItem::new("delete", "Delete")
            .icon("trash")
            .destructive(),
    );
    if state.can_operate {
        return items;
    }
    items
        .into_iter()
        .map(|item| {
            if !thread_action_requires_operate(&item.id) {
                return item;
            }
            let children = item
                .children
                .iter()
                .cloned()
                .map(|child| child.disabled(true))
                .collect();
            item.disabled(true).children(children)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::runtime::thread_settled::SnoozePresetId;
    use super::*;

    fn base() -> ThreadActionMenuState {
        ThreadActionMenuState {
            can_operate: true,
            branch: None,
            project_filter: None,
            is_pinned: false,
            is_settled: false,
            auto_settle_enabled: true,
            is_snoozed: false,
            can_snooze_now: true,
            is_regenerating_title: false,
            is_running: false,
            supports: ThreadActionSupports::ALL,
            snooze_presets: vec![SnoozePreset {
                id: SnoozePresetId::Hour,
                label: "In 1 hour",
                when_label: "3:00 PM".into(),
                snoozed_until: "2026-08-07T15:00:00Z".into(),
            }],
        }
    }

    fn ids(state: &ThreadActionMenuState) -> Vec<String> {
        build_thread_action_menu_items(state)
            .into_iter()
            .map(|item| item.id)
            .collect()
    }

    fn all_ids(state: &ThreadActionMenuState) -> Vec<String> {
        fn flatten(items: &[ContextMenuItem]) -> Vec<String> {
            items
                .iter()
                .flat_map(|item| std::iter::once(item.id.clone()).chain(flatten(&item.children)))
                .collect()
        }
        flatten(&build_thread_action_menu_items(state))
    }

    const NONE: ThreadActionSupports = ThreadActionSupports {
        settlement: false,
        auto_settle_opt_out: false,
        snooze: false,
        pinning: false,
        title_regeneration: false,
    };

    #[test]
    fn disables_both_lifecycle_directions_without_permission() {
        for reversed in [false, true] {
            let items = build_thread_action_menu_items(&ThreadActionMenuState {
                can_operate: false,
                is_pinned: reversed,
                is_settled: reversed,
                is_snoozed: reversed,
                ..base()
            });
            let expected: &[&str] = if reversed {
                &[
                    "unpin",
                    "unsettle",
                    "unsnooze",
                    "rename",
                    "regenerate-title",
                    "auto-settle",
                    "archive",
                    "delete",
                ]
            } else {
                &[
                    "pin",
                    "settle",
                    "snooze",
                    "rename",
                    "regenerate-title",
                    "auto-settle",
                    "archive",
                    "delete",
                ]
            };
            assert_eq!(
                items
                    .iter()
                    .filter(|i| i.disabled)
                    .map(|i| i.id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(
                items
                    .iter()
                    .find(|i| i.id == "snooze")
                    .is_none_or(|i| i.children.iter().all(|c| c.disabled))
            );
        }
    }

    #[test]
    fn preserves_local_actions_and_restores_mutations_after_a_grant() {
        let denied = build_thread_action_menu_items(&ThreadActionMenuState {
            can_operate: false,
            branch: Some("main".into()),
            ..base()
        });
        assert_eq!(
            denied
                .iter()
                .filter(|i| !i.disabled)
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "new-thread-on-branch",
                "mark-unread",
                "copy",
                "project-settings"
            ]
        );
        assert!(
            build_thread_action_menu_items(&base())
                .iter()
                .all(|i| !i.disabled)
        );
    }

    #[test]
    fn hides_lifecycle_items_when_the_environment_lacks_the_capabilities() {
        assert_eq!(
            ids(&ThreadActionMenuState {
                supports: NONE,
                ..base()
            }),
            vec![
                "rename",
                "mark-unread",
                "copy",
                "project-settings",
                "archive",
                "delete"
            ]
        );
    }

    #[test]
    fn groups_project_settings_with_utility_actions_before_archive() {
        let items = build_thread_action_menu_items(&base());
        let copy = items.iter().position(|i| i.id == "copy").unwrap();
        assert_eq!(items[copy + 1].id, "project-settings");
        assert_eq!(items[copy + 1].label, "Project settings");
        assert_eq!(items[copy + 1].icon, Some("settings"));
        assert_eq!(items[copy + 2].id, "archive");
    }

    #[test]
    fn offers_project_filtering_only_for_surfaces_with_a_scoped_thread_list() {
        assert!(!ids(&base()).contains(&"filter-by-project".to_string()));
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            project_filter: Some(ProjectFilterState {
                label: "Beta Project".into(),
                is_active: false,
            }),
            ..base()
        });
        let item = items.iter().find(|i| i.id == "filter-by-project").unwrap();
        assert_eq!(item.label, "Filter by Beta Project");
        assert_eq!(item.icon, Some("folder-tree"));
    }

    #[test]
    fn offers_the_way_back_to_all_projects_once_the_list_is_scoped() {
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            project_filter: Some(ProjectFilterState {
                label: "Beta Project".into(),
                is_active: true,
            }),
            ..base()
        });
        let index = items
            .iter()
            .position(|i| i.id == "filter-by-project")
            .unwrap();
        assert_eq!(items[index].label, "Show all projects");
        assert_eq!(items[index - 1].id, "mark-unread");
        assert_eq!(items[index + 1].id, "auto-settle");
    }

    #[test]
    fn includes_branch_items_only_for_threads_with_a_branch() {
        let with_branch = all_ids(&ThreadActionMenuState {
            branch: Some("feat/menu".into()),
            ..base()
        });
        assert!(with_branch.contains(&"new-thread-on-branch".to_string()));
        assert!(with_branch.contains(&"copy-branch".to_string()));
        let without = all_ids(&base());
        assert!(!without.contains(&"new-thread-on-branch".to_string()));
        assert!(!without.contains(&"copy-branch".to_string()));
    }

    #[test]
    fn flips_lifecycle_labels_with_thread_state() {
        let flipped = ids(&ThreadActionMenuState {
            is_pinned: true,
            is_settled: true,
            is_snoozed: true,
            ..base()
        });
        for id in ["unpin", "unsettle", "unsnooze"] {
            assert!(flipped.contains(&id.to_string()));
        }
        let plain = ids(&base());
        for id in ["pin", "settle", "snooze"] {
            assert!(plain.contains(&id.to_string()));
        }
    }

    #[test]
    fn offers_auto_settle_as_a_submenu_with_the_current_option_checked() {
        let find = |state: &ThreadActionMenuState| {
            build_thread_action_menu_items(state)
                .into_iter()
                .find(|i| i.id == "auto-settle")
        };
        let on = find(&base()).unwrap();
        assert_eq!(on.label, "Auto-settle behavior");
        assert_eq!(
            on.children
                .iter()
                .map(|c| (c.id.as_str(), c.checked))
                .collect::<Vec<_>>(),
            vec![
                ("auto-settle:enabled", Some(true)),
                ("auto-settle:disabled", Some(false))
            ]
        );
        let off = find(&ThreadActionMenuState {
            auto_settle_enabled: false,
            ..base()
        })
        .unwrap();
        assert_eq!(
            off.children.iter().map(|c| c.checked).collect::<Vec<_>>(),
            vec![Some(false), Some(true)]
        );
        let items = build_thread_action_menu_items(&base());
        let mark = items.iter().position(|i| i.id == "mark-unread").unwrap();
        assert_eq!(items[mark + 1].id, "auto-settle");
        let mut supports = ThreadActionSupports::ALL;
        supports.auto_settle_opt_out = false;
        assert!(
            !ids(&ThreadActionMenuState { supports, ..base() })
                .contains(&"auto-settle".to_string())
        );
    }

    #[test]
    fn disables_snooze_when_the_thread_cannot_snooze_keeping_presets_visible() {
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            can_snooze_now: false,
            ..base()
        });
        let snooze = items.iter().find(|i| i.id == "snooze").unwrap();
        assert!(snooze.disabled);
        assert_eq!(
            snooze
                .children
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["snooze:hour", "snooze:custom"]
        );
    }

    #[test]
    fn disables_title_regeneration_while_one_is_in_flight() {
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            is_regenerating_title: true,
            ..base()
        });
        let item = items.iter().find(|i| i.id == "regenerate-title").unwrap();
        assert_eq!(item.label, "Regenerating…");
        assert!(item.disabled);
    }

    #[test]
    fn marks_delete_as_destructive_and_keeps_it_last() {
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            branch: Some("main".into()),
            ..base()
        });
        let last = items.last().unwrap();
        assert_eq!(last.id, "delete");
        assert!(last.destructive);
    }

    #[test]
    fn offers_archive_as_a_non_destructive_action_right_before_delete() {
        let items = build_thread_action_menu_items(&base());
        let archive = &items[items.len() - 2];
        assert_eq!(archive.id, "archive");
        assert_eq!(archive.icon, Some("archive"));
        assert!(archive.separator_before);
        assert!(!archive.destructive);
        assert_eq!(items.last().unwrap().id, "delete");
    }

    #[test]
    fn keeps_archive_available_even_when_the_environment_lacks_every_other_capability() {
        assert!(
            ids(&ThreadActionMenuState {
                supports: NONE,
                ..base()
            })
            .contains(&"archive".to_string())
        );
    }

    #[test]
    fn disables_archive_while_the_thread_is_running() {
        let items = build_thread_action_menu_items(&ThreadActionMenuState {
            is_running: true,
            ..base()
        });
        assert!(items.iter().find(|i| i.id == "archive").unwrap().disabled);
    }

    // describe("buildDraftActionMenuItems")
    #[test]
    fn offers_only_the_copy_values_the_draft_has() {
        let items = build_draft_action_menu_items(false, true, true);
        assert_eq!(items[0].id, "copy");
        assert!(!items[0].disabled);
        assert_eq!(
            items[0]
                .children
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["copy-branch"]
        );
        let no_copy = build_draft_action_menu_items(false, false, true);
        assert!(no_copy[0].disabled);
        assert!(no_copy[0].children.is_empty());
    }

    #[test]
    fn drops_project_settings_without_a_project_and_keeps_discard_last() {
        let items = build_draft_action_menu_items(true, false, false);
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["copy", "discard"]
        );
        let last = items.last().unwrap();
        assert_eq!(last.label, "Discard draft");
        assert!(last.destructive);
    }
}
