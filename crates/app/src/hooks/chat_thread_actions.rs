//! Port of upstream/apps/web/src/lib/chatThreadActions.ts (+ test): the pure
//! decisions behind the chat's new-thread and thread-action entry points.
//!
//! Upstream's `ComposerThreadDraftState` lives in `composerDraftStore`, which is
//! not ported. The model-selection checks take the three fields they read
//! (`ComposerModelSelectionState<M>`), so the store can supply them when it
//! lands.
// Consumers are wired by the composer task.
#![allow(dead_code)]

use turin_contracts::environment::ScopedProjectRef;
use turin_contracts::{EnvironmentId, ProjectId};

use turin_contracts::environment::ThreadEnvMode;

/// The part of a composer draft the model-selection rules read.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerModelSelectionState<M> {
    pub active_provider: Option<String>,
    /// Keyed by provider instance id.
    pub model_selection_by_provider: Vec<(String, M)>,
    pub model_selection_explicit: bool,
}

impl<M> ComposerModelSelectionState<M> {
    fn selection_for(&self, provider: &str) -> Option<&M> {
        self.model_selection_by_provider
            .iter()
            .find(|(key, _)| key == provider)
            .map(|(_, selection)| selection)
    }
}

/// `hasExplicitComposerModelSelection`: only an active selection the user
/// picked counts as an explicit pick.
pub fn has_explicit_composer_model_selection<M>(
    draft: Option<&ComposerModelSelectionState<M>>,
) -> bool {
    let Some(draft) = draft else {
        return false;
    };
    draft.model_selection_explicit
        && draft
            .active_provider
            .as_deref()
            .is_some_and(|provider| draft.selection_for(provider).is_some())
}

/// `resolveNewDraftStartFromOrigin`: only a new worktree draft starts from
/// origin, and only when the project asks for it.
pub fn resolve_new_draft_start_from_origin(
    env_mode: ThreadEnvMode,
    new_worktrees_start_from_origin: bool,
) -> bool {
    env_mode == ThreadEnvMode::Worktree && new_worktrees_start_from_origin
}

/// `resolveNewThreadModelSelectionOverride`: the project default wins. A
/// carried selection applies only when it came from another draft, since a
/// draft must not carry its own non-explicit model back into itself.
pub fn resolve_new_thread_model_selection_override<M: Clone>(
    project_default_selection: Option<&M>,
    carry_selection: Option<&M>,
    carry_source_draft_id: Option<&str>,
    destination_draft_id: &str,
) -> Option<M> {
    project_default_selection.cloned().or_else(|| {
        if carry_source_draft_id == Some(destination_draft_id) {
            None
        } else {
            carry_selection.cloned()
        }
    })
}

/// A thread the context was opened on: the project it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadContextLike {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
}

/// `ChatThreadActionContext`, without the handler (see `start_new_thread_from_context`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatThreadActionContext {
    pub active_draft_thread: Option<ThreadContextLike>,
    pub active_thread: Option<ThreadContextLike>,
    pub default_project_ref: Option<ScopedProjectRef>,
}

fn project_ref(thread: &ThreadContextLike) -> ScopedProjectRef {
    ScopedProjectRef {
        environment_id: thread.environment_id.clone(),
        project_id: thread.project_id.clone(),
    }
}

/// `resolveThreadActionProjectRef`: the active thread's project, then the
/// active draft's, then the default.
pub fn resolve_thread_action_project_ref(
    context: &ChatThreadActionContext,
) -> Option<ScopedProjectRef> {
    if let Some(thread) = &context.active_thread {
        return Some(project_ref(thread));
    }
    if let Some(draft) = &context.active_draft_thread {
        return Some(project_ref(draft));
    }
    context.default_project_ref.clone()
}

/// `startNewThreadFromContext`: a new thread inherits only the project from
/// the context. Branch, worktree, and env mode come from the configured
/// defaults. Explicit affordances pass those to the handler directly.
///
/// Returns whether a thread was started. A handler error propagates, as upstream's rejection does.
pub fn start_new_thread_from_context<E>(
    context: &ChatThreadActionContext,
    handle_new_thread: impl FnOnce(&ScopedProjectRef) -> Result<(), E>,
) -> Result<bool, E> {
    let Some(project) = resolve_thread_action_project_ref(context) else {
        return Ok(false);
    };
    handle_new_thread(&project)?;
    Ok(true)
}

#[cfg(test)]
#[path = "chat_thread_actions_tests.rs"]
mod tests;
