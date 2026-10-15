//! The palette's item lists: the `useMemo` bodies of upstream
//! `OpenCommandPaletteDialog` that turn projects, threads and context into
//! `CommandPaletteItem`s, as pure functions of a [`PaletteSnapshot`] so they
//! are testable without a window.
//!
//! Commands for cut features are not built (PORTING.md "Cut list"): the
//! clone/remote sources of Add project, the WSL folder entry, environment
//! pickers, copy prompt link (prompt links are undecided, TASKS N10) and the
//! multi-environment labels.

use std::collections::HashMap;

use super::command_palette_logic::{
    AppearanceModeChoice, BuildProjectActionItemsInput, BuildThreadActionItemsInput,
    CommandPaletteGroup, CommandPaletteItem, CommandPaletteProject, PaletteAction, PaletteAddon,
    PaletteDescription, PaletteIcon, PaletteTitle, ThreadSubtitle, build_project_action_items,
    build_thread_action_items, enumerate_command_palette_items,
};
use super::palette_source::{PaletteContext, PaletteSnapshot};
use crate::components::sidebar::view_model::{SidebarProjectGroup, ThreadSearchMatch};
use crate::state::models::{EnvironmentProject, EnvironmentThreadShell};
use crate::ui::icon::IconName;

const ITEM_ICON: fn(IconName) -> PaletteIcon = PaletteIcon::Named;

/// `SidebarProjectPickerEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub group_key: String,
    pub group_display_name: String,
    pub member_project_keys: Vec<String>,
    /// The member the entry opens (`targetProject`), by `environmentId:projectId`.
    pub target_project_key: String,
    pub is_preferred: bool,
}

/// `buildSidebarProjectPickerEntries`: one entry per project group, the group
/// holding the preferred (current) project first.
pub fn build_sidebar_project_picker_entries(
    groups: &[SidebarProjectGroup],
    preferred_project_key: Option<&str>,
) -> Vec<ProjectPickerEntry> {
    let mut entries: Vec<ProjectPickerEntry> = groups
        .iter()
        .filter_map(|group| {
            let is_preferred = preferred_project_key
                .is_some_and(|key| group.member_project_keys.iter().any(|member| member == key));
            let preferred_member = preferred_project_key
                .filter(|key| group.member_project_keys.iter().any(|member| member == key));
            let target = preferred_member
                .map(str::to_owned)
                .or_else(|| {
                    group
                        .member_project_keys
                        .iter()
                        .find(|member| **member == group.project_key)
                        .cloned()
                })
                .or_else(|| group.member_project_keys.first().cloned())?;
            Some(ProjectPickerEntry {
                group_key: group.project_key.clone(),
                group_display_name: group.display_name.clone(),
                member_project_keys: group.member_project_keys.clone(),
                target_project_key: target,
                is_preferred,
            })
        })
        .collect();
    if let Some(index) = entries.iter().position(|entry| entry.is_preferred)
        && index > 0
    {
        let preferred = entries.remove(index);
        entries.insert(0, preferred);
    }
    entries
}

/// Derived state shared by the item builders.
pub struct PaletteModel<'a> {
    pub snapshot: &'a PaletteSnapshot,
    pub context: &'a PaletteContext,
    pub active_thread: Option<&'a EnvironmentThreadShell>,
    pub picker_entries: Vec<ProjectPickerEntry>,
    pub picker_projects: Vec<CommandPaletteProject>,
    pub mac: bool,
}

impl<'a> PaletteModel<'a> {
    pub fn new(snapshot: &'a PaletteSnapshot, context: &'a PaletteContext, mac: bool) -> Self {
        let active_thread = context.active_thread_key.as_deref().and_then(|key| {
            snapshot
                .sidebar
                .threads
                .iter()
                .find(|thread| thread.key() == key)
        });
        // `resolveThreadActionProjectRef`: the active thread's project, else the
        // active draft's, else the first ordered project.
        let preferred_project_key = active_thread
            .map(EnvironmentThreadShell::project_key)
            .or_else(|| context.active_draft_project_key.clone())
            .or_else(|| {
                snapshot
                    .sidebar
                    .projects
                    .first()
                    .map(EnvironmentProject::key)
            });
        let picker_entries = build_sidebar_project_picker_entries(
            &snapshot.sidebar.project_groups,
            preferred_project_key.as_deref(),
        );
        let picker_projects = picker_entries
            .iter()
            .filter_map(|entry| {
                let project = snapshot.sidebar.project(&entry.target_project_key)?;
                Some(CommandPaletteProject {
                    environment_id: project.environment_id.clone(),
                    id: project.id.as_str().to_owned(),
                    title: project.title.clone(),
                    workspace_root: project.workspace_root.clone(),
                    display_name: entry.group_display_name.clone(),
                })
            })
            .collect();
        Self {
            snapshot,
            context,
            active_thread,
            picker_entries,
            picker_projects,
            mac,
        }
    }

