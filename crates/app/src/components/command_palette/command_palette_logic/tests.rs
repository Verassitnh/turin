//! Port of upstream/apps/web/src/components/CommandPalette.logic.test.ts

use std::collections::HashMap;

use turin_contracts::filesystem::FilesystemBrowseEntry;
use turin_contracts::pull_request::PullRequestLinkedThreadsEntry;
use turin_contracts::{EnvironmentId, ProjectId, ThreadId, TrimmedNonEmptyString};

use super::*;
use crate::state::models::{EnvironmentThreadShell, PullRequestState, ThreadPullRequestLink};

fn env(id: &str) -> EnvironmentId {
    EnvironmentId::new(id).unwrap()
}

fn thread_id(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

fn open_thread(environment: &str, id: &str) -> PaletteAction {
    PaletteAction::OpenThread {
        environment_id: env(environment),
        thread_id: thread_id(id),
    }
}

fn plain_action(value: &str) -> CommandPaletteItem {
    CommandPaletteItem::action(value, value, PaletteIcon::None, PaletteAction::Noop)
}

const PROJECT: &str = "project-1";
// 2026-03-25T12:00:00.000Z
const NOW_MS: i64 = 1_774_440_000_000;

fn make_thread(id: &str, title: &str, updated_at: &str) -> EnvironmentThreadShell {
    let mut thread = EnvironmentThreadShell::new(
        env("environment-local"),
        thread_id(id),
        ProjectId::new(PROJECT).unwrap(),
        title,
        "2026-03-01T00:00:00.000Z",
    );
    thread.updated_at = updated_at.into();
    thread
}

fn project_titles() -> HashMap<String, String> {
    HashMap::from([(PROJECT.to_owned(), "Project".to_owned())])
}

fn thread_items<'a>(
    threads: &'a [EnvironmentThreadShell],
    titles: &'a HashMap<String, String>,
    sort_order: ThreadSortOrder,
    tweak: impl FnOnce(&mut BuildThreadActionItemsInput<'a, EnvironmentThreadShell>),
) -> Vec<CommandPaletteItem> {
    let mut input = BuildThreadActionItemsInput {
        threads,
        active_thread_id: None,
        project_title_by_id: titles,
        sort_order,
        icon: PaletteIcon::None,
        render_leading_content: None,
        render_trailing_content: None,
        render_description: None,
        get_content_match: None,
        limit: None,
        now_ms: NOW_MS,
    };
    tweak(&mut input);
    build_thread_action_items(&input)
}

fn filter(
    query: &str,
    active_groups: &[CommandPaletteGroup],
    projects: &[CommandPaletteItem],
    settings: &[CommandPaletteItem],
    threads: &[CommandPaletteItem],
) -> Vec<CommandPaletteGroup> {
    filter_command_palette_groups(&FilterCommandPaletteGroupsInput {
        active_groups,
        query,
        is_in_submenu: false,
        project_search_items: projects,
        settings_search_items: settings,
        thread_search_items: threads,
    })
}

fn values(group: &CommandPaletteGroup) -> Vec<&str> {
    group.items.iter().map(|item| item.value.as_str()).collect()
}

mod linked_pull_request_thread_navigation {
    use super::*;

    #[test]
    fn keeps_archived_relations_searchable_and_routes_them_through_the_pr_environment() {
        let environment_id = env("remote");
        let query = "https://github.com/acme/web/pull/42";
        let linked = CommandPaletteLinkedThreads {
            environment_id: environment_id.clone(),
            threads: vec![PullRequestLinkedThreadsEntry {
                id: thread_id("archived-thread"),
                project_id: ProjectId::new("project").unwrap(),
                title: "Completed work".into(),
                archived_at: Some("2026-09-01T00:00:00.000Z".to_owned()),
            }],
        };
        let state = reduce_command_palette_ui_state(
            &CommandPaletteUiState::default(),
            CommandPaletteUiAction::OpenSearch {
                query: query.into(),
                linked_threads: Some(linked.clone()),
            },
        );
        assert_eq!(
            state.open_intent,
            Some(CommandPaletteOpenIntent::Search {
                query: query.into(),
                linked_threads: Some(linked.clone())
            })
        );
        let items = build_linked_thread_action_items(&linked, query, PaletteIcon::None);
        let groups = filter(query, &[], &[], &[], &items);
        let found: Vec<CommandPaletteItem> =
            groups.into_iter().flat_map(|group| group.items).collect();
        assert_eq!(found, items);
        assert_eq!(
            items[0].description,
            Some(PaletteDescription::Text("Archived thread".into()))
        );
        assert_eq!(
            items[0].run(),
            Some(&open_thread("remote", "archived-thread"))
        );
    }
}

mod build_command_palette_project_metadata {
    use super::*;

    fn labels() -> HashMap<EnvironmentId, String> {
        HashMap::from([
            (env("environment-local"), "Local".to_owned()),
            (env("environment-build-box"), "Build box".to_owned()),
        ])
    }

    #[test]
    fn makes_every_member_environment_and_path_searchable() {
        let local = env("environment-local");
        let remote = env("environment-build-box");
        let metadata = build_command_palette_project_metadata(
            &[
                ProjectMetadataInput {
                    environment_id: &local,
                    title: "T3 Code",
                    workspace_root: "/Users/theo/Projects/t3code",
                },
                ProjectMetadataInput {
                    environment_id: &remote,
                    title: "t3code",
                    workspace_root: "/srv/t3code",
                },
            ],
            &labels(),
        );
        assert_eq!(
            metadata.search_terms,
            vec![
                "T3 Code",
                "/Users/theo/Projects/t3code",
                "Local",
                "t3code",
                "/srv/t3code",
                "Build box"
            ]
        );
        assert_eq!(metadata.environment_labels, vec!["Local", "Build box"]);

        let mut item = plain_action("project:t3code");
        item.search_terms = metadata.search_terms;
        let groups = filter("build box", &[], &[item], &[], &[]);
        assert_eq!(groups[0].items.len(), 1);
    }

    #[test]
    fn deduplicates_grouped_checkouts_by_environment() {
        let remote = env("environment-build-box");
        let metadata = build_command_palette_project_metadata(
            &[
                ProjectMetadataInput {
                    environment_id: &remote,
                    title: "T3 Code",
                    workspace_root: "/srv/t3code",
                },
                ProjectMetadataInput {
                    environment_id: &remote,
                    title: "T3 Code worktree",
                    workspace_root: "/srv/t3code-feature",
                },
            ],
            &labels(),
        );
        assert_eq!(metadata.environment_labels, vec!["Build box"]);
    }

    #[test]
    fn deduplicates_distinct_environments_with_the_same_label() {
        let first = env("environment-build-box");
        let second = env("environment-build-box-2");
        let metadata = build_command_palette_project_metadata(
            &[
                ProjectMetadataInput {
                    environment_id: &first,
                    title: "T3 Code",
                    workspace_root: "/srv/t3code",
                },
                ProjectMetadataInput {
                    environment_id: &second,
                    title: "T3 Code mirror",
                    workspace_root: "/srv/mirror/t3code",
                },
            ],
            &HashMap::from([
                (first.clone(), "Build box".to_owned()),
                (second.clone(), "Build box".to_owned()),
            ]),
        );
        assert_eq!(metadata.environment_labels, vec!["Build box"]);
    }

    #[test]
    fn uses_a_human_readable_fallback_when_presentation_data_is_unavailable() {
        let remote = env("environment-build-box");
        let metadata = build_command_palette_project_metadata(
            &[ProjectMetadataInput {
                environment_id: &remote,
                title: "T3 Code",
                workspace_root: "/srv/t3code",
            }],
            &HashMap::new(),
        );
        assert!(metadata.search_terms.contains(&"Remote".to_owned()));
        assert_eq!(metadata.environment_labels, vec!["Remote"]);
    }
}

mod reduce_command_palette_ui_state {
    use super::*;

    fn closed() -> CommandPaletteUiState {
        CommandPaletteUiState::default()
    }

    fn state(open: bool, mode: SearchOverlayMode) -> CommandPaletteUiState {
        CommandPaletteUiState {
            open,
            mode,
            open_intent: None,
        }
    }

    #[test]
    fn toggles_each_overlay_mode_open_and_closed() {
        let files_open = reduce_command_palette_ui_state(
            &closed(),
            CommandPaletteUiAction::ToggleMode(SearchOverlayMode::Files),
        );
        assert_eq!(files_open, state(true, SearchOverlayMode::Files));

        let content_open = reduce_command_palette_ui_state(
            &files_open,
            CommandPaletteUiAction::ToggleMode(SearchOverlayMode::Content),
        );
        assert_eq!(content_open, state(true, SearchOverlayMode::Content));

        assert_eq!(
            reduce_command_palette_ui_state(
                &content_open,
                CommandPaletteUiAction::ToggleMode(SearchOverlayMode::Content)
            ),
            state(false, SearchOverlayMode::Content)
        );
    }

    #[test]
    fn switches_between_open_modes_without_closing() {
        let files_open = state(true, SearchOverlayMode::Files);
        assert_eq!(
            reduce_command_palette_ui_state(
                &files_open,
                CommandPaletteUiAction::ToggleMode(SearchOverlayMode::Command)
            ),
            state(true, SearchOverlayMode::Command)
        );
    }

    #[test]
    fn opens_pr_search_from_another_overlay_and_replaces_an_earlier_search() {
        let search = |query: &str| CommandPaletteUiAction::OpenSearch {
            query: query.into(),
            linked_threads: None,
        };
        let first = reduce_command_palette_ui_state(
            &state(true, SearchOverlayMode::Files),
            search("https://github.com/acme/web/pull/7"),
        );
        assert_eq!(
            first,
            CommandPaletteUiState {
                open: true,
                mode: SearchOverlayMode::Command,
                open_intent: Some(CommandPaletteOpenIntent::Search {
                    query: "https://github.com/acme/web/pull/7".into(),
                    linked_threads: None
                })
            }
        );
        let second =
            reduce_command_palette_ui_state(&first, search("https://github.com/acme/web/pull/8"));
        assert_eq!(
            second.open_intent,
            Some(CommandPaletteOpenIntent::Search {
                query: "https://github.com/acme/web/pull/8".into(),
                linked_threads: None
            })
        );
        assert_eq!(
            reduce_command_palette_ui_state(&second, CommandPaletteUiAction::SetOpen(false))
                .open_intent,
            None
        );
    }

    #[test]
    fn routes_open_intents_to_command_mode() {
        let files_open = state(true, SearchOverlayMode::Files);
        assert_eq!(
            reduce_command_palette_ui_state(&files_open, CommandPaletteUiAction::OpenAddProject),
            CommandPaletteUiState {
                open: true,
                mode: SearchOverlayMode::Command,
                open_intent: Some(CommandPaletteOpenIntent::AddProject)
            }
        );
        assert_eq!(
            reduce_command_palette_ui_state(&files_open, CommandPaletteUiAction::OpenNewThreadIn),
            CommandPaletteUiState {
                open: true,
                mode: SearchOverlayMode::Command,
                open_intent: Some(CommandPaletteOpenIntent::NewThreadIn)
            }
        );
    }

    #[test]
    fn preserves_the_mode_on_close_and_resets_it_on_open() {
        let files_open = state(true, SearchOverlayMode::Files);
        assert_eq!(
            reduce_command_palette_ui_state(&files_open, CommandPaletteUiAction::SetOpen(false)),
            state(false, SearchOverlayMode::Files)
        );
        assert_eq!(
            reduce_command_palette_ui_state(&files_open, CommandPaletteUiAction::SetOpen(true)),
            state(true, SearchOverlayMode::Command)
        );
    }

    #[test]
    fn clearing_an_absent_intent_returns_the_same_state() {
        let open = state(true, SearchOverlayMode::Command);
        assert_eq!(
            reduce_command_palette_ui_state(&open, CommandPaletteUiAction::ClearOpenIntent),
            open
        );
    }
}

#[test]
fn enumerate_assigns_positional_jump_shortcuts_to_the_first_nine_displayed_items() {
    let items: Vec<CommandPaletteItem> = (1..=10)
        .map(|index| plain_action(&format!("project-{index}")).shortcut("chat.new"))
        .collect();
    let commands: Vec<Option<String>> = enumerate_command_palette_items(items)
        .into_iter()
        .map(|item| item.shortcut_command)
        .collect();
    let expected: Vec<Option<String>> = (1..=9)
        .map(|index| Some(format!("thread.jump.{index}")))
        .chain([None])
        .collect();
    assert_eq!(commands, expected);
}

mod build_project_action_items {
    use super::*;

    #[test]
    fn shows_the_grouped_display_name_but_keeps_the_real_title_for_icons() {
        let project = CommandPaletteProject {
            environment_id: env("environment-local"),
            id: "project-1".into(),
            title: "fleet".into(),
            workspace_root: "/Users/theo/Code/p/fleet".into(),
            display_name: "t3dotgg/fleet".into(),
        };
        let icon_titles = std::cell::RefCell::new(Vec::new());
        let items = build_project_action_items(&BuildProjectActionItemsInput {
            projects: &[project],
            value_prefix: "project",
            icon: &|candidate| {
                icon_titles.borrow_mut().push(candidate.title.clone());
                PaletteIcon::None
            },
            run_project: &|_| PaletteAction::Noop,
            search_terms: None,
            render_description: None,
            shortcut_command: None,
        });
        assert_eq!(items[0].title, PaletteTitle::Text("t3dotgg/fleet".into()));
        for term in ["t3dotgg/fleet", "fleet", "/Users/theo/Code/p/fleet"] {
            assert!(items[0].search_terms.contains(&term.to_owned()), "{term}");
        }
        assert_eq!(*icon_titles.borrow(), vec!["fleet"]);
    }
}

mod build_thread_action_items {
    use super::*;

    #[test]
    fn orders_threads_by_most_recent_activity_and_formats_timestamps_from_updated_at() {
        let mut newer = make_thread("thread-newer", "Newer thread", "2026-03-20T00:00:00.000Z");
        newer.created_at = "2026-03-20T00:00:00.000Z".into();
        let threads = [
            make_thread("thread-older", "Older thread", "2026-03-24T12:00:00.000Z"),
            newer,
        ];
        let titles = project_titles();
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |_| {});
        assert_eq!(
            items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(),
            vec!["thread:thread-older", "thread:thread-newer"]
        );
        assert_eq!(items[0].timestamp.as_deref(), Some("1d ago"));
        assert_eq!(items[1].timestamp.as_deref(), Some("5d ago"));
    }

    #[test]
    fn ranks_thread_title_matches_ahead_of_contextual_project_name_matches() {
        let threads = [
            make_thread(
                "thread-context-match",
                "Fix navbar spacing",
                "2026-03-20T00:00:00.000Z",
            ),
            {
                let mut thread = make_thread(
                    "thread-title-match",
                    "Project kickoff notes",
                    "2026-03-19T00:00:00.000Z",
                );
                thread.created_at = "2026-03-02T00:00:00.000Z".into();
                thread
            },
        ];
        let titles = project_titles();
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |_| {});
        let groups = filter("project", &[], &[], &[], &items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].value, "threads-search");
        assert_eq!(
            values(&groups[0]),
            vec!["thread:thread-title-match", "thread:thread-context-match"]
        );
    }

    #[test]
    fn orders_title_matches_by_recent_activity_before_older_prefix_matches() {
        let mut old_prefix = make_thread(
            "old-prefix",
            "Convex InvalidCursor in Convex threads query",
            "2026-01-02T00:00:00.000Z",
        );
        old_prefix.created_at = "2026-01-01T00:00:00.000Z".into();
        let mut recent_title = make_thread(
            "recent-title",
            "Disable Convex schema validation",
            "2026-03-24T00:00:00.000Z",
        );
        recent_title.created_at = "2025-12-01T00:00:00.000Z".into();
        let mut recent_content = make_thread(
            "recent-content",
            "Fix schema validation",
            "2026-03-25T00:00:00.000Z",
        );
        recent_content.created_at = "2026-03-25T00:00:00.000Z".into();
        let threads = [old_prefix, recent_title, recent_content];
        let titles = HashMap::from([(PROJECT.to_owned(), "T3 Code".to_owned())]);
        let content_match = |thread: &EnvironmentThreadShell| {
            (thread.id == thread_id("recent-content")).then(|| CommandPaletteThreadContentMatch {
                source: SearchMatchSource::User,
                snippet: "Please check Convex".into(),
                query: "convex".into(),
            })
        };
        let items = thread_items(&threads, &titles, ThreadSortOrder::CreatedAt, |input| {
            input.get_content_match = Some(&content_match);
        });
        let groups = filter("convex", &[], &[], &[], &items);
        assert_eq!(
            values(&groups[0]),
            vec![
                "thread:recent-title",
                "thread:old-prefix",
                "thread:recent-content"
            ]
        );
    }

    #[test]
    fn preserves_thread_project_name_matches_when_there_is_no_stronger_title_match() {
        let mut item = plain_action("thread:project-context-only");
        item.search_terms = vec!["Fix navbar spacing".into(), "Project".into()];
        let group = CommandPaletteGroup::new("threads-search", "Threads", vec![item]);
        let groups = filter("project", &[group], &[], &[], &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(values(&groups[0]), vec!["thread:project-context-only"]);
    }

    #[test]
    fn ranks_an_order_independent_setting_title_match_above_a_split_context_match() {
        let setting = |value: &str, terms: [&str; 2]| {
            let mut item = plain_action(value);
            item.search_terms = terms.map(String::from).to_vec();
            item
        };
        let settings = [
            setting(
                "setting:context-match",
                ["Pairing settings", "remote backend"],
            ),
            setting("setting:remote-pairing", ["Remote pairing", "connections"]),
        ];
        let groups = filter("pairing remote", &[], &[], &settings, &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].value, "settings-search");
        assert_eq!(
            values(&groups[0]),
            vec!["setting:remote-pairing", "setting:context-match"]
        );
    }

    #[test]
    fn keeps_accent_insensitive_setting_results() {
        let mut item = plain_action("setting:theme");
        item.search_terms = vec!["Themes".into(), "Appearance".into()];
        let groups = filter("thè\u{1ab0}mes", &[], &[], &[item], &[]);
        assert_eq!(values(&groups[0]), vec!["setting:theme"]);
    }

    #[test]
    fn normalizes_case_independently_of_the_host_locale() {
        let mut item = plain_action("setting:version-control");
        item.search_terms = vec!["git".into()];
        let groups = filter("GIT", &[], &[], &[item], &[]);
        assert_eq!(
            groups
                .iter()
                .flat_map(|group| values(group))
                .collect::<Vec<_>>(),
            vec!["setting:version-control"]
        );
    }

    #[test]
    fn keeps_message_excerpts_searchable_without_replacing_thread_metadata() {
        let mut thread = make_thread("thread-1", "Thread", "2026-03-01T00:00:00.000Z");
        thread.branch = Some("feat/search".into());
        let titles = HashMap::from([(PROJECT.to_owned(), "T3 Code".to_owned())]);
        let expected = CommandPaletteThreadContentMatch {
            source: SearchMatchSource::Assistant,
            snippet: "The relay reconnect is now bounded.".into(),
            query: "reconnect".into(),
        };
        let provide = |_: &EnvironmentThreadShell| Some(expected.clone());
        let threads = [thread];
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |input| {
            input.get_content_match = Some(&provide);
        });
        assert!(
            items[0]
                .search_terms
                .contains(&"The relay reconnect is now bounded.".to_owned())
        );
        assert_eq!(items[0].thread_content_match, Some(expected));
        assert_eq!(
            items[0].description,
            Some(PaletteDescription::Text("T3 Code · #feat/search".into()))
        );
    }

    #[test]
    fn surfaces_threads_when_the_query_is_their_id_without_outranking_title_matches() {
        let id_thread = make_thread(
            "thread-alpha-1234",
            "Unrelated work",
            "2026-03-05T00:00:00.000Z",
        );
        let title_thread = make_thread(
            "thread-other-9999",
            "Fix thread-alpha-1234 flakes",
            "2026-03-04T00:00:00.000Z",
        );
        let threads = [id_thread, title_thread];
        let titles = HashMap::from([(PROJECT.to_owned(), "T3 Code".to_owned())]);
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |_| {});
        let groups = filter("  THREAD-ALPHA-1234  ", &[], &[], &[], &items);
        assert_eq!(
            groups
                .iter()
                .flat_map(|group| values(group))
                .collect::<Vec<_>>(),
            vec!["thread:thread-other-9999", "thread:thread-alpha-1234"]
        );
    }

    #[test]
    fn prefers_render_description_when_provided() {
        let mut thread = make_thread("thread-1", "Thread", "2026-03-01T00:00:00.000Z");
        thread.branch = Some("feat/search".into());
        thread.worktree_path = Some("/tmp/wt".into());
        let titles = HashMap::from([(PROJECT.to_owned(), "T3 Code".to_owned())]);
        let render = |thread: &EnvironmentThreadShell, project_title: Option<&str>| {
            PaletteDescription::Text(format!(
                "{}:{}:{}",
                project_title.unwrap_or(""),
                thread.branch.as_deref().unwrap_or(""),
                if thread.worktree_path.is_some() {
                    "wt"
                } else {
                    "local"
                }
            ))
        };
        let threads = [thread];
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |input| {
            input.render_description = Some(&render);
        });
        assert_eq!(
            items[0].description,
            Some(PaletteDescription::Text("T3 Code:feat/search:wt".into()))
        );
    }

    #[test]
    fn filters_archived_threads_out_of_thread_search_items() {
        let mut archived = make_thread(
            "thread-archived",
            "Archived thread",
            "2026-03-20T00:00:00.000Z",
        );
        archived.archived_at = Some("2026-03-20T00:00:00.000Z".into());
        let mut active = make_thread("thread-active", "Active thread", "2026-03-19T00:00:00.000Z");
        active.created_at = "2026-03-02T00:00:00.000Z".into();
        let threads = [active, archived];
        let titles = project_titles();
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |_| {});
        assert_eq!(
            items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(),
            vec!["thread:thread-active"]
        );
    }

    #[test]
    fn describes_the_active_thread_and_honors_the_limit() {
        let threads = [
            make_thread("a", "A", "2026-03-03T00:00:00.000Z"),
            make_thread("b", "B", "2026-03-02T00:00:00.000Z"),
        ];
        let titles = project_titles();
        let active = thread_id("a");
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |input| {
            input.active_thread_id = Some(&active);
            input.limit = Some(1);
        });
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].description,
            Some(PaletteDescription::Text("Project · Current thread".into()))
        );
    }
}

