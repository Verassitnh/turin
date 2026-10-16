//! Port of upstream/apps/web/src/components/BranchToolbar.logic.test.ts

use turin_contracts::EnvironmentId;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::git::VcsRef;

use super::*;

fn env(id: &str) -> EnvironmentId {
    EnvironmentId::new(id).unwrap()
}

fn thread<'a>(
    branch: Option<&'a str>,
    worktree_path: Option<&'a str>,
    updated_at: &'a str,
    archived_at: Option<&'a str>,
) -> WorktreeThread<'a> {
    WorktreeThread {
        branch,
        worktree_path,
        updated_at,
        archived_at,
    }
}

fn seed(branch: Option<&str>, worktree_path: &str) -> PreviousWorktreeSeed {
    PreviousWorktreeSeed {
        branch: branch.map(str::to_owned),
        worktree_path: worktree_path.to_owned(),
    }
}

mod resolve_previous_worktree_seed_tests {
    use super::*;

    #[test]
    fn picks_the_most_recently_updated_worktree_thread() {
        let threads = [
            thread(
                Some("t3/older"),
                Some("/repo/.t3/worktrees/older"),
                "2026-07-20T00:00:00.000Z",
                None,
            ),
            thread(
                Some("t3/newer"),
                Some("/repo/.t3/worktrees/newer"),
                "2026-07-22T00:00:00.000Z",
                None,
            ),
            thread(Some("main"), None, "2026-07-23T00:00:00.000Z", None),
        ];
        assert_eq!(
            resolve_previous_worktree_seed(&threads, None),
            Some(seed(Some("t3/newer"), "/repo/.t3/worktrees/newer"))
        );
    }

    #[test]
    fn skips_the_worktree_the_composer_already_points_at() {
        let threads = [thread(
            Some("t3/current"),
            Some("/repo/.t3/worktrees/current"),
            "2026-07-22T00:00:00.000Z",
            None,
        )];
        assert_eq!(
            resolve_previous_worktree_seed(&threads, Some("/repo/.t3/worktrees/current")),
            None
        );
    }

    #[test]
    fn returns_none_when_no_thread_has_a_worktree() {
        let threads = [thread(Some("main"), None, "2026-07-22T00:00:00.000Z", None)];
        assert_eq!(resolve_previous_worktree_seed(&threads, None), None);
    }

    #[test]
    fn ignores_archived_threads_and_threads_with_unparseable_timestamps() {
        let threads = [
            thread(
                Some("t3/archived"),
                Some("/repo/.t3/worktrees/archived"),
                "2026-07-23T00:00:00.000Z",
                Some("2026-07-23T01:00:00.000Z"),
            ),
            thread(
                Some("t3/garbage-timestamp"),
                Some("/repo/.t3/worktrees/garbage"),
                "not-a-date",
                None,
            ),
            thread(
                Some("t3/live"),
                Some("/repo/.t3/worktrees/live"),
                "2026-07-21T00:00:00.000Z",
                None,
            ),
        ];
        assert_eq!(
            resolve_previous_worktree_seed(&threads, None),
            Some(seed(Some("t3/live"), "/repo/.t3/worktrees/live"))
        );
    }
}

#[test]
fn previous_worktree_label_includes_the_branch_when_known() {
    assert_eq!(
        resolve_previous_worktree_label(&seed(Some("t3/fix-thing"), "/wt")),
        "Previous worktree (t3/fix-thing)"
    );
    assert_eq!(
        resolve_previous_worktree_label(&seed(None, "/wt")),
        "Previous worktree"
    );
}

mod resolve_draft_env_mode_after_branch_change_tests {
    use super::*;

    #[test]
    fn switches_to_local_mode_when_returning_from_an_existing_worktree_to_the_main_worktree() {
        assert_eq!(
            resolve_draft_env_mode_after_branch_change(
                None,
                Some("/repo/.t3/worktrees/feature-a"),
                EnvMode::Worktree
            ),
            EnvMode::Local
        );
    }

