//! Port of upstream/apps/web/src/hooks/useThreadActionMenu.ts (+ test): the
//! click-to-plan mapping of the per-thread action menu.
//!
//! Upstream's `openMenu` reads the thread at click time, shows the native menu,
//! then rechecks the operate grant, and only then runs the action. Here the
//! decision is `plan_thread_action`, a pure function over that click-time
//! snapshot. The caller shows the menu, reads `can_operate` again after it
//! closes, and executes the plan: `ThreadActions` for the commands, the
//! dialogs, clipboard, router and toast for the rest.
//!
//! Both consumers call this: the sidebar row menu (`handle_thread_menu`) and
//! the chat header. The surfaces differ in one place. Settle and snooze park
//! the thread, and the sidebar then moves the reader forward. The chat header
//! stays where it is. `ThreadMenuPlan` carries that choice as
//! `navigate_after_park`.
// Consumers are wired by T22.
#![allow(dead_code)]

use crate::components::sidebar::runtime::thread_settled::SnoozePreset;
use crate::components::thread_action_menu_logic::thread_action_requires_operate;

/// Where the menu opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadMenuSurface {
    /// The sidebar row: settle and snooze park the thread, then move the reader forward.
    Sidebar,
    /// The chat header: settle and snooze stay on the thread the reader is reading.
    ChatHeader,
}

/// The click-time snapshot `plan_thread_action` reads.
#[derive(Clone, Debug)]
pub struct ThreadMenuInput<'a> {
    pub surface: ThreadMenuSurface,
    /// Read again after the native menu closes, so a grant revoked while the
    /// menu was open is honored.
    pub can_operate: bool,
    pub thread_title: &'a str,
    pub thread_id: &'a str,
    pub branch: Option<&'a str>,
    pub worktree_path: Option<&'a str>,
    /// Fallback for "Copy path" when the thread has no worktree.
    pub project_cwd: Option<&'a str>,
    pub is_regenerating_title: bool,
    pub confirm_thread_archive: bool,
    pub confirm_thread_delete: bool,
    pub snooze_presets: &'a [SnoozePreset],
}

/// What to do for a clicked menu item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadMenuPlan {
    /// No item, a stale id, or a guard that stops the action.
    Nothing,
    /// The connection lacks operate. Toast "Thread action unavailable" with
    /// "This connection cannot change threads.".
    Unavailable,
    /// Sidebar only: scope the list to the thread's project.
    FilterByProject,
    OpenProjectSettings,
    /// Create a thread on this thread's branch: its worktree when it has one,
    /// otherwise its branch on the local checkout.
    NewThreadOnBranch {
        branch: Option<String>,
        worktree_path: Option<String>,
    },
    Settle {
        navigate_after_park: bool,
    },
    Unsettle,
    Unsnooze,
    Pin,
    /// Confirms first when the user asked for it (`confirmAndUnpinThread`).
    Unpin,
    AutoSettle {
        enabled: bool,
    },
    /// The chosen preset's wake time.
    Snooze {
        snoozed_until: String,
        navigate_after_park: bool,
    },
    /// The custom-snooze dialog picks the time.
    SnoozeCustom,
    Rename,
    RegenerateTitle,
    MarkUnread,
    CopyPath(String),
    /// "Path unavailable" toast: no worktree and no project root.
    PathUnavailable,
    CopyBranch(String),
    CopyThreadId(String),
    /// Archive, after `confirm` when present. `Archive thread "<title>"?`.
    Archive {
        confirm: Option<String>,
    },
    /// Delete, after `confirm` when present.
    Delete {
        confirm: Option<String>,
    },
}

impl ThreadMenuPlan {
    /// The toast title for a failed command, when the plan dispatches one.
    /// Archive and delete pick their title from the outcome; see
    /// `archive_failure_title` and `should_report_delete_failure`.
    pub fn failure_title(&self) -> Option<&'static str> {
        Some(match self {
            Self::NewThreadOnBranch { .. } => "Could not create thread",
            Self::Settle { .. } => "Failed to settle thread",
            Self::Unsettle => "Failed to un-settle thread",
            Self::Unsnooze => "Failed to wake thread",
            Self::Pin => "Failed to pin thread",
            Self::Unpin => "Failed to unpin thread",
            Self::AutoSettle { .. } => "Failed to update auto-settle",
            Self::Snooze { .. } => "Failed to snooze thread",
            Self::RegenerateTitle => "Failed to regenerate thread title",
            Self::Archive { .. } => "Failed to archive thread",
            Self::Delete { .. } => "Failed to delete thread",
            _ => return None,
        })
    }
}