mod build_browse_groups {
    use super::*;

    #[test]
    fn browse_rows_run_navigation_actions_and_keep_the_palette_open() {
        let entries = [FilesystemBrowseEntry {
            name: TrimmedNonEmptyString::new("Downloads").unwrap(),
            full_path: TrimmedNonEmptyString::new("/Users/test/Downloads").unwrap(),
        }];
        let refs: Vec<&FilesystemBrowseEntry> = entries.iter().collect();
        let groups = build_browse_groups(&BuildBrowseGroupsInput {
            browse_entries: &refs,
            browse_query: "~/",
            can_browse_up: true,
            up_icon: PaletteIcon::None,
            directory_icon: PaletteIcon::None,
        });
        assert_eq!(groups[0].value, "directories");
        let up = &groups[0].items[0];
        assert_eq!(up.run(), Some(&PaletteAction::BrowseUp));
        assert!(up.keeps_open());
        let item = &groups[0].items[1];
        assert_eq!(
            item.run(),
            Some(&PaletteAction::BrowseTo {
                name: "Downloads".into()
            })
        );
        assert!(item.keeps_open());
        assert_eq!(item.value, "browse:/Users/test/Downloads");
    }
}

mod filter_pinned_browse_entries {
    use super::*;

    fn entry(name: &str, full_path: &str) -> FilesystemBrowseEntry {
        FilesystemBrowseEntry {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            full_path: TrimmedNonEmptyString::new(full_path).unwrap(),
        }
    }

