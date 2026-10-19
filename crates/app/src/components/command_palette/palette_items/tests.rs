use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

use super::*;
use crate::components::command_palette::command_palette_logic::PaletteItemKind;
use crate::components::command_palette::palette_source::{
    PaletteContext, PaletteSnapshot, ReferenceCopyTarget, SettingsSearchItem, ThemeCard,
};
use crate::components::sidebar::runtime::thread_sort::ThreadSortOrder;
use crate::components::sidebar::view_model::{SidebarProjectGroup, SidebarSnapshot};
use crate::state::models::{EnvironmentProject, EnvironmentThreadShell};

fn project(id: &str, title: &str, root: &str, scratch: bool) -> EnvironmentProject {
    EnvironmentProject {
        environment_id: EnvironmentId::new("local").unwrap(),
        id: ProjectId::new(id).unwrap(),
        title: title.into(),
        workspace_root: root.into(),
        favicon_path: None,
        project_icon: None,
        created_at: None,
        updated_at: None,
        is_scratch: scratch,
        source: None,
    }
}

fn group(project: &EnvironmentProject) -> SidebarProjectGroup {
    SidebarProjectGroup {
        project_key: project.key(),
        display_name: project.title.clone(),
        created_at: None,
        updated_at: None,
        member_project_keys: vec![project.key()],
    }
}

fn thread(id: &str, project: &EnvironmentProject, title: &str) -> EnvironmentThreadShell {
    EnvironmentThreadShell::new(
        EnvironmentId::new("local").unwrap(),
        ThreadId::new(id).unwrap(),
        project.id.clone(),
        title,
        "2026-03-09T10:00:00.000Z",
    )
}

fn snapshot() -> (PaletteSnapshot, EnvironmentProject, EnvironmentProject) {
    let turin = project("turin", "turin", "/code/turin", false);
    let site = project("site", "website", "/code/site", false);
    let sidebar = SidebarSnapshot {
        projects: vec![turin.clone(), site.clone()],
        project_groups: vec![group(&turin), group(&site)],
        threads: vec![thread("t1", &site, "Draft the launch post")],
        ..Default::default()
    };
    (
        PaletteSnapshot {
            sidebar,
            thread_sort_order: ThreadSortOrder::UpdatedAt,
            environment_label: "Local".into(),
            new_projects_root: None,
            scratch_thread_available: false,
            supports_thread_find: false,
            pull_requests_available: true,
            link_pull_request_available: true,
            thread_pull_requests_available: true,
            reference_copy_target: None,
            add_project_base_directory: None,
            current_project_cwd: None,
            appearance_mode: AppearanceModeChoice::Dark,
            active_theme_id: None,
            theme_cards: vec![
                ThemeCard {
                    id: None,
                    label: "T3 Code".into(),
                    previews: Vec::new(),
                },
                ThemeCard {
                    id: Some("grove".into()),
                    label: "Grove".into(),
                    previews: Vec::new(),
                },
            ],
        },
        turin,
        site,
    )
}

fn values(items: &[CommandPaletteItem]) -> Vec<&str> {
    items.iter().map(|item| item.value.as_str()).collect()
}

#[test]
fn the_picker_puts_the_current_projects_group_first() {
    let groups = vec![
        SidebarProjectGroup {
            project_key: "local:a".into(),
            display_name: "A".into(),
            created_at: None,
            updated_at: None,
            member_project_keys: vec!["local:a".into()],
        },
        SidebarProjectGroup {
            project_key: "local:b".into(),
            display_name: "B".into(),
            created_at: None,
            updated_at: None,
            member_project_keys: vec!["local:b".into(), "local:b2".into()],
        },
    ];
    let entries = build_sidebar_project_picker_entries(&groups, Some("local:b2"));
    assert_eq!(entries[0].group_key, "local:b");
    assert!(entries[0].is_preferred);
    // The preferred member is the target, not the group's own project.
    assert_eq!(entries[0].target_project_key, "local:b2");
    assert_eq!(entries[1].target_project_key, "local:a");
    let untouched = build_sidebar_project_picker_entries(&groups, None);
    assert_eq!(untouched[0].group_key, "local:a");
}

