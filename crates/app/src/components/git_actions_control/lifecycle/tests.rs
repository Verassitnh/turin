//! Port of upstream/apps/web/src/components/GitActionsControl.lifecycle.test.ts
//! and the thread-branch behaviours of GitActionsControl.test.ts. The tests
//! that only exercised permission scopes (`AuthSourceControlWriteScope`,
//! `AuthOrchestrationOperateScope`) went with the auth cut.

use serde_json::{Value, json};
use turin_contracts::git::{GitRunStackedActionResult, GitStackedAction, VcsStatusResult};
use turin_contracts::{EnvironmentId, ThreadId};

use super::*;

fn target(thread: &str, cwd: &str) -> GitActionTarget {
    GitActionTarget {
        environment_id: EnvironmentId::new("environment").unwrap(),
        thread_id: ThreadId::new(thread).unwrap(),
        cwd: cwd.to_owned(),
    }
}

fn status(overrides: Value) -> VcsStatusResult {
    let mut base = json!({
        "isRepo": true,
        "refName": "main",
        "isDefaultRef": false,
        "hasPrimaryRemote": true,
        "hasWorkingTreeChanges": true,
        "workingTree": {
            "files": [{ "path": "file.ts", "insertions": 1, "deletions": 0 }],
            "insertions": 1,
            "deletions": 0,
        },
        "hasUpstream": true,
        "aheadCount": 0,
        "behindCount": 0,
        "pr": null,
    });
    for (key, value) in overrides.as_object().unwrap() {
        base.as_object_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    serde_json::from_value(base).unwrap()
}

fn result(branch: Value, toast: Value) -> GitRunStackedActionResult {
    serde_json::from_value(json!({
        "action": "commit",
        "branch": branch,
        "commit": {
            "status": "created",
            "commitSha": "89abcdef01234567",
            "subject": "feat: add ref sync",
        },
        "push": { "status": "pushed", "branch": "feature/fix-toast-copy" },
        "pr": { "status": "skipped_not_requested" },
        "toast": toast,
    }))
    .unwrap()
}

fn committed_toast() -> Value {
    json!({
        "title": "Committed",
        "description": "Committed",
        "cta": {
            "kind": "run_action",
            "label": "Push & create PR",
            "action": { "kind": "commit_push_pr" },
        },
    })
}

fn server_thread(branch: &str) -> GitThreadContext {
    GitThreadContext {
        is_server_thread: true,
        project_id: None,
        branch: Some(branch.to_owned()),
    }
}

fn plan(
    input: &RunGitActionInput,
    thread_target: &GitActionTarget,
    thread: &GitThreadContext,
    status: &VcsStatusResult,
    is_default_ref: bool,
) -> GitActionPlan {
    plan_git_action(
        input,
        Some(status),
        is_default_ref,
        thread,
        &thread_target.thread_id,
        "action".into(),
    )
}

mod git_action_targets_across_thread_navigation {
    use super::*;

    #[test]
    fn runs_a_retained_pr_toast_in_the_thread_that_produced_it() {
        let first = target("first", "/first");
        let produced = result(
            json!({ "status": "skipped_not_requested" }),
            committed_toast(),
        );
        let ActionPresentation::Toast { cta, .. } = present_action_result(&produced, &first, false)
        else {
            panic!("expected a toast");
        };
        // The panel moves on to another thread.
        let _second = target("second", "/second");
        let ResultToastCta::RunAction {
            target: retained,
            action,
            ..
        } = cta
        else {
            panic!("expected a run-action button");
        };
        let request = plan(
            &RunGitActionInput::new(action),
            &retained,
            &server_thread("main"),
            &status(json!({})),
            false,
        );
        let GitActionPlan::Run(request) = request else {
            panic!("expected the action to run");
        };
        assert_eq!(retained.cwd, "/first");
        assert_eq!(request.thread_id, Some(ThreadId::new("first").unwrap()));
        assert_eq!(request.action, GitStackedAction::CommitPushPr);
    }

    #[test]
    fn runs_the_panel_button_in_the_current_thread_after_navigation() {
        for (thread, cwd) in [
            ("first", "/first"),
            ("second", "/second"),
            ("first", "/first"),
        ] {
            let current = target(thread, cwd);
            let GitActionPlan::Run(request) = plan(
                &RunGitActionInput::new(GitStackedAction::Commit),
                &current,
                &server_thread("main"),
                &status(json!({})),
                false,
            ) else {
                panic!("expected the action to run");
            };
            assert_eq!(request.thread_id, Some(ThreadId::new(thread).unwrap()));
            assert_eq!(current.cwd, cwd);
        }
    }

    #[test]
    fn the_panel_shows_results_inline_instead_of_a_toast() {
        let produced = result(
            json!({ "status": "skipped_not_requested" }),
            committed_toast(),
        );
        assert_eq!(
            present_action_result(&produced, &target("t", "/t"), true),
            ActionPresentation::Inline {
                title: "Committed".into(),
                description: Some("Committed".into())
            }
        );
    }

    #[test]
    fn success_toasts_dismiss_after_the_visible_window_and_other_ctas_pass_through() {
        let produced = result(
            json!({ "status": "skipped_not_requested" }),
            json!({
                "title": "Opened PR",
                "cta": { "kind": "open_pr", "label": "View PR", "url": "https://example.com/pr/1" },
            }),
        );
        assert_eq!(
            present_action_result(&produced, &target("t", "/t"), false),
            ActionPresentation::Toast {
                title: "Opened PR".into(),
                description: None,
                cta: ResultToastCta::OpenPr {
                    label: "View PR".into(),
                    url: "https://example.com/pr/1".into()
                },
                dismiss_after_visible_ms: Some(INLINE_SUCCESS_VISIBLE_MS),
            }
        );
    }
}

mod default_ref_confirmation {
    use super::*;

    #[test]
    fn asks_before_pushing_on_the_default_ref_with_the_commit_flag() {
        let current = target("t", "/repo");
        let on_main = status(json!({ "refName": "main", "isDefaultRef": true }));
        let plan = plan(
            &RunGitActionInput {
                commit_message: Some("msg".into()),
                ..RunGitActionInput::new(GitStackedAction::CommitPush)
            },
            &current,
            &server_thread("main"),
            &on_main,
            true,
        );
        assert_eq!(
            plan,
            GitActionPlan::ConfirmDefaultBranch(PendingDefaultBranchAction {
                action: DefaultBranchConfirmableAction::CommitPush,
                branch_name: "main".into(),
                includes_commit: true,
                commit_message: Some("msg".into()),
                file_paths: None,
            })
        );
    }

    #[test]
    fn skips_the_prompt_on_a_feature_branch_run_and_for_commit_only() {
        let current = target("t", "/repo");
        let on_main = status(json!({ "refName": "main", "isDefaultRef": true }));
        for input in [
            RunGitActionInput {
                feature_branch: true,
                ..RunGitActionInput::new(GitStackedAction::CommitPush)
            },
            RunGitActionInput::new(GitStackedAction::Commit),
            RunGitActionInput {
                skip_default_branch_prompt: true,
                ..RunGitActionInput::new(GitStackedAction::Push)
            },
        ] {
            assert!(matches!(
                plan(&input, &current, &server_thread("main"), &on_main, true),
                GitActionPlan::Run(_)
            ));
        }
    }

    #[test]
    fn continuing_a_pending_action_runs_it_without_the_prompt_optionally_on_a_feature_branch() {
        let pending = PendingDefaultBranchAction {
            action: DefaultBranchConfirmableAction::CreatePr,
            branch_name: "main".into(),
            includes_commit: false,
            commit_message: None,
            file_paths: Some(vec!["a.ts".into()]),
        };
        let continued = continue_pending_action(&pending, true);
        assert_eq!(continued.action, GitStackedAction::CreatePr);
        assert!(continued.skip_default_branch_prompt);
        assert!(continued.feature_branch);
        assert_eq!(continued.file_paths, Some(vec!["a.ts".to_owned()]));
    }
}

mod thread_branch_sync {
    use super::*;

    fn created(name: &str) -> GitRunStackedActionResult {
        result(
            json!({ "status": "created", "name": name }),
            json!({ "title": "Committed", "cta": { "kind": "none" } }),
        )
    }

    #[test]
    fn synchronizes_the_server_thread_after_a_feature_branch_commit() {
        assert_eq!(
            sync_after_git_action(&created("feature"), &server_thread("main"), false, None),
            BranchSync::UpdateServerThread {
                branch: Some("feature".into()),
                expected_branch: Some("main".into()),
            }
        );
    }

    #[test]
    fn leaves_the_server_thread_alone_when_the_branch_already_matches_or_none_was_created() {
        assert_eq!(
            sync_after_git_action(&created("main"), &server_thread("main"), false, None),
            BranchSync::None
        );
        let ordinary = result(
            json!({ "status": "skipped_not_requested" }),
            json!({ "title": "Committed", "cta": { "kind": "none" } }),
        );
        assert_eq!(
            sync_after_git_action(&ordinary, &server_thread("main"), false, None),
            BranchSync::None
        );
    }

    #[test]
    fn keeps_feature_branch_commits_available_for_a_local_draft() {
        let draft = GitThreadContext {
            is_server_thread: false,
            project_id: Some("project".into()),
            branch: None,
        };
        assert_eq!(
            sync_after_git_action(&created("feature"), &draft, true, Some("main")),
            BranchSync::SetDraftBranch {
                branch: Some("feature".into()),
                manual: true
            }
        );
        // A draft's action is not linked to a thread; it carries its project instead.
        let GitActionPlan::Run(request) = plan(
            &RunGitActionInput::new(GitStackedAction::Commit),
            &target("draft", "/repo"),
            &draft,
            &status(json!({})),
            false,
        ) else {
            panic!("expected the action to run");
        };
        assert_eq!(request.thread_id, None);
        assert_eq!(request.project_id.as_deref(), Some("project"));
    }

    #[test]
    fn a_draft_follows_the_live_checkout_unless_an_action_runs() {
        let draft = GitThreadContext {
            is_server_thread: false,
            project_id: None,
            branch: None,
        };
        let on_feature = status(json!({ "refName": "feature/x" }));
        assert_eq!(
            live_draft_branch_sync(&draft, Some("main"), Some(&on_feature), false, false),
            BranchSync::SetDraftBranch {
                branch: Some("feature/x".into()),
                manual: false
            }
        );
        assert_eq!(
            live_draft_branch_sync(&draft, Some("main"), Some(&on_feature), true, false),
            BranchSync::None
        );
        assert_eq!(
            live_draft_branch_sync(&draft, Some("main"), Some(&on_feature), false, true),
            BranchSync::None
        );
        assert_eq!(
            live_draft_branch_sync(
                &server_thread("main"),
                Some("main"),
                Some(&on_feature),
                false,
                false
            ),
            BranchSync::None
        );
    }
}

mod menu_action_disabled_reasons {
    use turin_review::action_state::git_actions_control_logic::{
        GitActionIconName, GitActionMenuItem, GitActionMenuItemId, GitDialogAction,
    };

    use super::*;

    fn item(id: GitActionMenuItemId, disabled: bool) -> GitActionMenuItem {
        GitActionMenuItem {
            id,
            label: "x".into(),
            disabled,
            icon: GitActionIconName::Commit,
            dialog_action: GitDialogAction::Commit,
        }
    }

    fn reason(
        id: GitActionMenuItemId,
        overrides: Value,
        busy: bool,
        remote: bool,
    ) -> Option<String> {
        menu_action_disabled_reason(&item(id, true), Some(&status(overrides)), busy, remote)
    }

    #[test]
    fn enabled_items_have_no_reason() {
        assert_eq!(
            menu_action_disabled_reason(
                &item(GitActionMenuItemId::Commit, false),
                None,
                false,
                true
            ),
            None
        );
    }

    #[test]
    fn busy_and_missing_status_come_first() {
        assert_eq!(
            reason(GitActionMenuItemId::Push, json!({}), true, true).as_deref(),
            Some("Git action in progress.")
        );
        assert_eq!(
            menu_action_disabled_reason(&item(GitActionMenuItemId::Push, true), None, false, true)
                .as_deref(),
            Some("Git status is unavailable.")
        );
    }

    #[test]
    fn commit_explains_a_clean_worktree() {
        assert_eq!(
            reason(
                GitActionMenuItemId::Commit,
                json!({ "hasWorkingTreeChanges": false }),
                false,
                true
            )
            .as_deref(),
            Some("Worktree is clean. Make changes before committing.")
        );
    }

    #[test]
    fn push_walks_its_blockers_in_order() {
        let push = |overrides: Value, remote: bool| {
            reason(GitActionMenuItemId::Push, overrides, false, remote)
        };
        assert_eq!(
            push(json!({ "refName": null }), true).as_deref(),
            Some("Detached HEAD: check out a branch before pushing.")
        );
        assert_eq!(
            push(json!({ "hasWorkingTreeChanges": true }), true).as_deref(),
            Some("Commit or stash local changes before pushing.")
        );
        assert_eq!(
            push(
                json!({ "hasWorkingTreeChanges": false, "behindCount": 2 }),
                true
            )
            .as_deref(),
            Some("Branch is behind upstream. Pull/rebase before pushing.")
        );
        assert_eq!(
            push(
                json!({ "hasWorkingTreeChanges": false, "hasUpstream": false }),
                false
            )
            .as_deref(),
            Some("Add an \"origin\" remote before pushing.")
        );
        assert_eq!(
            push(json!({ "hasWorkingTreeChanges": false }), true).as_deref(),
            Some("No local commits to push.")
        );
    }

    #[test]
    fn create_pr_uses_the_hosts_terminology() {
        let pr = |overrides: Value| reason(GitActionMenuItemId::Pr, overrides, false, true);
        assert_eq!(
            pr(json!({ "refName": null })).as_deref(),
            Some("Detached HEAD: check out a branch before creating a pull request.")
        );
        assert_eq!(
            pr(json!({ "hasWorkingTreeChanges": false, "aheadCount": 0 })).as_deref(),
            Some("No local commits to include in a pull request.")
        );
        assert_eq!(
            pr(json!({ "hasWorkingTreeChanges": false, "aheadCount": 1, "behindCount": 1 }))
                .as_deref(),
            Some("Branch is behind upstream. Pull/rebase before creating a pull request.")
        );
    }
}
