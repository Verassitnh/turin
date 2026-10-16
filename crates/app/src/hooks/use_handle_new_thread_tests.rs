//! Tests for `use_handle_new_thread.rs`. Upstream's `useHandleNewThread.test.ts`
//! drives the store, the router and the project-file read, so it is not
//! ported. These cases cover the decisions that file's cases exercise: the
//! abandon rules, the empty-versus-invested draft choice, explicit options
//! winning outright, and the stale-await bailout.

use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::provider_policy::RuntimeMode;

use super::{
    NewThreadOptions, ReuseRaceSnapshot, ReusedDraftWorkspace, StoredDraftDecision,
    StoredDraftFacts, WorkspaceOptions, abandoned_since_request, carry_interaction_mode,
    carry_model_selection, classify_stored_draft, initial_env_mode, lost_reuse_race,
    new_draft_fields, pick_explicit_workspace_options, reused_draft_workspace,
};
use crate::hooks::chat_thread_actions::ComposerModelSelectionState;

fn stored(promoted: bool, thread_exists: bool, has_user_content: bool) -> StoredDraftFacts {
    StoredDraftFacts {
        promoted,
        thread_exists,
        has_user_content,
    }
}

#[test]
fn a_stored_draft_is_reused_only_when_empty_and_not_yet_a_thread() {
    assert_eq!(classify_stored_draft(None), StoredDraftDecision::None);
    assert_eq!(
        classify_stored_draft(Some(stored(false, false, false))),
        StoredDraftDecision::ReuseEmpty
    );
    assert_eq!(
        classify_stored_draft(Some(stored(false, false, true))),
        StoredDraftDecision::Invested
    );
    assert_eq!(
        classify_stored_draft(Some(stored(true, false, false))),
        StoredDraftDecision::Promote
    );
    assert_eq!(
        classify_stored_draft(Some(stored(false, true, false))),
        StoredDraftDecision::Promote
    );
}

#[test]
fn explicit_workspace_options_win_outright_over_a_reused_draft() {
    let options = NewThreadOptions {
        branch: Some(Some("feature".into())),
        env_mode: Some(ThreadEnvMode::Worktree),
        ..NewThreadOptions::default()
    };
    let decision = reused_draft_workspace(
        &options,
        true,
        ThreadEnvMode::Local,
        true,
        RuntimeMode::FullAccess,
    );
    assert_eq!(
        decision.workspace,
        ReusedDraftWorkspace::Explicit(WorkspaceOptions {
            branch: Some(Some("feature".into())),
            worktree_path: None,
            env_mode: Some(ThreadEnvMode::Worktree),
            start_from_origin: None,
        })
    );
    // Already open: upstream omits runtimeMode even when options are explicit.
    assert_eq!(decision.runtime_mode, None);
}

#[test]
fn an_open_draft_keeps_its_workspace_context_when_no_options_are_passed() {
    let decision = reused_draft_workspace(
        &NewThreadOptions::default(),
        true,
        ThreadEnvMode::Local,
        false,
        RuntimeMode::Auto,
    );
    assert_eq!(decision.workspace, ReusedDraftWorkspace::KeepOpenDraft);
    assert_eq!(decision.runtime_mode, None);
}

#[test]
fn a_closed_draft_resets_to_the_configured_defaults() {
    let decision = reused_draft_workspace(
        &NewThreadOptions::default(),
        false,
        ThreadEnvMode::Worktree,
        true,
        RuntimeMode::ApprovalRequired,
    );
    assert_eq!(
        decision.workspace,
        ReusedDraftWorkspace::Defaults {
            env_mode: ThreadEnvMode::Worktree,
            start_from_origin: true,
        }
    );
    assert_eq!(decision.runtime_mode, Some(RuntimeMode::ApprovalRequired));
}

#[test]
fn picking_workspace_options_keeps_absent_keys_absent() {
    let options = NewThreadOptions {
        worktree_path: Some(None),
        ..NewThreadOptions::default()
    };
    assert_eq!(
        pick_explicit_workspace_options(&options),
        WorkspaceOptions {
            branch: None,
            worktree_path: Some(None),
            env_mode: None,
            start_from_origin: None,
        }
    );
}

#[test]
fn a_draft_abandons_its_open_when_the_route_moves_or_the_project_is_removed() {
    assert!(abandoned_since_request(true, true, false));
    assert!(abandoned_since_request(false, true, true));
    // A project unknown at request time cannot be removed out from under it.
    assert!(!abandoned_since_request(false, false, true));
    assert!(!abandoned_since_request(false, true, false));
}