    #[test]
    fn keeps_new_worktree_mode_when_selecting_a_base_ref_before_worktree_creation() {
        assert_eq!(
            resolve_draft_env_mode_after_branch_change(None, None, EnvMode::Worktree),
            EnvMode::Worktree
        );
    }

    #[test]
    fn uses_worktree_mode_when_selecting_a_ref_already_attached_to_a_worktree() {
        assert_eq!(
            resolve_draft_env_mode_after_branch_change(
                Some("/repo/.t3/worktrees/feature-a"),
                None,
                EnvMode::Local
            ),
            EnvMode::Worktree
        );
    }
}

mod resolve_branch_toolbar_value_tests {
    use super::*;

    #[test]
    fn defaults_new_worktree_mode_to_current_git_ref_when_no_explicit_base_ref_is_set() {
        assert_eq!(
            resolve_branch_toolbar_value(EnvMode::Worktree, None, None, Some("main")),
            Some("main".to_owned())
        );
    }

    #[test]
    fn keeps_an_explicitly_selected_worktree_base_ref() {
        assert_eq!(
            resolve_branch_toolbar_value(
                EnvMode::Worktree,
                None,
                Some("feature/base"),
                Some("main")
            ),
            Some("feature/base".to_owned())
        );
    }

    #[test]
    fn shows_the_actual_checked_out_ref_when_not_selecting_a_new_worktree_base() {
        assert_eq!(
            resolve_branch_toolbar_value(EnvMode::Local, None, Some("feature/base"), Some("main")),
            Some("main".to_owned())
        );
    }
}

mod resolve_branch_trigger_label_tests {
    use super::*;

    fn label(
        env_mode: EnvMode,
        branch: Option<&str>,
        is_remote: Option<bool>,
        start_from_origin: bool,
    ) -> String {
        resolve_branch_trigger_label(&ResolveBranchTriggerLabelInput {
            active_worktree_path: None,
            effective_env_mode: env_mode,
            resolved_active_branch: branch,
            resolved_active_branch_is_remote: is_remote,
            start_from_origin,
        })
    }

    #[test]
    fn shows_the_origin_ref_when_a_new_worktree_will_start_from_origin() {
        assert_eq!(
            label(EnvMode::Worktree, Some("main"), Some(false), true),
            "From origin/main"
        );
    }

    #[test]
    fn shows_the_origin_ref_for_local_branch_names_that_contain_slashes() {
        assert_eq!(
            label(EnvMode::Worktree, Some("feature/demo"), Some(false), true),
            "From origin/feature/demo"
        );
    }

    #[test]
    fn shows_the_local_ref_when_start_from_origin_is_disabled() {
        assert_eq!(
            label(EnvMode::Worktree, Some("main"), Some(false), false),
            "From main"
        );
    }

    #[test]
    fn does_not_duplicate_the_origin_prefix_for_an_explicit_remote_ref() {
        assert_eq!(
            label(
                EnvMode::Worktree,
                Some("origin/feature/demo"),
                Some(true),
                true
            ),
            "From origin/feature/demo"
        );
    }

    #[test]
    fn preserves_an_explicit_ref_from_a_non_origin_remote() {
        assert_eq!(
            label(
                EnvMode::Worktree,
                Some("upstream/feature/demo"),
                Some(true),
                true
            ),
            "From upstream/feature/demo"
        );
    }

    #[test]
    fn keeps_current_checkout_labels_and_empty_state_unchanged() {
        assert_eq!(
            label(EnvMode::Local, Some("main"), Some(false), true),
            "main"
        );
        assert_eq!(label(EnvMode::Worktree, None, None, true), "Select ref");
    }

    #[test]
    fn does_not_fabricate_an_origin_ref_while_branch_metadata_is_loading() {
        assert_eq!(
            label(EnvMode::Worktree, Some("upstream/feature/demo"), None, true),
            "From upstream/feature/demo"
        );
    }
}

