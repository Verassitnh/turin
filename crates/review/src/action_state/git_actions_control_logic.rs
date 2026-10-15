//! Port of upstream/apps/web/src/components/GitActionsControl.logic.ts
//!
//! Pure state for the git action button, its menu, the default-ref confirmation copy, the
//! progress rows, and the thread branch reconciliation. Everything here is a function of
//! `VcsStatusResult` and the action in flight, so the UI only renders what these return.
//!
//! Deviations (see PORTING.md): the change request terminology comes from a local table of the
//! one GitHub-only host Turin ships (the unknown-host default otherwise), and `kind` is dropped
//! from menu items because every item opens a dialog.

use turin_contracts::base_schemas::js_trim;
use turin_contracts::git::{
    GitBranchStepStatus, GitRunStackedActionResult, GitStackedAction, VcsStatusResult,
};
use turin_git::shared_git::is_temporary_worktree_branch;

pub use turin_git::shared_git::resolve_auto_feature_branch_name;

/// `ChangeRequestTerminology` from `source-control-core`: what a host calls a change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestTerminology {
    pub short_label: &'static str,
    pub singular: &'static str,
}

const GITHUB_TERMINOLOGY: ChangeRequestTerminology = ChangeRequestTerminology {
    short_label: "PR",
    singular: "pull request",
};

/// `UNKNOWN_SOURCE_CONTROL_CLIENT`'s terminology, used for any provider Turin does not ship.
const UNKNOWN_TERMINOLOGY: ChangeRequestTerminology = ChangeRequestTerminology {
    short_label: "change request",
    singular: "change request",
};

/// `sourceControlClients.get(kind).changeRequest`. A missing kind reads as GitHub, the first
/// definition in the registry, which is also what the upstream lookup returns for `undefined`.
pub fn change_request_terminology(kind: Option<&str>) -> ChangeRequestTerminology {
    match kind {
        None | Some("github") => GITHUB_TERMINOLOGY,
        Some(_) => UNKNOWN_TERMINOLOGY,
    }
}

fn resolve_change_request_terminology(
    git_status: Option<&VcsStatusResult>,
) -> ChangeRequestTerminology {
    change_request_terminology(
        git_status
            .and_then(|status| status.source_control_provider.as_ref())
            .map(|provider| provider.kind.as_str()),
    )
}

