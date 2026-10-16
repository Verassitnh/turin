//! Port of the pure decisions in upstream/apps/web/src/hooks/useHandleNewThread.ts
//! (+ its test file, which is store-driven and not ported here).
//!
//! The draft store (`composerDraftStore`), the router, and the awaited project
//! file read are not ported. What stays here is every decision the handler
//! makes from a snapshot of them:
//!
//! - which stored draft is reusable, and whether it is empty or invested;
//! - the workspace context a reused draft takes (explicit options win, an
//!   open draft keeps its own, otherwise configured defaults), and the
//!   target environment's `runtimeMode`, written only when that draft is
//!   not already open;
//! - the snapshot the handler compares before and after each await, so a
//!   stale request bails out instead of clobbering a winner's writes;
//! - the mint-fresh draft fields, including that same `runtimeMode`, and
//!   the carry-over precedence (composer state, then the viewed thread,
//!   then the viewed draft).
//!
//! The async handler that writes the store and navigates is the composer
//! task's. It calls these with the facts it reads.
// Consumers are wired by the composer task.
#![allow(dead_code)]

use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::provider_policy::RuntimeMode;

use crate::hooks::chat_thread_actions::{
    ComposerModelSelectionState, resolve_new_draft_start_from_origin,
};

/// `NewThreadWorkspaceOptions` and the handler's own options. `None` is an
/// absent key (`!== undefined`). `Some(None)` is an explicit `null`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewThreadOptions {
    pub branch: Option<Option<String>>,
    pub worktree_path: Option<Option<String>>,
    pub env_mode: Option<ThreadEnvMode>,
    pub start_from_origin: Option<bool>,
    /// `replace`: navigate without a history entry.
    pub replace: bool,
}

/// The workspace options the caller passed, with absent keys still absent, so
/// they never overwrite existing draft state (`pickExplicitWorkspaceOptions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceOptions {
    pub branch: Option<Option<String>>,
    pub worktree_path: Option<Option<String>>,
    pub env_mode: Option<ThreadEnvMode>,
    pub start_from_origin: Option<bool>,
}

/// `pickExplicitWorkspaceOptions`.
pub fn pick_explicit_workspace_options(options: &NewThreadOptions) -> WorkspaceOptions {
    WorkspaceOptions {
        branch: options.branch.clone(),
        worktree_path: options.worktree_path.clone(),
        env_mode: options.env_mode,
        start_from_origin: options.start_from_origin,
    }
}

/// Whether the caller passed any workspace option (`hasExplicitWorkspaceOption`).
pub fn has_explicit_workspace_option(options: &NewThreadOptions) -> bool {
    options.branch.is_some()
        || options.worktree_path.is_some()
        || options.env_mode.is_some()
        || options.start_from_origin.is_some()
}

/// What the handler finds in the store for this project's stored draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredDraftFacts {
    /// `promotedTo != null`: the draft already became a thread.
    pub promoted: bool,
    /// The draft's thread exists in the thread store.
    pub thread_exists: bool,
    /// The draft has typed text or attachments.
    pub has_user_content: bool,
}

/// How the handler treats the stored draft for this project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredDraftDecision {
    /// Nothing is stored for the project.
    None,
    /// Stored, but not reusable. Upstream marks it promoted, so it is not
    /// offered again, and mints a fresh draft.
    Promote,
    /// Reusable but invested. It stays where it is and a fresh draft is minted.
    Invested,
    /// Reusable and empty. The handler resurrects it.
    ReuseEmpty,
}

/// `reusableStoredDraftThread` and `emptyStoredDraftThread`. New-thread
/// surfaces only reuse a draft the user has not invested in.
pub fn classify_stored_draft(stored: Option<StoredDraftFacts>) -> StoredDraftDecision {
    let Some(stored) = stored else {
        return StoredDraftDecision::None;
    };
    if stored.promoted || stored.thread_exists {
        return StoredDraftDecision::Promote;
    }
    if stored.has_user_content {
        return StoredDraftDecision::Invested;
    }
    StoredDraftDecision::ReuseEmpty
}

/// The workspace a reused empty draft takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReusedDraftWorkspace {
    /// The caller passed workspace options; they win outright.
    Explicit(WorkspaceOptions),
    /// The draft is already open and no options were passed. Leave its
    /// context alone, since the user may have just picked a branch.
    KeepOpenDraft,
    /// Reset to the configured defaults: no branch or worktree, the project's
    /// default env mode, and start-from-origin as the mode decides.
    Defaults {
        env_mode: ThreadEnvMode,
        start_from_origin: bool,
    },
}

/// Workspace plus `runtimeMode` for a reused empty draft.
///
/// Upstream writes `projectSettings.settings.defaultRuntimeMode` only when
/// `!isDraftAlreadyOpen`, including when explicit workspace options are
/// passed. An open draft omits the key, so it keeps the mode it already has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReusedDraftDecision {
    pub workspace: ReusedDraftWorkspace,
    pub runtime_mode: Option<RuntimeMode>,
}

