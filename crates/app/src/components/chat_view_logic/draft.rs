//! Port of upstream/apps/web/src/components/ChatView.logic.ts (draft hero and promotion).

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

use crate::state::contracts::{EnvironmentId, ProjectId, ThreadId};

use crate::state::models::{
    EnvironmentThreadShell, InteractionMode, ModelSelection, ThreadRunSummary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmissionIntent {
    Foreground,
    Background,
}

pub fn should_dock_draft_hero_for_submission(input: ShouldDockDraftHeroInput<'_>) -> bool {
    input.submission_intent == ComposerSubmissionIntent::Foreground
        && input.is_draft_hero_state
        && input.active_thread_key.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShouldDockDraftHeroInput<'a> {
    pub is_draft_hero_state: bool,
    pub active_thread_key: Option<&'a str>,
    pub submission_intent: ComposerSubmissionIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftHeroInput {
    pub is_local_draft_thread: bool,
    pub has_timeline_entries: bool,
    pub is_working: bool,
    pub draft_hero_dock_requested: bool,
    pub background_submission_pending: bool,
    pub has_worktree_setup_card: bool,
}

pub fn resolve_draft_hero_state(input: DraftHeroInput) -> bool {
    if input.has_worktree_setup_card {
        return false;
    }
    if input.background_submission_pending {
        return true;
    }
    input.is_local_draft_thread
        && !input.has_timeline_entries
        && !input.is_working
        && !input.draft_hero_dock_requested
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftPromotionServerThread<'a> {
    pub latest_run: Option<&'a ThreadRunSummary>,
    pub latest_user_message_at: Option<&'a str>,
}

pub fn resolve_draft_promotion_navigation_target(
    server_thread_ref: Option<&ScopedThreadRef>,
    server_thread: Option<DraftPromotionServerThread<'_>>,
    background_submission_pending: bool,
) -> Option<ScopedThreadRef> {
    if background_submission_pending {
        return None;
    }
    let server_thread_ref = server_thread_ref?;
    let server_thread = server_thread?;
    let latest_run = server_thread.latest_run;
    let run_started = latest_run
        .and_then(|run| run.started_at.as_deref())
        .is_some();
    let startup_stopped = latest_run.is_some_and(|run| {
        matches!(
            run.status,
            crate::state::models::RunStatus::Failed
                | crate::state::models::RunStatus::Interrupted
                | crate::state::models::RunStatus::Cancelled
        )
    });
    let message_persisted = server_thread.latest_user_message_at.is_some();
    if run_started || startup_stopped || message_persisted {
        Some(server_thread_ref.clone())
    } else {
        None
    }
}

/// Pre-thread draft routing and execution context (upstream `DraftThreadState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftThreadState {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub created_at: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

pub fn build_local_draft_thread(
    thread_id: ThreadId,
    draft_thread: &DraftThreadState,
    fallback_model_selection: ModelSelection,
) -> EnvironmentThreadShell {
    let mut shell = EnvironmentThreadShell::new(
        draft_thread.environment_id.clone(),
        thread_id,
        draft_thread.project_id.clone(),
        "New thread",
        draft_thread.created_at.clone(),
    );
    shell.model_selection = fallback_model_selection;
    shell.interaction_mode = match draft_thread.interaction_mode {
        ProviderInteractionMode::Default => InteractionMode::Default,
        ProviderInteractionMode::Plan => InteractionMode::Plan,
    };
    shell.branch = draft_thread.branch.clone();
    shell.worktree_path = draft_thread.worktree_path.clone();
    shell
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::RunId;
    use crate::state::models::{RunStatus, fixtures};

    fn completed_turn() -> ThreadRunSummary {
        ThreadRunSummary {
            run_id: RunId::new("turn-1").unwrap(),
            status: RunStatus::Completed,
            requested_at: Some("2026-03-29T00:00:00.000Z".to_owned()),
            started_at: Some("2026-03-29T00:00:01.000Z".to_owned()),
            completed_at: Some("2026-03-29T00:00:10.000Z".to_owned()),
        }
    }

    #[test]
    fn does_not_dock_the_composer_before_a_background_submission() {
        assert!(!should_dock_draft_hero_for_submission(
            ShouldDockDraftHeroInput {
                is_draft_hero_state: true,
                active_thread_key: Some("environment-local:thread-1"),
                submission_intent: ComposerSubmissionIntent::Background,
            }
        ));
    }

    #[test]
    fn draft_hero_leaves_layout_while_a_worktree_setup_card_is_on_the_timeline() {
        let base = DraftHeroInput {
            is_local_draft_thread: true,
            has_timeline_entries: false,
            is_working: false,
            draft_hero_dock_requested: false,
            background_submission_pending: false,
            has_worktree_setup_card: true,
        };
        assert!(!resolve_draft_hero_state(base));
        assert!(!resolve_draft_hero_state(DraftHeroInput {
            background_submission_pending: true,
            ..base
        }));
    }

    #[test]
    fn draft_hero_keeps_composer_layout_until_navigation_after_server_promotion() {
        assert!(resolve_draft_hero_state(DraftHeroInput {
            is_local_draft_thread: false,
            has_timeline_entries: true,
            is_working: true,
            draft_hero_dock_requested: false,
            background_submission_pending: true,
            has_worktree_setup_card: false,
        }));
    }

    #[test]
    fn draft_promotion_respects_background_submission_and_server_state() {
        let server_ref = ScopedThreadRef {
            environment_id: fixtures::env("environment-local"),
            thread_id: fixtures::thread_id("thread-1"),
        };
        let turn = completed_turn();
        assert_eq!(
            resolve_draft_promotion_navigation_target(
                Some(&server_ref),
                Some(DraftPromotionServerThread {
                    latest_run: Some(&turn),
                    latest_user_message_at: None,
                }),
                true,
            ),
            None
        );
        assert_eq!(
            resolve_draft_promotion_navigation_target(
                Some(&server_ref),
                Some(DraftPromotionServerThread {
                    latest_run: Some(&turn),
                    latest_user_message_at: None,
                }),
                false,
            ),
            Some(server_ref.clone())
        );
    }

    #[test]
    fn build_local_draft_thread_sets_new_thread_defaults() {
        let thread_id = fixtures::thread_id("draft-1");
        let draft = DraftThreadState {
            environment_id: fixtures::env("environment-local"),
            project_id: fixtures::project_id("project-1"),
            created_at: "2026-03-29T00:00:00.000Z".to_owned(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Plan,
            branch: Some("feature".to_owned()),
            worktree_path: None,
        };
        let shell = build_local_draft_thread(
            thread_id.clone(),
            &draft,
            ModelSelection {
                instance_id: "codex".to_owned(),
                model: "gpt-5.4".to_owned(),
            },
        );
        assert_eq!(shell.id, thread_id);
        assert_eq!(shell.title, "New thread");
        assert_eq!(shell.interaction_mode, InteractionMode::Plan);
        assert_eq!(shell.branch.as_deref(), Some("feature"));
    }
}
