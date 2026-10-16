//! Port of upstream/apps/server/src/orchestration-v2/pullRequestWatch.ts
//!
//! Compares a watched pull request with what its agent was last told. Pure:
//! the reactors fetch the detail and the remarks, then call
//! [`evaluate_pull_request_watch`] and record `next`.

use std::sync::OnceLock;

use regex::Regex;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_len};
use turin_contracts::orchestration_v2::{
    OrchestrationV2Notification, OrchestrationV2NotificationOutcome,
    OrchestrationV2NotificationSource,
};
use turin_contracts::pull_request::{
    PullRequestCheck, PullRequestCheckStatus, PullRequestComment, PullRequestMergeability,
};
use turin_contracts::thread_pull_request::ThreadPullRequestWatch;

/// Wakes in a row that bring only comments. Check, conflict, or push news resets the count, so
/// this only stops a chatty bot looping an agent that is replying to it.
pub const PULL_REQUEST_WATCH_WAKE_LIMIT: u64 = 10;
const LISTED_ITEMS: usize = 10;
const SNIPPET_LENGTH: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum PullRequestWatchChange {
    ChecksFailed { failed: Vec<PullRequestCheck> },
    ChecksPassed { count: usize, required: bool },
    Remarks { remarks: Vec<PullRequestComment> },
    Conflicting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestWatchReport {
    /// What the agent has not been told yet. Empty means no wake.
    pub changes: Vec<PullRequestWatchChange>,
    /// The watch to record, whether or not anything is reported.
    pub next: ThreadPullRequestWatch,
    /// This report spends the last wake before the limit, so watching stops after it.
    pub exhausted: bool,
}

/// The fields of `PullRequestDetail` the watch reads (`Pick<…, headSha | checks |
/// mergeability | viewer | author>`). `author` and `viewer` are logins.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchDetail {
    pub head_sha: Option<String>,
    pub checks: Vec<PullRequestCheck>,
    pub mergeability: PullRequestMergeability,
    pub viewer: Option<String>,
    pub author: Option<String>,
}

// "action-required" is a finished check that needs someone, so the agent hears about it.
fn is_failed_check(check: &PullRequestCheck) -> bool {
    matches!(
        check.status,
        PullRequestCheckStatus::Failure
            | PullRequestCheckStatus::Cancelled
            | PullRequestCheckStatus::ActionRequired
    )
}

/// `Date.parse` as milliseconds. An unparseable string is `NaN`, which fails
/// every comparison, the way it does upstream.
fn parse_millis(value: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.timestamp_millis() as f64)
        .unwrap_or(f64::NAN)
}

/// `Math.max(...values)`: `NaN` if any value is `NaN`.
fn js_max(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().fold(f64::NEG_INFINITY, |acc, value| {
        if acc.is_nan() || value.is_nan() {
            f64::NAN
        } else {
            acc.max(value)
        }
    })
}

/// `editedAt ?? createdAt`. An edit counts as new activity.
fn active_at(remark: &PullRequestComment) -> &str {
    match &remark.edited_at {
        Some(Some(edited)) => edited,
        _ => &remark.created_at,
    }
}

fn lowercase_login(login: Option<&str>) -> Option<String> {
    login.map(str::to_lowercase)
}

fn names(checks: &[PullRequestCheck]) -> Vec<TrimmedNonEmptyString> {
    checks.iter().map(|check| check.name.clone()).collect()
}

