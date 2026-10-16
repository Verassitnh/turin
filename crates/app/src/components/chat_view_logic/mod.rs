//! Port of the pure functions in upstream/apps/web/src/components/ChatView.logic.ts.
//!
//! Batch 1 (this file): interaction and environment modes, branch mismatch banner,
//! plan follow-up gate, session-scoped dismissal. Batch 2 (submodules): timeline holds,
//! local dispatch, composer provider selection, worktree setup, and the remaining
//! ChatView.logic helpers listed in docs/handoff/U3c.md.
//!
//! Upstream keeps the dismissed-mismatch keys in a module-level `Set`. A process-wide
//! `Mutex<HashSet>` is the same thing here, and it survives view remounts the same way.

#![allow(unused_imports)]

mod anchor;
mod artifact;
mod checkout_memory;
mod composer;
mod dispatch;
mod draft;
mod metadata;
mod model_change;
mod plan_follow_up;
mod preview_urls;
mod provider_instances;
mod refocus;
mod revert;
mod terminal_send;
mod thread;
mod timeline_hold;
mod worktree;

#[cfg(test)]
mod test_fixtures;

#[cfg(test)]
mod composer_logic_matrix;

pub use anchor::*;
pub use artifact::*;
pub use checkout_memory::*;
pub use composer::*;
pub use dispatch::*;
pub use draft::*;
pub use metadata::*;
pub use model_change::*;
pub use plan_follow_up::*;
pub use preview_urls::*;
pub use provider_instances::*;
pub use refocus::*;
pub use revert::*;
pub use terminal_send::*;
pub use thread::*;
pub use timeline_hold::*;
pub use worktree::*;

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::provider_policy::ProviderInteractionMode;

/// The interaction mode a send uses. Legacy plan mode off forces build mode; otherwise
/// the composer's mode wins over the thread's, and both fall back to build mode.
pub fn resolve_effective_interaction_mode(
    plan_mode_enabled: bool,
    composer_interaction_mode: Option<ProviderInteractionMode>,
    thread_interaction_mode: Option<ProviderInteractionMode>,
) -> ProviderInteractionMode {
    if !plan_mode_enabled {
        return ProviderInteractionMode::Default;
    }
    composer_interaction_mode
        .or(thread_interaction_mode)
        .unwrap_or(ProviderInteractionMode::Default)
}

/// Worktree mode needs a git repository; anything else sends local.
pub fn resolve_send_env_mode(
    requested_env_mode: ThreadEnvMode,
    is_git_repo: bool,
) -> ThreadEnvMode {
    if is_git_repo {
        requested_env_mode
    } else {
        ThreadEnvMode::Local
    }
}

/// The workspace options for a draft that a background submission launches. The
/// worktree path is always empty, so the launch never reuses another worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundDraftWorkspaceOptions {
    pub env_mode: ThreadEnvMode,
    pub branch: Option<String>,
    pub start_from_origin: bool,
}

pub fn resolve_background_draft_workspace_options(
    env_mode: ThreadEnvMode,
    branch: Option<String>,
    start_from_origin: bool,
) -> BackgroundDraftWorkspaceOptions {
    BackgroundDraftWorkspaceOptions {
        env_mode,
        branch,
        start_from_origin: env_mode == ThreadEnvMode::Worktree && start_from_origin,
    }
}

/// The branches a thread was created on and the checkout now sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchMismatch<'a> {
    pub thread_branch: &'a str,
    pub current_branch: &'a str,
}