mod resolve_branch_toolbar_pr_branch_tests {
    use super::*;

    #[test]
    fn uses_the_explicit_thread_branch_when_it_matches_the_displayed_branch() {
        assert_eq!(
            resolve_branch_toolbar_pr_branch(Some("feature/current"), Some("feature/current")),
            Some("feature/current")
        );
    }

    #[test]
    fn hides_pr_state_while_an_optimistic_branch_switch_is_in_flight() {
        assert_eq!(
            resolve_branch_toolbar_pr_branch(Some("feature/current"), Some("feature/next")),
            None
        );
    }

    #[test]
    fn does_not_infer_pr_state_without_an_explicit_thread_branch() {
        assert_eq!(
            resolve_branch_toolbar_pr_branch(None, Some("feature/current")),
            None
        );
    }
}

mod resolve_local_checkout_branch_mismatch_tests {
    use super::*;

    #[test]
    fn detects_when_a_local_thread_is_associated_with_a_different_branch_than_the_checkout() {
        assert_eq!(
            resolve_local_checkout_branch_mismatch(
                EnvMode::Local,
                None,
                Some("feature/thread"),
                Some("feature/current")
            ),
            Some(LocalCheckoutBranchMismatch {
                thread_branch: "feature/thread".into(),
                current_branch: "feature/current".into()
            })
        );
    }

    #[test]
    fn ignores_matching_local_checkout_state() {
        assert_eq!(
            resolve_local_checkout_branch_mismatch(
                EnvMode::Local,
                None,
                Some("feature/thread"),
                Some("feature/thread")
            ),
            None
        );
    }

    #[test]
    fn ignores_dedicated_worktrees_because_their_checkout_is_already_thread_scoped() {
        assert_eq!(
            resolve_local_checkout_branch_mismatch(
                EnvMode::Worktree,
                Some("/repo/.t3/worktrees/feature-thread"),
                Some("feature/thread"),
                Some("feature/current")
            ),
            None
        );
    }

    #[test]
    fn ignores_new_worktree_base_selection_before_a_worktree_exists() {
        assert_eq!(
            resolve_local_checkout_branch_mismatch(
                EnvMode::Worktree,
                None,
                Some("feature/base"),
                Some("main")
            ),
            None
        );
    }
}

mod resolve_environment_option_label_tests {
    use super::*;

    #[test]
    fn prefers_the_primary_environments_machine_label() {
        let id = env("environment-local");
        assert_eq!(
            resolve_environment_option_label(&ResolveEnvironmentOptionLabelInput {
                is_primary: true,
                environment_id: &id,
                runtime_label: Some("Julius's Mac mini"),
                saved_label: Some("Local environment"),
            }),
            "Julius's Mac mini"
        );
    }

    #[test]
    fn falls_back_to_this_device_for_generic_primary_labels() {
        let id = env("environment-local");
        assert_eq!(
            resolve_environment_option_label(&ResolveEnvironmentOptionLabelInput {
                is_primary: true,
                environment_id: &id,
                runtime_label: Some("Local environment"),
                saved_label: Some("Local"),
            }),
            "This device"
        );
    }

    #[test]
    fn keeps_configured_labels_for_non_primary_environments() {
        let id = env("environment-remote");
        assert_eq!(
            resolve_environment_option_label(&ResolveEnvironmentOptionLabelInput {
                is_primary: false,
                environment_id: &id,
                runtime_label: None,
                saved_label: Some("Build box"),
            }),
            "Build box"
        );
    }

    #[test]
    fn a_non_primary_environment_without_labels_shows_its_id() {
        let id = env("environment-remote");
        assert_eq!(
            resolve_environment_option_label(&ResolveEnvironmentOptionLabelInput {
                is_primary: false,
                environment_id: &id,
                runtime_label: Some("  "),
                saved_label: None,
            }),
            "environment-remote"
        );
    }
}

mod should_show_environment_indicator_tests {
    use super::*;