    fn project_title_by_id(&self) -> HashMap<String, String> {
        self.snapshot
            .sidebar
            .projects
            .iter()
            .map(|project| (project.id.as_str().to_owned(), project.title.clone()))
            .collect()
    }

    fn is_scratch(&self, project: &CommandPaletteProject) -> bool {
        self.snapshot
            .sidebar
            .project(&project.key())
            .is_some_and(|project| project.is_scratch)
    }

    /// The group (entry) a picker project stands for.
    fn entry_for(&self, project: &CommandPaletteProject) -> Option<&ProjectPickerEntry> {
        self.picker_entries
            .iter()
            .find(|entry| entry.target_project_key == project.key())
    }

    /// The preferred project's group name, else the current project's title.
    fn active_project_title(&self) -> Option<String> {
        self.picker_entries
            .iter()
            .find(|entry| entry.is_preferred)
            .map(|entry| entry.group_display_name.clone())
            .or_else(|| {
                let key = self
                    .active_thread
                    .map(EnvironmentThreadShell::project_key)
                    .or_else(|| self.context.active_draft_project_key.clone())?;
                self.snapshot
                    .sidebar
                    .project(&key)
                    .map(|project| project.title.clone())
            })
    }

    fn project_icon(&self) -> impl Fn(&CommandPaletteProject) -> PaletteIcon {
        |_| PaletteIcon::ProjectFavicon
    }
}

/// `projectSearchItems`: projects found by typing a query.
pub fn build_project_search_items(model: &PaletteModel<'_>) -> Vec<CommandPaletteItem> {
    let environment_label = model.snapshot.environment_label.clone();
    let icon = model.project_icon();
    let search_terms = |project: &CommandPaletteProject| {
        let mut terms = Vec::new();
        if let Some(entry) = model.entry_for(project) {
            for key in &entry.member_project_keys {
                if let Some(member) = model.snapshot.sidebar.project(key) {
                    terms.push(member.title.clone());
                    terms.push(member.workspace_root.clone());
                }
            }
        }
        terms.push(environment_label.clone());
        terms
    };
    let description = |project: &CommandPaletteProject| {
        let grouped = model
            .entry_for(project)
            .is_some_and(|entry| entry.member_project_keys.len() > 1);
        if grouped {
            PaletteDescription::ProjectEnvironments(environment_label.clone())
        } else {
            PaletteDescription::ProjectLocation {
                location_label: environment_label.clone(),
                workspace_root: project.workspace_root.clone(),
            }
        }
    };
    build_project_action_items(&BuildProjectActionItemsInput {
        projects: &model.picker_projects,
        value_prefix: "project",
        icon: &icon,
        run_project: &|project| PaletteAction::OpenProject {
            project_key: project.key(),
        },
        search_terms: Some(&search_terms),
        render_description: Some(&description),
        shortcut_command: None,
    })
}

