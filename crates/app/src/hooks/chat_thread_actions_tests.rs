//! Tests for `chat_thread_actions.rs`, ported from upstream
//! `apps/web/src/lib/chatThreadActions.test.ts` (all 11 cases).

use std::cell::Cell;

use turin_contracts::environment::ScopedProjectRef;
use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::{EnvironmentId, ProjectId};

use super::{
    ChatThreadActionContext, ComposerModelSelectionState, ThreadContextLike,
    has_explicit_composer_model_selection, resolve_new_draft_start_from_origin,
    resolve_new_thread_model_selection_override, resolve_thread_action_project_ref,
    start_new_thread_from_context,
};

const PROJECT_DEFAULT_SELECTION: &str = "codex/project-default";
const CARRIED_SELECTION: &str = "codex/carried-model";

fn environment() -> EnvironmentId {
    EnvironmentId::new("environment-1").unwrap()
}

fn project(id: &str) -> ProjectId {
    ProjectId::new(id).unwrap()
}

fn project_ref(id: &str) -> ScopedProjectRef {
    ScopedProjectRef {
        environment_id: environment(),
        project_id: project(id),
    }
}

fn context() -> ChatThreadActionContext {
    ChatThreadActionContext {
        active_draft_thread: None,
        active_thread: None,
        default_project_ref: Some(project_ref("project-2")),
    }
}

fn at(id: &str) -> ThreadContextLike {
    ThreadContextLike {
        environment_id: environment(),
        project_id: project(id),
    }
}

#[test]
fn only_treats_an_active_stored_selection_marked_explicit_as_an_explicit_pick() {
    let draft = ComposerModelSelectionState {
        active_provider: Some("codex".to_owned()),
        model_selection_by_provider: vec![("codex".to_owned(), PROJECT_DEFAULT_SELECTION)],
        model_selection_explicit: true,
    };

    assert!(has_explicit_composer_model_selection(Some(&draft)));
    assert!(!has_explicit_composer_model_selection(Some(
        &ComposerModelSelectionState {
            model_selection_explicit: false,
            ..draft.clone()
        }
    )));
    assert!(!has_explicit_composer_model_selection(Some(
        &ComposerModelSelectionState {
            active_provider: None,
            ..draft.clone()
        }
    )));
    assert!(!has_explicit_composer_model_selection::<&str>(None));
}

#[test]
fn does_not_carry_a_non_explicit_model_from_the_destination_draft_back_into_itself() {
    assert_eq!(
        resolve_new_thread_model_selection_override(
            None,
            Some(&CARRIED_SELECTION),
            Some("draft-a"),
            "draft-a",
        ),
        None
    );
}

#[test]
fn still_carries_models_between_different_threads_when_the_project_has_no_default() {
    assert_eq!(
        resolve_new_thread_model_selection_override(
            None,
            Some(&CARRIED_SELECTION),
            Some("draft-a"),
            "draft-b",
        ),
        Some(CARRIED_SELECTION)
    );
}

#[test]
fn keeps_the_project_default_above_any_carried_selection() {
    assert_eq!(
        resolve_new_thread_model_selection_override(
            Some(&PROJECT_DEFAULT_SELECTION),
            Some(&CARRIED_SELECTION),
            Some("draft-a"),
            "draft-b",
        ),
        Some(PROJECT_DEFAULT_SELECTION)
    );
}

#[test]
fn only_applies_the_start_from_origin_default_to_new_worktree_drafts() {
    assert!(resolve_new_draft_start_from_origin(
        ThreadEnvMode::Worktree,
        true
    ));
    assert!(!resolve_new_draft_start_from_origin(
        ThreadEnvMode::Local,
        true
    ));
}

#[test]
fn prefers_the_active_thread_project_when_resolving_thread_actions() {
    let resolved = resolve_thread_action_project_ref(&ChatThreadActionContext {
        active_thread: Some(at("project-1")),
        ..context()
    });
    assert_eq!(resolved, Some(project_ref("project-1")));
}

#[test]
fn falls_back_to_the_active_draft_thread_project_when_there_is_no_active_thread() {
    let resolved = resolve_thread_action_project_ref(&ChatThreadActionContext {
        active_draft_thread: Some(at("project-1")),
        ..context()
    });
    assert_eq!(resolved, Some(project_ref("project-1")));
}

#[test]
fn falls_back_to_the_default_project_ref_when_there_is_no_active_thread_context() {
    let resolved = resolve_thread_action_project_ref(&ChatThreadActionContext {
        default_project_ref: Some(project_ref("project-1")),
        ..context()
    });
    assert_eq!(resolved, Some(project_ref("project-1")));
}

#[test]
fn inherits_only_the_project_from_context_never_branch_or_worktree_state() {
    let handled = Cell::new(None);
    let started = start_new_thread_from_context(
        &ChatThreadActionContext {
            active_thread: Some(at("project-1")),
            ..context()
        },
        |project| -> Result<(), ()> {
            handled.set(Some(project.clone()));
            Ok(())
        },
    );
    assert_eq!(started, Ok(true));
    assert_eq!(handled.take(), Some(project_ref("project-1")));
}

#[test]
fn does_not_start_a_thread_when_there_is_no_project_context() {
    let calls = Cell::new(0);
    let started = start_new_thread_from_context(
        &ChatThreadActionContext {
            default_project_ref: None,
            ..context()
        },
        |_| -> Result<(), ()> {
            calls.set(calls.get() + 1);
            Ok(())
        },
    );
    assert_eq!(started, Ok(false));
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_handler_failure_propagates_as_the_rejection_does_upstream() {
    let started = start_new_thread_from_context(&context(), |_| Err("handler failed"));
    assert_eq!(started, Err("handler failed"));
}