    #[test]
    fn shows_the_indicator_whenever_multiple_environments_are_pickable() {
        assert!(should_show_environment_indicator(Some(true), true));
    }

    #[test]
    fn shows_a_sole_remote_environment_so_the_user_knows_where_the_project_runs() {
        assert!(should_show_environment_indicator(Some(false), false));
    }

    #[test]
    fn hides_a_sole_primary_this_device_environment() {
        assert!(!should_show_environment_indicator(Some(true), false));
    }

    #[test]
    fn hides_the_indicator_when_the_active_environment_is_unknown() {
        assert!(!should_show_environment_indicator(None, false));
    }
}

mod should_show_composer_context_strip_tests {
    use super::*;

    fn input(
        is_draft_hero_state: bool,
        persist_in_active_threads: bool,
        is_git_repo: bool,
        show_environment_indicator: bool,
        hosts_resting_composer_controls: bool,
    ) -> ShouldShowComposerContextStripInput {
        ShouldShowComposerContextStripInput {
            is_draft_hero_state,
            persist_in_active_threads,
            has_active_project: true,
            is_git_repo,
            show_environment_indicator,
            hosts_resting_composer_controls,
        }
    }

    #[test]
    fn honors_the_active_thread_preference_with_resting_controls() {
        for hosts in [false, true] {
            assert!(!should_show_composer_context_strip(&input(
                false, false, true, true, hosts
            )));
            assert!(should_show_composer_context_strip(&input(
                false, true, true, true, hosts
            )));
        }
    }

    #[test]
    fn keeps_the_environment_indicator_visible_for_a_non_git_project() {
        assert!(should_show_composer_context_strip(&input(
            true, false, false, true, false
        )));
    }

    #[test]
    fn hides_the_strip_when_a_non_git_project_has_nothing_to_show() {
        assert!(!should_show_composer_context_strip(&input(
            true, false, false, false, false
        )));
    }

    #[test]
    fn keeps_the_strip_for_visible_resting_composer_controls_in_a_non_git_thread() {
        assert!(should_show_composer_context_strip(&input(
            true, false, false, false, true
        )));
    }

    #[test]
    fn shows_git_controls_without_requiring_an_environment_indicator() {
        assert!(should_show_composer_context_strip(&input(
            true, false, true, false, false
        )));
    }

    #[test]
    fn needs_an_active_project() {
        let mut no_project = input(true, true, true, true, true);
        no_project.has_active_project = false;
        assert!(!should_show_composer_context_strip(&no_project));
    }
}

mod resolve_effective_env_mode_tests {
    use super::*;

    fn mode(
        path: Option<&str>,
        has_server_thread: bool,
        draft: Option<EnvMode>,
        preparing: bool,
    ) -> EnvMode {
        resolve_effective_env_mode(&ResolveEffectiveEnvModeInput {
            active_worktree_path: path,
            has_server_thread,
            draft_thread_env_mode: draft,
            preparing_worktree: preparing,
        })
    }

    #[test]
    fn treats_draft_threads_already_attached_to_a_worktree_as_current_checkout_mode() {
        assert_eq!(
            mode(
                Some("/repo/.t3/worktrees/feature-a"),
                false,
                Some(EnvMode::Worktree),
                false
            ),
            EnvMode::Local
        );
    }

    #[test]
    fn keeps_explicit_new_worktree_mode_for_draft_threads_without_a_worktree_path() {
        assert_eq!(
            mode(None, false, Some(EnvMode::Worktree), false),
            EnvMode::Worktree
        );
    }

    #[test]
    fn keeps_a_server_thread_in_worktree_mode_while_its_worktree_is_still_being_created() {
        assert_eq!(mode(None, true, None, true), EnvMode::Worktree);
        assert_eq!(mode(None, true, None, false), EnvMode::Local);
    }
}

