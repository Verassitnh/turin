//! The `to*` normalizers: raw GitHub strings and shapes into the neutral types.

use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheck, PullRequestCheckStatus, PullRequestChecksState,
    PullRequestComment, PullRequestCommentKind, PullRequestCommit, PullRequestLabel,
    PullRequestMergeMethod, PullRequestMergeability, PullRequestReaction,
    PullRequestReactionContent, PullRequestReviewDecision, PullRequestStackMembership,
    PullRequestState,
};

use super::raw::{
    RawActivityCommitAuthor, RawActor, RawCheck, RawComment, RawDetailExtras, RawLabel,
    RawListCommon, RawPageInfo, RawReactionGroups, RawReview, RawStackMembership, RawViewerFields,
};
use super::types::{GitHubPullRequestDetail, GitHubPullRequestListItem, GitHubViewerFlags};
use crate::shared::js::js_trim;
use crate::source_control_core::pull_request_checks::{CheckEntry, dedupe_checks};

/// `trimmed`: the string without surrounding whitespace, or `None` when nothing is left.
pub(super) fn trimmed(value: Option<&str>) -> Option<String> {
    trimmed_ref(value).map(str::to_owned)
}

pub(super) fn trimmed_ref(value: Option<&str>) -> Option<&str> {
    let text = js_trim(value?);
    (!text.is_empty()).then_some(text)
}