#[test]
fn root_actions_follow_upstream_order_for_the_local_toolbox() {
    let (snapshot, _, _) = snapshot();
    let context = PaletteContext::default();
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_action_items(&model);
    assert_eq!(
        values(&items),
        vec![
            "action:new-thread",
            "action:new-thread-in",
            "action:open-file-picker",
            "action:search-project-contents",
            "action:add-project",
            "action:change-theme",
            "action:change-appearance",
            "action:theme-editor",
            "action:pull-requests",
            "action:usage",
            "action:settings",
            "action:project-settings",
        ]
    );
    // With no routed thread the first ordered project is current.
    assert_eq!(
        items[0].title,
        PaletteTitle::NewThreadIn {
            project: "turin".into()
        }
    );
}

#[test]
fn contextual_actions_need_an_active_thread_and_capabilities() {
    let (mut snapshot, _, site) = snapshot();
    snapshot.scratch_thread_available = true;
    snapshot.supports_thread_find = true;
    snapshot.new_projects_root = Some("~/Projects".into());
    snapshot.reference_copy_target = Some(ReferenceCopyTarget {
        kind: turin_shared::thread_reference::ThreadReferenceKind::Thread,
        value: "t1".into(),
        success_title: "Thread ID copied".into(),
        failure_title: "Failed to copy thread ID".into(),
    });
    let context = PaletteContext {
        active_thread_key: Some(format!("local:{}", "t1")),
        active_draft_project_key: None,
    };
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_action_items(&model);
    let ids = values(&items);
    for expected in [
        "action:new-thread-without-project",
        "find-current-thread",
        "action:copy-thread-reference",
        "action:link-pull-request",
        "action:open-thread-pull-requests",
        "action:restart-agent-session",
        "action:new-project",
    ] {
        assert!(ids.contains(&expected), "{expected} in {ids:?}");
    }
    // The routed thread's project is current: website, not the first project.
    assert_eq!(
        items[0].title,
        PaletteTitle::NewThreadIn {
            project: site.title.clone()
        }
    );
    // No linked PRs: the list entry is disabled.
    let linked = items
        .iter()
        .find(|item| item.value == "action:open-thread-pull-requests")
        .unwrap();
    assert!(linked.disabled);
    let copy = items
        .iter()
        .find(|item| item.value == "action:copy-thread-reference")
        .unwrap();
    assert_eq!(copy.title, PaletteTitle::Text("Copy thread ID".into()));
}

#[test]
fn new_thread_in_lists_the_current_project_first_with_jump_shortcuts_and_no_project_second() {
    let (mut snapshot, _, _) = snapshot();
    snapshot.scratch_thread_available = true;
    let context = PaletteContext {
        active_thread_key: Some("local:t1".into()),
        active_draft_project_key: None,
    };
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_project_thread_items(&model);
    assert_eq!(
        values(&items),
        vec![
            "new-thread-in:local:site",
            "new-thread-in:no-project",
            "new-thread-in:local:turin"
        ]
    );
    assert_eq!(items[0].shortcut_command.as_deref(), Some("thread.jump.1"));
    // "No project" keeps its own shortcut so the projects' ⌘1..9 hold.
    assert_eq!(
        items[1].shortcut_command.as_deref(),
        Some("chat.newWithoutProject")
    );
    assert_eq!(items[2].shortcut_command.as_deref(), Some("thread.jump.2"));
}

#[test]
fn scratch_projects_are_listed_once_as_no_project() {
    let (mut snapshot, _, _) = snapshot();
    let scratch = project("scratch", "scratch", "/scratch", true);
    snapshot.sidebar.project_groups.insert(0, group(&scratch));
    snapshot.sidebar.projects.push(scratch);
    snapshot.scratch_thread_available = true;
    let context = PaletteContext::default();
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_project_thread_items(&model);
    assert!(
        values(&items)
            .iter()
            .all(|value| *value != "new-thread-in:local:scratch")
    );
    assert_eq!(
        values(&items)
            .iter()
            .filter(|value| **value == "new-thread-in:no-project")
            .count(),
        1
    );
}

