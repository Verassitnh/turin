//! Port of upstream/apps/web/src/components/pullRequest/pullRequestPresentation.tsx (pure parts).
//!
//! JSX/React-only components skipped: PullRequestLabelChip, PullRequestReviewDecisionGlyph,
//! PullRequestStateGlyph, PullRequestConflictGlyph, PullRequestCheckStatusIcon,
//! PullRequestReviewOutcomeIcon, PullRequestReviewOutcomeBadge, PullRequestActorAvatar,
//! PullRequestActorLabel, PullRequestDiffStat, PullRequestMetaLine. Icon values are represented
//! by glyph names so GPUI views can choose their own icon objects.

use turin_contracts::pull_request::{
    PullRequestCheck, PullRequestCheckStatus, PullRequestChecksState, PullRequestMergeability,
    PullRequestState,
};
pub use super::pull_request_detail_logic::PullRequestReviewOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullRequestStatePresentation {
    pub label: &'static str,
    pub tone_class_name: &'static str,
    pub glyph: &'static str,
}

/// Closed and merged take precedence over a stale draft flag.
pub fn resolve_pull_request_state(
    state: PullRequestState,
    is_draft: bool,
) -> PullRequestStatePresentation {
    match (state, is_draft) {
        (PullRequestState::Open, true) => PullRequestStatePresentation {
            label: "Draft",
            tone_class_name: "text-zinc-500 dark:text-zinc-400/80",
            glyph: "draft",
        },
        (PullRequestState::Open, false) => PullRequestStatePresentation {
            label: "Open",
            tone_class_name: "text-emerald-600 dark:text-emerald-300/90",
            glyph: "pullRequest",
        },
        (PullRequestState::Closed, _) => PullRequestStatePresentation {
            label: "Closed",
            tone_class_name: "text-red-600 dark:text-red-300/90",
            glyph: "closed",
        },
        (PullRequestState::Merged, _) => PullRequestStatePresentation {
            label: "Merged",
            tone_class_name: "text-violet-600 dark:text-violet-300/90",
            glyph: "merged",
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestConflictPresentation {
    pub label: String,
    pub tone_class_name: &'static str,
    pub glyph: &'static str,
}

pub fn resolve_pull_request_conflict(
    state: PullRequestState,
    is_draft: bool,
    mergeability: Option<PullRequestMergeability>,
    base_branch: Option<&str>,
) -> Option<PullRequestConflictPresentation> {
    if state != PullRequestState::Open
        || is_draft
        || mergeability != Some(PullRequestMergeability::Conflicting)
    {
        return None;
    }
    Some(PullRequestConflictPresentation {
        label: base_branch.filter(|branch| !branch.is_empty()).map_or_else(
            || "Has conflicts".into(),
            |branch| format!("Conflicts with {branch}"),
        ),
        tone_class_name: "text-destructive",
        glyph: "conflicting",
    })
}

fn is_workflow_approval_check(check: &PullRequestCheck) -> bool {
    if check.status != PullRequestCheckStatus::ActionRequired {
        return false;
    }
    let Some(url) = check.url.as_deref() else {
        return false;
    };
    let Some(after) = url.split("/actions/runs/").nth(1) else {
        return false;
    };
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && (after.len() == digits || after.as_bytes()[digits] == b'/')
}

pub fn pull_request_check_status_label(check: &PullRequestCheck) -> &'static str {
    if is_workflow_approval_check(check) {
        return "Awaiting approval";
    }
    match check.status {
        PullRequestCheckStatus::Pending => "Running",
        PullRequestCheckStatus::ActionRequired => "Awaiting action",
        PullRequestCheckStatus::Success => "Passed",
        PullRequestCheckStatus::Failure => "Failed",
        PullRequestCheckStatus::Cancelled => "Cancelled",
        PullRequestCheckStatus::Skipped => "Skipped",
        PullRequestCheckStatus::Neutral => "Neutral",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksStatePresentation {
    pub label: &'static str,
    pub tone_class_name: &'static str,
    pub glyph: &'static str,
}

pub fn pull_request_checks_state_presentation(
    state: PullRequestChecksState,
) -> ChecksStatePresentation {
    match state {
        PullRequestChecksState::Passing => ChecksStatePresentation {
            label: "All checks have passed",
            tone_class_name: "text-emerald-600 dark:text-emerald-300/90",
            glyph: "circle-check",
        },
        PullRequestChecksState::Failing => ChecksStatePresentation {
            label: "Some checks were not successful",
            tone_class_name: "text-destructive",
            glyph: "circle-x",
        },
        PullRequestChecksState::Pending => ChecksStatePresentation {
            label: "Some checks haven't completed yet",
            tone_class_name: "text-amber-600 dark:text-amber-400/90",
            glyph: "circle-dot",
        },
    }
}

/// No checks earns no tick.
pub fn pull_request_checks_state(checks: &[PullRequestCheck]) -> Option<PullRequestChecksState> {
    if checks.is_empty() {
        return None;
    }
    let has = |status| checks.iter().any(|check| check.status == status);
    if has(PullRequestCheckStatus::Failure) || has(PullRequestCheckStatus::Cancelled) {
        return Some(PullRequestChecksState::Failing);
    }
    if has(PullRequestCheckStatus::Pending) || has(PullRequestCheckStatus::ActionRequired) {
        return Some(PullRequestChecksState::Pending);
    }
    has(PullRequestCheckStatus::Success).then_some(PullRequestChecksState::Passing)
}

pub fn pull_request_review_outcome_tone_class_name(
    outcome: PullRequestReviewOutcome,
) -> &'static str {
    match outcome {
        PullRequestReviewOutcome::Approved => "text-emerald-600 dark:text-emerald-300/90",
        PullRequestReviewOutcome::ChangesRequested => "text-destructive",
        PullRequestReviewOutcome::Dismissed => "text-muted-foreground/70",
    }
}

/// Mixed opaque rings keep overlapping avatars visually distinct.
pub fn pull_request_review_outcome_ring_class_name(
    outcome: PullRequestReviewOutcome,
    stale: bool,
) -> &'static str {
    match (outcome, stale) {
        (PullRequestReviewOutcome::Approved, false) => {
            "ring-2 ring-emerald-500 dark:ring-emerald-400"
        }
        (PullRequestReviewOutcome::Approved, true) => {
            "ring-2 ring-[color-mix(in_srgb,var(--color-emerald-500)_35%,var(--background))] dark:ring-[color-mix(in_srgb,var(--color-emerald-400)_35%,var(--background))]"
        }
        (PullRequestReviewOutcome::ChangesRequested, false) => "ring-2 ring-destructive",
        (PullRequestReviewOutcome::ChangesRequested, true) => {
            "ring-2 ring-[color-mix(in_srgb,var(--destructive)_35%,var(--background))]"
        }
        (PullRequestReviewOutcome::Dismissed, false) => "ring-2 ring-muted-foreground/60",
        (PullRequestReviewOutcome::Dismissed, true) => {
            "ring-2 ring-[color-mix(in_srgb,var(--contrast-muted-foreground)_30%,var(--background))]"
        }
    }
}

pub fn pull_request_review_outcome_label(outcome: PullRequestReviewOutcome) -> &'static str {
    match outcome {
        PullRequestReviewOutcome::Approved => "Approved",
        PullRequestReviewOutcome::ChangesRequested => "Changes requested",
        PullRequestReviewOutcome::Dismissed => "Review dismissed",
    }
}
pub fn pull_request_review_outcome_stale_label(outcome: PullRequestReviewOutcome) -> String {
    format!(
        "{} earlier changes",
        pull_request_review_outcome_label(outcome)
    )
}

pub fn summarize_pull_request_checks(checks: &[PullRequestCheck]) -> String {
    if checks.is_empty() {
        return "No checks reported".into();
    }
    let action_required: Vec<_> = checks
        .iter()
        .filter(|check| check.status == PullRequestCheckStatus::ActionRequired)
        .collect();
    let workflow = action_required
        .iter()
        .filter(|check| is_workflow_approval_check(check))
        .count();
    let other = action_required.len() - workflow;
    let failed = checks
        .iter()
        .filter(|check| {
            matches!(
                check.status,
                PullRequestCheckStatus::Failure | PullRequestCheckStatus::Cancelled
            )
        })
        .count();
    let pending = checks
        .iter()
        .filter(|check| check.status == PullRequestCheckStatus::Pending)
        .count();
    let passed = checks
        .iter()
        .filter(|check| check.status == PullRequestCheckStatus::Success)
        .count();
    if failed > 0 {
        return format!("{failed} of {} failing", checks.len());
    }
    if workflow > 0 && other > 0 {
        return format!(
            "{workflow} {} and {other} {} awaiting action",
            if workflow == 1 {
                "workflow"
            } else {
                "workflows"
            },
            if other == 1 { "check" } else { "checks" }
        );
    }
    if workflow > 0 {
        return format!(
            "{workflow} {} awaiting approval",
            if workflow == 1 {
                "workflow"
            } else {
                "workflows"
            }
        );
    }
    if other > 0 {
        return format!(
            "{other} {} awaiting action",
            if other == 1 { "check" } else { "checks" }
        );
    }
    if pending > 0 {
        return format!("{pending} of {} running", checks.len());
    }
    if passed == checks.len() {
        "All checks passed".into()
    } else {
        format!("{passed} of {} passing", checks.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[test]
    fn resolves_lifecycle_presentation() {
        let cases = [
            (
                PullRequestState::Open,
                false,
                "pullRequest",
                "Open",
                "text-emerald-600 dark:text-emerald-300/90",
            ),
            (
                PullRequestState::Open,
                true,
                "draft",
                "Draft",
                "text-zinc-500 dark:text-zinc-400/80",
            ),
            (
                PullRequestState::Closed,
                false,
                "closed",
                "Closed",
                "text-red-600 dark:text-red-300/90",
            ),
            (
                PullRequestState::Merged,
                false,
                "merged",
                "Merged",
                "text-violet-600 dark:text-violet-300/90",
            ),
        ];
        for (state, draft, glyph, label, tone) in cases {
            let result = resolve_pull_request_state(state, draft);
            assert_eq!(
                (result.glyph, result.label, result.tone_class_name),
                (glyph, label, tone)
            );
        }
    }
    #[test]
    fn merged_stays_merged_with_stale_draft() {
        assert_eq!(
            resolve_pull_request_state(PullRequestState::Merged, true).glyph,
            "merged"
        );
    }
    #[test]
    fn closed_stays_closed_with_stale_draft() {
        assert_eq!(
            resolve_pull_request_state(PullRequestState::Closed, true).glyph,
            "closed"
        );
    }
    #[test]
    fn lifecycle_and_conflict_are_independent() {
        assert_eq!(
            resolve_pull_request_state(PullRequestState::Open, false).glyph,
            "pullRequest"
        );
        assert_eq!(
            resolve_pull_request_conflict(
                PullRequestState::Open,
                false,
                Some(PullRequestMergeability::Conflicting),
                Some("main")
            )
            .unwrap()
            .label,
            "Conflicts with main"
        );
    }
    #[test]
    fn terminal_and_draft_states_do_not_report_conflict() {
        for (state, draft) in [
            (PullRequestState::Closed, false),
            (PullRequestState::Merged, false),
            (PullRequestState::Closed, true),
            (PullRequestState::Merged, true),
            (PullRequestState::Open, true),
        ] {
            assert_eq!(
                resolve_pull_request_conflict(
                    state,
                    draft,
                    Some(PullRequestMergeability::Conflicting),
                    Some("main")
                ),
                None
            );
        }
    }
    #[test]
    fn unknown_and_mergeable_do_not_report_conflict() {
        for mergeability in [
            None,
            Some(PullRequestMergeability::Unknown),
            Some(PullRequestMergeability::Mergeable),
        ] {
            assert_eq!(
                resolve_pull_request_conflict(PullRequestState::Open, false, mergeability, None),
                None
            );
        }
    }
    #[test]
    fn reports_known_conflict_with_base() {
        assert_eq!(
            resolve_pull_request_conflict(
                PullRequestState::Open,
                false,
                Some(PullRequestMergeability::Conflicting),
                Some("main")
            ),
            Some(PullRequestConflictPresentation {
                label: "Conflicts with main".into(),
                tone_class_name: "text-destructive",
                glyph: "conflicting"
            })
        );
    }
    #[test]
    fn reports_known_conflict_without_base() {
        assert_eq!(
            resolve_pull_request_conflict(
                PullRequestState::Open,
                false,
                Some(PullRequestMergeability::Conflicting),
                None
            )
            .unwrap()
            .label,
            "Has conflicts"
        );
    }
}