pub const GIT_ACTION_SUCCESS_VISIBLE_MS: u64 = 10_000;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitActionMenuItem {
    pub id: GitActionMenuItemId,
    pub label: String,
    pub disabled: bool,
    pub icon: GitActionIconName,
    pub dialog_action: GitDialogAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitQuickActionKind {
    RunAction,
    RunPull,
    OpenPublish,
    ShowHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitQuickAction {
    pub label: String,
    pub disabled: bool,
    pub kind: GitQuickActionKind,
    pub action: Option<GitStackedAction>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBranchActionDialogCopy {
    pub title: String,
    pub description: String,
    pub continue_label: String,
}

/// The actions that need a confirmation before they run on the default ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultBranchConfirmableAction {
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitActionProgressPresentation {
    pub status: String,
    pub output: Option<String>,
    pub started_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitActionResultToastTiming {
    pub timeout: u64,
    pub dismiss_after_visible_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitActionToastType {
    Error,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadBranchMetadataPatch {
    pub branch: Option<String>,
    pub expected_branch: Option<String>,
}

/// `{ branch }` from `resolveLiveThreadBranchUpdate`: `branch` is the live ref, `None` when
/// detached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveThreadBranchUpdate {
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitActionProgressInput<'a> {
    pub is_running: bool,
    pub operation: Option<&'a str>,
    pub current_label: Option<&'a str>,
    pub last_output_line: Option<&'a str>,
    pub phase_started_at_ms: Option<i64>,
    pub hook_started_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct GitActionProgressStagesInput<'a> {
    pub action: GitStackedAction,
    pub has_custom_commit_message: bool,
    pub has_working_tree_changes: bool,
    pub push_target: Option<&'a str>,
    pub feature_branch: bool,
    pub should_push_before_pr: bool,
    pub terminology: Option<ChangeRequestTerminology>,
}

pub fn resolve_git_action_result_toast_timing(
    toast: GitActionToastType,
) -> GitActionResultToastTiming {
    GitActionResultToastTiming {
        timeout: 0,
        dismiss_after_visible_ms: match toast {
            GitActionToastType::Success => Some(GIT_ACTION_SUCCESS_VISIBLE_MS),
            GitActionToastType::Error => None,
        },
    }
}

pub fn resolve_git_action_progress_presentation(
    input: &GitActionProgressInput<'_>,
) -> Option<GitActionProgressPresentation> {
    let operation = input.operation?;
    if !input.is_running || (operation != "run_change_request" && operation != "pull") {
        return None;
    }

    let current_label = input.current_label.map(js_trim);
    let output = input.last_output_line.map(js_trim);
    let is_pull = operation == "pull";
    let status = match current_label {
        Some(label) if !label.is_empty() && label != "Running source control action" => label,
        _ if is_pull => "Pulling latest changes...",
        _ => "Starting source control action...",
    };
    let output = match output {
        Some(output) if !is_pull && !output.is_empty() => Some(output.to_owned()),
        _ => None,
    };
    Some(GitActionProgressPresentation {
        status: status.to_owned(),
        output,
        started_at_ms: if is_pull {
            input.phase_started_at_ms
        } else {
            input.hook_started_at_ms.or(input.phase_started_at_ms)
        },
    })
}

/// Compact elapsed label such as `4s` or `1m 5s`. Never negative: a clock that runs behind the
/// start time reads as `0s`.
pub fn format_git_action_elapsed(started_at_ms: Option<i64>, now_ms: i64) -> Option<String> {
    let started_at_ms = started_at_ms?;
    let elapsed_seconds = ((now_ms - started_at_ms) / 1_000).max(0);
    if elapsed_seconds < 60 {
        return Some(format!("{elapsed_seconds}s"));
    }
    Some(format!(
        "{}m {}s",
        elapsed_seconds / 60,
        elapsed_seconds % 60
    ))
}

pub fn build_git_action_progress_stages(input: &GitActionProgressStagesInput<'_>) -> Vec<String> {
    let terminology = input.terminology.unwrap_or(GITHUB_TERMINOLOGY);
    let branch_stages: Vec<String> = if input.feature_branch {
        vec!["Preparing feature ref...".to_owned()]
    } else {
        Vec::new()
    };
    let push_stage = match input.push_target.filter(|target| !target.is_empty()) {
        Some(target) => format!("Pushing to {target}..."),
        None => "Pushing...".to_owned(),
    };
    let pr_stages = vec![
        format!("Preparing {}...", terminology.short_label),
        format!("Generating {} content...", terminology.short_label),
        format!("Creating {}...", terminology.singular),
    ];

    match input.action {
        GitStackedAction::Push => return vec![push_stage],
        GitStackedAction::CreatePr => {
            return if input.should_push_before_pr {
                std::iter::once(push_stage).chain(pr_stages).collect()
            } else {
                pr_stages
            };
        }
        GitStackedAction::Commit
        | GitStackedAction::CommitPush
        | GitStackedAction::CommitPushPr => {}
    }

    let should_include_commit_stages =
        input.action == GitStackedAction::Commit || input.has_working_tree_changes;
    let commit_stages: Vec<String> = if !should_include_commit_stages {
        Vec::new()
    } else if input.has_custom_commit_message {
        vec!["Committing...".to_owned()]
    } else {
        vec![
            "Generating commit message...".to_owned(),
            "Committing...".to_owned(),
        ]
    };

    let mut stages = branch_stages;
    stages.extend(commit_stages);
    match input.action {
        GitStackedAction::Commit => {}
        GitStackedAction::CommitPush => stages.push(push_stage),
        _ => {
            stages.push(push_stage);
            stages.extend(pr_stages);
        }
    }
    stages
}

fn is_open_pr(git_status: &VcsStatusResult) -> bool {
    git_status
        .pr
        .as_ref()
        .is_some_and(|pr| pr.state == turin_contracts::git::GitPullRequestState::Open)
}

pub fn build_menu_items(
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    has_primary_remote: bool,
) -> Vec<GitActionMenuItem> {
    let Some(git_status) = git_status else {
        return Vec::new();
    };
    let terminology = resolve_change_request_terminology(Some(git_status));

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let has_open_pr = is_open_pr(git_status);
    let is_behind = git_status.behind_count > 0;
    let has_default_branch_delta = git_status
        .ahead_of_default_count
        .unwrap_or(git_status.ahead_count)
        > 0;
    let can_push_without_upstream = has_primary_remote && !git_status.has_upstream;
    let can_commit = !is_busy && has_changes;
    let can_push = !is_busy
        && has_branch
        && !is_behind
        && git_status.ahead_count > 0
        && (git_status.has_upstream || can_push_without_upstream);
    let can_create_pr = !is_busy
        && has_branch
        && !has_open_pr
        && has_default_branch_delta
        && !is_behind
        && (git_status.has_upstream || can_push_without_upstream);

    let commit_item = GitActionMenuItem {
        id: GitActionMenuItemId::Commit,
        label: "Commit".to_owned(),
        disabled: !can_commit,
        icon: GitActionIconName::Commit,
        dialog_action: GitDialogAction::Commit,
    };

    if !has_primary_remote {
        return vec![commit_item];
    }

    let push_item = GitActionMenuItem {
        id: GitActionMenuItemId::Push,
        label: "Push".to_owned(),
        disabled: !can_push,
        icon: GitActionIconName::Push,
        dialog_action: GitDialogAction::Push,
    };

    // An open change request is surfaced by the standalone attribution row, so the menu offers
    // no change-request entry at all while one is open.
    if has_open_pr {
        return vec![commit_item, push_item];
    }

    vec![
        commit_item,
        push_item,
        GitActionMenuItem {
            id: GitActionMenuItemId::Pr,
            label: format!("Create {}", terminology.short_label),
            disabled: !can_create_pr,
            icon: GitActionIconName::Pr,
            dialog_action: GitDialogAction::CreatePr,
        },
    ]
}

fn hint_action(label: &str, hint: &str) -> GitQuickAction {
    GitQuickAction {
        label: label.to_owned(),
        disabled: true,
        kind: GitQuickActionKind::ShowHint,
        action: None,
        hint: Some(hint.to_owned()),
    }
}

fn run_action(label: String, action: GitStackedAction) -> GitQuickAction {
    GitQuickAction {
        label,
        disabled: false,
        kind: GitQuickActionKind::RunAction,
        action: Some(action),
        hint: None,
    }
}

pub fn resolve_quick_action(
    git_status: Option<&VcsStatusResult>,
    is_busy: bool,
    is_default_ref: bool,
    has_primary_remote: bool,
) -> GitQuickAction {
    if is_busy {
        return hint_action("Commit", "Git action in progress.");
    }

    let Some(git_status) = git_status else {
        return hint_action("Commit", "Git status is unavailable.");
    };

    let has_branch = git_status.ref_name.is_some();
    let has_changes = git_status.has_working_tree_changes;
    let has_open_pr = is_open_pr(git_status);
    let is_ahead = git_status.ahead_count > 0;
    let has_default_branch_delta = git_status
        .ahead_of_default_count
        .unwrap_or(git_status.ahead_count)
        > 0;
    let is_behind = git_status.behind_count > 0;
    let is_diverged = is_ahead && is_behind;
    let terminology = resolve_change_request_terminology(Some(git_status));

    if !has_branch {
        return hint_action(
            "Commit",
            &format!(
                "Create and checkout a ref before pushing or opening a {}.",
                terminology.singular
            ),
        );
    }

    if has_changes {
        if !git_status.has_upstream && !has_primary_remote {
            return run_action("Commit".to_owned(), GitStackedAction::Commit);
        }
        if has_open_pr || is_default_ref {
            return run_action("Commit & push".to_owned(), GitStackedAction::CommitPush);
        }
        return run_action(
            format!("Commit, push & {}", terminology.short_label),
            GitStackedAction::CommitPushPr,
        );
    }

    if !git_status.has_upstream {
        if !has_primary_remote {
            return GitQuickAction {
                label: "Publish repository".to_owned(),
                disabled: false,
                kind: GitQuickActionKind::OpenPublish,
                action: None,
                hint: None,
            };
        }
        if !is_ahead {
            if has_open_pr {
                return hint_action("Commit", "Branch is up to date. No action needed.");
            }
            return hint_action("Push", "No local commits to push.");
        }
        if has_open_pr || is_default_ref {
            let action = if is_default_ref {
                GitStackedAction::CommitPush
            } else {
                GitStackedAction::Push
            };
            return run_action("Push".to_owned(), action);
        }
        return run_action(
            format!("Push & create {}", terminology.short_label),
            GitStackedAction::CreatePr,
        );
    }

    if is_diverged {
        return hint_action(
            "Sync ref",
            "Branch has diverged from upstream. Rebase/merge first.",
        );
    }

    if is_behind {
        return GitQuickAction {
            label: "Pull".to_owned(),
            disabled: false,
            kind: GitQuickActionKind::RunPull,
            action: None,
            hint: None,
        };
    }

    if is_ahead {
        if has_open_pr || is_default_ref {
            let action = if is_default_ref {
                GitStackedAction::CommitPush
            } else {
                GitStackedAction::Push
            };
            return run_action("Push".to_owned(), action);
        }
        return run_action(
            format!("Push & create {}", terminology.short_label),
            GitStackedAction::CreatePr,
        );
    }

    // An open change request is surfaced by the standalone attribution row in the details panel,
    // so the action button rests in its disabled up-to-date state.
    if has_open_pr && git_status.has_upstream {
        return hint_action("Commit", "Branch is up to date. No action needed.");
    }

    if has_default_branch_delta && !is_default_ref {
        return run_action(
            format!("Create {}", terminology.short_label),
            GitStackedAction::CreatePr,
        );
    }

    hint_action("Commit", "Branch is up to date. No action needed.")
}

pub fn requires_default_branch_confirmation(
    action: GitStackedAction,
    is_default_ref: bool,
) -> bool {
    if !is_default_ref {
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
    terminology: Option<ChangeRequestTerminology>,
) -> DefaultBranchActionDialogCopy {
    let suffix = format!(
        " on \"{branch_name}\". You can continue on this ref or create a feature ref and run the same action there."
    );
    let terminology = terminology.unwrap_or(GITHUB_TERMINOLOGY);

    if matches!(
        action,
        DefaultBranchConfirmableAction::Push | DefaultBranchConfirmableAction::CommitPush
    ) {
        if includes_commit {
            return DefaultBranchActionDialogCopy {
                title: "Commit & push to default ref?".to_owned(),
                description: format!("This action will commit and push changes{suffix}"),
                continue_label: format!("Commit & push to {branch_name}"),
            };
        }
        return DefaultBranchActionDialogCopy {
            title: "Push to default ref?".to_owned(),
            description: format!("This action will push local commits{suffix}"),
            continue_label: format!("Push to {branch_name}"),
        };
    }

    if includes_commit {
        return DefaultBranchActionDialogCopy {
            title: format!(
                "Commit, push & create {} from default ref?",
                terminology.short_label
            ),
            description: format!(
                "This action will commit, push, and create a {}{suffix}",
                terminology.singular
            ),
            continue_label: format!("Commit, push & create {}", terminology.short_label),
        };
    }
    DefaultBranchActionDialogCopy {
        title: format!(
            "Push & create {} from default ref?",
            terminology.short_label
        ),
        description: format!(
            "This action will push local commits and create a {}{suffix}",
            terminology.singular
        ),
        continue_label: format!("Push & create {}", terminology.short_label),
    }
}

/// The branch name to store on the thread, when the action created one.
pub fn resolve_thread_branch_update(result: &GitRunStackedActionResult) -> Option<String> {
    if result.branch.status != GitBranchStepStatus::Created {
        return None;
    }
    result
        .branch
        .name
        .as_ref()
        .map(|name| name.as_str().to_owned())
}

pub fn resolve_thread_branch_metadata_patch(
    branch: Option<&str>,
    expected_branch: Option<&str>,
) -> ThreadBranchMetadataPatch {
    ThreadBranchMetadataPatch {
        branch: branch.map(str::to_owned),
        expected_branch: expected_branch.map(str::to_owned),
    }
}

/// Reconciles the thread's stored ref with the live checkout. A detached HEAD never clears a
/// stored ref, and a temporary worktree ref never replaces a semantic one.
pub fn resolve_live_thread_branch_update(
    thread_branch: Option<&str>,
    git_status: Option<&VcsStatusResult>,
) -> Option<LiveThreadBranchUpdate> {
    let git_status = git_status?;
    let ref_name = git_status.ref_name.as_ref().map(|name| name.as_str());

    if ref_name.is_none() && thread_branch.is_some() {
        return None;
    }

    if thread_branch == ref_name {
        return None;
    }

    if let (Some(thread_branch), Some(ref_name)) = (thread_branch, ref_name)
        && !is_temporary_worktree_branch(thread_branch)
        && is_temporary_worktree_branch(ref_name)
    {
        return None;
    }

    Some(LiveThreadBranchUpdate {
        branch: ref_name.map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// Upstream's `status()` fixture: a clean feature ref that is level with its upstream, with
    /// `overrides` merged over the top-level keys.
    fn status(overrides: Value) -> VcsStatusResult {
        let mut base = json!({
            "isRepo": true,
            "hasPrimaryRemote": true,
            "isDefaultRef": false,
            "refName": "feature/test",
            "hasWorkingTreeChanges": false,
            "workingTree": { "files": [], "insertions": 0, "deletions": 0 },
            "hasUpstream": true,
            "aheadCount": 0,
            "behindCount": 0,
            "pr": null,
        });
        let base_object = base.as_object_mut().unwrap();
        for (key, value) in overrides.as_object().unwrap() {
            base_object.insert(key.clone(), value.clone());
        }
        serde_json::from_value(base).unwrap()
    }

    fn open_pr(number: u64) -> Value {
        json!({
            "number": number,
            "title": "Existing PR",
            "url": format!("https://example.com/pr/{number}"),
            "baseRef": "main",
            "headRef": "feature/test",
            "state": "open",
        })
    }

    fn commit_item(disabled: bool) -> GitActionMenuItem {
        GitActionMenuItem {
            id: GitActionMenuItemId::Commit,
            label: "Commit".to_owned(),
            disabled,
            icon: GitActionIconName::Commit,
            dialog_action: GitDialogAction::Commit,
        }
    }

    fn push_item(disabled: bool) -> GitActionMenuItem {
        GitActionMenuItem {
            id: GitActionMenuItemId::Push,
            label: "Push".to_owned(),
            disabled,
            icon: GitActionIconName::Push,
            dialog_action: GitDialogAction::Push,
        }
    }

    fn pr_item(disabled: bool) -> GitActionMenuItem {
        GitActionMenuItem {
            id: GitActionMenuItemId::Pr,
            label: "Create PR".to_owned(),
            disabled,
            icon: GitActionIconName::Pr,
            dialog_action: GitDialogAction::CreatePr,
        }
    }

    fn hint(label: &str, disabled: bool, message: &str) -> GitQuickAction {
        GitQuickAction {
            label: label.to_owned(),
            disabled,
            kind: GitQuickActionKind::ShowHint,
            action: None,
            hint: Some(message.to_owned()),
        }
    }

    fn run(label: &str, action: GitStackedAction) -> GitQuickAction {
        run_action(label.to_owned(), action)
    }

    const UP_TO_DATE: &str = "Branch is up to date. No action needed.";

    #[test]
    fn progress_keeps_phase_on_first_row_and_hook_output_on_second() {
        let presentation = resolve_git_action_progress_presentation(&GitActionProgressInput {
            is_running: true,
            operation: Some("run_change_request"),
            current_label: Some("Running pre-commit..."),
            last_output_line: Some("Checking formatting and lint rules"),
            phase_started_at_ms: Some(1_000),
            hook_started_at_ms: Some(2_000),
        });
        assert_eq!(
            presentation,
            Some(GitActionProgressPresentation {
                status: "Running pre-commit...".to_owned(),
                output: Some("Checking formatting and lint rules".to_owned()),
                started_at_ms: Some(2_000),
            })
        );
    }

    #[test]
    fn progress_omits_empty_output_and_supplies_a_brief_startup_label() {
        let presentation = resolve_git_action_progress_presentation(&GitActionProgressInput {
            is_running: true,
            operation: Some("run_change_request"),
            current_label: Some("Running source control action"),
            last_output_line: Some("  "),
            phase_started_at_ms: Some(1_000),
            hook_started_at_ms: None,
        });
        assert_eq!(
            presentation,
            Some(GitActionProgressPresentation {
                status: "Starting source control action...".to_owned(),
                output: None,
                started_at_ms: Some(1_000),
            })
        );
    }

    #[test]
    fn progress_presents_pull_inline_without_a_second_row() {
        let presentation = resolve_git_action_progress_presentation(&GitActionProgressInput {
            is_running: true,
            operation: Some("pull"),
            current_label: Some("Pulling latest changes..."),
            last_output_line: None,
            phase_started_at_ms: Some(1_000),
            hook_started_at_ms: None,
        });
        assert_eq!(
            presentation,
            Some(GitActionProgressPresentation {
                status: "Pulling latest changes...".to_owned(),
                output: None,
                started_at_ms: Some(1_000),
            })
        );
    }

    #[test]
    fn progress_does_not_present_unrelated_or_completed_actions() {
        let input = GitActionProgressInput {
            is_running: true,
            operation: Some("publish_repository"),
            current_label: Some("Publishing repository"),
            last_output_line: None,
            phase_started_at_ms: Some(1_000),
            hook_started_at_ms: None,
        };
        assert_eq!(resolve_git_action_progress_presentation(&input), None);
        assert_eq!(
            resolve_git_action_progress_presentation(&GitActionProgressInput {
                is_running: false,
                ..input
            }),
            None
        );
    }

    #[test]
    fn elapsed_formats_compactly_and_never_goes_negative() {
        assert_eq!(format_git_action_elapsed(None, 10_000), None);
        assert_eq!(
            format_git_action_elapsed(Some(10_000), 9_000).as_deref(),
            Some("0s")
        );
        assert_eq!(
            format_git_action_elapsed(Some(10_000), 14_900).as_deref(),
            Some("4s")
        );
        assert_eq!(
            format_git_action_elapsed(Some(10_000), 75_000).as_deref(),
            Some("1m 5s")
        );
    }

    #[test]
    fn toast_keeps_errors_sticky_and_dismisses_successes_after_visible_time() {
        assert_eq!(
            resolve_git_action_result_toast_timing(GitActionToastType::Error),
            GitActionResultToastTiming {
                timeout: 0,
                dismiss_after_visible_ms: None,
            }
        );
        assert_eq!(
            resolve_git_action_result_toast_timing(GitActionToastType::Success),
            GitActionResultToastTiming {
                timeout: 0,
                dismiss_after_visible_ms: Some(10_000),
            }
        );
    }

    #[test]
    fn clean_ref_with_open_pr_rests_disabled_and_omits_the_pr_entry() {
        let git_status = status(json!({ "pr": open_pr(10) }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint("Commit", true, UP_TO_DATE)
        );

        let git_status = status(json!({ "pr": open_pr(11) }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true)]
        );
    }

    #[test]
    fn busy_actions_disable_everything() {
        assert_eq!(
            resolve_quick_action(Some(&status(json!({}))), true, false, true),
            hint("Commit", true, "Git action in progress.")
        );
        assert_eq!(
            build_menu_items(Some(&status(json!({}))), true, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn unavailable_status_shows_a_hint_and_no_menu() {
        assert_eq!(
            resolve_quick_action(None, false, false, true),
            hint("Commit", true, "Git status is unavailable.")
        );
        assert_eq!(build_menu_items(None, false, true), Vec::new());
    }

    #[test]
    fn clean_ahead_ref_with_open_pr_prefers_push() {
        let git_status = status(json!({ "aheadCount": 3, "pr": open_pr(13) }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Push", GitStackedAction::Push)
        );

        let git_status = status(json!({ "aheadCount": 2, "pr": open_pr(12) }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(false)]
        );
    }

    #[test]
    fn clean_ahead_ref_without_pr_pushes_and_creates_one() {
        let git_status = status(json!({ "aheadCount": 2, "pr": null }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Push & create PR", GitStackedAction::CreatePr)
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(false), pr_item(false)]
        );
    }

    #[test]
    fn unknown_provider_uses_generic_change_request_terminology() {
        // Upstream also has GitLab's "MR" wording here; GitLab is cut from Turin.
        let git_status = status(json!({
            "aheadCount": 2,
            "sourceControlProvider": {
                "kind": "gitlab",
                "name": "GitLab",
                "baseUrl": "https://gitlab.com",
            },
        }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Push & create change request", GitStackedAction::CreatePr)
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true)[2].label,
            "Create change request"
        );
    }

    #[test]
    fn synced_ref_ahead_of_default_can_create_a_pr_even_with_local_changes() {
        let synced_feature = status(json!({
            "aheadCount": 0,
            "behindCount": 0,
            "aheadOfDefaultCount": 1,
            "pr": null,
        }));
        assert_eq!(
            resolve_quick_action(Some(&synced_feature), false, false, true),
            run("Create PR", GitStackedAction::CreatePr)
        );
        assert!(!build_menu_items(Some(&synced_feature), false, true)[2].disabled);

        let mut dirty_feature = synced_feature.clone();
        dirty_feature.has_working_tree_changes = true;
        assert!(!build_menu_items(Some(&dirty_feature), false, true)[2].disabled);
    }

    #[test]
    fn synced_ref_with_nothing_to_do_disables_every_action() {
        let git_status = status(json!({ "aheadCount": 0, "behindCount": 0, "pr": null }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint("Commit", true, UP_TO_DATE)
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn behind_ref_pulls_and_keeps_push_and_pr_disabled() {
        let git_status = status(json!({ "behindCount": 2 }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            GitQuickAction {
                label: "Pull".to_owned(),
                disabled: false,
                kind: GitQuickActionKind::RunPull,
                action: None,
                hint: None,
            }
        );

        let git_status = status(json!({ "behindCount": 1, "pr": null }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn diverged_ref_shows_a_sync_hint() {
        let git_status = status(json!({ "aheadCount": 2, "behindCount": 1 }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint(
                "Sync ref",
                true,
                "Branch has diverged from upstream. Rebase/merge first."
            )
        );
    }

    #[test]
    fn local_changes_offer_commit_push_and_pr() {
        let git_status = status(json!({ "hasWorkingTreeChanges": true }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Commit, push & PR", GitStackedAction::CommitPushPr)
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(false), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn local_changes_without_an_origin_remote_fall_back_to_commit() {
        let git_status = status(json!({ "hasWorkingTreeChanges": true, "hasUpstream": false }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, false),
            GitQuickAction {
                label: "Commit".to_owned(),
                disabled: false,
                kind: GitQuickActionKind::RunAction,
                action: Some(GitStackedAction::Commit),
                hint: None,
            }
        );
    }

    #[test]
    fn local_changes_with_an_open_pr_commit_and_push() {
        let git_status = status(json!({ "hasWorkingTreeChanges": true, "pr": open_pr(16) }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Commit & push", GitStackedAction::CommitPush)
        );
    }

    #[test]
    fn local_changes_with_ahead_commits_keep_push_and_pr_enabled() {
        let git_status = status(json!({
            "refName": "feature/test",
            "hasWorkingTreeChanges": true,
            "aheadCount": 1,
            "workingTree": {
                "files": [{ "path": ".vercel/project.json", "insertions": 1, "deletions": 0 }],
                "insertions": 1,
                "deletions": 0,
            },
        }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(false), push_item(false), pr_item(false)]
        );
    }

    #[test]
    fn default_ref_with_local_changes_commits_and_pushes() {
        let git_status = status(json!({ "refName": "main", "hasWorkingTreeChanges": true }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, true, true),
            run("Commit & push", GitStackedAction::CommitPush)
        );
    }

    #[test]
    fn default_ref_that_is_ahead_pushes_only() {
        let git_status = status(json!({ "refName": "main", "aheadCount": 2, "pr": null }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, true, true),
            run("Push", GitStackedAction::CommitPush)
        );
    }

    #[test]
    fn local_changes_while_behind_still_prefer_commit_push_and_pr() {
        let git_status = status(json!({ "hasWorkingTreeChanges": true, "behindCount": 1 }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Commit, push & PR", GitStackedAction::CommitPushPr)
        );

        let git_status = status(json!({ "hasWorkingTreeChanges": true, "behindCount": 2 }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(false), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn detached_head_without_changes_shows_a_hint_and_disables_the_menu() {
        let git_status = status(json!({ "refName": null, "hasUpstream": false }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint(
                "Commit",
                true,
                "Create and checkout a ref before pushing or opening a pull request."
            )
        );

        let git_status = status(json!({ "refName": null, "hasWorkingTreeChanges": false }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn no_upstream_and_no_local_commits_is_disabled() {
        let git_status = status(json!({ "hasUpstream": false, "pr": null, "aheadCount": 0 }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint("Push", true, "No local commits to push.")
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn no_upstream_with_open_pr_and_no_commits_rests_disabled() {
        let git_status = status(json!({
            "hasUpstream": false,
            "aheadCount": 0,
            "pr": open_pr(14),
        }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            hint("Commit", true, UP_TO_DATE)
        );
    }

    #[test]
    fn no_upstream_with_local_commits_pushes() {
        let git_status = status(json!({
            "hasUpstream": false,
            "aheadCount": 1,
            "pr": open_pr(15),
        }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Push", GitStackedAction::Push)
        );
    }

    #[test]
    fn no_upstream_with_commits_pushes_and_creates_a_pr() {
        let git_status = status(json!({ "hasUpstream": false, "aheadCount": 2, "pr": null }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, true),
            run("Push & create PR", GitStackedAction::CreatePr)
        );
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(false), pr_item(false)]
        );
    }

    #[test]
    fn no_upstream_without_an_origin_remote_offers_publish() {
        let git_status = status(json!({ "hasUpstream": false, "aheadCount": 2, "pr": null }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, false, false),
            GitQuickAction {
                label: "Publish repository".to_owned(),
                disabled: false,
                kind: GitQuickActionKind::OpenPublish,
                action: None,
                hint: None,
            }
        );
    }

    #[test]
    fn no_upstream_without_an_origin_remote_hides_push_and_pr_from_the_menu() {
        let git_status = status(json!({ "hasUpstream": false, "aheadCount": 2, "pr": null }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, false),
            vec![commit_item(true)]
        );
    }

    #[test]
    fn default_ref_without_upstream_and_no_commits_is_disabled() {
        let git_status = status(json!({
            "refName": "main",
            "hasUpstream": false,
            "aheadCount": 0,
            "pr": null,
        }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, true, true),
            hint("Push", true, "No local commits to push.")
        );
    }

    #[test]
    fn default_ref_without_upstream_pushes_only_when_ahead() {
        let git_status = status(json!({
            "refName": "main",
            "hasUpstream": false,
            "aheadCount": 1,
            "pr": null,
        }));
        assert_eq!(
            resolve_quick_action(Some(&git_status), false, true, true),
            run("Push", GitStackedAction::CommitPush)
        );
    }

    #[test]
    fn behind_ref_without_upstream_still_disables_push_and_pr() {
        let git_status = status(json!({
            "hasUpstream": false,
            "behindCount": 1,
            "aheadCount": 0,
            "pr": null,
        }));
        assert_eq!(
            build_menu_items(Some(&git_status), false, true),
            vec![commit_item(true), push_item(true), pr_item(true)]
        );
    }

    #[test]
    fn requires_confirmation_only_for_pushing_actions_on_the_default_ref() {
        assert!(!requires_default_branch_confirmation(
            GitStackedAction::Commit,
            true
        ));
        assert!(requires_default_branch_confirmation(
            GitStackedAction::Push,
            true
        ));
        assert!(requires_default_branch_confirmation(
            GitStackedAction::CreatePr,
            true
        ));
        assert!(requires_default_branch_confirmation(
            GitStackedAction::CommitPush,
            true
        ));
        assert!(requires_default_branch_confirmation(
            GitStackedAction::CommitPushPr,
            true
        ));
        assert!(!requires_default_branch_confirmation(
            GitStackedAction::CommitPush,
            false
        ));
        assert!(!requires_default_branch_confirmation(
            GitStackedAction::Push,
            false
        ));
    }

    #[test]
    fn dialog_copy_uses_push_only_wording_when_pushing_without_a_commit() {
        assert_eq!(
            resolve_default_branch_action_dialog_copy(
                DefaultBranchConfirmableAction::CommitPush,
                "main",
                false,
                None,
            ),
            DefaultBranchActionDialogCopy {
                title: "Push to default ref?".to_owned(),
                description: "This action will push local commits on \"main\". You can continue on this ref or create a feature ref and run the same action there.".to_owned(),
                continue_label: "Push to main".to_owned(),
            }
        );
    }

    #[test]
    fn dialog_copy_uses_push_and_pr_wording_when_creating_a_pr_without_a_commit() {
        assert_eq!(
            resolve_default_branch_action_dialog_copy(
                DefaultBranchConfirmableAction::CommitPushPr,
                "main",
                false,
                None,
            ),
            DefaultBranchActionDialogCopy {
                title: "Push & create PR from default ref?".to_owned(),
                description: "This action will push local commits and create a pull request on \"main\". You can continue on this ref or create a feature ref and run the same action there.".to_owned(),
                continue_label: "Push & create PR".to_owned(),
            }
        );
    }

    #[test]
    fn dialog_copy_keeps_commit_wording_when_the_action_includes_a_commit() {
        assert_eq!(
            resolve_default_branch_action_dialog_copy(
                DefaultBranchConfirmableAction::CommitPushPr,
                "main",
                true,
                None,
            ),
            DefaultBranchActionDialogCopy {
                title: "Commit, push & create PR from default ref?".to_owned(),
                description: "This action will commit, push, and create a pull request on \"main\". You can continue on this ref or create a feature ref and run the same action there.".to_owned(),
                continue_label: "Commit, push & create PR".to_owned(),
            }
        );
    }

    fn stages_input(action: GitStackedAction) -> GitActionProgressStagesInput<'static> {
        GitActionProgressStagesInput {
            action,
            has_custom_commit_message: false,
            has_working_tree_changes: false,
            push_target: None,
            feature_branch: false,
            should_push_before_pr: false,
            terminology: None,
        }
    }

    #[test]
    fn explicit_push_shows_only_push_progress() {
        let stages = build_git_action_progress_stages(&GitActionProgressStagesInput {
            push_target: Some("origin/feature/test"),
            ..stages_input(GitStackedAction::Push)
        });
        assert_eq!(stages, vec!["Pushing to origin/feature/test...".to_owned()]);
    }

    #[test]
    fn create_pr_that_needs_a_push_shows_push_and_pr_progress() {
        let stages = build_git_action_progress_stages(&GitActionProgressStagesInput {
            push_target: Some("origin/feature/test"),
            should_push_before_pr: true,
            ..stages_input(GitStackedAction::CreatePr)
        });
        assert_eq!(
            stages,
            vec![
                "Pushing to origin/feature/test...",
                "Preparing PR...",
                "Generating PR content...",
                "Creating pull request...",
            ]
        );
    }

    #[test]
    fn create_pr_that_can_skip_the_push_shows_only_pr_progress() {
        let stages = build_git_action_progress_stages(&stages_input(GitStackedAction::CreatePr));
        assert_eq!(
            stages,
            vec![
                "Preparing PR...",
                "Generating PR content...",
                "Creating pull request...",
            ]
        );
    }

    #[test]
    fn commit_push_with_a_dirty_tree_includes_commit_stages() {
        let stages = build_git_action_progress_stages(&GitActionProgressStagesInput {
            has_working_tree_changes: true,
            push_target: Some("origin/feature/test"),
            ..stages_input(GitStackedAction::CommitPush)
        });
        assert_eq!(
            stages,
            vec![
                "Generating commit message...",
                "Committing...",
                "Pushing to origin/feature/test...",
            ]
        );
    }

    #[test]
    fn commit_push_pr_shows_granular_pr_stages() {
        let stages = build_git_action_progress_stages(&GitActionProgressStagesInput {
            has_custom_commit_message: true,
            has_working_tree_changes: true,
            push_target: Some("origin/feature/test"),
            ..stages_input(GitStackedAction::CommitPushPr)
        });
        assert_eq!(
            stages,
            vec![
                "Committing...",
                "Pushing to origin/feature/test...",
                "Preparing PR...",
                "Generating PR content...",
                "Creating pull request...",
            ]
        );
    }

    fn stacked_result(branch: Value) -> GitRunStackedActionResult {
        serde_json::from_value(json!({
            "action": "commit_push_pr",
            "branch": branch,
            "commit": {
                "status": "created",
                "commitSha": "89abcdef01234567",
                "subject": "feat: add ref sync",
            },
            "push": { "status": "pushed", "branch": "feature/fix-toast-copy" },
            "pr": { "status": "skipped_not_requested" },
            "toast": {
                "title": "Pushed 89abcde to origin/feature/fix-toast-copy",
                "cta": { "kind": "none" },
            },
        }))
        .unwrap()
    }

    #[test]
    fn thread_branch_update_follows_a_branch_the_action_created() {
        let result = stacked_result(json!({
            "status": "created",
            "name": "feature/fix-toast-copy",
        }));
        assert_eq!(
            resolve_thread_branch_update(&result),
            Some("feature/fix-toast-copy".to_owned())
        );
    }

    #[test]
    fn thread_branch_update_is_none_when_the_action_stayed_on_the_branch() {
        let result = stacked_result(json!({ "status": "skipped_not_requested" }));
        assert_eq!(resolve_thread_branch_update(&result), None);
    }

    #[test]
    fn live_update_follows_a_changed_checkout() {
        assert_eq!(
            resolve_live_thread_branch_update(
                Some("feature/old-ref"),
                Some(&status(json!({ "refName": "effect-atom" }))),
            ),
            Some(LiveThreadBranchUpdate {
                branch: Some("effect-atom".to_owned()),
            })
        );
    }

    #[test]
    fn live_update_is_none_when_git_status_is_unavailable() {
        assert_eq!(
            resolve_live_thread_branch_update(Some("feature/old-ref"), None),
            None
        );
    }

    #[test]
    fn live_update_is_none_when_the_stored_ref_already_matches() {
        assert_eq!(
            resolve_live_thread_branch_update(
                Some("effect-atom"),
                Some(&status(json!({ "refName": "effect-atom" }))),
            ),
            None
        );
    }

    #[test]
    fn live_update_keeps_the_stored_ref_on_detached_head() {
        assert_eq!(
            resolve_live_thread_branch_update(
                Some("effect-atom"),
                Some(&status(json!({ "refName": null }))),
            ),
            None
        );
    }

    #[test]
    fn live_update_does_not_regress_a_semantic_ref_to_a_temporary_worktree_ref() {
        assert_eq!(
            resolve_live_thread_branch_update(
                Some("t3code/github-query-rate-limit"),
                Some(&status(json!({ "refName": "t3code/bda76797" }))),
            ),
            None
        );
    }

    #[test]
    fn live_update_lets_a_temporary_worktree_ref_reconcile_to_a_semantic_branch() {
        assert_eq!(
            resolve_live_thread_branch_update(
                Some("t3code/a9628676"),
                Some(&status(json!({ "refName": "feature/diff-panel-toggle" }))),
            ),
            Some(LiveThreadBranchUpdate {
                branch: Some("feature/diff-panel-toggle".to_owned()),
            })
        );
    }

    #[test]
    fn metadata_patch_does_not_overwrite_worktree_metadata_while_reconciling() {
        assert_eq!(
            resolve_thread_branch_metadata_patch(
                Some("feature/current-ref"),
                Some("feature/previous-ref"),
            ),
            ThreadBranchMetadataPatch {
                branch: Some("feature/current-ref".to_owned()),
                expected_branch: Some("feature/previous-ref".to_owned()),
            }
        );
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn auto_feature_branch_uses_semantic_preferred_names() {
        assert_eq!(
            resolve_auto_feature_branch_name(
                &names(&["main", "feature/other"]),
                Some("fix toast copy")
            ),
            "feature/fix-toast-copy"
        );
    }

    #[test]
    fn auto_feature_branch_normalizes_names_that_already_have_a_namespace() {
        assert_eq!(
            resolve_auto_feature_branch_name(
                &names(&["main"]),
                Some("feature/refine-toolbar-actions"),
            ),
            "feature/refine-toolbar-actions"
        );
    }

    #[test]
    fn auto_feature_branch_increments_the_suffix_on_collision() {
        assert_eq!(
            resolve_auto_feature_branch_name(
                &names(&["main", "feature/fix-toast-copy", "feature/fix-toast-copy-2"]),
                Some("fix toast copy"),
            ),
            "feature/fix-toast-copy-3"
        );
    }

    #[test]
    fn auto_feature_branch_collision_check_is_case_insensitive() {
        assert_eq!(
            resolve_auto_feature_branch_name(
                &names(&["Feature/Ticket-1"]),
                Some("feature/ticket-1")
            ),
            "feature/ticket-1-2"
        );
    }

    #[test]
    fn auto_feature_branch_falls_back_to_feature_update() {
        assert_eq!(
            resolve_auto_feature_branch_name(&names(&["main"]), None),
            "feature/update"
        );
    }
}