#[test]
fn any_store_change_during_the_await_marks_this_invocation_as_a_loser() {
    let quiet = ReuseRaceSnapshot {
        opened_meanwhile: false,
        promoted_meanwhile: false,
        remapped_meanwhile: false,
        invested_meanwhile: false,
    };
    assert!(!lost_reuse_race(quiet));
    for changed in [
        ReuseRaceSnapshot {
            opened_meanwhile: true,
            ..quiet
        },
        ReuseRaceSnapshot {
            promoted_meanwhile: true,
            ..quiet
        },
        ReuseRaceSnapshot {
            remapped_meanwhile: true,
            ..quiet
        },
        ReuseRaceSnapshot {
            invested_meanwhile: true,
            ..quiet
        },
    ] {
        assert!(lost_reuse_race(changed));
    }
}

#[test]
fn the_caller_env_mode_skips_the_default_resolution() {
    let options = NewThreadOptions {
        env_mode: Some(ThreadEnvMode::Local),
        ..NewThreadOptions::default()
    };
    let mut resolved = false;
    let mode = initial_env_mode(&options, || {
        resolved = true;
        ThreadEnvMode::Worktree
    });
    assert_eq!(mode, ThreadEnvMode::Local);
    assert!(!resolved);
}

#[test]
fn a_fresh_draft_takes_its_branch_and_path_from_the_options_and_defaults_the_rest() {
    let options = NewThreadOptions {
        branch: Some(Some("main".into())),
        ..NewThreadOptions::default()
    };
    assert_eq!(
        new_draft_fields(
            &options,
            ThreadEnvMode::Worktree,
            true,
            RuntimeMode::FullAccess
        ),
        super::NewDraftFields {
            branch: Some("main".into()),
            worktree_path: None,
            env_mode: ThreadEnvMode::Worktree,
            start_from_origin: true,
            runtime_mode: RuntimeMode::FullAccess,
        }
    );
    // An explicit `startFromOrigin` beats the project default. Runtime mode does not.
    let explicit = NewThreadOptions {
        start_from_origin: Some(false),
        ..NewThreadOptions::default()
    };
    let fields = new_draft_fields(
        &explicit,
        ThreadEnvMode::Worktree,
        true,
        RuntimeMode::AutoAcceptEdits,
    );
    assert!(!fields.start_from_origin);
    assert_eq!(fields.runtime_mode, RuntimeMode::AutoAcceptEdits);
}

/// `useHandleNewThread.test.ts` "uses the target environment's %s permissions
/// for new threads": both a mint-fresh draft and a reusable empty draft take
/// `projectSettings.settings.defaultRuntimeMode`.
#[test]
fn new_and_reused_drafts_take_the_target_environment_runtime_mode() {
    let modes = [
        RuntimeMode::ApprovalRequired,
        RuntimeMode::AutoAcceptEdits,
        RuntimeMode::Auto,
        RuntimeMode::FullAccess,
    ];
    for mode in modes {
        assert_eq!(
            new_draft_fields(&NewThreadOptions::default(), ThreadEnvMode::Local, false, mode)
                .runtime_mode,
            mode
        );
        let reused = reused_draft_workspace(
            &NewThreadOptions::default(),
            false,
            ThreadEnvMode::Local,
            false,
            mode,
        );
        assert_eq!(reused.runtime_mode, Some(mode));
        // Explicit workspace options do not replace the environment's runtime mode.
        let explicit = NewThreadOptions {
            branch: Some(Some("feature".into())),
            ..NewThreadOptions::default()
        };
        let with_options =
            reused_draft_workspace(&explicit, false, ThreadEnvMode::Local, false, mode);
        assert!(matches!(
            with_options.workspace,
            ReusedDraftWorkspace::Explicit(_)
        ));
        assert_eq!(with_options.runtime_mode, Some(mode));
    }
}

#[test]
fn the_composer_selection_wins_over_the_viewed_thread_when_carrying_a_model() {
    assert_eq!(
        carry_model_selection(Some("composer"), Some("thread")),
        Some("composer")
    );
    assert_eq!(carry_model_selection(None, Some("thread")), Some("thread"));
    assert_eq!(carry_model_selection::<&str>(None, None), None);
}

#[test]
fn interaction_mode_falls_through_composer_then_thread_then_draft() {
    assert_eq!(
        carry_interaction_mode(Some("plan"), Some("default"), Some("auto")),
        Some("plan")
    );
    assert_eq!(
        carry_interaction_mode(None, Some("default"), Some("auto")),
        Some("default")
    );
    assert_eq!(
        carry_interaction_mode::<&str>(None, None, Some("auto")),
        Some("auto")
    );
    assert_eq!(carry_interaction_mode::<&str>(None, None, None), None);
}

#[test]
fn an_explicit_composer_pick_is_recognized_for_the_model_override() {
    let draft = ComposerModelSelectionState {
        active_provider: Some("codex".into()),
        model_selection_by_provider: vec![("codex".into(), "pinned")],
        model_selection_explicit: true,
    };
    assert!(crate::hooks::chat_thread_actions::has_explicit_composer_model_selection(Some(&draft)));
}