#[test]
fn env_mode_labels_are_explicit_workspace_labels() {
    assert_eq!(resolve_env_mode_label(EnvMode::Local), "Current checkout");
    assert_eq!(resolve_env_mode_label(EnvMode::Worktree), "New worktree");
}

#[test]
fn current_workspace_label_describes_the_checkout() {
    assert_eq!(resolve_current_workspace_label(None), "Current checkout");
    assert_eq!(
        resolve_current_workspace_label(Some("/repo/.t3/worktrees/feature-a")),
        "Current worktree"
    );
}

mod resolve_locked_workspace_label_tests {
    use super::*;

    #[test]
    fn uses_a_shorter_label_for_the_main_repo_checkout() {
        assert_eq!(
            resolve_locked_workspace_label(None, EnvMode::Local),
            "Local checkout"
        );
    }

    #[test]
    fn uses_a_shorter_label_for_an_attached_worktree() {
        assert_eq!(
            resolve_locked_workspace_label(
                Some("/repo/.t3/worktrees/feature-a"),
                EnvMode::Worktree
            ),
            "Worktree"
        );
    }

    #[test]
    fn describes_a_worktree_that_is_still_being_created_as_a_new_worktree() {
        assert_eq!(
            resolve_locked_workspace_label(None, EnvMode::Worktree),
            "New worktree"
        );
    }
}

#[test]
fn derives_local_branch_names_from_remote_refs() {
    assert_eq!(
        derive_local_branch_name_from_remote_ref("origin/feature/demo"),
        "feature/demo"
    );
    assert_eq!(
        derive_local_branch_name_from_remote_ref("my-org/upstream/feature/demo"),
        "upstream/feature/demo"
    );
    assert_eq!(
        derive_local_branch_name_from_remote_ref("origin/"),
        "origin/"
    );
    assert_eq!(
        derive_local_branch_name_from_remote_ref("/feature/demo"),
        "/feature/demo"
    );
}

mod dedupe_remote_branches_with_local_matches_tests {
    use super::*;

    fn local(name: &str) -> VcsRef {
        VcsRef {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            is_remote: None,
            remote_name: None,
            current: false,
            is_default: false,
            worktree_path: None,
        }
    }

    fn remote(name: &str, remote_name: &str) -> VcsRef {
        VcsRef {
            is_remote: Some(true),
            remote_name: Some(TrimmedNonEmptyString::new(remote_name).unwrap()),
            ..local(name)
        }
    }

    fn names(refs: &[VcsRef]) -> Vec<&str> {
        refs.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn hides_remote_refs_when_the_matching_local_ref_exists() {
        let input = [
            local("feature/demo"),
            remote("origin/feature/demo", "origin"),
            remote("origin/feature/remote-only", "origin"),
        ];
        let out = dedupe_remote_branches_with_local_matches(&input);
        assert_eq!(
            names(&out),
            vec!["feature/demo", "origin/feature/remote-only"]
        );
    }

    #[test]
    fn keeps_all_entries_when_no_local_match_exists_for_a_remote_ref() {
        let input = [
            local("feature/local"),
            remote("origin/feature/remote-only", "origin"),
        ];
        let out = dedupe_remote_branches_with_local_matches(&input);
        assert_eq!(
            names(&out),
            vec!["feature/local", "origin/feature/remote-only"]
        );
    }

    #[test]
    fn keeps_non_origin_remote_refs_visible_even_when_a_matching_local_ref_exists() {
        let input = [
            local("feature/demo"),
            remote("my-org/upstream/feature/demo", "my-org/upstream"),
        ];
        let out = dedupe_remote_branches_with_local_matches(&input);
        assert_eq!(
            names(&out),
            vec!["feature/demo", "my-org/upstream/feature/demo"]
        );
    }

    #[test]
    fn keeps_non_origin_remote_refs_visible_when_git_tracks_with_first_slash_local_naming() {
        let input = [
            local("upstream/feature"),
            remote("my-org/upstream/feature", "my-org/upstream"),
        ];
        let out = dedupe_remote_branches_with_local_matches(&input);
        assert_eq!(
            names(&out),
            vec!["upstream/feature", "my-org/upstream/feature"]
        );
    }
}

mod resolve_branch_selection_target_tests {
    use super::*;