    #[test]
    fn shows_sibling_folders_without_losing_an_existing_pinned_destination() {
        let entries = [
            entry("repo", "/projects/repo"),
            entry("work", "/projects/work"),
        ];
        let result = filter_pinned_browse_entries(&entries, "repo", "repo", true);
        assert_eq!(result.visible_entries, vec![&entries[0], &entries[1]]);
        assert_eq!(result.exact_entry, Some(&entries[0]));
    }

    #[test]
    fn matches_an_existing_pinned_destination_without_windows_casing() {
        let entries = [
            entry("Repo", "C:\\projects\\Repo"),
            entry("work", "C:\\projects\\work"),
        ];
        let result = filter_pinned_browse_entries(&entries, "repo", "repo", false);
        assert_eq!(result.visible_entries, vec![&entries[0], &entries[1]]);
        assert_eq!(result.exact_entry, Some(&entries[0]));
    }
}

#[test]
fn finds_linked_threads_from_pr_queries() {
    for query in [
        "#10839",
        "10839",
        "pingdotgg/t3code#10839",
        "https://github.com/pingdotgg/t3code/pull/10839",
    ] {
        let mut implementation =
            make_thread("thread-1", "Implementation", "2026-03-01T00:00:00.000Z");
        implementation.pull_requests = vec![ThreadPullRequestLink {
            repository: "pingdotgg/t3code".into(),
            number: 10839,
            url: "https://github.com/pingdotgg/t3code/pull/10839".into(),
            title: None,
            state: Some(PullRequestState::Open),
            is_draft: false,
            dismissed: false,
        }];
        let other = make_thread("unrelated", "Other work", "2026-03-01T00:00:00.000Z");
        let threads = [implementation, other];
        let titles = HashMap::new();
        let items = thread_items(&threads, &titles, ThreadSortOrder::UpdatedAt, |_| {});
        let groups = filter(query, &[], &[], &[], &items);
        let titles: Vec<String> = groups
            .iter()
            .flat_map(|group| group.items.iter().map(|item| item.title.plain()))
            .collect();
        assert_eq!(titles, vec!["Implementation"], "{query}");
    }
}