/// The identity of one mismatch, so a dismissal applies to that mismatch only.
pub fn branch_mismatch_key(
    thread_id: Option<&str>,
    mismatch: Option<BranchMismatch<'_>>,
) -> Option<String> {
    let (thread_id, mismatch) = (thread_id?, mismatch?);
    Some(format!(
        "{thread_id}:{}:{}",
        mismatch.thread_branch, mismatch.current_branch
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchMismatchBannerInput {
    pub has_mismatch: bool,
    pub is_dismissed: bool,
    pub composer_has_content: bool,
    pub was_shown_for_current_mismatch: bool,
}

/// The mismatch banner only matters when the user is about to send: passive reading of
/// an old thread carries no risk. Draft content is the intent signal, since the
/// composer autofocuses on every thread open. Once shown, the banner stays mounted so
/// it does not flicker away when the draft is cleared.
pub fn should_show_branch_mismatch_banner(input: BranchMismatchBannerInput) -> bool {
    if !input.has_mismatch || input.is_dismissed {
        return false;
    }
    input.composer_has_content || input.was_shown_for_current_mismatch
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanFollowUpInput {
    pub pending_user_input_count: usize,
    pub interaction_mode: ProviderInteractionMode,
    pub latest_turn_settled: bool,
    pub has_actionable_proposed_plan: bool,
    pub has_composer_attachments: bool,
}

pub fn should_show_plan_follow_up_prompt(input: PlanFollowUpInput) -> bool {
    input.pending_user_input_count == 0
        && input.interaction_mode == ProviderInteractionMode::Plan
        && input.latest_turn_settled
        && input.has_actionable_proposed_plan
        && !input.has_composer_attachments
}

/// Session-scoped: survives ChatView remounts (route changes). Durable cross-device
/// dismissal is planned as a server-side ack.
static DISMISSED_BRANCH_MISMATCH_KEYS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn dismiss_branch_mismatch_for_session(key: &str) {
    DISMISSED_BRANCH_MISMATCH_KEYS
        .lock()
        .expect("dismissed branch mismatch keys lock")
        .insert(key.to_owned());
}

pub fn is_branch_mismatch_dismissed_for_session(key: Option<&str>) -> bool {
    key.is_some_and(|key| {
        DISMISSED_BRANCH_MISMATCH_KEYS
            .lock()
            .expect("dismissed branch mismatch keys lock")
            .contains(key)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_build_mode_when_legacy_plan_mode_is_disabled() {
        assert_eq!(
            resolve_effective_interaction_mode(
                false,
                Some(ProviderInteractionMode::Plan),
                Some(ProviderInteractionMode::Plan),
            ),
            ProviderInteractionMode::Default
        );
    }

    #[test]
    fn uses_the_saved_mode_while_legacy_plan_mode_is_enabled() {
        assert_eq!(
            resolve_effective_interaction_mode(true, None, Some(ProviderInteractionMode::Plan)),
            ProviderInteractionMode::Plan
        );
        assert_eq!(
            resolve_effective_interaction_mode(true, None, None),
            ProviderInteractionMode::Default
        );
    }

    #[test]
    fn keeps_worktree_mode_only_for_git_repositories() {
        assert_eq!(
            resolve_send_env_mode(ThreadEnvMode::Worktree, true),
            ThreadEnvMode::Worktree
        );
        assert_eq!(
            resolve_send_env_mode(ThreadEnvMode::Worktree, false),
            ThreadEnvMode::Local
        );
    }

    #[test]
    fn keeps_new_worktree_selected_without_reusing_the_launched_worktree() {
        assert_eq!(
            resolve_background_draft_workspace_options(
                ThreadEnvMode::Worktree,
                Some("main".to_owned()),
                true,
            ),
            BackgroundDraftWorkspaceOptions {
                env_mode: ThreadEnvMode::Worktree,
                branch: Some("main".to_owned()),
                start_from_origin: true,
            }
        );
        // Origin only applies to a worktree; a local draft never fetches it.
        assert!(
            !resolve_background_draft_workspace_options(ThreadEnvMode::Local, None, true)
                .start_from_origin
        );
    }

    #[test]
    fn builds_a_key_from_thread_id_and_both_branches() {
        assert_eq!(
            branch_mismatch_key(
                Some("thread-1"),
                Some(BranchMismatch {
                    thread_branch: "feat/a",
                    current_branch: "feat/b",
                }),
            )
            .as_deref(),
            Some("thread-1:feat/a:feat/b")
        );
    }

    #[test]
    fn returns_no_key_without_a_thread_or_mismatch() {
        let mismatch = BranchMismatch {
            thread_branch: "a",
            current_branch: "b",
        };
        assert_eq!(branch_mismatch_key(None, Some(mismatch)), None);
        assert_eq!(branch_mismatch_key(Some("thread-1"), None), None);
    }

    fn banner_base() -> BranchMismatchBannerInput {
        BranchMismatchBannerInput {
            has_mismatch: true,
            is_dismissed: false,
            composer_has_content: false,
            was_shown_for_current_mismatch: false,
        }
    }

    #[test]
    fn banner_stays_hidden_during_passive_browsing() {
        assert!(!should_show_branch_mismatch_banner(banner_base()));
    }

    #[test]
    fn banner_shows_once_the_composer_has_draft_content() {
        assert!(should_show_branch_mismatch_banner(
            BranchMismatchBannerInput {
                composer_has_content: true,
                ..banner_base()
            }
        ));
    }

    #[test]
    fn banner_stays_mounted_after_the_draft_clears_once_shown_for_the_current_mismatch() {
        assert!(should_show_branch_mismatch_banner(
            BranchMismatchBannerInput {
                was_shown_for_current_mismatch: true,
                ..banner_base()
            }
        ));
    }

    #[test]
    fn banner_never_shows_when_dismissed_or_without_a_mismatch() {
        assert!(!should_show_branch_mismatch_banner(
            BranchMismatchBannerInput {
                composer_has_content: true,
                is_dismissed: true,
                ..banner_base()
            }
        ));
        assert!(!should_show_branch_mismatch_banner(
            BranchMismatchBannerInput {
                composer_has_content: true,
                has_mismatch: false,
                ..banner_base()
            }
        ));
    }

    fn plan_base() -> PlanFollowUpInput {
        PlanFollowUpInput {
            pending_user_input_count: 0,
            interaction_mode: ProviderInteractionMode::Plan,
            latest_turn_settled: true,
            has_actionable_proposed_plan: true,
            has_composer_attachments: false,
        }
    }

    #[test]
    fn plan_actions_show_for_a_settled_actionable_plan_without_attachments() {
        assert!(should_show_plan_follow_up_prompt(plan_base()));
    }

    #[test]
    fn plan_actions_hide_while_the_composer_has_staged_attachments() {
        assert!(!should_show_plan_follow_up_prompt(PlanFollowUpInput {
            has_composer_attachments: true,
            ..plan_base()
        }));
    }

    #[test]
    fn plan_follow_up_keeps_its_gates() {
        assert!(!should_show_plan_follow_up_prompt(PlanFollowUpInput {
            pending_user_input_count: 1,
            ..plan_base()
        }));
        assert!(!should_show_plan_follow_up_prompt(PlanFollowUpInput {
            interaction_mode: ProviderInteractionMode::Default,
            ..plan_base()
        }));
        assert!(!should_show_plan_follow_up_prompt(PlanFollowUpInput {
            latest_turn_settled: false,
            ..plan_base()
        }));
        assert!(!should_show_plan_follow_up_prompt(PlanFollowUpInput {
            has_actionable_proposed_plan: false,
            ..plan_base()
        }));
    }

    #[test]
    fn session_dismissal_tracks_dismissed_keys_and_treats_other_keys_as_active() {
        assert!(!is_branch_mismatch_dismissed_for_session(Some("t1:a:b")));
        dismiss_branch_mismatch_for_session("t1:a:b");
        assert!(is_branch_mismatch_dismissed_for_session(Some("t1:a:b")));
        assert!(!is_branch_mismatch_dismissed_for_session(Some("t1:a:c")));
        assert!(!is_branch_mismatch_dismissed_for_session(None));
    }
}
