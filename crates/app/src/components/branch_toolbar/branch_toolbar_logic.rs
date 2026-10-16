//! Port of upstream/apps/web/src/components/BranchToolbar.logic.ts
//!
//! Pure rules behind the composer's context strip: which workspace and
//! branch labels show, the previous-worktree seed, how a branch pick maps to
//! a checkout, and the branch picker's item filter. The three ref helpers
//! upstream re-exports from `@t3tools/shared/git` are re-exported the same way.
// A faithful port keeps upstream's whole API (rule 3); the view wires the
// rest as the palette and toolbar land.
#![allow(dead_code)]

use turin_contracts::EnvironmentId;
use turin_contracts::environment::{EnvironmentMachineKind, ThreadEnvMode, WorktreeSubmodules};
use turin_contracts::git::VcsRef;

use crate::components::sidebar::runtime::thread_sort::to_sortable_timestamp;

pub use turin_shared::git::{
    dedupe_remote_branches_with_local_matches, derive_local_branch_name_from_remote_ref,
    sanitize_new_ref_name,
};

/// `EnvMode`: where a new thread runs.
pub type EnvMode = ThreadEnvMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentOption {
    pub environment_id: EnvironmentId,
    /// `None` when the machine's "No project" folder is not created yet.
    pub project_id: Option<String>,
    pub label: String,
    pub is_primary: bool,
    pub machine: EnvironmentMachineKind,
}

const GENERIC_LOCAL_ENVIRONMENT_LABELS: [&str; 2] = ["local", "local environment"];

fn normalize_display_label(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|trimmed| !trimmed.is_empty())
}

pub struct ResolveEnvironmentOptionLabelInput<'a> {
    pub is_primary: bool,
    pub environment_id: &'a EnvironmentId,
    pub runtime_label: Option<&'a str>,
    pub saved_label: Option<&'a str>,
}

pub fn resolve_environment_option_label(input: &ResolveEnvironmentOptionLabelInput<'_>) -> String {
    let runtime_label = normalize_display_label(input.runtime_label);
    let saved_label = normalize_display_label(input.saved_label);

    if input.is_primary {
        let preferred = [runtime_label, saved_label]
            .into_iter()
            .flatten()
            .find(|label| {
                !GENERIC_LOCAL_ENVIRONMENT_LABELS.contains(&label.to_lowercase().as_str())
            });
        return preferred.unwrap_or("This device").to_owned();
    }

    runtime_label
        .or(saved_label)
        .map_or_else(|| input.environment_id.as_str().to_owned(), str::to_owned)
}

/// A remote (non-primary) environment is always surfaced, even when it is the
/// only environment available: with a single connected machine there is nothing
/// to pick, but the user still needs to see where the project runs.
pub fn should_show_environment_indicator(
    active_environment_is_primary: Option<bool>,
    can_pick_environment: bool,
) -> bool {
    if can_pick_environment {
        return true;
    }
    active_environment_is_primary.is_some_and(|is_primary| !is_primary)
}

pub struct ShouldShowComposerContextStripInput {
    pub is_draft_hero_state: bool,
    pub persist_in_active_threads: bool,
    pub has_active_project: bool,
    pub is_git_repo: bool,
    pub show_environment_indicator: bool,
    /// A collapsed composer's controls currently fit in their measured strip host.
    pub hosts_resting_composer_controls: bool,
}

pub fn should_show_composer_context_strip(input: &ShouldShowComposerContextStripInput) -> bool {
    input.has_active_project
        && (input.is_draft_hero_state || input.persist_in_active_threads)
        && (input.is_git_repo
            || input.show_environment_indicator
            || input.hosts_resting_composer_controls)
}

/// Labels collapse to icons when the strip's content no longer fits. A small
/// hysteresis on the way back out keeps the boundary from flapping.
const CONTEXT_STRIP_COMPACT_EXPAND_HYSTERESIS_PX: f32 = 16.;

pub fn resolve_context_strip_labels_compact(
    compact: bool,
    needed_width: f32,
    available_width: f32,
) -> bool {
    if compact {
        needed_width > available_width - CONTEXT_STRIP_COMPACT_EXPAND_HYSTERESIS_PX
    } else {
        needed_width > available_width
    }
}

pub fn resolve_env_mode_label(mode: EnvMode) -> &'static str {
    match mode {
        EnvMode::Worktree => "New worktree",
        EnvMode::Local => "Current checkout",
    }
}

pub fn worktree_submodules_label(mode: WorktreeSubmodules) -> &'static str {
    match mode {
        WorktreeSubmodules::Recursive => "Recursive",
        WorktreeSubmodules::TopLevel => "Top level only",
        WorktreeSubmodules::None => "Skip",
    }
}