mod filter_command_palette_groups {
    use super::*;

    #[test]
    fn sorts_secondary_settings_results_after_other_matches() {
        let item = |value: &str, title: &str, secondary: bool| {
            let mut item =
                CommandPaletteItem::action(value, title, PaletteIcon::None, PaletteAction::Noop);
            item.search_terms = vec![title.into(), "General".into()];
            item.secondary = secondary;
            item
        };
        let settings = [
            item(
                "setting:keybinding-modelPicker.toggle",
                "Model Picker: Toggle",
                true,
            ),
            item("setting:default-model", "Default model", false),
        ];
        let groups = filter("model", &[], &[], &settings, &[]);
        assert_eq!(
            values(&groups[0]),
            vec![
                "setting:default-model",
                "setting:keybinding-modelPicker.toggle"
            ]
        );
    }

    #[test]
    fn an_empty_actions_filter_keeps_only_the_actions_group() {
        let groups = vec![
            CommandPaletteGroup::new("actions", "Actions", vec![plain_action("a")]),
            CommandPaletteGroup::new("recent-threads", "Recent Threads", vec![plain_action("t")]),
        ];
        let filtered = filter(">", &groups, &[], &[], &[]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].value, "actions");
        // A plain query drops the recent threads group and searches threads instead.
        let filtered = filter("t", &groups, &[], &[], &[]);
        assert!(filtered.iter().all(|group| group.value != "recent-threads"));
    }
}