/// Compares a watched pull request with what its agent was last told. Each check is reported as
/// soon as it fails, so a check that never finishes (an advisory review bot) cannot hold the
/// news back. "Passed" is reported once the checks the base branch requires all passed, or all
/// checks where the host marks none required. Remarks count when someone other than the agent's
/// own account wrote them, so its own replies never wake it. `remarks` is `None` when the
/// conversation could not be read; remarks then wait for a later pass.
pub fn evaluate_pull_request_watch(
    watch: &ThreadPullRequestWatch,
    detail: &WatchDetail,
    remarks: Option<&[PullRequestComment]>,
) -> PullRequestWatchReport {
    let mut changes = Vec::new();
    let head_sha = detail.head_sha.clone();
    let head_moved = head_sha.as_deref() != watch.head_sha.as_ref().map(|sha| sha.as_str());

    // An empty list keeps the last state: a host can answer with one when its check read fails.
    let mut failed_checks = if head_moved {
        Vec::new()
    } else {
        watch.failed_checks.clone()
    };
    let mut passed = if head_moved { false } else { watch.passed };
    let mut passed_checks = if head_moved {
        Vec::new()
    } else {
        watch.passed_checks.clone()
    };
    if !detail.checks.is_empty() {
        let failed: Vec<PullRequestCheck> = detail
            .checks
            .iter()
            .filter(|check| is_failed_check(check))
            .cloned()
            .collect();
        let newly_failed: Vec<PullRequestCheck> = failed
            .iter()
            .filter(|check| !failed_checks.iter().any(|name| name == &check.name))
            .cloned()
            .collect();
        if !newly_failed.is_empty() {
            changes.push(PullRequestWatchChange::ChecksFailed {
                failed: newly_failed,
            });
        }
        // A check that runs again leaves the list, so a rerun that fails again is reported.
        failed_checks = names(&failed);

        let required: Vec<PullRequestCheck> = detail
            .checks
            .iter()
            .filter(|check| check.required == Some(true))
            .cloned()
            .collect();
        let gate: &[PullRequestCheck] = if required.is_empty() {
            &detail.checks
        } else {
            &required
        };
        let passed_now = gate.iter().all(|check| {
            check.status != PullRequestCheckStatus::Pending && !is_failed_check(check)
        });
        let gate_names = names(gate);
        // A watch saved before passedChecks existed takes the current names, so it does not wake.
        let told: Vec<TrimmedNonEmptyString> = if passed && passed_checks.is_empty() {
            gate_names.clone()
        } else {
            passed_checks.clone()
        };
        // A required job created and finished between two passes is never seen pending. Without
        // required checks, any check counts, and advisory bots keep adding passed ones: no wake.
        let gate_grew = !required.is_empty()
            && gate_names
                .iter()
                .any(|name| !told.iter().any(|seen| seen == name));
        if passed_now && (!passed || gate_grew) {
            changes.push(PullRequestWatchChange::ChecksPassed {
                count: gate.len(),
                required: !required.is_empty(),
            });
        }
        passed = passed_now;
        passed_checks = if passed_now { gate_names } else { Vec::new() };
    }

    let own = lowercase_login(detail.viewer.as_deref().or(detail.author.as_deref()));
    let through = parse_millis(&watch.remarks_through);
    // GitHub times are per second, so remarks at the boundary time are told apart by ID.
    let is_fresh = |remark: &PullRequestComment| {
        let at = parse_millis(active_at(remark));
        (at > through || (at == through && !watch.remark_ids.iter().any(|id| id == &remark.id)))
            && lowercase_login(remark.author.as_ref().map(|actor| actor.login.as_str())) != own
    };
    let fresh: Vec<PullRequestComment> = remarks
        .unwrap_or(&[])
        .iter()
        .filter(|remark| is_fresh(remark))
        .cloned()
        .collect();
    if !fresh.is_empty() {
        changes.push(PullRequestWatchChange::Remarks {
            remarks: fresh.clone(),
        });
    }
    let latest = js_max(
        std::iter::once(through).chain(fresh.iter().map(|remark| parse_millis(active_at(remark)))),
    );
    let at_latest: Vec<&PullRequestComment> = fresh
        .iter()
        .filter(|remark| parse_millis(active_at(remark)) == latest)
        .collect();
    // An all-NaN latest matches nothing, so the watermark stays where it was.
    let remarks_through = match at_latest.first() {
        Some(first) if latest != through => active_at(first).to_owned(),
        _ => watch.remarks_through.clone(),
    };
    let mut remark_ids = if latest == through {
        watch.remark_ids.clone()
    } else {
        Vec::new()
    };
    remark_ids.extend(at_latest.iter().map(|remark| remark.id.clone()));

    if detail.mergeability == PullRequestMergeability::Conflicting && !watch.conflicting {
        changes.push(PullRequestWatchChange::Conflicting);
    }
    // "unknown" is GitHub still computing after a push; only a clean answer clears a conflict.
    let conflicting = match detail.mergeability {
        PullRequestMergeability::Unknown => watch.conflicting,
        PullRequestMergeability::Conflicting => true,
        PullRequestMergeability::Mergeable => false,
    };

    let comments_only = !changes.is_empty()
        && changes
            .iter()
            .all(|change| matches!(change, PullRequestWatchChange::Remarks { .. }));
    let progress = head_moved || (!changes.is_empty() && !comments_only);
    let wakes = (if progress { 0 } else { watch.wakes }) + u64::from(comments_only);
    let exhausted = comments_only && wakes >= PULL_REQUEST_WATCH_WAKE_LIMIT;
    PullRequestWatchReport {
        changes,
        next: ThreadPullRequestWatch {
            started_at: watch.started_at.clone(),
            head_sha: head_sha.and_then(|sha| TrimmedNonEmptyString::new(sha).ok()),
            failed_checks,
            passed,
            passed_checks,
            remarks_through,
            remark_ids,
            conflicting,
            wakes,
        },
        exhausted,
    }
}