pub fn resolve_current_workspace_label(active_worktree_path: Option<&str>) -> &'static str {
    if active_worktree_path.is_some_and(|path| !path.is_empty()) {
        "Current worktree"
    } else {
        resolve_env_mode_label(EnvMode::Local)
    }
}

/// A locked thread in worktree mode with no path is still creating its
/// worktree, so it reads as a new worktree rather than the project checkout.
pub fn resolve_locked_workspace_label(
    active_worktree_path: Option<&str>,
    effective_env_mode: EnvMode,
) -> &'static str {
    if active_worktree_path.is_some_and(|path| !path.is_empty()) {
        return "Worktree";
    }
    if effective_env_mode == EnvMode::Worktree {
        resolve_env_mode_label(EnvMode::Worktree)
    } else {
        "Local checkout"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviousWorktreeSeed {
    pub branch: Option<String>,
    pub worktree_path: String,
}

/// The thread fields `resolvePreviousWorktreeSeed` reads.
pub struct WorktreeThread<'a> {
    pub branch: Option<&'a str>,
    pub worktree_path: Option<&'a str>,
    pub updated_at: &'a str,
    pub archived_at: Option<&'a str>,
}

/// The most recently touched worktree in the project that the composer isn't
/// already pointing at. Backs the "Previous worktree" entry in the workspace
/// selector so a follow-up thread can hop back into the worktree you just
/// worked in without hunting for its branch. Archived threads don't compete —
/// the rest of the UI hides them, so their worktrees shouldn't resurface here.
pub fn resolve_previous_worktree_seed(
    threads: &[WorktreeThread<'_>],
    current_worktree_path: Option<&str>,
) -> Option<PreviousWorktreeSeed> {
    let mut latest: Option<(Option<&str>, &str, i64)> = None;
    for thread in threads {
        let Some(worktree_path) = thread.worktree_path.filter(|path| !path.is_empty()) else {
            continue;
        };
        if Some(worktree_path) == current_worktree_path || thread.archived_at.is_some() {
            continue;
        }
        let Some(updated_at) = to_sortable_timestamp(Some(thread.updated_at)) else {
            continue;
        };
        if latest.is_none_or(|(_, _, latest_updated_at)| updated_at > latest_updated_at) {
            latest = Some((thread.branch, worktree_path, updated_at));
        }
    }
    latest.map(|(branch, worktree_path, _)| PreviousWorktreeSeed {
        branch: branch.map(str::to_owned),
        worktree_path: worktree_path.to_owned(),
    })
}

pub fn resolve_previous_worktree_label(seed: &PreviousWorktreeSeed) -> String {
    match seed.branch.as_deref().filter(|branch| !branch.is_empty()) {
        Some(branch) => format!("Previous worktree ({branch})"),
        None => "Previous worktree".to_owned(),
    }
}

pub struct ResolveEffectiveEnvModeInput<'a> {
    pub active_worktree_path: Option<&'a str>,
    pub has_server_thread: bool,
    pub draft_thread_env_mode: Option<EnvMode>,
    /// The server is still creating this thread's worktree. The thread exists
    /// from the start of that setup but gets its worktree path only at the end.
    pub preparing_worktree: bool,
}

pub fn resolve_effective_env_mode(input: &ResolveEffectiveEnvModeInput<'_>) -> EnvMode {
    let has_path = input
        .active_worktree_path
        .is_some_and(|path| !path.is_empty());
    if !input.has_server_thread {
        if has_path {
            return EnvMode::Local;
        }
        return if input.draft_thread_env_mode == Some(EnvMode::Worktree) {
            EnvMode::Worktree
        } else {
            EnvMode::Local
        };
    }
    if has_path || input.preparing_worktree {
        EnvMode::Worktree
    } else {
        EnvMode::Local
    }
}

pub fn resolve_draft_env_mode_after_branch_change(
    next_worktree_path: Option<&str>,
    current_worktree_path: Option<&str>,
    effective_env_mode: EnvMode,
) -> EnvMode {
    if next_worktree_path.is_some_and(|path| !path.is_empty()) {
        return EnvMode::Worktree;
    }
    if effective_env_mode == EnvMode::Worktree
        && !current_worktree_path.is_some_and(|path| !path.is_empty())
    {
        return EnvMode::Worktree;
    }
    EnvMode::Local
}

pub fn resolve_branch_toolbar_value(
    env_mode: EnvMode,
    active_worktree_path: Option<&str>,
    active_thread_branch: Option<&str>,
    current_git_branch: Option<&str>,
) -> Option<String> {
    let has_path = active_worktree_path.is_some_and(|path| !path.is_empty());
    let chosen = if env_mode == EnvMode::Worktree && !has_path {
        active_thread_branch.or(current_git_branch)
    } else {
        current_git_branch.or(active_thread_branch)
    };
    chosen.map(str::to_owned)
}