mod virtualized_command_palette_rows {
    use super::*;

    fn groups() -> Vec<CommandPaletteGroup> {
        vec![
            CommandPaletteGroup::new(
                "actions",
                "Actions",
                vec![
                    plain_action("new-thread"),
                    plain_action("offline").disabled(),
                ],
            ),
            CommandPaletteGroup::new(
                "threads",
                "Threads",
                vec![plain_action("thread-a"), plain_action("thread-b")],
            ),
        ]
    }

    #[test]
    fn keeps_group_order_and_headings_while_indexing_only_enabled_items() {
        let built = build_command_palette_rows(&groups());
        let keys: Vec<String> = built
            .rows
            .iter()
            .map(|row| match row {
                CommandPaletteRow::Label { label, .. } => format!("# {label}"),
                CommandPaletteRow::Item { key, .. } => key.clone(),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                "# Actions",
                "actions:new-thread",
                "actions:offline",
                "# Threads",
                "threads:thread-a",
                "threads:thread-b"
            ]
        );
        assert_eq!(
            built.item_values,
            vec!["new-thread", "thread-a", "thread-b"]
        );
        assert_eq!(built.row_index_by_item_index, vec![1, 4, 5]);
        let indices: Vec<Option<usize>> = built
            .rows
            .iter()
            .filter_map(|row| match row {
                CommandPaletteRow::Item { item_index, .. } => Some(*item_index),
                CommandPaletteRow::Label { .. } => None,
            })
            .collect();
        assert_eq!(indices, vec![Some(0), None, Some(1), Some(2)]);
    }