/// A contract string that must be non-blank. Every call site trims first, so this only returns
/// `None` for a value the contract could not carry anyway.
pub(super) fn non_empty(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// A count the neutral types hold unsigned.
pub(super) fn count(value: i64) -> u64 {
    value.max(0) as u64
}

/// Null once a connection has nothing further, which is what ends every walk. GitHub sends an
/// `endCursor` on a page that is also the last one, so the flag is what decides, not the cursor's
/// presence.
pub(super) fn next_cursor_of(page_info: Option<&RawPageInfo>) -> Option<String> {
    match page_info {
        Some(info) if info.has_next_page == Some(true) => trimmed(info.end_cursor.as_deref()),
        _ => None,
    }
}

/// The viewer's standing on one pull request. The two halves take opposite defaults on purpose.
///
/// Updating is a permission, so an install that does not report it grants it and lets the host's
/// own refusal explain anything that fails. Authorship is not a permission but a fact about who
/// wrote the thing, and it is read to decide what an author may do to their own change, so an
/// unknown answer is "not the author", which grants nothing it should not.
pub(super) fn to_pull_request_viewer_fields(raw: Option<&RawViewerFields>) -> GitHubViewerFlags {
    GitHubViewerFlags {
        can_update: raw.and_then(|raw| raw.viewer_can_update) != Some(false),
        did_author: raw.and_then(|raw| raw.viewer_did_author) == Some(true),
    }
}

pub(super) fn to_actor(raw: Option<&RawActor>) -> Option<PullRequestActor> {
    let raw = raw?;
    let login = non_empty(trimmed_ref(raw.login.as_deref())?)?;
    Some(PullRequestActor {
        is_bot: (raw.typename.as_deref() == Some("Bot") || raw.is_bot == Some(true))
            .then_some(true),
        login,
        name: trimmed(raw.name.as_deref()),
        avatar_url: trimmed(raw.avatar_url.as_deref()),
    })
}

/// A requested team as a reviewer row. It goes by `org/slug`, the handle GitHub mentions it by.
/// No GitHub login contains a slash, so a team never shares a row or a verdict with a user.
pub(super) fn to_requested_team(
    combined_slug: Option<&str>,
    name: Option<&str>,
    avatar_url: Option<&str>,
) -> Option<PullRequestActor> {
    let login = non_empty(trimmed_ref(combined_slug)?)?;
    Some(PullRequestActor {
        is_bot: None,
        login,
        name: trimmed(name),
        avatar_url: trimmed(avatar_url),
    })
}

/// An author off `gh pr view --json commits`'s flat shape (`login` copied off the signature).
pub(super) fn to_commit_actor(
    login: Option<&str>,
    name: Option<&str>,
    email: Option<&str>,
) -> Option<PullRequestActor> {
    // An email-linked GitHub account has a login; an unlinked signature only has a name or email.
    // Keep that signature visible instead of silently turning a co-authored commit into one author.
    let login = trimmed_ref(login)
        .or_else(|| trimmed_ref(name))
        .or_else(|| trimmed_ref(email))?;
    Some(PullRequestActor {
        is_bot: None,
        login: non_empty(login)?,
        name: trimmed(name),
        avatar_url: None,
    })
}

/// An author off the GraphQL commits connection, which names an account by `user.login` where
/// `gh pr view --json commits` names it by a flat `login` copied off the signature.
pub(super) fn to_graphql_commit_actor(
    user_login: Option<&str>,
    name: Option<&str>,
    avatar_url: Option<&str>,
) -> Option<PullRequestActor> {
    let login = trimmed_ref(user_login).or_else(|| trimmed_ref(name))?;
    Some(PullRequestActor {
        is_bot: None,
        login: non_empty(login)?,
        name: trimmed(name),
        avatar_url: trimmed(avatar_url),
    })
}

pub(super) fn to_state(state: Option<&str>, merged_at: Option<&str>) -> PullRequestState {
    if trimmed_ref(merged_at).is_some() {
        return PullRequestState::Merged;
    }
    match state.map(|state| js_trim(state).to_uppercase()).as_deref() {
        Some("MERGED") => PullRequestState::Merged,
        Some("CLOSED") => PullRequestState::Closed,
        _ => PullRequestState::Open,
    }
}

pub(super) fn to_mergeability(value: Option<&str>) -> PullRequestMergeability {
    match value.map(|value| js_trim(value).to_uppercase()).as_deref() {
        Some("MERGEABLE") => PullRequestMergeability::Mergeable,
        Some("CONFLICTING") => PullRequestMergeability::Conflicting,
        _ => PullRequestMergeability::Unknown,
    }
}

pub(super) fn to_merge_method(value: Option<&str>) -> Option<PullRequestMergeMethod> {
    match value.map(|value| js_trim(value).to_uppercase()).as_deref() {
        Some("MERGE") => Some(PullRequestMergeMethod::Merge),
        Some("SQUASH") => Some(PullRequestMergeMethod::Squash),
        Some("REBASE") => Some(PullRequestMergeMethod::Rebase),
        _ => None,
    }
}

pub(super) fn to_review_decision(value: Option<&str>) -> Option<PullRequestReviewDecision> {
    match value.map(|value| js_trim(value).to_uppercase()).as_deref() {
        Some("APPROVED") => Some(PullRequestReviewDecision::Approved),
        Some("CHANGES_REQUESTED") => Some(PullRequestReviewDecision::ChangesRequested),
        Some("REVIEW_REQUIRED") => Some(PullRequestReviewDecision::ReviewRequired),
        _ => None,
    }
}

/// GitHub's own `reviewDecision` counts only reviews that satisfy the branch rules, so an
/// approval from an app (a review bot) or from anyone without the required permission leaves it
/// empty. The reviewers still said something, and a row should show it: when GitHub reports no
/// verdict, the latest review per reviewer decides, changes requested outranking approval.
///
/// `latest_review_states` is one entry per latest review: its `state`, if any.
pub(super) fn to_review_decision_with_reviews(
    value: Option<&str>,
    latest_review_states: &[Option<&str>],
) -> Option<PullRequestReviewDecision> {
    let summarized = to_review_decision(value);
    if matches!(
        summarized,
        Some(PullRequestReviewDecision::Approved)
            | Some(PullRequestReviewDecision::ChangesRequested)
    ) {
        return summarized;
    }
    let states: Vec<String> = latest_review_states
        .iter()
        .map(|state| {
            state
                .map(|state| js_trim(state).to_uppercase())
                .unwrap_or_default()
        })
        .collect();
    if states.iter().any(|state| state == "CHANGES_REQUESTED") {
        return Some(PullRequestReviewDecision::ChangesRequested);
    }
    if states.iter().any(|state| state == "APPROVED") {
        return Some(PullRequestReviewDecision::Approved);
    }
    summarized
}

pub(super) fn to_labels(raw: &[&RawLabel]) -> Vec<PullRequestLabel> {
    raw.iter()
        .filter_map(|label| {
            let name = non_empty(trimmed_ref(Some(&label.name))?)?;
            Some(PullRequestLabel {
                name,
                color: trimmed(label.color.as_deref()),
            })
        })
        .collect()
}

/// A review request as the listing normalizers read it: a user's login, or a team's slug / name.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ReviewRequestRef<'a> {
    pub login: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub name: Option<&'a str>,
}