    fn target(
        active_worktree_path: Option<&str>,
        is_default: bool,
        worktree_path: Option<&str>,
    ) -> BranchSelectionTarget {
        resolve_branch_selection_target(
            "/repo",
            active_worktree_path,
            &BranchSelectionRef {
                is_default,
                worktree_path,
            },
        )
    }

    #[test]
    fn reuses_an_existing_secondary_worktree_for_the_selected_ref() {
        assert_eq!(
            target(
                Some("/repo/.t3/worktrees/feature-a"),
                false,
                Some("/repo/.t3/worktrees/feature-b")
            ),
            BranchSelectionTarget {
                checkout_cwd: "/repo/.t3/worktrees/feature-b".into(),
                next_worktree_path: Some("/repo/.t3/worktrees/feature-b".into()),
                reuse_existing_worktree: true,
            }
        );
    }

    #[test]
    fn switches_back_to_the_main_repo_when_the_ref_already_lives_there() {
        assert_eq!(
            target(Some("/repo/.t3/worktrees/feature-a"), true, Some("/repo")),
            BranchSelectionTarget {
                checkout_cwd: "/repo".into(),
                next_worktree_path: None,
                reuse_existing_worktree: true,
            }
        );
    }

    #[test]
    fn checks_out_the_default_ref_in_the_main_repo_when_leaving_a_secondary_worktree() {
        assert_eq!(
            target(Some("/repo/.t3/worktrees/feature-a"), true, None),
            BranchSelectionTarget {
                checkout_cwd: "/repo".into(),
                next_worktree_path: None,
                reuse_existing_worktree: false,
            }
        );
    }

    #[test]
    fn keeps_checkout_in_the_current_worktree_for_non_default_refs() {
        assert_eq!(
            target(Some("/repo/.t3/worktrees/feature-a"), false, None),
            BranchSelectionTarget {
                checkout_cwd: "/repo/.t3/worktrees/feature-a".into(),
                next_worktree_path: Some("/repo/.t3/worktrees/feature-a".into()),
                reuse_existing_worktree: false,
            }
        );
    }
}

mod should_include_branch_picker_item_tests {
    use super::*;

    fn include(
        item_value: &str,
        normalized_query: &str,
        create: Option<&str>,
        checkout: Option<&str>,
    ) -> bool {
        should_include_branch_picker_item(&ShouldIncludeBranchPickerItemInput {
            item_value,
            normalized_query,
            create_branch_item_value: create,
            checkout_pull_request_item_value: checkout,
        })
    }

    #[test]
    fn keeps_the_synthetic_checkout_pr_item_visible_for_gh_pr_checkout_input() {
        assert!(include(
            "__checkout_pull_request__:1359",
            "gh pr checkout 1359",
            Some("__create_new_branch__:gh pr checkout 1359"),
            Some("__checkout_pull_request__:1359"),
        ));
    }

    #[test]
    fn keeps_the_synthetic_create_ref_item_visible_for_arbitrary_ref_input() {
        assert!(include(
            "__create_new_branch__:feature/demo",
            "feature/demo",
            Some("__create_new_branch__:feature/demo"),
            None,
        ));
    }

    #[test]
    fn still_filters_ordinary_ref_items_by_query_text() {
        assert!(!include(
            "main",
            "gh pr checkout 1359",
            Some("__create_new_branch__:gh pr checkout 1359"),
            Some("__checkout_pull_request__:1359"),
        ));
    }

    // Typing a spaced name must still surface the ref it would have been created
    // as, or the picker shows nothing at all for that query.
    #[test]
    fn surfaces_an_existing_ref_matching_the_sanitized_query() {
        assert!(include("new-branch", "new branch", None, None));
    }