    #[test]
    fn resolves_enter_to_the_highlighted_item_without_needing_its_row_mounted() {
        let groups = groups();
        assert_eq!(
            find_highlighted_command_palette_item(&groups, Some("thread-b"))
                .map(|i| i.value.as_str()),
            Some("thread-b")
        );
        assert!(find_highlighted_command_palette_item(&groups, Some("offline")).is_none());
        assert!(find_highlighted_command_palette_item(&groups, None).is_none());
    }

    #[test]
    fn the_first_label_is_flagged_first() {
        let built = build_command_palette_rows(&groups());
        let firsts: Vec<bool> = built
            .rows
            .iter()
            .filter_map(|row| match row {
                CommandPaletteRow::Label { first, .. } => Some(*first),
                _ => None,
            })
            .collect();
        assert_eq!(firsts, vec![true, false]);
    }
}

#[test]
fn palette_mode_and_placeholder_follow_the_view_and_browse_state() {
    let view = CommandPaletteView {
        addon_icon: PaletteIcon::None,
        groups: Vec::new(),
        initial_query: None,
    };
    assert_eq!(
        get_command_palette_mode(None, false),
        CommandPaletteMode::Root
    );
    assert_eq!(
        get_command_palette_mode(None, true),
        CommandPaletteMode::RootBrowse
    );
    assert_eq!(
        get_command_palette_mode(Some(&view), false),
        CommandPaletteMode::Submenu
    );
    assert_eq!(
        get_command_palette_mode(Some(&view), true),
        CommandPaletteMode::SubmenuBrowse
    );
    assert_eq!(
        get_command_palette_input_placeholder(CommandPaletteMode::Root),
        "Search commands, projects, and threads..."
    );
    assert_eq!(
        get_command_palette_input_placeholder(CommandPaletteMode::SubmenuBrowse),
        "Enter path (e.g. ~/projects/my-app)"
    );
}

#[test]
fn root_groups_omit_empty_sections() {
    let groups = build_root_groups(vec![plain_action("a")], Vec::new());
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].label, "Actions");
    let groups = build_root_groups(vec![plain_action("a")], vec![plain_action("t")]);
    assert_eq!(groups[1].value, "recent-threads");
}

#[test]
fn browse_input_end_padding_reserves_room_for_the_submit_label() {
    assert_eq!(browse_input_end_padding(true, false), 9.5);
    assert_eq!(browse_input_end_padding(false, true), 7.5);
    assert_eq!(browse_input_end_padding(false, false), 6.);
}