#[test]
fn theme_and_appearance_lists_mark_the_current_choice() {
    let (snapshot, _, _) = snapshot();
    let groups = build_theme_groups(&snapshot);
    assert_eq!(groups[0].label, "Change theme");
    assert_eq!(groups[0].items[0].value, "theme:standard");
    assert_eq!(groups[0].items[1].value, "theme:palette:grove");
    assert!(matches!(
        groups[0].items[0].title_trailing_content[0],
        PaletteAddon::ThemePreview { current: true, .. }
    ));
    let appearance = build_appearance_groups(&snapshot);
    let dark = appearance[0]
        .items
        .iter()
        .find(|item| item.value == "appearance:dark")
        .unwrap();
    assert_eq!(dark.title_trailing_content, vec![PaletteAddon::Current]);
    let light = appearance[0]
        .items
        .iter()
        .find(|item| item.value == "appearance:light")
        .unwrap();
    assert!(light.title_trailing_content.is_empty());
}

#[test]
fn settings_hits_become_secondary_aware_items() {
    let items = build_settings_search_items(&[SettingsSearchItem {
        id: "default-model".into(),
        title: "Default model".into(),
        section_label: "General".into(),
        search_terms: vec!["gpt".into()],
        secondary: true,
    }]);
    assert_eq!(items[0].value, "setting:default-model");
    assert_eq!(
        items[0].search_terms,
        vec!["Default model", "General", "gpt"]
    );
    assert!(items[0].secondary);
    assert!(matches!(items[0].kind, PaletteItemKind::Action { .. }));
}

#[test]
fn project_search_items_describe_grouped_projects_by_their_environments() {
    let (snapshot, turin, _) = snapshot();
    let context = PaletteContext::default();
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_project_search_items(&model);
    assert_eq!(items[0].value, "project:local:turin");
    assert_eq!(
        items[0].description,
        Some(PaletteDescription::ProjectLocation {
            location_label: "Local".into(),
            workspace_root: turin.workspace_root.clone()
        })
    );
    assert!(items[0].search_terms.contains(&"Local".to_owned()));
}

#[test]
fn the_live_palette_lists_no_command_it_cannot_run() {
    let (mut snapshot, _, _) = snapshot();
    // What the mock backend (and so the live one, before the filter) advertises.
    snapshot.scratch_thread_available = true;
    snapshot.supports_thread_find = true;
    let snapshot =
        crate::components::command_palette::live_source::hide_unported_commands(snapshot);
    let context = PaletteContext {
        active_thread_key: Some("local:t1".to_owned()),
        active_draft_project_key: None,
    };
    let model = PaletteModel::new(&snapshot, &context, true);
    let items = build_action_items(&model);
    let ids = values(&items);
    assert!(
        !ids.contains(&"action:new-thread-without-project"),
        "{ids:?}"
    );
    // The find bar is live, so the palette keeps its row.
    assert!(ids.contains(&"find-current-thread"), "{ids:?}");
}

#[test]
fn new_project_goes_under_the_data_directorys_projects_folder_not_a_fixed_home_path() {
    use crate::components::command_palette::live_source::with_new_projects_root;
    let (mut snapshot, _, _) = snapshot();
    snapshot.new_projects_root = Some("~/Projects/t3".into());

    let with_root = with_new_projects_root(
        snapshot.clone(),
        Some(std::path::Path::new("/Users/me/.turin/userdata")),
    );
    assert_eq!(
        with_root.new_projects_root.as_deref(),
        Some("/Users/me/.turin/projects")
    );
    // Without a data folder there is nowhere to put one, so the row is not offered.
    let without = with_new_projects_root(snapshot, None);
    assert_eq!(without.new_projects_root, None);
}