/// `snippet`: strips HTML comments and collapses whitespace, then clips to
/// 200 UTF-16 units (JS `string.length` / `slice`) with a trailing `...`.
fn snippet(body: &str) -> String {
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    static SPACE: OnceLock<Regex> = OnceLock::new();
    let comment = COMMENT.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").expect("valid regex"));
    let space = SPACE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"));
    let stripped = comment.replace_all(body, " ");
    let collapsed = space.replace_all(&stripped, " ");
    let text = collapsed.trim();
    if js_len(text) <= SNIPPET_LENGTH {
        return text.to_owned();
    }
    let units: Vec<u16> = text.encode_utf16().take(SNIPPET_LENGTH - 3).collect();
    format!("{}...", String::from_utf16_lossy(&units))
}

fn listed<T>(items: &[T], line: impl Fn(&T) -> String) -> Vec<String> {
    let mut lines: Vec<String> = items.iter().take(LISTED_ITEMS).map(line).collect();
    if items.len() > LISTED_ITEMS {
        lines.push(format!("  - and {} more", items.len() - LISTED_ITEMS));
    }
    lines
}

/// Kebab-case status names, as the wire form spells them.
fn check_status_name(status: &PullRequestCheckStatus) -> &'static str {
    match status {
        PullRequestCheckStatus::Pending => "pending",
        PullRequestCheckStatus::ActionRequired => "action-required",
        PullRequestCheckStatus::Success => "success",
        PullRequestCheckStatus::Failure => "failure",
        PullRequestCheckStatus::Skipped => "skipped",
        PullRequestCheckStatus::Neutral => "neutral",
        PullRequestCheckStatus::Cancelled => "cancelled",
    }
}

struct ChangeContext<'a> {
    base_branch: &'a str,
    commit: String,
}