pub struct ResolveBranchTriggerLabelInput<'a> {
    pub active_worktree_path: Option<&'a str>,
    pub effective_env_mode: EnvMode,
    pub resolved_active_branch: Option<&'a str>,
    pub resolved_active_branch_is_remote: Option<bool>,
    pub start_from_origin: bool,
}

pub fn resolve_branch_trigger_label(input: &ResolveBranchTriggerLabelInput<'_>) -> String {
    let Some(branch) = input
        .resolved_active_branch
        .filter(|branch| !branch.is_empty())
    else {
        return "Select ref".to_owned();
    };
    let has_path = input
        .active_worktree_path
        .is_some_and(|path| !path.is_empty());
    if input.effective_env_mode == EnvMode::Worktree && !has_path {
        let base_ref =
            if input.start_from_origin && input.resolved_active_branch_is_remote == Some(false) {
                format!("origin/{branch}")
            } else {
                branch.to_owned()
            };
        return format!("From {base_ref}");
    }
    branch.to_owned()
}

pub fn resolve_branch_toolbar_pr_branch<'a>(
    active_thread_branch: Option<&'a str>,
    resolved_active_branch: Option<&str>,
) -> Option<&'a str> {
    if active_thread_branch == resolved_active_branch {
        active_thread_branch
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCheckoutBranchMismatch {
    pub thread_branch: String,
    pub current_branch: String,
}

pub fn resolve_local_checkout_branch_mismatch(
    effective_env_mode: EnvMode,
    active_worktree_path: Option<&str>,
    active_thread_branch: Option<&str>,
    current_git_branch: Option<&str>,
) -> Option<LocalCheckoutBranchMismatch> {
    if effective_env_mode != EnvMode::Local || active_worktree_path.is_some() {
        return None;
    }
    let thread_branch = active_thread_branch.filter(|branch| !branch.is_empty())?;
    let current_branch = current_git_branch.filter(|branch| !branch.is_empty())?;
    if thread_branch == current_branch {
        return None;
    }
    Some(LocalCheckoutBranchMismatch {
        thread_branch: thread_branch.to_owned(),
        current_branch: current_branch.to_owned(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSelectionTarget {
    pub checkout_cwd: String,
    pub next_worktree_path: Option<String>,
    pub reuse_existing_worktree: bool,
}

/// The `VcsRef` fields a branch pick reads.
pub struct BranchSelectionRef<'a> {
    pub is_default: bool,
    pub worktree_path: Option<&'a str>,
}

impl<'a> From<&'a VcsRef> for BranchSelectionRef<'a> {
    fn from(value: &'a VcsRef) -> Self {
        Self {
            is_default: value.is_default,
            worktree_path: value.worktree_path.as_ref().map(|path| path.as_str()),
        }
    }
}

pub fn resolve_branch_selection_target(
    active_project_cwd: &str,
    active_worktree_path: Option<&str>,
    ref_name: &BranchSelectionRef<'_>,
) -> BranchSelectionTarget {
    if let Some(worktree_path) = ref_name.worktree_path.filter(|path| !path.is_empty()) {
        return BranchSelectionTarget {
            checkout_cwd: worktree_path.to_owned(),
            next_worktree_path: (worktree_path != active_project_cwd)
                .then(|| worktree_path.to_owned()),
            reuse_existing_worktree: true,
        };
    }

    let next_worktree_path = if active_worktree_path.is_some() && ref_name.is_default {
        None
    } else {
        active_worktree_path
    };

    BranchSelectionTarget {
        checkout_cwd: next_worktree_path.unwrap_or(active_project_cwd).to_owned(),
        next_worktree_path: next_worktree_path.map(str::to_owned),
        reuse_existing_worktree: false,
    }
}

pub struct ShouldIncludeBranchPickerItemInput<'a> {
    pub item_value: &'a str,
    pub normalized_query: &'a str,
    pub create_branch_item_value: Option<&'a str>,
    pub checkout_pull_request_item_value: Option<&'a str>,
}

pub fn should_include_branch_picker_item(input: &ShouldIncludeBranchPickerItemInput<'_>) -> bool {
    let item_value = input.item_value;
    let normalized_query = input.normalized_query;

    if normalized_query.is_empty() {
        return true;
    }
    if input.create_branch_item_value == Some(item_value) {
        return true;
    }
    if input.checkout_pull_request_item_value == Some(item_value) {
        return true;
    }

    let lower_item_value = item_value.to_lowercase();
    if lower_item_value.contains(normalized_query) {
        return true;
    }

    // A query containing whitespace can only ever match a ref under its sanitized
    // name, because that is the name such a ref would have been created with.
    // Without this, typing "new branch" hides an existing "new-branch".
    let sanitized_query = sanitize_new_ref_name(normalized_query);
    !sanitized_query.is_empty()
        && sanitized_query != normalized_query
        && lower_item_value.contains(&sanitized_query)
}

#[cfg(test)]
mod tests;