/// `projectThreadItems`: the "New thread in…" list, with "No project" right
/// after the current project.
pub fn build_project_thread_items(model: &PaletteModel<'_>) -> Vec<CommandPaletteItem> {
    let environment_label = model.snapshot.environment_label.clone();
    let real_projects: Vec<CommandPaletteProject> = model
        .picker_projects
        .iter()
        .filter(|project| !model.is_scratch(project))
        .cloned()
        .collect();
    let icon = model.project_icon();
    let search_terms = |project: &CommandPaletteProject| {
        let mut terms = Vec::new();
        if let Some(entry) = model.entry_for(project) {
            for key in &entry.member_project_keys {
                if let Some(member) = model.snapshot.sidebar.project(key) {
                    terms.push(member.title.clone());
                    terms.push(member.workspace_root.clone());
                }
            }
        }
        terms.push(environment_label.clone());
        terms
    };
    let description = |project: &CommandPaletteProject| PaletteDescription::ProjectLocation {
        location_label: environment_label.clone(),
        workspace_root: project.workspace_root.clone(),
    };
    let preferred_project_key = model
        .active_thread
        .map(EnvironmentThreadShell::project_key)
        .or_else(|| model.context.active_draft_project_key.clone())
        .or_else(|| {
            model
                .snapshot
                .sidebar
                .projects
                .first()
                .map(EnvironmentProject::key)
        });
    let run = |project: &CommandPaletteProject| {
        // The current project wins when it belongs to the picked group.
        let contextual_belongs = preferred_project_key.as_ref().is_some_and(|key| {
            model
                .entry_for(project)
                .is_some_and(|entry| entry.member_project_keys.contains(key))
        });
        PaletteAction::NewThreadInProject {
            project_key: if contextual_belongs {
                preferred_project_key
                    .clone()
                    .unwrap_or_else(|| project.key())
            } else {
                project.key()
            },
        }
    };
    let project_items = enumerate_command_palette_items(build_project_action_items(
        &BuildProjectActionItemsInput {
            projects: &real_projects,
            value_prefix: "new-thread-in",
            icon: &icon,
            run_project: &run,
            search_terms: Some(&search_terms),
            render_description: Some(&description),
            shortcut_command: None,
        },
    ));
    if !model.snapshot.scratch_thread_available {
        return project_items;
    }

    // "No project" goes right after the current project: visible without
    // scrolling past every project, while Enter still starts in the current
    // one. When the current thread has no project, it is the current entry and
    // goes first. It keeps its own shortcut, so the projects' mod+1..9 hold.
    let no_project_index = match model.picker_projects.first() {
        Some(first) if model.is_scratch(first) => 0,
        _ => 1.min(project_items.len()),
    };
    let mut items = project_items;
    items.insert(
        no_project_index,
        CommandPaletteItem::action(
            "new-thread-in:no-project",
            "No project",
            ITEM_ICON(IconName::MessageSquareDashed),
            PaletteAction::NewThreadWithoutProject,
        )
        .search_terms(["no project", "without project", "none"])
        .shortcut("chat.newWithoutProject"),
    );
    items
}

/// Per-thread lookups the thread items need besides the shell itself.
pub struct ThreadItemInput<'a> {
    pub model: &'a PaletteModel<'a>,
    /// Message hits keyed by scoped thread key.
    pub content_matches: &'a HashMap<String, ThreadSearchMatch>,
    pub search_query: &'a str,
    /// The query the hits were found for.
    pub matched_query: &'a str,
    pub now_ms: i64,
    pub limit: Option<usize>,
}

/// `allThreadItems` (and, with a limit, `recentThreadItems`).
pub fn build_all_thread_items(input: &ThreadItemInput<'_>) -> Vec<CommandPaletteItem> {
    let model = input.model;
    let snapshot = &model.snapshot.sidebar;
    let project_titles = model.project_title_by_id();
    let active_thread_id = model.active_thread.map(|thread| thread.id.clone());
    let leading = |thread: &EnvironmentThreadShell| {
        Some(PaletteAddon::ThreadLeadingStatus {
            thread_key: thread.key(),
        })
    };
    let trailing = |thread: &EnvironmentThreadShell| {
        vec![PaletteAddon::ThreadTrailing {
            thread_key: thread.key(),
            query: input.search_query.to_owned(),
            settled: thread.settled_override
                == Some(crate::state::models::SettledOverride::Settled),
        }]
    };
    let describe = |thread: &EnvironmentThreadShell, project_title: Option<&str>| {
        let instance_id = thread
            .runtime
            .as_ref()
            .map(|runtime| runtime.provider_instance_id.clone())
            .unwrap_or_else(|| thread.model_selection.instance_id.clone());
        let entry = snapshot.provider_entry(&instance_id);
        PaletteDescription::Thread(ThreadSubtitle {
            project_key: Some(thread.project_key()),
            project_title: project_title.map(str::to_owned),
            environment_label: Some(model.snapshot.environment_label.clone()),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            is_current: active_thread_id.as_ref() == Some(&thread.id),
            driver_kind: entry.map(|entry| entry.driver_kind),
            provider_display_name: thread
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.provider_name.clone())
                .or_else(|| entry.map(|entry| entry.display_name.clone()))
                .or(Some(instance_id)),
        })
    };
    let content_match = |thread: &EnvironmentThreadShell| {
        let hit = input.content_matches.get(&thread.key())?;
        Some(
            super::command_palette_logic::CommandPaletteThreadContentMatch {
                source: hit.source,
                snippet: hit.snippet.clone(),
                query: input.matched_query.to_owned(),
            },
        )
    };
    build_thread_action_items(&BuildThreadActionItemsInput {
        threads: &snapshot.threads,
        active_thread_id: active_thread_id.as_ref(),
        project_title_by_id: &project_titles,
        sort_order: model.snapshot.thread_sort_order,
        icon: ITEM_ICON(IconName::MessageSquare),
        render_leading_content: Some(&leading),
        render_trailing_content: Some(&trailing),
        render_description: Some(&describe),
        get_content_match: Some(&content_match),
        limit: input.limit,
        now_ms: input.now_ms,
    })
}

