//! Port of upstream/apps/web/src/components/pullRequest/usePullRequestActions.ts
//! (labels, default merge-method resolver, and toast copy).

#![allow(dead_code)]

use std::collections::BTreeMap;

use turin_contracts::EnvironmentId;
use turin_contracts::ProjectId;
use turin_contracts::pull_request::{PullRequestAction, PullRequestMergeMethod};

pub fn pull_request_action_success_label(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => "Merge requested",
        PullRequestAction::Ready => "Marked ready for review",
        PullRequestAction::Draft => "Converted to draft",
        PullRequestAction::Close => "Pull request closed",
        PullRequestAction::Reopen => "Pull request reopened",
        PullRequestAction::UpdateBranch => "Branch updated with the base branch",
        PullRequestAction::EnableAutoMerge => "Auto-merge enabled",
        PullRequestAction::DisableAutoMerge => "Auto-merge disabled",
        PullRequestAction::Revert => "Revert pull request opened",
        PullRequestAction::ApproveWorkflows => "Workflows approved",
    }
}

pub fn pull_request_action_failure_label(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => "Could not merge this pull request",
        PullRequestAction::Ready => "Could not mark this ready for review",
        PullRequestAction::Draft => "Could not convert this to a draft",
        PullRequestAction::Close => "Could not close this pull request",
        PullRequestAction::Reopen => "Could not reopen this pull request",
        PullRequestAction::UpdateBranch => "Could not update this branch",
        PullRequestAction::EnableAutoMerge => "Could not enable auto-merge",
        PullRequestAction::DisableAutoMerge => "Could not disable auto-merge",
        PullRequestAction::Revert => "Could not open a revert pull request",
        PullRequestAction::ApproveWorkflows => "Could not approve workflows",
    }
}

pub fn pull_request_action_failure_hint(action: PullRequestAction) -> &'static str {
    match action {
        PullRequestAction::Merge => {
            "The host refused the merge. Check that you have write access, that the checks it requires have passed, and that the branch is not conflicting."
        }
        PullRequestAction::Ready | PullRequestAction::Draft => {
            "The host refused it. Check that you have write access to this repository."
        }
        PullRequestAction::Close => {
            "The host refused it. Check that you have write access, or that you opened it."
        }
        PullRequestAction::Reopen => {
            "The host refused it. Check that you have write access, and that the branch still exists."
        }
        PullRequestAction::UpdateBranch => {
            "The host refused it. Check that you have write access, and that the base branch has not diverged in a way the host cannot merge."
        }
        PullRequestAction::EnableAutoMerge => {
            "The host refused it. Check that auto-merge is enabled for this repository and that you have write access."
        }
        PullRequestAction::DisableAutoMerge => {
            "The host refused it. Check that you have write access to this repository."
        }
        PullRequestAction::Revert => {
            "The host refused it. Check that you have write access and that this pull request was merged on the host."
        }
        PullRequestAction::ApproveWorkflows => {
            "The host refused it. Check that you have Actions write access and that these workflow runs are still awaiting approval."
        }
    }
}

/// `usePullRequestDefaultMergeMethodResolver` without React: project default, then legacy override by logical key.
pub fn resolve_pull_request_default_merge_method(
    project_default: Option<PullRequestMergeMethod>,
    legacy_overrides: &BTreeMap<String, PullRequestMergeMethod>,
    logical_project_key: Option<&str>,
) -> Option<PullRequestMergeMethod> {
    if let Some(method) = project_default {
        return Some(method);
    }
    if legacy_overrides.is_empty() {
        return None;
    }
    logical_project_key.and_then(|key| legacy_overrides.get(key).copied())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestDefaultMergeMethodInput<'a> {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub project_default: Option<PullRequestMergeMethod>,
    pub legacy_overrides: &'a BTreeMap<String, PullRequestMergeMethod>,
    pub logical_project_key: Option<&'a str>,
}

pub fn resolve_pull_request_default_merge_method_for_project(
    input: PullRequestDefaultMergeMethodInput<'_>,
) -> Option<PullRequestMergeMethod> {
    let _ = (input.environment_id, input.project_id);
    resolve_pull_request_default_merge_method(
        input.project_default,
        input.legacy_overrides,
        input.logical_project_key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_project_default_over_legacy_override() {
        let mut overrides = BTreeMap::new();
        overrides.insert("logical".into(), PullRequestMergeMethod::Squash);
        assert_eq!(
            resolve_pull_request_default_merge_method(
                Some(PullRequestMergeMethod::Rebase),
                &overrides,
                Some("logical"),
            ),
            Some(PullRequestMergeMethod::Rebase)
        );
    }

    #[test]
    fn reads_legacy_override_when_no_project_default() {
        let mut overrides = BTreeMap::new();
        overrides.insert("logical".into(), PullRequestMergeMethod::Squash);
        assert_eq!(
            resolve_pull_request_default_merge_method(None, &overrides, Some("logical")),
            Some(PullRequestMergeMethod::Squash)
        );
    }
}