/// The archive failure title. A thread that archived but could not navigate
/// away reports that instead.
pub fn archive_failure_title(archived: bool) -> &'static str {
    if archived {
        "Thread archived, but navigation failed"
    } else {
        "Failed to archive thread"
    }
}

/// A failed delete is reported only while the thread is still in the store. A
/// thread already gone means worktree cleanup failed after a good delete, and
/// `deleteThread` has toasted that itself.
pub fn should_report_delete_failure(thread_still_in_store: bool) -> bool {
    thread_still_in_store
}

/// `archive`'s dialog text.
pub fn archive_confirm_message(title: &str) -> String {
    format!("Archive thread \"{title}\"?")
}

/// `delete`'s dialog text.
pub fn delete_confirm_message(title: &str) -> String {
    [
        format!("Delete thread \"{title}\"?"),
        "This permanently clears conversation history for this thread.".to_owned(),
    ]
    .join("\n")
}

/// The plan for a clicked item. `action` is the menu id, as upstream builds it.
pub fn plan_thread_action(action: &str, input: &ThreadMenuInput<'_>) -> ThreadMenuPlan {
    if thread_action_requires_operate(action) && !input.can_operate {
        return ThreadMenuPlan::Unavailable;
    }
    let park_navigates = input.surface == ThreadMenuSurface::Sidebar;
    if let Some(choice) = action.strip_prefix("snooze:") {
        if choice == "custom" {
            return ThreadMenuPlan::SnoozeCustom;
        }
        return match input
            .snooze_presets
            .iter()
            .find(|preset| preset.id.as_str() == choice)
        {
            Some(preset) => ThreadMenuPlan::Snooze {
                snoozed_until: preset.snoozed_until.clone(),
                navigate_after_park: park_navigates,
            },
            None => ThreadMenuPlan::Nothing,
        };
    }
    match action {
        "filter-by-project" => ThreadMenuPlan::FilterByProject,
        "project-settings" => ThreadMenuPlan::OpenProjectSettings,
        "new-thread-on-branch" => ThreadMenuPlan::NewThreadOnBranch {
            branch: input.branch.map(str::to_owned),
            worktree_path: input.worktree_path.map(str::to_owned),
        },
        "settle" => ThreadMenuPlan::Settle {
            navigate_after_park: park_navigates,
        },
        "unsettle" => ThreadMenuPlan::Unsettle,
        "unsnooze" => ThreadMenuPlan::Unsnooze,
        "pin" => ThreadMenuPlan::Pin,
        "unpin" => ThreadMenuPlan::Unpin,
        "auto-settle:enabled" => ThreadMenuPlan::AutoSettle { enabled: true },
        "auto-settle:disabled" => ThreadMenuPlan::AutoSettle { enabled: false },
        "rename" => ThreadMenuPlan::Rename,
        "regenerate-title" if input.is_regenerating_title => ThreadMenuPlan::Nothing,
        "regenerate-title" => ThreadMenuPlan::RegenerateTitle,
        "mark-unread" => ThreadMenuPlan::MarkUnread,
        "copy-path" => match input.worktree_path.or(input.project_cwd) {
            Some(path) => ThreadMenuPlan::CopyPath(path.to_owned()),
            None => ThreadMenuPlan::PathUnavailable,
        },
        "copy-branch" => match input.branch {
            Some(branch) => ThreadMenuPlan::CopyBranch(branch.to_owned()),
            None => ThreadMenuPlan::Nothing,
        },
        "copy-thread-id" => ThreadMenuPlan::CopyThreadId(input.thread_id.to_owned()),
        "archive" => ThreadMenuPlan::Archive {
            confirm: input
                .confirm_thread_archive
                .then(|| archive_confirm_message(input.thread_title)),
        },
        "delete" => ThreadMenuPlan::Delete {
            confirm: input
                .confirm_thread_delete
                .then(|| delete_confirm_message(input.thread_title)),
        },
        _ => ThreadMenuPlan::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ThreadMenuInput, ThreadMenuPlan, ThreadMenuSurface, archive_failure_title,
        delete_confirm_message, plan_thread_action, should_report_delete_failure,
    };
    use crate::components::sidebar::runtime::thread_settled::{SnoozePreset, SnoozePresetId};

    fn preset(id: SnoozePresetId, until: &str) -> SnoozePreset {
        SnoozePreset {
            id,
            label: "In 1 hour",
            when_label: "3 PM".into(),
            snoozed_until: until.into(),
        }
    }

    fn input<'a>(presets: &'a [SnoozePreset]) -> ThreadMenuInput<'a> {
        ThreadMenuInput {
            surface: ThreadMenuSurface::ChatHeader,
            can_operate: true,
            thread_title: "Thread",
            thread_id: "thread",
            branch: Some("main"),
            worktree_path: None,
            project_cwd: Some("/project"),
            is_regenerating_title: false,
            confirm_thread_archive: true,
            confirm_thread_delete: true,
            snooze_presets: presets,
        }
    }

    /// Upstream: "rechecks after the native menu closes". The grant is read
    /// after the menu, so a denial there blocks each mutation.
    #[test]
    fn mutations_recheck_the_grant_after_the_native_menu_closes() {
        let presets: [SnoozePreset; 0] = [];
        let denied = ThreadMenuInput {
            can_operate: false,
            ..input(&presets)
        };
        for action in [
            "rename",
            "regenerate-title",
            "delete",
            "pin",
            "settle",
            "archive",
        ] {
            assert_eq!(
                plan_thread_action(action, &denied),
                ThreadMenuPlan::Unavailable,
                "{action}"
            );
        }
    }

    /// Upstream: "keeps %s available without task permission".
    #[test]
    fn local_navigation_read_markers_and_copies_stay_available_without_permission() {
        let presets: [SnoozePreset; 0] = [];
        let denied = ThreadMenuInput {
            can_operate: false,
            ..input(&presets)
        };
        assert!(matches!(
            plan_thread_action("new-thread-on-branch", &denied),
            ThreadMenuPlan::NewThreadOnBranch { .. }
        ));
        assert_eq!(
            plan_thread_action("copy-thread-id", &denied),
            ThreadMenuPlan::CopyThreadId("thread".into())
        );
        assert_eq!(
            plan_thread_action("mark-unread", &denied),
            ThreadMenuPlan::MarkUnread
        );
        assert_eq!(
            plan_thread_action("project-settings", &denied),
            ThreadMenuPlan::OpenProjectSettings
        );
    }

    #[test]
    fn a_granted_menu_runs_the_mutation_it_names() {
        let presets: [SnoozePreset; 0] = [];
        assert_eq!(
            plan_thread_action("pin", &input(&presets)),
            ThreadMenuPlan::Pin
        );
        assert_eq!(
            plan_thread_action("auto-settle:disabled", &input(&presets)),
            ThreadMenuPlan::AutoSettle { enabled: false }
        );
    }

    #[test]
    fn the_sidebar_parks_then_moves_forward_while_the_chat_header_stays() {
        let presets: [SnoozePreset; 0] = [];
        let sidebar = ThreadMenuInput {
            surface: ThreadMenuSurface::Sidebar,
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("settle", &sidebar),
            ThreadMenuPlan::Settle {
                navigate_after_park: true
            }
        );
        assert_eq!(
            plan_thread_action("settle", &input(&presets)),
            ThreadMenuPlan::Settle {
                navigate_after_park: false
            }
        );
    }

    #[test]
    fn snooze_prefix_resolves_a_preset_or_opens_the_custom_dialog() {
        let presets = [preset(SnoozePresetId::Hour, "2099-01-01T00:00:00Z")];
        assert_eq!(
            plan_thread_action("snooze:hour", &input(&presets)),
            ThreadMenuPlan::Snooze {
                snoozed_until: "2099-01-01T00:00:00Z".into(),
                navigate_after_park: false,
            }
        );
        assert_eq!(
            plan_thread_action("snooze:custom", &input(&presets)),
            ThreadMenuPlan::SnoozeCustom
        );
        // An id that is not in the menu is ignored, as upstream's `find` does.
        assert_eq!(
            plan_thread_action("snooze:missing", &input(&presets)),
            ThreadMenuPlan::Nothing
        );
    }

    #[test]
    fn regenerating_a_title_is_a_no_op() {
        let presets: [SnoozePreset; 0] = [];
        let busy = ThreadMenuInput {
            is_regenerating_title: true,
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("regenerate-title", &busy),
            ThreadMenuPlan::Nothing
        );
        assert_eq!(
            plan_thread_action("regenerate-title", &input(&presets)),
            ThreadMenuPlan::RegenerateTitle
        );
    }

    #[test]
    fn copy_path_falls_back_to_the_project_root_then_reports_unavailable() {
        let presets: [SnoozePreset; 0] = [];
        assert_eq!(
            plan_thread_action("copy-path", &input(&presets)),
            ThreadMenuPlan::CopyPath("/project".into())
        );
        let worktree = ThreadMenuInput {
            worktree_path: Some("/worktrees/thread"),
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("copy-path", &worktree),
            ThreadMenuPlan::CopyPath("/worktrees/thread".into())
        );
        let nowhere = ThreadMenuInput {
            project_cwd: None,
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("copy-path", &nowhere),
            ThreadMenuPlan::PathUnavailable
        );
    }

    #[test]
    fn copy_branch_needs_a_branch() {
        let presets: [SnoozePreset; 0] = [];
        let detached = ThreadMenuInput {
            branch: None,
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("copy-branch", &detached),
            ThreadMenuPlan::Nothing
        );
        assert_eq!(
            plan_thread_action("copy-branch", &input(&presets)),
            ThreadMenuPlan::CopyBranch("main".into())
        );
    }

    #[test]
    fn archive_and_delete_confirm_only_when_the_setting_is_on() {
        let presets: [SnoozePreset; 0] = [];
        assert_eq!(
            plan_thread_action("archive", &input(&presets)),
            ThreadMenuPlan::Archive {
                confirm: Some("Archive thread \"Thread\"?".into())
            }
        );
        let quiet = ThreadMenuInput {
            confirm_thread_archive: false,
            confirm_thread_delete: false,
            ..input(&presets)
        };
        assert_eq!(
            plan_thread_action("archive", &quiet),
            ThreadMenuPlan::Archive { confirm: None }
        );
        assert_eq!(
            plan_thread_action("delete", &quiet),
            ThreadMenuPlan::Delete { confirm: None }
        );
    }

    #[test]
    fn the_delete_dialog_names_the_thread_and_the_permanence() {
        assert_eq!(
            delete_confirm_message("Release prep"),
            "Delete thread \"Release prep\"?\nThis permanently clears conversation history for this thread."
        );
    }

    #[test]
    fn archive_failure_title_reports_a_navigation_failure_after_archiving() {
        assert_eq!(
            archive_failure_title(true),
            "Thread archived, but navigation failed"
        );
        assert_eq!(archive_failure_title(false), "Failed to archive thread");
    }

    #[test]
    fn a_delete_failure_is_toasted_only_while_the_thread_is_still_there() {
        assert!(should_report_delete_failure(true));
        assert!(!should_report_delete_failure(false));
    }

    #[test]
    fn unknown_ids_do_nothing() {
        let presets: [SnoozePreset; 0] = [];
        assert_eq!(
            plan_thread_action("nope", &input(&presets)),
            ThreadMenuPlan::Nothing
        );
    }

    #[test]
    fn dispatching_plans_name_their_failure_toast() {
        let presets: [SnoozePreset; 0] = [];
        assert_eq!(
            plan_thread_action("unsettle", &input(&presets)).failure_title(),
            Some("Failed to un-settle thread")
        );
        assert_eq!(
            plan_thread_action("rename", &input(&presets)).failure_title(),
            None
        );
    }
}