/// The root "Actions" list (`actionItems`).
pub fn build_action_items(model: &PaletteModel<'_>) -> Vec<CommandPaletteItem> {
    let snapshot = model.snapshot;
    let mut items: Vec<CommandPaletteItem> = Vec::new();
    let named = |name: IconName| ITEM_ICON(name);

    if !snapshot.sidebar.projects.is_empty() {
        if let Some(title) = model.active_project_title() {
            items.push(
                CommandPaletteItem::action(
                    "action:new-thread",
                    PaletteTitle::NewThreadIn { project: title },
                    named(IconName::SquarePen),
                    PaletteAction::NewThreadFromContext,
                )
                .search_terms(["new thread", "chat", "create", "draft"])
                .shortcut("chat.newLocal"),
            );
        }
        items.push(
            CommandPaletteItem::submenu(
                "action:new-thread-in",
                "New thread in...",
                named(IconName::SquarePen),
                named(IconName::SquarePen),
                vec![CommandPaletteGroup::new(
                    "projects",
                    "Projects",
                    build_project_thread_items(model),
                )],
            )
            .search_terms(["new thread", "project", "pick", "choose", "select"]),
        );
    }

    if snapshot.scratch_thread_available {
        items.push(
            CommandPaletteItem::action(
                "action:new-thread-without-project",
                "New thread without a project",
                named(IconName::MessageSquareDashed),
                PaletteAction::NewThreadWithoutProject,
            )
            .search_terms([
                "new thread",
                "no project",
                "without project",
                "none",
                "chat",
            ])
            .shortcut("chat.newWithoutProject"),
        );
    }

    let has_active_thread = model.active_thread.is_some();
    if has_active_thread && snapshot.supports_thread_find {
        items.push(
            CommandPaletteItem::action(
                "find-current-thread",
                "Find in current thread",
                named(IconName::TextSearch),
                PaletteAction::FindInCurrentThread,
            )
            .search_terms(["find", "search", "messages", "plans"])
            .shortcut("chat.find"),
        );
    }

    if let Some(target) = &snapshot.reference_copy_target {
        items.push(
            CommandPaletteItem::action(
                "action:copy-thread-reference",
                if target.kind == turin_shared::thread_reference::ThreadReferenceKind::PullRequest {
                    "Copy PR link"
                } else {
                    "Copy thread ID"
                },
                named(IconName::Link),
                PaletteAction::CopyThreadReference,
            )
            .search_terms(["copy", "pull request", "pr link", "thread id", "reference"])
            .description(target.value.clone())
            .shortcut("thread.copyReference"),
        );
    }

    if let Some(thread) = model.active_thread
        && snapshot.link_pull_request_available
    {
        items.push(
            CommandPaletteItem::action(
                "action:link-pull-request",
                "Link pull request to thread",
                named(IconName::GitPullRequestArrow),
                PaletteAction::LinkPullRequest,
            )
            .search_terms(["link", "pull request", "pr", "attach", "stack"]),
        );
        if snapshot.thread_pull_requests_available {
            let has_visible = thread.pull_requests.iter().any(|link| !link.dismissed);
            let mut item = CommandPaletteItem::action(
                "action:open-thread-pull-requests",
                "Show linked pull requests",
                named(IconName::GitPullRequestArrow),
                PaletteAction::ShowLinkedPullRequests,
            )
            .search_terms(["pull requests", "linked", "stack", "prs"]);
            item.disabled = !has_visible;
            items.push(item);
        }
    }

    if has_active_thread {
        items.push(
            CommandPaletteItem::action(
                "action:restart-agent-session",
                "Restart agent session",
                named(IconName::RotateCcw),
                PaletteAction::RestartAgentSession,
            )
            .search_terms([
                "restart", "reset", "reload", "agent", "session", "skills", "plugins", "mcp",
            ]),
        );
    }

    items.push(
        CommandPaletteItem::action(
            "action:open-file-picker",
            "Go to file",
            named(IconName::FileSearch),
            PaletteAction::OpenFilePicker,
        )
        .search_terms([
            "go to file",
            "open file",
            "file picker",
            "find file",
            "quick open",
        ])
        .keep_open()
        .shortcut("filePicker.toggle"),
    );
    items.push(
        CommandPaletteItem::action(
            "action:search-project-contents",
            "Search project contents",
            named(IconName::TextSearch),
            PaletteAction::SearchProjectContents,
        )
        .search_terms([
            "search project",
            "find in files",
            "grep",
            "content search",
            "text search",
        ])
        .keep_open()
        .shortcut("projectSearch.toggle"),
    );

    if snapshot.new_projects_root.is_some() {
        items.push(
            CommandPaletteItem::action(
                "action:new-project",
                "New project",
                named(IconName::FolderGit2),
                PaletteAction::NewProject,
            )
            .search_terms([
                "new project",
                "create project",
                "empty",
                "repository",
                "repo",
                "git init",
            ])
            .keep_open(),
        );
    }

    items.push(
        CommandPaletteItem::action(
            "action:add-project",
            "Add project",
            named(IconName::FolderPlus),
            PaletteAction::AddProject,
        )
        .search_terms([
            "add project",
            "folder",
            "directory",
            "browse",
            "repository",
            "repo",
            "git",
            "github",
        ])
        .keep_open(),
    );

    items.push(build_change_theme_item(snapshot));
    items.push(build_change_appearance_item(snapshot));

    items.push(
        CommandPaletteItem::action(
            "action:theme-editor",
            "Toggle theme editor",
            named(IconName::Palette),
            PaletteAction::ToggleThemeEditor,
        )
        .search_terms(["theme", "appearance", "colors", "palette", "customize"])
        .shortcut("themeEditor.toggle"),
    );

    if snapshot.pull_requests_available {
        items.push(
            CommandPaletteItem::action(
                "action:pull-requests",
                "Open pull requests",
                named(IconName::GitPullRequestArrow),
                PaletteAction::OpenPullRequests,
            )
            .search_terms([
                "pull requests",
                "prs",
                "pr",
                "github",
                "review",
                "merge",
                "branch",
            ]),
        );
    }

    items.push(
        CommandPaletteItem::action(
            "action:usage",
            "Open usage",
            named(IconName::ChartNoAxesColumn),
            PaletteAction::OpenUsage,
        )
        .search_terms([
            "usage",
            "use",
            "tokens",
            "cost",
            "spend",
            "limits",
            "stats",
            "analytics",
        ])
        .shortcut("usage.open"),
    );
    items.push(
        CommandPaletteItem::action(
            "action:settings",
            "Open settings",
            named(IconName::Settings),
            PaletteAction::OpenSettings,
        )
        .search_terms(["settings", "preferences", "configuration", "keybindings"]),
    );

    // Target the active thread or draft's project, falling back to the first sidebar group.
    let contextual_group = model
        .picker_entries
        .iter()
        .find(|entry| entry.is_preferred)
        .or_else(|| model.picker_entries.first());
    if let Some(group) = contextual_group {
        items.push(
            CommandPaletteItem::action(
                "action:project-settings",
                "Project settings",
                named(IconName::Folder),
                PaletteAction::OpenProjectSettings {
                    project_key: group.group_key.clone(),
                },
            )
            .search_terms([
                "project",
                "settings",
                "name",
                "icon",
                "scripts",
                "model",
                "workspace",
                "grouping",
                "checkout",
                "remove",
                "t3.json",
            ])
            .description(group.group_display_name.clone()),
        );
    }

    items
}