/// User review requests only. Team requests are tracked separately because a slug cannot be
/// compared with the viewer's login.
pub(super) fn to_review_request_logins(raw: &[ReviewRequestRef<'_>]) -> Vec<String> {
    raw.iter()
        .filter_map(|request| trimmed(request.login))
        .collect()
}

pub(super) fn has_team_review_request(raw: &[ReviewRequestRef<'_>]) -> bool {
    raw.iter().any(|request| {
        trimmed_ref(request.login).is_none()
            && (trimmed_ref(request.slug).is_some() || trimmed_ref(request.name).is_some())
    })
}

pub(super) fn to_check_status(raw: &RawCheck) -> PullRequestCheckStatus {
    // Commit statuses report a single `state`; check runs report `status` plus a `conclusion`
    // that only exists once the run has completed.
    let status = raw
        .status
        .as_deref()
        .map(|status| js_trim(status).to_uppercase());
    if let Some(status) = &status
        && status != "COMPLETED"
        && !status.is_empty()
    {
        return PullRequestCheckStatus::Pending;
    }
    let verdict = raw.conclusion.as_deref().or(raw.state.as_deref());
    match verdict
        .map(|verdict| js_trim(verdict).to_uppercase())
        .as_deref()
    {
        Some("SUCCESS") => PullRequestCheckStatus::Success,
        Some("ACTION_REQUIRED") => PullRequestCheckStatus::ActionRequired,
        Some("FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE") => {
            PullRequestCheckStatus::Failure
        }
        Some("CANCELLED") => PullRequestCheckStatus::Cancelled,
        Some("SKIPPED") => PullRequestCheckStatus::Skipped,
        Some("PENDING" | "EXPECTED") => PullRequestCheckStatus::Pending,
        _ => PullRequestCheckStatus::Neutral,
    }
}

/// What GitHub writes where a run has not reached that moment yet, which is not a time.
const UNSET_TIMESTAMP: &str = "0001-01-01T00:00:00Z";

/// Sorts after every real timestamp, so the deduper keeps a run that has not finished.
const STILL_RUNNING: &str = "9999-12-31T23:59:59Z";

fn real_timestamp(value: Option<&str>) -> Option<String> {
    let at = trimmed(value)?;
    (at != UNSET_TIMESTAMP).then_some(at)
}

/// Only a row the rollup gives no name of any kind, which is not a check anyone can show.
fn is_nameless_check(raw: &RawCheck) -> bool {
    trimmed_ref(raw.name.as_deref()).is_none() && trimmed_ref(raw.context.as_deref()).is_none()
}

/// The rollup as the deduper reads it: a check, the workflow that owns it, and when the run last
/// had something to say. A run that has not finished outranks any finished run of the same check:
/// a workflow triggered by both `push` and `pull_request` runs the check twice on one commit, and
/// the run that finished first must not hide the one still going.
fn to_check_entries(raw: &[RawCheck]) -> Vec<CheckEntry> {
    raw.iter()
        .filter_map(|check| {
            let name = trimmed_ref(check.name.as_deref())
                .or_else(|| trimmed_ref(check.context.as_deref()))?;
            let status = to_check_status(check);
            Some(CheckEntry {
                check: PullRequestCheck {
                    name: non_empty(name)?,
                    status,
                    description: trimmed(check.description.as_deref()),
                    url: trimmed(check.details_url.as_deref())
                        .or_else(|| trimmed(check.target_url.as_deref())),
                    required: check.is_required,
                },
                workflow_name: trimmed(check.workflow_name.as_deref()),
                at: if status == PullRequestCheckStatus::Pending {
                    Some(STILL_RUNNING.to_owned())
                } else {
                    real_timestamp(check.completed_at.as_deref())
                        .or_else(|| real_timestamp(check.started_at.as_deref()))
                },
            })
        })
        .collect()
}

pub(super) fn to_checks(raw: Option<&[RawCheck]>) -> Vec<PullRequestCheck> {
    dedupe_checks(to_check_entries(raw.unwrap_or_default()))
}

/// The one word a listing row has space for. A failure outranks anything still running, the way
/// GitHub's own indicator reads: a run that has already gone red will not go green by finishing.
///
/// Null rather than "passing" for a head commit with no checks at all, so a repository that runs
/// none shows nothing instead of a green tick it never earned. A cancelled run is a failure, as
/// GitHub's own rollup and the client's detail rollup both read it; skipped and neutral count
/// towards neither, so the row and the detail header never disagree about one head commit.
///
/// Counted off the deduped checks rather than the raw rollup, so the word and the list under it
/// cannot disagree: the run a re-run replaced is not a verdict twice. A row with no name at all is
/// counted as it comes, since the cross-repository search dresses GitHub's own rollup enum as one
/// nameless row, and nothing nameless can collide with anything.
pub(super) fn rollup_checks_state(raw: Option<&[RawCheck]>) -> Option<PullRequestChecksState> {
    let raw_slice = raw.unwrap_or_default();
    let statuses: Vec<PullRequestCheckStatus> = to_checks(raw)
        .into_iter()
        .map(|check| check.status)
        .chain(
            raw_slice
                .iter()
                .filter(|check| is_nameless_check(check))
                .map(to_check_status),
        )
        .collect();
    if statuses.is_empty() {
        return None;
    }
    if statuses.contains(&PullRequestCheckStatus::Failure)
        || statuses.contains(&PullRequestCheckStatus::Cancelled)
    {
        return Some(PullRequestChecksState::Failing);
    }
    if statuses.contains(&PullRequestCheckStatus::Pending)
        || statuses.contains(&PullRequestCheckStatus::ActionRequired)
    {
        return Some(PullRequestChecksState::Pending);
    }
    statuses
        .contains(&PullRequestCheckStatus::Success)
        .then_some(PullRequestChecksState::Passing)
}

/// The states that are a verdict in themselves, rather than a wrapper around line comments.
fn is_review_verdict(review_state: Option<&str>) -> bool {
    matches!(
        review_state.map(str::to_uppercase).as_deref(),
        Some("APPROVED" | "CHANGES_REQUESTED" | "DISMISSED")
    )
}

/// Issue comments and reviews as one flat, time-ordered conversation.
pub(super) fn to_comments(
    comments: &[RawComment],
    reviews: &[RawReview],
) -> Vec<PullRequestComment> {
    let issue_comments = comments.iter().filter_map(|comment| {
        Some(PullRequestComment {
            id: non_empty(&comment.id)?,
            kind: PullRequestCommentKind::IssueComment,
            author: to_actor(comment.author.as_ref()),
            body: comment.body.clone().unwrap_or_default(),
            created_at: comment.created_at.clone(),
            edited_at: Some(comment.last_edited_at.clone()),
            url: trimmed(comment.url.as_deref()),
            path: None,
            review_state: None,
            reactions: None,
            can_edit: None,
        })
    });
    // A review with no body is kept only when its state is the event itself: an approval, a
    // request for changes, a dismissal. GitHub also opens a bodiless `COMMENTED` review as the
    // container for line comments, and those comments are read from the review threads, so
    // keeping the container too would show a row with a name and nothing under it.
    let reviews = reviews.iter().filter_map(|review| {
        let submitted_at = trimmed(review.submitted_at.as_deref())?;
        let review_state = trimmed(review.state.as_deref());
        if js_trim(review.body.as_deref().unwrap_or_default()).is_empty()
            && !is_review_verdict(review_state.as_deref())
        {
            return None;
        }
        Some(PullRequestComment {
            id: non_empty(&review.id)?,
            kind: PullRequestCommentKind::Review,
            author: to_actor(review.author.as_ref()),
            body: review.body.clone().unwrap_or_default(),
            created_at: submitted_at,
            edited_at: Some(review.last_edited_at.clone()),
            url: trimmed(review.url.as_deref()),
            path: None,
            review_state,
            reactions: None,
            can_edit: None,
        })
    });
    let mut all: Vec<PullRequestComment> = issue_comments.chain(reviews).collect();
    // `localeCompare` on ISO timestamps is a plain text comparison; the sort is stable.
    all.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    all
}

/// The authors of an activity-page commit, in `toCommitActor`'s terms.
pub(super) fn to_activity_commit_authors(
    authors: &[RawActivityCommitAuthor],
) -> Vec<PullRequestActor> {
    authors
        .iter()
        .filter_map(|author| {
            to_commit_actor(
                author.user.as_ref().and_then(|user| user.login.as_deref()),
                author.name.as_deref(),
                author.email.as_deref(),
            )
        })
        .collect()
}

pub(super) fn to_commit(
    oid: &str,
    message_headline: Option<&str>,
    committed_date: &str,
    authors: Vec<PullRequestActor>,
) -> Option<PullRequestCommit> {
    Some(PullRequestCommit {
        oid: non_empty(oid)?,
        message_headline: message_headline.unwrap_or_default().to_owned(),
        committed_date: committed_date.to_owned(),
        additions: None,
        deletions: None,
        authors: Some(authors),
    })
}

/// A listing row's inputs, after the shape it arrived in (`gh` flat arrays, or GraphQL
/// connections) has been reduced to the same plain lists.
pub(super) struct ListInput<'a> {
    pub common: &'a RawListCommon,
    pub latest_review_states: Vec<Option<&'a str>>,
    pub review_requests: Vec<ReviewRequestRef<'a>>,
    pub labels: Vec<&'a RawLabel>,
    pub status_check_rollup: Option<&'a [RawCheck]>,
}

pub(super) fn to_list_item(input: &ListInput<'_>) -> GitHubPullRequestListItem {
    let raw = input.common;
    GitHubPullRequestListItem {
        stack: None,
        author_id: trimmed(raw.author.as_ref().and_then(|author| author.id.as_deref())),
        number: raw.number.non_negative(),
        title: raw.title.clone(),
        url: raw.url.clone(),
        author: to_actor(raw.author.as_ref()),
        head_branch: raw.head_ref_name.clone(),
        base_branch: raw.base_ref_name.clone(),
        state: to_state(raw.state.as_deref(), raw.merged_at.as_deref()),
        is_draft: raw.is_draft.unwrap_or(false),
        mergeability: to_mergeability(raw.mergeable.as_deref()),
        review_decision: to_review_decision_with_reviews(
            raw.review_decision.as_deref(),
            &input.latest_review_states,
        ),
        additions: raw.additions.map_or(0, |value| value.non_negative()),
        deletions: raw.deletions.map_or(0, |value| value.non_negative()),
        created_at: raw.created_at.clone(),
        updated_at: raw.updated_at.clone(),
        review_request_logins: to_review_request_logins(&input.review_requests),
        has_team_review_request: has_team_review_request(&input.review_requests),
        labels: to_labels(&input.labels),
        checks_state: rollup_checks_state(input.status_check_rollup),
    }
}

pub(super) fn to_detail(
    input: &ListInput<'_>,
    extras: &RawDetailExtras,
) -> GitHubPullRequestDetail {
    let auto_merge_method = to_merge_method(
        extras
            .auto_merge_request
            .as_ref()
            .and_then(|request| request.as_ref())
            .and_then(|request| request.merge_method.as_deref()),
    );
    GitHubPullRequestDetail {
        base: to_list_item(input),
        is_cross_repository: extras.is_cross_repository,
        head_repository_owner: trimmed(
            extras
                .head_repository_owner
                .as_ref()
                .map(|owner| owner.login.as_str()),
        ),
        head_sha: trimmed(extras.head_ref_oid.as_deref()),
        body: extras.body.clone().unwrap_or_default(),
        changed_files: extras.changed_files.map_or(0, |value| value.non_negative()),
        merged_at: trimmed(input.common.merged_at.as_deref()),
        closed_at: trimmed(extras.closed_at.as_deref()),
        checks: to_checks(input.status_check_rollup),
        // A JSON null is GitHub saying "nobody armed this"; a missing key is GitHub not saying,
        // and the difference survives here rather than being flattened into false.
        auto_merge_enabled: extras
            .auto_merge_request
            .as_ref()
            .map(|request| request.is_some()),
        auto_merge_method,
    }
}

pub(super) fn to_stack_membership(raw: &RawStackMembership) -> Option<PullRequestStackMembership> {
    let stack = raw.stack.as_ref()?.as_ref()?;
    let entry = raw.stack_entry.as_ref()?;
    Some(PullRequestStackMembership {
        number: PositiveInt::new(u64::try_from(stack.number.0).ok()?)?,
        position: PositiveInt::new(u64::try_from(entry.position.0).ok()?)?,
        size: PositiveInt::new(u64::try_from(stack.size.0).ok()?)?,
        base: non_empty(&stack.base_ref_name)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------------------------

/// GitHub's reaction names, which are the same eight the contract carries under other spellings.
fn reaction_content_by_github(name: &str) -> Option<PullRequestReactionContent> {
    Some(match name {
        "THUMBS_UP" => PullRequestReactionContent::ThumbsUp,
        "THUMBS_DOWN" => PullRequestReactionContent::ThumbsDown,
        "LAUGH" => PullRequestReactionContent::Laugh,
        "HOORAY" => PullRequestReactionContent::Hooray,
        "CONFUSED" => PullRequestReactionContent::Confused,
        "HEART" => PullRequestReactionContent::Heart,
        "ROCKET" => PullRequestReactionContent::Rocket,
        "EYES" => PullRequestReactionContent::Eyes,
        _ => return None,
    })
}

/// `gitHubReactionContent`: the name GitHub's mutations take for a contract reaction.
pub fn github_reaction_content(content: PullRequestReactionContent) -> &'static str {
    match content {
        PullRequestReactionContent::ThumbsUp => "THUMBS_UP",
        PullRequestReactionContent::ThumbsDown => "THUMBS_DOWN",
        PullRequestReactionContent::Laugh => "LAUGH",
        PullRequestReactionContent::Hooray => "HOORAY",
        PullRequestReactionContent::Confused => "CONFUSED",
        PullRequestReactionContent::Heart => "HEART",
        PullRequestReactionContent::Rocket => "ROCKET",
        PullRequestReactionContent::Eyes => "EYES",
    }
}

/// The groups GitHub answered with, as the contract carries them. A group with nobody behind it
/// is dropped: GitHub answers with a group per content it knows, including the ones nobody
/// chose. The viewer's own login is left out of `actors` (the page names them "You" instead, and
/// leaving it in would name them twice) but `count` still counts them along with everyone else.
pub(super) fn to_reactions(
    groups: &RawReactionGroups,
    viewer: Option<&str>,
) -> Vec<PullRequestReaction> {
    let normalized_viewer = viewer.map(str::to_lowercase);
    let mut reactions = Vec::new();
    for group in groups.as_deref().unwrap_or_default() {
        let Some(content) = trimmed_ref(group.content.as_deref())
            .map(str::to_uppercase)
            .and_then(|name| reaction_content_by_github(&name))
        else {
            continue;
        };
        let logins: Vec<String> = group
            .reactors
            .as_ref()
            .and_then(|reactors| reactors.nodes.as_deref())
            .unwrap_or_default()
            .iter()
            .filter_map(|node| trimmed(node.as_ref().and_then(|node| node.login.as_deref())))
            .collect();
        let total = group
            .reactors
            .as_ref()
            .and_then(|reactors| reactors.total_count)
            .map_or(logins.len() as i64, |total| total.0);
        let reaction_count = total.max(logins.len() as i64);
        let Some(reaction_count) = u64::try_from(reaction_count)
            .ok()
            .and_then(PositiveInt::new)
        else {
            continue;
        };
        let actors = logins
            .iter()
            .filter(|login| match &normalized_viewer {
                None => true,
                Some(viewer) => &login.to_lowercase() != viewer,
            })
            .filter_map(|login| non_empty(login))
            .collect();
        reactions.push(PullRequestReaction {
            content,
            count: reaction_count,
            actors,
            viewer_has_reacted: group.viewer_has_reacted == Some(true),
        });
    }
    reactions
}

// ---------------------------------------------------------------------------------------------
// Repository role
// ---------------------------------------------------------------------------------------------

/// Whether the viewer's role on the repository is one that can push, which is what merging needs.
/// TRIAGE and READ are not: a triager moves issues about and neither of them lands a commit.
///
/// `viewer_permission` is `None` where the install reported no permission at all and
/// `Some(None)` for a JSON `null`. An install that reports no permission does not count as write.
/// This is the exception to "an unknown permission is granted": write is what merging and closing
/// somebody else's change need, and offering those to a reader who cannot use them wastes the
/// press and reads as the app being wrong. Everything softer (commenting, reviewing, resolving)
/// keeps the granting default, because being unable to say something is the worse failure there.
///
/// A null permission is not that. GitHub answers null only to a GitHub App installation, and no
/// API says what an installation may do, so null is granted and GitHub refuses whatever the app
/// was not given. A person always gets a role, READ at the least.
pub fn to_can_write(viewer_permission: Option<Option<&str>>) -> bool {
    match viewer_permission {
        Some(None) => true,
        Some(Some(permission)) => matches!(
            js_trim(permission).to_uppercase().as_str(),
            "ADMIN" | "MAINTAIN" | "WRITE"
        ),
        None => false,
    }
}

/// Triage is the least role GitHub lets label a pull request; it is not a write.
pub(super) fn to_can_triage(viewer_permission: Option<Option<&str>>) -> bool {
    matches!(
        viewer_permission,
        Some(Some(permission)) if js_trim(permission).to_uppercase() == "TRIAGE"
    ) || to_can_write(viewer_permission)
}