/// The workspace decision for a reused empty draft. `default_env_mode` is the
/// project's resolved env mode, which the caller reads (the project file
/// read is async). `default_runtime_mode` is
/// `projectSettings.settings.defaultRuntimeMode`.
pub fn reused_draft_workspace(
    options: &NewThreadOptions,
    is_draft_already_open: bool,
    default_env_mode: ThreadEnvMode,
    new_worktrees_start_from_origin: bool,
    default_runtime_mode: RuntimeMode,
) -> ReusedDraftDecision {
    let runtime_mode = (!is_draft_already_open).then_some(default_runtime_mode);
    let workspace = if has_explicit_workspace_option(options) {
        ReusedDraftWorkspace::Explicit(pick_explicit_workspace_options(options))
    } else if is_draft_already_open {
        ReusedDraftWorkspace::KeepOpenDraft
    } else {
        ReusedDraftWorkspace::Defaults {
            env_mode: default_env_mode,
            start_from_origin: resolve_new_draft_start_from_origin(
                default_env_mode,
                new_worktrees_start_from_origin,
            ),
        }
    };
    ReusedDraftDecision {
        workspace,
        runtime_mode,
    }
}

/// The store facts the handler re-reads after each await. A change in any of
/// them means another invocation's writes have landed, so this one is a stale
/// loser. Resetting context, remapping, or navigating would clobber them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReuseRaceSnapshot {
    /// The route now shows this draft, so a concurrent invocation opened it.
    pub opened_meanwhile: bool,
    /// The stored draft was promoted to a thread.
    pub promoted_meanwhile: bool,
    /// The project now maps to a different draft.
    pub remapped_meanwhile: bool,
    /// The draft gained user content and is no longer reusable.
    pub invested_meanwhile: bool,
}

/// True when any fact moved while the handler awaited, so it must bail out.
pub fn lost_reuse_race(snapshot: ReuseRaceSnapshot) -> bool {
    snapshot.opened_meanwhile
        || snapshot.promoted_meanwhile
        || snapshot.remapped_meanwhile
        || snapshot.invested_meanwhile
}

/// `abandonedSinceRequest`: the route moved, or the project was removed while
/// the request was in flight. The project was known when the request started.
pub fn abandoned_since_request(
    route_changed_since_request: bool,
    project_known_at_request: bool,
    project_removed_now: bool,
) -> bool {
    route_changed_since_request || (project_known_at_request && project_removed_now)
}

/// `initialEnvMode`: the caller's env mode, else the resolved default.
pub fn initial_env_mode(
    options: &NewThreadOptions,
    resolve_default: impl FnOnce() -> ThreadEnvMode,
) -> ThreadEnvMode {
    options.env_mode.unwrap_or_else(resolve_default)
}

/// The fields a freshly minted draft starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDraftFields {
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub env_mode: ThreadEnvMode,
    pub start_from_origin: bool,
    /// `projectSettings.settings.defaultRuntimeMode`. Options never override it.
    pub runtime_mode: RuntimeMode,
}

/// The mint-fresh draft's fields: explicit options, with the start-from-origin
/// default when the caller did not pass one (`options?.startFromOrigin ?? ...`).
/// `runtime_mode` is always the target environment's default.
pub fn new_draft_fields(
    options: &NewThreadOptions,
    initial_env_mode: ThreadEnvMode,
    new_worktrees_start_from_origin: bool,
    default_runtime_mode: RuntimeMode,
) -> NewDraftFields {
    NewDraftFields {
        branch: options.branch.clone().flatten(),
        worktree_path: options.worktree_path.clone().flatten(),
        env_mode: initial_env_mode,
        start_from_origin: options.start_from_origin.unwrap_or_else(|| {
            resolve_new_draft_start_from_origin(initial_env_mode, new_worktrees_start_from_origin)
        }),
        runtime_mode: default_runtime_mode,
    }
}

/// The composer's selection for its active provider (`composerModelSelection`).
pub fn composer_active_model_selection<M: Clone>(
    draft: Option<&ComposerModelSelectionState<M>>,
) -> Option<M> {
    let draft = draft?;
    let provider = draft.active_provider.as_deref()?;
    draft
        .model_selection_by_provider
        .iter()
        .find(|(key, _)| key == provider)
        .map(|(_, selection)| selection.clone())
}

/// `carryModelSelection`: the composer's selection wins over the viewed
/// thread's stored one.
pub fn carry_model_selection<M>(composer: Option<M>, viewed_thread: Option<M>) -> Option<M> {
    composer.or(viewed_thread)
}

/// `carryInteractionMode`: composer, then the viewed thread, then the viewed
/// draft. Each is read on its own, so an unset composer value falls through.
pub fn carry_interaction_mode<T>(
    composer: Option<T>,
    viewed_thread: Option<T>,
    viewed_draft: Option<T>,
) -> Option<T> {
    composer.or(viewed_thread).or(viewed_draft)
}

#[cfg(test)]
#[path = "use_handle_new_thread_tests.rs"]
mod tests;