/// The "Change theme" submenu.
pub fn build_change_theme_item(snapshot: &PaletteSnapshot) -> CommandPaletteItem {
    CommandPaletteItem::submenu(
        "action:change-theme",
        "Change theme",
        ITEM_ICON(IconName::Palette),
        ITEM_ICON(IconName::Palette),
        build_theme_groups(snapshot),
    )
    .search_terms(["change theme", "appearance", "colors", "palette"])
    .shortcut("theme.select")
}

pub fn build_theme_groups(snapshot: &PaletteSnapshot) -> Vec<CommandPaletteGroup> {
    let items = snapshot
        .theme_cards
        .iter()
        .map(|card| {
            let mut item = CommandPaletteItem::action(
                match &card.id {
                    None => "theme:standard".to_owned(),
                    Some(id) => format!("theme:palette:{id}"),
                },
                card.label.clone(),
                ITEM_ICON(IconName::Palette),
                PaletteAction::SetTheme {
                    id: card.id.clone(),
                },
            )
            .search_terms([card.label.as_str(), "theme", "appearance"]);
            if let [only] = card.previews.as_slice() {
                item = item.description(format!(
                    "For {} mode",
                    if only.dark { "dark" } else { "light" }
                ));
            }
            item.trailing(PaletteAddon::ThemePreview {
                current: snapshot.active_theme_id == card.id,
                previews: card.previews.clone(),
            })
        })
        .collect();
    vec![CommandPaletteGroup::new("themes", "Change theme", items)]
}

