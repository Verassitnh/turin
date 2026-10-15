//! Port of upstream/packages/client-runtime/src/state/gitActions.ts

use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::git::{GitFilePath, GitPullRequestState, GitStackedAction, VcsStatusResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitActionIconName {
    Commit,
    Push,
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitDialogAction {
    Commit,
    Push,
    CreatePr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitActionMenuItemId {
    Commit,
    Push,
    Pr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitActionMenuItemKind {
    OpenDialog,
    OpenPr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitActionMenuItem {
    pub id: GitActionMenuItemId,
    pub label: &'static str,
    pub disabled: bool,
    pub icon: GitActionIconName,
    pub kind: GitActionMenuItemKind,
    pub dialog_action: Option<GitDialogAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitQuickActionKind {
    RunAction,
    RunPull,
    OpenPr,
    ShowHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitQuickAction {
    pub label: &'static str,
    pub disabled: bool,
    pub kind: GitQuickActionKind,
    pub action: Option<GitStackedAction>,
    pub hint: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBranchActionDialogCopy {
    pub title: &'static str,
    pub description: String,
    pub continue_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultBranchConfirmableAction {
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
}

/// `Pick<GitRunStackedActionInput, "action" | "commitMessage" | "featureBranch" | "filePaths">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitActionRequestInput {
    pub action: GitStackedAction,
    pub commit_message: Option<TrimmedNonEmptyString>,
    pub feature_branch: Option<bool>,
    pub file_paths: Option<Vec<GitFilePath>>,
}

fn has_open_pr(git_status: &VcsStatusResult) -> bool {
    git_status
        .pr
        .as_ref()
        .is_some_and(|pr| pr.state == GitPullRequestState::Open)
}

fn ahead_of_default(git_status: &VcsStatusResult) -> u64 {
    git_status
        .ahead_of_default_count
        .unwrap_or(git_status.ahead_count)
}

pub fn build_menu_items(
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    has_origin_remote: bool,
) -> Vec<GitActionMenuItem> {
    let Some(git_status) = git_status else {
        return Vec::new();
    };

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let has_open_pr = has_open_pr(git_status);
    let is_behind = git_status.behind_count > 0;
    let can_push_without_upstream = has_origin_remote && !git_status.has_upstream;
    let can_commit = !is_busy && has_changes;
    let can_push = !is_busy
        && has_branch
        && !has_changes
        && !is_behind
        && git_status.ahead_count > 0
        && (git_status.has_upstream || can_push_without_upstream);
    let can_create_pr = !is_busy
        && has_branch
        && !has_open_pr
        && ahead_of_default(git_status) > 0
        && !is_behind
        && (git_status.has_upstream || can_push_without_upstream);
    let can_open_pr = !is_busy && has_open_pr;

    vec![
        GitActionMenuItem {
            id: GitActionMenuItemId::Commit,
            label: "Commit",
            disabled: !can_commit,
            icon: GitActionIconName::Commit,
            kind: GitActionMenuItemKind::OpenDialog,
            dialog_action: Some(GitDialogAction::Commit),
        },
        GitActionMenuItem {
            id: GitActionMenuItemId::Push,
            label: "Push",
            disabled: !can_push,
            icon: GitActionIconName::Push,
            kind: GitActionMenuItemKind::OpenDialog,
            dialog_action: Some(GitDialogAction::Push),
        },
        if has_open_pr {
            GitActionMenuItem {
                id: GitActionMenuItemId::Pr,
                label: "View PR",
                disabled: !can_open_pr,
                icon: GitActionIconName::Pr,
                kind: GitActionMenuItemKind::OpenPr,
                dialog_action: None,
            }
        } else {
            GitActionMenuItem {
                id: GitActionMenuItemId::Pr,
                label: "Create PR",
                disabled: !can_create_pr,
                icon: GitActionIconName::Pr,
                kind: GitActionMenuItemKind::OpenDialog,
                dialog_action: Some(GitDialogAction::CreatePr),
            }
        },
    ]
}

fn hint(label: &'static str, hint: &'static str) -> GitQuickAction {
    GitQuickAction {
        label,
        disabled: true,
        kind: GitQuickActionKind::ShowHint,
        action: None,
        hint: Some(hint),
    }
}

fn run_action(label: &'static str, action: GitStackedAction) -> GitQuickAction {
    GitQuickAction {
        label,
        disabled: false,
        kind: GitQuickActionKind::RunAction,
        action: Some(action),
        hint: None,
    }
}

fn open_pr() -> GitQuickAction {
    GitQuickAction {
        label: "View PR",
        disabled: false,
        kind: GitQuickActionKind::OpenPr,
        action: None,
        hint: None,
    }
}

pub fn resolve_quick_action(
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    is_default_branch: bool,
    has_origin_remote: bool,
) -> GitQuickAction {
    if is_busy {
        return hint("Commit", "Git action in progress.");
    }

    let Some(git_status) = git_status else {
        return hint("Commit", "Git status is unavailable.");
    };

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let has_open_pr = has_open_pr(git_status);
    let is_ahead = git_status.ahead_count > 0;
    let is_behind = git_status.behind_count > 0;
    let is_diverged = is_ahead && is_behind;

    if !has_branch {
        return hint(
            "Commit",
            "Create and checkout a branch before pushing or opening a PR.",
        );
    }

    if has_changes {
        if !git_status.has_upstream && !has_origin_remote {
            return run_action("Commit", GitStackedAction::Commit);
        }
        if has_open_pr || is_default_branch {
            return run_action("Commit & push", GitStackedAction::CommitPush);
        }
        return run_action("Commit, push & PR", GitStackedAction::CommitPushPr);
    }

    if !git_status.has_upstream {
        if !has_origin_remote {
            if has_open_pr && !is_ahead {
                return open_pr();
            }
            return hint(
                "Push",
                "Add an \"origin\" remote before pushing or creating a PR.",
            );
        }
        if !is_ahead {
            if has_open_pr {
                return open_pr();
            }
            return hint("Push", "No local commits to push.");
        }
        if has_open_pr || is_default_branch {
            return run_action(
                "Push",
                if is_default_branch {
                    GitStackedAction::CommitPush
                } else {
                    GitStackedAction::Push
                },
            );
        }
        return run_action("Push & create PR", GitStackedAction::CreatePr);
    }

    if is_diverged {
        return hint(
            "Sync branch",
            "Branch has diverged from upstream. Rebase/merge first.",
        );
    }

    if is_behind {
        return GitQuickAction {
            label: "Pull",
            disabled: false,
            kind: GitQuickActionKind::RunPull,
            action: None,
            hint: None,
        };
    }

    if is_ahead {
        if has_open_pr || is_default_branch {
            return run_action(
                "Push",
                if is_default_branch {
                    GitStackedAction::CommitPush
                } else {
                    GitStackedAction::Push
                },
            );
        }
        return run_action("Push & create PR", GitStackedAction::CreatePr);
    }

    if has_open_pr && git_status.has_upstream {
        return open_pr();
    }

    hint("Commit", "Branch is up to date. No action needed.")
}

pub fn get_git_action_disabled_reason(
    item: &GitActionMenuItem,
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    has_origin_remote: bool,
) -> Option<&'static str> {
    if !item.disabled {
        return None;
    }
    if is_busy {
        return Some("Git action in progress.");
    }
    let Some(git_status) = git_status else {
        return Some("Git status is unavailable.");
    };

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let has_open_pr = has_open_pr(git_status);
    let is_ahead = git_status.ahead_count > 0;
    let is_behind = git_status.behind_count > 0;

    if item.id == GitActionMenuItemId::Commit {
        if !has_changes {
            return Some("Worktree is clean. Make changes before committing.");
        }
        return Some("Commit is currently unavailable.");
    }

    if item.id == GitActionMenuItemId::Push {
        if !has_branch {
            return Some("Detached HEAD: checkout a branch before pushing.");
        }
        if has_changes {
            return Some("Commit or stash local changes before pushing.");
        }
        if is_behind {
            return Some("Branch is behind upstream. Pull/rebase before pushing.");
        }
        if !git_status.has_upstream && !has_origin_remote {
            return Some("Add an \"origin\" remote before pushing.");
        }
        if !is_ahead {
            return Some("No local commits to push.");
        }
        return Some("Push is currently unavailable.");
    }

    if has_open_pr {
        return Some("View PR is currently unavailable.");
    }
    if !has_branch {
        return Some("Detached HEAD: checkout a branch before creating a PR.");
    }
    if !git_status.has_upstream && !has_origin_remote {
        return Some("Add an \"origin\" remote before creating a PR.");
    }
    if ahead_of_default(git_status) == 0 {
        return Some("No local commits to include in a PR.");
    }
    if is_behind {
        return Some("Branch is behind upstream. Pull/rebase before creating a PR.");
    }
    Some("Create PR is currently unavailable.")
}

pub fn requires_default_branch_confirmation(
    action: GitStackedAction,
    is_default_branch: bool,
) -> bool {
    if !is_default_branch {
        return false;
    }
    matches!(
        action,
        GitStackedAction::Push
            | GitStackedAction::CreatePr
            | GitStackedAction::CommitPush
            | GitStackedAction::CommitPushPr
    )
}

pub fn resolve_default_branch_action_dialog_copy(
    action: DefaultBranchConfirmableAction,
    branch_name: &str,
    includes_commit: bool,
) -> DefaultBranchActionDialogCopy {
    let branch_label = branch_name;
    let suffix = format!(
        " on \"{branch_label}\". You can continue on this branch or create a feature branch and run the same action there."
    );

    if matches!(
        action,
        DefaultBranchConfirmableAction::Push | DefaultBranchConfirmableAction::CommitPush
    ) {
        if includes_commit {
            return DefaultBranchActionDialogCopy {
                title: "Commit & push to default branch?",
                description: format!("This action will commit and push changes{suffix}"),
                continue_label: format!("Commit & push to {branch_label}"),
            };
        }
        return DefaultBranchActionDialogCopy {
            title: "Push to default branch?",
            description: format!("This action will push local commits{suffix}"),
            continue_label: format!("Push to {branch_label}"),
        };
    }

    if includes_commit {
        return DefaultBranchActionDialogCopy {
            title: "Commit, push & create PR from default branch?",
            description: format!("This action will commit, push, and create a PR{suffix}"),
            continue_label: "Commit, push & create PR".to_owned(),
        };
    }
    DefaultBranchActionDialogCopy {
        title: "Push & create PR from default branch?",
        description: format!("This action will push local commits and create a PR{suffix}"),
        continue_label: "Push & create PR".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::PositiveInt;
    use turin_contracts::git::{VcsStatusChangeRequest, VcsStatusWorkingTree};

    /// A clean, tracked branch in sync with its upstream.
    fn status() -> VcsStatusResult {
        VcsStatusResult {
            is_repo: true,
            source_control_provider: None,
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some(TrimmedNonEmptyString::new("feature").unwrap()),
            has_working_tree_changes: false,
            working_tree: VcsStatusWorkingTree {
                files: vec![],
                insertions: 0,
                deletions: 0,
            },
            branch_changes: None,
            has_upstream: true,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: None,
            pr: None,
        }
    }

    fn pr(state: GitPullRequestState) -> VcsStatusChangeRequest {
        VcsStatusChangeRequest {
            number: PositiveInt::new(1).unwrap(),
            title: TrimmedNonEmptyString::new("Title").unwrap(),
            url: "https://example.test/pull/1".into(),
            base_ref: TrimmedNonEmptyString::new("main").unwrap(),
            head_ref: TrimmedNonEmptyString::new("feature").unwrap(),
            state,
            is_draft: None,
            updated_at: None,
        }
    }

    fn quick(status: &VcsStatusResult, default: bool, origin: bool) -> GitQuickAction {
        resolve_quick_action(Some(status), false, default, origin)
    }

    #[test]
    fn menu_is_empty_without_status() {
        assert!(build_menu_items(None, false, true).is_empty());
    }

    #[test]
    fn menu_enables_commit_only_with_changes_and_not_busy() {
        let mut dirty = status();
        dirty.has_working_tree_changes = true;
        assert!(!build_menu_items(Some(&dirty), false, true)[0].disabled);
        assert!(build_menu_items(Some(&dirty), true, true)[0].disabled);
        assert!(build_menu_items(Some(&status()), false, true)[0].disabled);
    }

    #[test]
    fn menu_push_requires_clean_ahead_branch_with_an_upstream_or_origin() {
        let mut ahead = status();
        ahead.ahead_count = 2;
        assert!(!build_menu_items(Some(&ahead), false, true)[1].disabled);
        ahead.behind_count = 1;
        assert!(build_menu_items(Some(&ahead), false, true)[1].disabled);
        ahead.behind_count = 0;
        ahead.has_upstream = false;
        assert!(!build_menu_items(Some(&ahead), false, true)[1].disabled);
        assert!(build_menu_items(Some(&ahead), false, false)[1].disabled);
        ahead.has_working_tree_changes = true;
        assert!(build_menu_items(Some(&ahead), false, true)[1].disabled);
    }

    #[test]
    fn menu_pr_item_switches_between_create_and_view() {
        let mut ahead = status();
        ahead.ahead_count = 1;
        let create = &build_menu_items(Some(&ahead), false, true)[2];
        assert_eq!(create.label, "Create PR");
        assert!(!create.disabled);
        assert_eq!(create.dialog_action, Some(GitDialogAction::CreatePr));

        // Ahead of the default branch decides, not of the upstream.
        ahead.ahead_of_default_count = Some(0);
        assert!(build_menu_items(Some(&ahead), false, true)[2].disabled);

        ahead.pr = Some(pr(GitPullRequestState::Open));
        let view = &build_menu_items(Some(&ahead), false, true)[2];
        assert_eq!(view.label, "View PR");
        assert_eq!(view.kind, GitActionMenuItemKind::OpenPr);
        assert!(!view.disabled);
        assert_eq!(view.dialog_action, None);

        // A closed PR does not count as open.
        ahead.pr = Some(pr(GitPullRequestState::Closed));
        ahead.ahead_of_default_count = None;
        assert_eq!(
            build_menu_items(Some(&ahead), false, true)[2].label,
            "Create PR"
        );
    }

    #[test]
    fn quick_action_hints_for_busy_missing_and_detached() {
        let busy = resolve_quick_action(Some(&status()), true, false, true);
        assert_eq!(busy.hint, Some("Git action in progress."));
        assert!(busy.disabled);
        let missing = resolve_quick_action(None, false, false, true);
        assert_eq!(missing.hint, Some("Git status is unavailable."));
        let mut detached = status();
        detached.ref_name = None;
        assert_eq!(
            quick(&detached, false, true).hint,
            Some("Create and checkout a branch before pushing or opening a PR.")
        );
    }

    #[test]
    fn quick_action_with_changes() {
        let mut dirty = status();
        dirty.has_working_tree_changes = true;
        assert_eq!(
            quick(&dirty, false, true).action,
            Some(GitStackedAction::CommitPushPr)
        );
        assert_eq!(
            quick(&dirty, true, true).action,
            Some(GitStackedAction::CommitPush)
        );
        dirty.pr = Some(pr(GitPullRequestState::Open));
        assert_eq!(quick(&dirty, false, true).label, "Commit & push");
        dirty.pr = None;
        dirty.has_upstream = false;
        let local_only = quick(&dirty, false, false);
        assert_eq!(local_only.label, "Commit");
        assert_eq!(local_only.action, Some(GitStackedAction::Commit));
    }

    #[test]
    fn quick_action_without_upstream() {
        let mut branch = status();
        branch.has_upstream = false;
        assert_eq!(
            quick(&branch, false, false).hint,
            Some("Add an \"origin\" remote before pushing or creating a PR.")
        );
        assert_eq!(
            quick(&branch, false, true).hint,
            Some("No local commits to push.")
        );
        branch.ahead_count = 1;
        assert_eq!(quick(&branch, false, true).label, "Push & create PR");
        assert_eq!(
            quick(&branch, false, true).action,
            Some(GitStackedAction::CreatePr)
        );
        assert_eq!(
            quick(&branch, true, true).action,
            Some(GitStackedAction::CommitPush)
        );
        branch.pr = Some(pr(GitPullRequestState::Open));
        assert_eq!(
            quick(&branch, false, true).action,
            Some(GitStackedAction::Push)
        );
        branch.ahead_count = 0;
        assert_eq!(quick(&branch, false, true).kind, GitQuickActionKind::OpenPr);
        assert_eq!(
            quick(&branch, false, false).kind,
            GitQuickActionKind::OpenPr
        );
    }

    #[test]
    fn quick_action_with_upstream() {
        let mut branch = status();
        branch.ahead_count = 1;
        branch.behind_count = 1;
        let diverged = quick(&branch, false, true);
        assert_eq!(diverged.label, "Sync branch");
        assert!(diverged.disabled);
        branch.ahead_count = 0;
        assert_eq!(
            quick(&branch, false, true).kind,
            GitQuickActionKind::RunPull
        );
        branch.behind_count = 0;
        assert_eq!(
            quick(&branch, false, true).hint,
            Some("Branch is up to date. No action needed.")
        );
        branch.pr = Some(pr(GitPullRequestState::Open));
        assert_eq!(quick(&branch, false, true).kind, GitQuickActionKind::OpenPr);
        branch.pr = None;
        branch.ahead_count = 3;
        assert_eq!(
            quick(&branch, false, true).action,
            Some(GitStackedAction::CreatePr)
        );
        assert_eq!(
            quick(&branch, true, true).action,
            Some(GitStackedAction::CommitPush)
        );
    }

    #[test]
    fn disabled_reason_follows_the_blocking_condition() {
        let items = |s: &VcsStatusResult, origin: bool| build_menu_items(Some(s), false, origin);
        let clean = status();
        let enabled = GitActionMenuItem {
            disabled: false,
            ..items(&clean, true)[0].clone()
        };
        assert_eq!(
            get_git_action_disabled_reason(&enabled, Some(&clean), false, true),
            None
        );
        let commit = items(&clean, true)[0].clone();
        assert_eq!(
            get_git_action_disabled_reason(&commit, Some(&clean), true, true),
            Some("Git action in progress.")
        );
        assert_eq!(
            get_git_action_disabled_reason(&commit, None, false, true),
            Some("Git status is unavailable.")
        );
        assert_eq!(
            get_git_action_disabled_reason(&commit, Some(&clean), false, true),
            Some("Worktree is clean. Make changes before committing.")
        );

        let push = items(&clean, true)[1].clone();
        let mut detached = status();
        detached.ref_name = None;
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&detached), false, true),
            Some("Detached HEAD: checkout a branch before pushing.")
        );
        let mut dirty = status();
        dirty.has_working_tree_changes = true;
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&dirty), false, true),
            Some("Commit or stash local changes before pushing.")
        );
        let mut behind = status();
        behind.behind_count = 1;
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&behind), false, true),
            Some("Branch is behind upstream. Pull/rebase before pushing.")
        );
        let mut no_upstream = status();
        no_upstream.has_upstream = false;
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&no_upstream), false, false),
            Some("Add an \"origin\" remote before pushing.")
        );
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&clean), false, true),
            Some("No local commits to push.")
        );
        let mut ahead = status();
        ahead.ahead_count = 1;
        assert_eq!(
            get_git_action_disabled_reason(&push, Some(&ahead), false, true),
            Some("Push is currently unavailable.")
        );

        let create_pr = items(&clean, true)[2].clone();
        assert_eq!(
            get_git_action_disabled_reason(&create_pr, Some(&detached), false, true),
            Some("Detached HEAD: checkout a branch before creating a PR.")
        );
        assert_eq!(
            get_git_action_disabled_reason(&create_pr, Some(&no_upstream), false, false),
            Some("Add an \"origin\" remote before creating a PR.")
        );
        assert_eq!(
            get_git_action_disabled_reason(&create_pr, Some(&clean), false, true),
            Some("No local commits to include in a PR.")
        );
        let mut behind_ahead = status();
        behind_ahead.ahead_count = 1;
        behind_ahead.behind_count = 1;
        assert_eq!(
            get_git_action_disabled_reason(&create_pr, Some(&behind_ahead), false, true),
            Some("Branch is behind upstream. Pull/rebase before creating a PR.")
        );
        let mut with_pr = status();
        with_pr.pr = Some(pr(GitPullRequestState::Open));
        let view_pr = GitActionMenuItem {
            disabled: true,
            ..items(&with_pr, true)[2].clone()
        };
        assert_eq!(
            get_git_action_disabled_reason(&view_pr, Some(&with_pr), false, true),
            Some("View PR is currently unavailable.")
        );
    }

    #[test]
    fn default_branch_confirmation_covers_pushing_actions_only() {
        assert!(!requires_default_branch_confirmation(
            GitStackedAction::Push,
            false
        ));
        assert!(!requires_default_branch_confirmation(
            GitStackedAction::Commit,
            true
        ));
        for action in [
            GitStackedAction::Push,
            GitStackedAction::CreatePr,
            GitStackedAction::CommitPush,
            GitStackedAction::CommitPushPr,
        ] {
            assert!(requires_default_branch_confirmation(action, true));
        }
    }

    #[test]
    fn default_branch_dialog_copy() {
        let suffix = " on \"main\". You can continue on this branch or create a feature branch and run the same action there.";
        let copy = |action, includes_commit| {
            resolve_default_branch_action_dialog_copy(action, "main", includes_commit)
        };
        assert_eq!(
            copy(DefaultBranchConfirmableAction::CommitPush, true),
            DefaultBranchActionDialogCopy {
                title: "Commit & push to default branch?",
                description: format!("This action will commit and push changes{suffix}"),
                continue_label: "Commit & push to main".into(),
            }
        );
        assert_eq!(
            copy(DefaultBranchConfirmableAction::Push, false),
            DefaultBranchActionDialogCopy {
                title: "Push to default branch?",
                description: format!("This action will push local commits{suffix}"),
                continue_label: "Push to main".into(),
            }
        );
        assert_eq!(
            copy(DefaultBranchConfirmableAction::CommitPushPr, true),
            DefaultBranchActionDialogCopy {
                title: "Commit, push & create PR from default branch?",
                description: format!("This action will commit, push, and create a PR{suffix}"),
                continue_label: "Commit, push & create PR".into(),
            }
        );
        assert_eq!(
            copy(DefaultBranchConfirmableAction::CreatePr, false),
            DefaultBranchActionDialogCopy {
                title: "Push & create PR from default branch?",
                description: format!("This action will push local commits and create a PR{suffix}"),
                continue_label: "Push & create PR".into(),
            }
        );
    }
}
