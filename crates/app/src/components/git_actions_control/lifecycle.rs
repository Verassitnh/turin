//! The lifecycle rules of upstream/apps/web/src/components/GitActionsControl.tsx
//! as pure functions, covering GitActionsControl.lifecycle.test.ts and the
//! behaviours of GitActionsControl.test.ts that survive the auth-scope cut:
//!
//! - an action is *planned* against the target it was requested for: the
//!   default-ref confirmation, the file selection and the feature-branch flag
//!   all travel with it;
//! - the result's toast carries that same target, so a retained "Push &
//!   create PR" button runs in the thread that produced it even after the
//!   panel moved on to another thread;
//! - a created feature branch is persisted to the thread (a server thread's
//!   metadata, with the branch it expected to replace; a draft's context).

use turin_contracts::ThreadId;
use turin_contracts::git::{
    GitRunStackedActionResult, GitRunStackedActionToastCta, GitStackedAction, VcsStatusResult,
};
use turin_review::action_state::git_actions_control_logic::{
    DefaultBranchConfirmableAction, GIT_ACTION_SUCCESS_VISIBLE_MS, GitActionMenuItem,
    GitActionMenuItemId, GitActionToastType, change_request_terminology,
    requires_default_branch_confirmation, resolve_git_action_result_toast_timing,
    resolve_live_thread_branch_update, resolve_thread_branch_update,
};

use super::backend::{GitActionTarget, GitThreadContext, RunStackedActionInput};

/// `RunGitActionWithToastInput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunGitActionInput {
    pub action: GitStackedAction,
    pub commit_message: Option<String>,
    pub skip_default_branch_prompt: bool,
    pub feature_branch: bool,
    pub file_paths: Option<Vec<String>>,
}

impl RunGitActionInput {
    pub fn new(action: GitStackedAction) -> Self {
        Self {
            action,
            commit_message: None,
            skip_default_branch_prompt: false,
            feature_branch: false,
            file_paths: None,
        }
    }
}

/// `PendingDefaultBranchAction`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingDefaultBranchAction {
    pub action: DefaultBranchConfirmableAction,
    pub branch_name: String,
    pub includes_commit: bool,
    pub commit_message: Option<String>,
    pub file_paths: Option<Vec<String>>,
}

/// What requesting an action does first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitActionPlan {
    /// Nothing to run (the branch is unknown and a prompt would be empty).
    Skip,
    /// Ask before running on the default ref.
    ConfirmDefaultBranch(PendingDefaultBranchAction),
    Run(RunStackedActionInput),
}

fn confirmable(action: GitStackedAction) -> Option<DefaultBranchConfirmableAction> {
    match action {
        GitStackedAction::Push => Some(DefaultBranchConfirmableAction::Push),
        GitStackedAction::CreatePr => Some(DefaultBranchConfirmableAction::CreatePr),
        GitStackedAction::CommitPush => Some(DefaultBranchConfirmableAction::CommitPush),
        GitStackedAction::CommitPushPr => Some(DefaultBranchConfirmableAction::CommitPushPr),
        GitStackedAction::Commit => None,
    }
}

/// The head of `runGitActionWithToast`: confirm on the default ref, else the
/// request to run.
pub fn plan_git_action(
    input: &RunGitActionInput,
    status: Option<&VcsStatusResult>,
    is_default_ref: bool,
    thread: &GitThreadContext,
    thread_id: &ThreadId,
    action_id: String,
) -> GitActionPlan {
    let action_branch = status.and_then(|status| status.ref_name.as_ref()).map(|name| name.as_str().to_owned());
    let action_is_default_branch = if input.feature_branch { false } else { is_default_ref };
    let action_can_commit = matches!(
        input.action,
        GitStackedAction::Commit | GitStackedAction::CommitPush | GitStackedAction::CommitPushPr
    );
    let includes_commit = action_can_commit
        && (input.action == GitStackedAction::Commit
            || status.is_some_and(|status| status.has_working_tree_changes)
            || input.feature_branch);

    if !input.skip_default_branch_prompt
        && requires_default_branch_confirmation(input.action, action_is_default_branch)
        && let Some(branch_name) = action_branch
    {
        let Some(action) = confirmable(input.action) else {
            return GitActionPlan::Skip;
        };
        return GitActionPlan::ConfirmDefaultBranch(PendingDefaultBranchAction {
            action,
            branch_name,
            includes_commit,
            commit_message: input.commit_message.clone(),
            file_paths: input.file_paths.clone(),
        });
    }

    GitActionPlan::Run(RunStackedActionInput {
        action_id,
        action: input.action,
        commit_message: input.commit_message.clone(),
        feature_branch: input.feature_branch,
        file_paths: input.file_paths.clone(),
        // A pull request the action opens is linked to the thread it ran
        // beside. Drafts have no server thread yet, so there is nothing to link to.
        thread_id: thread.is_server_thread.then(|| thread_id.clone()),
        project_id: (!thread.is_server_thread).then(|| thread.project_id.clone()).flatten(),
    })
}