/// The "Change appearance" submenu.
pub fn build_change_appearance_item(snapshot: &PaletteSnapshot) -> CommandPaletteItem {
    CommandPaletteItem::submenu(
        "action:change-appearance",
        "Change appearance",
        ITEM_ICON(IconName::Monitor),
        ITEM_ICON(IconName::Monitor),
        build_appearance_groups(snapshot),
    )
    .search_terms([
        "change appearance",
        "light",
        "dark",
        "system",
        "mode",
        "toggle",
    ])
    .shortcut("appearance.cycle")
}

pub fn build_appearance_groups(snapshot: &PaletteSnapshot) -> Vec<CommandPaletteGroup> {
    let options = [
        (
            "system",
            "System",
            IconName::Monitor,
            AppearanceModeChoice::System,
        ),
        ("light", "Light", IconName::Sun, AppearanceModeChoice::Light),
        ("dark", "Dark", IconName::Moon, AppearanceModeChoice::Dark),
    ];
    let items = options
        .into_iter()
        .map(|(mode, label, icon, choice)| {
            let mut item = CommandPaletteItem::action(
                format!("appearance:{mode}"),
                label,
                ITEM_ICON(icon),
                PaletteAction::SetAppearance { mode: choice },
            )
            .search_terms([label, "appearance", "mode"]);
            if snapshot.appearance_mode == choice {
                item = item.trailing(PaletteAddon::Current);
            }
            item
        })
        .collect();
    vec![CommandPaletteGroup::new(
        "appearance",
        "Change appearance",
        items,
    )]
}

/// `settingsSearchItems`.
pub fn build_settings_search_items(
    hits: &[super::palette_source::SettingsSearchItem],
) -> Vec<CommandPaletteItem> {
    hits.iter()
        .map(|hit| {
            let mut terms = vec![hit.title.clone(), hit.section_label.clone()];
            terms.extend(hit.search_terms.iter().cloned());
            let mut item = CommandPaletteItem::action(
                format!("setting:{}", hit.id),
                hit.title.clone(),
                ITEM_ICON(IconName::Settings),
                PaletteAction::OpenSetting { id: hit.id.clone() },
            )
            .description(format!("Settings · {}", hit.section_label));
            item.search_terms = terms;
            item.secondary = hit.secondary;
            item
        })
        .collect()
}

#[cfg(test)]
mod tests;