fn change_lines(change: &PullRequestWatchChange, context: &ChangeContext<'_>) -> Vec<String> {
    match change {
        PullRequestWatchChange::ChecksFailed { failed } => {
            let mut lines = vec![format!("- Checks failed{}:", context.commit)];
            lines.extend(listed(failed, |check| {
                let status = if check.status == PullRequestCheckStatus::Failure {
                    String::new()
                } else {
                    format!(" ({})", check_status_name(&check.status))
                };
                let url = check
                    .url
                    .as_deref()
                    .filter(|url| !url.is_empty())
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default();
                format!("  - {}{status}{url}", check.name)
            }));
            lines
        }
        PullRequestWatchChange::ChecksPassed { count, required } => {
            let kind = if *required { "required " } else { "" };
            let noun = if *count == 1 { "check" } else { "checks" };
            vec![format!(
                "- All {count} {kind}{noun} passed{}.",
                context.commit
            )]
        }
        PullRequestWatchChange::Remarks { remarks } => {
            let noun = if remarks.len() == 1 {
                "comment"
            } else {
                "comments"
            };
            let mut lines = vec![format!("- {} new {noun}:", remarks.len())];
            lines.extend(listed(remarks, |remark| {
                let where_ = remark
                    .path
                    .as_deref()
                    .map(|path| format!(" on {path}"))
                    .unwrap_or_default();
                let body = snippet(&remark.body);
                let said = if body.is_empty() {
                    remark
                        .review_state
                        .clone()
                        .unwrap_or_else(|| "reviewed".to_owned())
                } else {
                    format!("\"{body}\"")
                };
                let author = remark
                    .author
                    .as_ref()
                    .map_or("someone", |actor| actor.login.as_str());
                let url = remark
                    .url
                    .as_deref()
                    .filter(|url| !url.is_empty())
                    .map(|url| format!(" {url}"))
                    .unwrap_or_default();
                format!("  - {author}{where_}: {said}{url}")
            }));
            lines
        }
        PullRequestWatchChange::Conflicting => {
            vec![format!(
                "- The branch now conflicts with {}.",
                context.base_branch
            )]
        }
    }
}

fn summary_label(change: &PullRequestWatchChange) -> &'static str {
    match change {
        PullRequestWatchChange::ChecksFailed { .. } => "checks failed",
        PullRequestWatchChange::ChecksPassed { .. } => "checks passed",
        PullRequestWatchChange::Remarks { .. } => "new comments",
        PullRequestWatchChange::Conflicting => "merge conflict",
    }
}

pub struct PullRequestWatchMessageInput<'a> {
    pub number: u64,
    pub url: &'a str,
    pub base_branch: &'a str,
    pub head_sha: Option<&'a str>,
    pub report: &'a PullRequestWatchReport,
}

/// The wake the agent reads and the timeline notification the user sees.
pub fn pull_request_watch_message(
    input: PullRequestWatchMessageInput<'_>,
) -> (String, OrchestrationV2Notification) {
    let report = input.report;
    let context = ChangeContext {
        base_branch: input.base_branch,
        commit: input
            .head_sha
            .map(|sha| format!(" on {}", sha.chars().take(7).collect::<String>()))
            .unwrap_or_default(),
    };
    let mut lines = vec![format!(
        "Update on pull request #{} ({}), which T3 Code is watching for you:",
        input.number, input.url
    )];
    lines.extend(
        report
            .changes
            .iter()
            .flat_map(|change| change_lines(change, &context)),
    );
    lines.push(String::new());
    lines.push(if report.exhausted {
        format!(
            "T3 Code stopped watching after {PULL_REQUEST_WATCH_WAKE_LIMIT} comment-only updates in a row. Call watch_pull_request to watch it again."
        )
    } else {
        "Look into each item and act on it as your task requires. T3 Code keeps watching and wakes you on the next change, so end your turn when you are done. When you hand the work back to the user, call unwatch_pull_request first so the thread returns to their inbox.".to_owned()
    });
    let text = lines.join("\n");

    let failed = report.changes.iter().any(|change| {
        matches!(
            change,
            PullRequestWatchChange::ChecksFailed { .. } | PullRequestWatchChange::Conflicting
        )
    });
    let outcome = if failed {
        OrchestrationV2NotificationOutcome::Failed
    } else if report
        .changes
        .iter()
        .all(|change| matches!(change, PullRequestWatchChange::ChecksPassed { .. }))
    {
        OrchestrationV2NotificationOutcome::Completed
    } else {
        OrchestrationV2NotificationOutcome::Updated
    };
    let mut summary: Vec<&str> = report.changes.iter().map(summary_label).collect();
    if report.exhausted {
        summary.push("stopped watching");
    }
    let notification = OrchestrationV2Notification {
        source: OrchestrationV2NotificationSource::Monitor,
        outcome,
        summary: TrimmedNonEmptyString::new(format!("#{}: {}", input.number, summary.join(", ")))
            .expect("summary starts with the pull request number"),
        detail: None,
    };
    (text, notification)
}

#[cfg(test)]
mod tests;