/// A pending confirmation, continued: the same action without the prompt.
pub fn continue_pending_action(pending: &PendingDefaultBranchAction, feature_branch: bool) -> RunGitActionInput {
    RunGitActionInput {
        action: match pending.action {
            DefaultBranchConfirmableAction::Push => GitStackedAction::Push,
            DefaultBranchConfirmableAction::CreatePr => GitStackedAction::CreatePr,
            DefaultBranchConfirmableAction::CommitPush => GitStackedAction::CommitPush,
            DefaultBranchConfirmableAction::CommitPushPr => GitStackedAction::CommitPushPr,
        },
        commit_message: pending.commit_message.clone(),
        skip_default_branch_prompt: true,
        feature_branch,
        file_paths: pending.file_paths.clone(),
    }
}

/// The button a result toast offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultToastCta {
    None,
    /// Runs `action` in `target`: the thread that produced the toast.
    RunAction {
        label: String,
        target: GitActionTarget,
        action: GitStackedAction,
    },
    OpenPr {
        label: String,
        url: String,
    },
}

/// How a finished action is presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionPresentation {
    /// The panel shows the result inline for [`GIT_ACTION_SUCCESS_VISIBLE_MS`].
    Inline {
        title: String,
        description: Option<String>,
    },
    Toast {
        title: String,
        description: Option<String>,
        cta: ResultToastCta,
        dismiss_after_visible_ms: Option<u64>,
    },
}

pub fn present_action_result(
    result: &GitRunStackedActionResult,
    target: &GitActionTarget,
    is_panel: bool,
) -> ActionPresentation {
    let title = result.toast.title.as_str().to_owned();
    let description = result
        .toast
        .description
        .as_ref()
        .map(|description| description.as_str().to_owned());
    if is_panel {
        return ActionPresentation::Inline { title, description };
    }
    let cta = match &result.toast.cta {
        GitRunStackedActionToastCta::None => ResultToastCta::None,
        GitRunStackedActionToastCta::RunAction { label, action } => ResultToastCta::RunAction {
            label: label.as_str().to_owned(),
            target: target.clone(),
            action: action.kind,
        },
        GitRunStackedActionToastCta::OpenPr { label, url } => ResultToastCta::OpenPr {
            label: label.as_str().to_owned(),
            url: url.clone(),
        },
    };
    ActionPresentation::Toast {
        title,
        description,
        cta,
        dismiss_after_visible_ms: resolve_git_action_result_toast_timing(GitActionToastType::Success)
            .dismiss_after_visible_ms,
    }
}

/// `persistThreadBranchSync`'s decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchSync {
    None,
    /// `threadEnvironment.updateMetadata` with the branch it expected to replace.
    UpdateServerThread {
        branch: Option<String>,
        expected_branch: Option<String>,
    },
    /// `setDraftThreadContext`; `manual` is `environmentSelection: "manual"`.
    SetDraftBranch { branch: Option<String>, manual: bool },
}