    // A partial query has to reach the ref it would have been created as, so
    // searching "hello w" still finds an existing hello-world.
    #[test]
    fn surfaces_a_ref_from_a_partial_query_containing_a_space() {
        assert!(include("hello-world", "hello w", None, None));
    }

    #[test]
    fn excludes_refs_matching_neither_the_raw_nor_the_sanitized_query() {
        assert!(!include("main", "new branch", None, None));
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        assert!(include("main", "", None, None));
    }
}

// Git rejects ASCII space and the ASCII control characters in ref names, so a
// typed name like "new branch" can only ever fail. Replacing exactly those can
// turn a failing name into a working one without touching a name git already
// accepts, including one holding non-ASCII whitespace such as U+00A0.
mod sanitize_new_ref_name_tests {
    use super::*;

    #[test]
    fn replaces_a_space_with_a_dash() {
        assert_eq!(sanitize_new_ref_name("new branch"), "new-branch");
    }

    #[test]
    fn collapses_a_run_of_whitespace_into_a_single_dash() {
        assert_eq!(sanitize_new_ref_name("new   branch"), "new-branch");
    }

    #[test]
    fn trims_surrounding_whitespace_instead_of_turning_it_into_dashes() {
        assert_eq!(sanitize_new_ref_name("  new branch  "), "new-branch");
    }

    #[test]
    fn replaces_tabs_which_git_rejects_just_like_spaces() {
        assert_eq!(sanitize_new_ref_name("new\tbranch"), "new-branch");
    }

    // git accepts U+00A0, U+2009 and other non-ASCII whitespace in ref names, so
    // rewriting them would silently create a ref the user never typed.
    #[test]
    fn preserves_whitespace_that_git_accepts() {
        assert_eq!(sanitize_new_ref_name("new\u{a0}branch"), "new\u{a0}branch");
        assert_eq!(
            sanitize_new_ref_name("new\u{2009}branch"),
            "new\u{2009}branch"
        );
    }

    #[test]
    fn keeps_slashes_so_nested_ref_names_survive() {
        assert_eq!(
            sanitize_new_ref_name("feature/new thing"),
            "feature/new-thing"
        );
    }

    #[test]
    fn preserves_case_because_git_ref_names_are_case_sensitive() {
        assert_eq!(
            sanitize_new_ref_name("Feature/New Thing"),
            "Feature/New-Thing"
        );
    }

    #[test]
    fn leaves_an_already_valid_ref_name_untouched() {
        assert_eq!(sanitize_new_ref_name("feature/login"), "feature/login");
    }

    #[test]
    fn returns_an_empty_string_for_whitespace_only_input() {
        assert_eq!(sanitize_new_ref_name("   "), "");
    }

    // Scoped deliberately to whitespace: git accepts consecutive dashes, so
    // collapsing them would rewrite names the user may have typed on purpose.
    #[test]
    fn does_not_collapse_dashes_the_user_typed() {
        assert_eq!(sanitize_new_ref_name("new - branch"), "new---branch");
        assert_eq!(sanitize_new_ref_name("foo--bar"), "foo--bar");
    }
}

#[test]
fn context_strip_labels_collapse_with_hysteresis() {
    // Expanded: collapse only once the content no longer fits.
    assert!(!resolve_context_strip_labels_compact(false, 300., 300.));
    assert!(resolve_context_strip_labels_compact(false, 301., 300.));
    // Compact: stay collapsed until there is 16px of slack.
    assert!(resolve_context_strip_labels_compact(true, 290., 300.));
    assert!(!resolve_context_strip_labels_compact(true, 284., 300.));
}

#[test]
fn worktree_submodule_labels() {
    assert_eq!(
        worktree_submodules_label(WorktreeSubmodules::Recursive),
        "Recursive"
    );
    assert_eq!(
        worktree_submodules_label(WorktreeSubmodules::TopLevel),
        "Top level only"
    );
    assert_eq!(worktree_submodules_label(WorktreeSubmodules::None), "Skip");
}