pub fn resolve_branch_sync(
    thread: &GitThreadContext,
    is_draft: bool,
    draft_branch: Option<&str>,
    branch: Option<&str>,
    manual_selection: bool,
) -> BranchSync {
    if thread.is_server_thread {
        if thread.branch.as_deref() == branch {
            return BranchSync::None;
        }
        return BranchSync::UpdateServerThread {
            branch: branch.map(str::to_owned),
            expected_branch: thread.branch.clone(),
        };
    }
    if !is_draft || draft_branch == branch {
        return BranchSync::None;
    }
    BranchSync::SetDraftBranch {
        branch: branch.map(str::to_owned),
        manual: manual_selection,
    }
}

/// `syncThreadBranchAfterGitAction`: a branch the action created becomes the
/// thread's branch, by manual selection.
pub fn sync_after_git_action(
    result: &GitRunStackedActionResult,
    thread: &GitThreadContext,
    is_draft: bool,
    draft_branch: Option<&str>,
) -> BranchSync {
    match resolve_thread_branch_update(result) {
        None => BranchSync::None,
        Some(branch) => resolve_branch_sync(thread, is_draft, draft_branch, Some(&branch), true),
    }
}

/// The draft reconciliation effect: follow the live checkout while no action
/// runs, no worktree base is being chosen and the thread has not launched.
pub fn live_draft_branch_sync(
    thread: &GitThreadContext,
    draft_branch: Option<&str>,
    status: Option<&VcsStatusResult>,
    is_action_running: bool,
    is_selecting_worktree_base: bool,
) -> BranchSync {
    if is_action_running || is_selecting_worktree_base || thread.is_server_thread {
        return BranchSync::None;
    }
    let Some(update) = resolve_live_thread_branch_update(draft_branch, status) else {
        return BranchSync::None;
    };
    resolve_branch_sync(thread, true, draft_branch, update.branch.as_deref(), false)
}

/// `getMenuActionDisabledReason`: why a disabled menu item is disabled.
pub fn menu_action_disabled_reason(
    item: &GitActionMenuItem,
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    has_primary_remote: bool,
) -> Option<String> {
    if !item.disabled {
        return None;
    }
    if is_busy {
        return Some("Git action in progress.".to_owned());
    }
    let Some(git_status) = git_status else {
        return Some("Git status is unavailable.".to_owned());
    };

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let is_ahead = git_status.ahead_count > 0;
    let is_behind = git_status.behind_count > 0;
    let terminology = change_request_terminology(
        git_status
            .source_control_provider
            .as_ref()
            .map(|provider| provider.kind.as_str()),
    );

    if item.id == GitActionMenuItemId::Commit {
        return Some(if !has_changes {
            "Worktree is clean. Make changes before committing.".to_owned()
        } else {
            "Commit is currently unavailable.".to_owned()
        });
    }

    if item.id == GitActionMenuItemId::Push {
        let reason = if !has_branch {
            "Detached HEAD: check out a branch before pushing."
        } else if has_changes {
            "Commit or stash local changes before pushing."
        } else if is_behind {
            "Branch is behind upstream. Pull/rebase before pushing."
        } else if !git_status.has_upstream && !has_primary_remote {
            "Add an \"origin\" remote before pushing."
        } else if !is_ahead {
            "No local commits to push."
        } else {
            "Push is currently unavailable."
        };
        return Some(reason.to_owned());
    }

    let singular = terminology.singular;
    Some(if !has_branch {
        format!("Detached HEAD: check out a branch before creating a {singular}.")
    } else if !git_status.has_upstream && !has_primary_remote {
        format!("Add an \"origin\" remote before creating a {singular}.")
    } else if git_status
        .ahead_of_default_count
        .unwrap_or(git_status.ahead_count)
        == 0
    {
        format!("No local commits to include in a {singular}.")
    } else if is_behind {
        format!("Branch is behind upstream. Pull/rebase before creating a {singular}.")
    } else {
        format!("Create {singular} is currently unavailable.")
    })
}

/// How long the inline success stays visible (`GIT_ACTION_SUCCESS_VISIBLE_MS`).
pub const INLINE_SUCCESS_VISIBLE_MS: u64 = GIT_ACTION_SUCCESS_VISIBLE_MS;

#[cfg(test)]
mod tests;
