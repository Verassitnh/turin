//! Port of upstream/packages/contracts/src/pullRequest.ts
//!
//! Host-generic change-request contracts; the only host kept by Turin is
//! GitHub, but every host's data shapes stay decodable so upstream rows and
//! caches import cleanly.

use crate::base_schemas::{
    ContractError, IsoDateTime, NonNegativeInt, PositiveInt, ProjectId, ThreadId,
    TrimmedNonEmptyString, js_len, js_trim,
};
use crate::source_control::SourceControlProviderKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Defines a `deserialize_with` module that decodes `$ty` normally and then
/// runs `$check` (returning `Some(message)` on failure) over the decoded
/// value, i.e. a decode-time `Schema.check(...)`. The nested `optional`
/// module is the same check for an `Option<$ty>` field (absent/`None` pass).
macro_rules! checked_field {
    ($name:ident: $ty:ty, |$v:ident| $check:expr) => {
        #[allow(dead_code)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;

            #[allow(clippy::ptr_arg)]
            fn check($v: &$ty) -> Option<&'static str> {
                $check
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<$ty, D::Error> {
                let value = <$ty>::deserialize(d)?;
                match check(&value) {
                    Some(message) => Err(serde::de::Error::custom(message)),
                    None => Ok(value),
                }
            }

            pub mod optional {
                #[allow(unused_imports, dead_code)]
                use super::super::*;

                pub fn deserialize<'de, D: Deserializer<'de>>(
                    d: D,
                ) -> Result<Option<$ty>, D::Error> {
                    let value = Option::<$ty>::deserialize(d)?;
                    if let Some(inner) = &value {
                        if let Some(message) = super::check(inner) {
                            return Err(serde::de::Error::custom(message));
                        }
                    }
                    Ok(value)
                }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestInvolvement {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "reviewing")]
    Reviewing,
    #[serde(rename = "authored")]
    Authored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestState {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "merged")]
    Merged,
}

/// What a listing asks for, which is the three states a change request can be
/// in plus the option to span them. Separate from `PullRequestState` because
/// a change request is never "all" — only a request for one can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestListState {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "merged")]
    Merged,
}

/// Where a review stands overall, as a host that summarises its reviews reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

/// One qualifier's value, bounded because it is written into a host's own search query.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PullRequestQualifierValue(TrimmedNonEmptyString);

impl PullRequestQualifierValue {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        if js_len(value.as_str()) > 200 {
            return Err(ContractError {
                expected: "a search qualifier of at most 200 characters",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for PullRequestQualifierValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for PullRequestQualifierValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for PullRequestQualifierValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestDraftFilter {
    #[serde(rename = "only")]
    Only,
    #[serde(rename = "hide")]
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestReviewFilter {
    #[serde(rename = "approved")]
    Approved,
    #[serde(rename = "changes-requested")]
    ChangesRequested,
    #[serde(rename = "review-required")]
    ReviewRequired,
    #[serde(rename = "none")]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestChecksFilter {
    #[serde(rename = "passing")]
    Passing,
    #[serde(rename = "failing")]
    Failing,
}

// `PullRequestQualifierValues`: at most ten values; `labels`: at most ten groups.
checked_field!(qualifier_values: Vec<PullRequestQualifierValue>, |v| {
    (v.len() > 10).then_some("at most 10 qualifier values")
});
checked_field!(label_groups: Vec<Vec<PullRequestQualifierValue>>, |v| {
    if v.len() > 10 {
        Some("at most 10 label groups")
    } else if v.iter().any(|group| group.len() > 10) {
        Some("at most 10 qualifier values per label group")
    } else {
        None
    }
});

/// Narrowings beyond state and involvement, each absent by default — an
/// absent field filters nothing, which is what every listing did before there
/// were any. Optional as a whole so a page and a server of different ages
/// still speak to each other.
///
/// `checks` is host-side only: no row carries its own check state, so a host
/// that cannot match it answers unnarrowed rather than the page pretending
/// to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<PullRequestDraftFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PullRequestReviewFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<PullRequestChecksFilter>,
    /// Labels as GitHub's own search reads them: each group is one `label:`
    /// qualifier, a row must satisfy every group, and a group holding several
    /// names is satisfied by any one of them — `label:size:S,size:XS` finds
    /// either size. Typed rather than picked, since a project's labels are
    /// its own and no menu can list them. Bounded because each becomes a
    /// host search qualifier.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "label_groups::optional::deserialize"
    )]
    pub labels: Option<Vec<Vec<PullRequestQualifierValue>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "qualifier_values::optional::deserialize"
    )]
    pub excluded_labels: Option<Vec<PullRequestQualifierValue>>,
    /// One login, as `author:` names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PullRequestQualifierValue>,
}

/// The one-glyph summary of a change request's checks, as its list row wears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestChecksState {
    Passing,
    Failing,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestMergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestAction {
    #[serde(rename = "merge")]
    Merge,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "draft")]
    Draft,
    #[serde(rename = "close")]
    Close,
    #[serde(rename = "reopen")]
    Reopen,
    /// Bring the base branch's commits into this one, which is what unblocks a stale branch.
    #[serde(rename = "update-branch")]
    UpdateBranch,
    /// Hand the merge to the host: it waits for whatever it requires — a
    /// green pipeline, the approvals, an unblocked branch — and merges
    /// without anybody coming back. Separate from `merge` because the two
    /// answer different questions. `merge` is refused outright by a host
    /// that is not ready yet, and this one is for exactly that moment.
    #[serde(rename = "enable-auto-merge")]
    EnableAutoMerge,
    /// Take the standing instruction back, which leaves the change request where it was.
    #[serde(rename = "disable-auto-merge")]
    DisableAutoMerge,
    /// Open a new change request that reverses a merged one.
    #[serde(rename = "revert")]
    Revert,
    /// Allow Actions workflows from a fork pull request to begin running.
    #[serde(rename = "approve-workflows")]
    ApproveWorkflows,
}

/// How a stale branch catches up with its base: a merge commit, or a rebase
/// onto it. The two are the host's own choices, not this page's — GitHub
/// offers both and refuses a rebase it cannot replay, so what is offered
/// comes from the host and what is allowed comes from the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestUpdateMethod {
    Merge,
    Rebase,
}

/// Where the branch stands against the base it would merge into. Separate
/// from `mergeability`, which answers a different question: a branch can be
/// behind and still merge cleanly, and that pairing — out of date, no
/// conflicts — is the one an update button exists for.
///
/// "unknown" where the host was not asked or could not say, which is every
/// host but GitHub and every pull request whose head repository could not be
/// compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestBaseComparison {
    UpToDate,
    Behind,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActor {
    /// Present when the host identifies an automated account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    pub login: TrimmedNonEmptyString,
    pub name: Option<String>,
    /// Null where a host does not report one, which is what the initials fall back to.
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLabel {
    pub name: TrimmedNonEmptyString,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestCheckStatus {
    Pending,
    ActionRequired,
    Success,
    Failure,
    Skipped,
    Neutral,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCheck {
    pub name: TrimmedNonEmptyString,
    pub status: PullRequestCheckStatus,
    pub description: Option<String>,
    pub url: Option<String>,
    /// The base branch requires this check to merge. Absent where the host does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// The reactions a remark can carry. GitHub's eight, which is also what the
/// picker offers: GitLab accepts any emoji as an award, and the ones outside
/// this set are read as nothing rather than shown under a name no other host
/// would recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

/// One reaction and everyone behind it, which is what the hover on a reaction pill says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReaction {
    pub content: PullRequestReactionContent,
    pub count: PositiveInt,
    /// Who reacted, as far as the host named them. A host that reports
    /// fewer names than it counts leaves the rest out, so this is never
    /// longer than `count` and may be shorter. When `viewerHasReacted` is
    /// true, the viewer's own login is left out of this list — the page
    /// names them "You" instead — but `count` still counts them along with
    /// everyone else.
    pub actors: Vec<TrimmedNonEmptyString>,
    /// The signed-in account is one of them, so pressing the pill takes the reaction back.
    pub viewer_has_reacted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestCommentKind {
    IssueComment,
    ReviewComment,
    Review,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestComment {
    pub id: TrimmedNonEmptyString,
    pub kind: PullRequestCommentKind,
    pub author: Option<PullRequestActor>,
    pub body: String,
    pub created_at: IsoDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub edited_at: Option<Option<IsoDateTime>>,
    pub url: Option<String>,
    pub path: Option<String>,
    pub review_state: Option<String>,
    /// Absent from a host with no reactions at all, which is a different thing from none on this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<PullRequestReaction>>,
    /// The host's own answer to whether this reader may rewrite this remark.
    /// Absent where the host doesn't say, which leaves the page to guess from
    /// authorship. A remark on a line appears both here and in its review
    /// thread; a host that sets this sets it on both copies alike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
}

/// Which file a diff line belongs to: `left` is the version before the
/// change, `right` the version after. A comment has to name one, because a
/// unified diff shows both at once and the same line number means two
/// different lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestDiffSide {
    Left,
    Right,
}

/// What submitting a review says about the change, beyond the words in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestReviewVerdict {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestThreadComment {
    pub id: TrimmedNonEmptyString,
    pub author: Option<PullRequestActor>,
    pub body: String,
    pub created_at: IsoDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub edited_at: Option<Option<IsoDateTime>>,
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<PullRequestReaction>>,
    /// The host's own answer to whether this reader may rewrite this remark.
    /// Absent where the host doesn't say, which leaves the page to guess from
    /// authorship. A remark on a line appears both here and in its review
    /// thread; a host that sets this sets it on both copies alike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
}

/// A conversation anchored to a line of the diff. The detail carries these
/// alongside `comments` rather than instead of them: the timeline wants one
/// flat, chronological list, and the diff wants whole threads pinned to
/// their line — the same remarks, read two different ways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewThread {
    pub id: TrimmedNonEmptyString,
    pub path: TrimmedNonEmptyString,
    /// Null when the host anchors the thread to a file rather than to a line.
    pub line: Option<PositiveInt>,
    pub side: PullRequestDiffSide,
    pub is_resolved: bool,
    /// The line the thread was written against is no longer in the diff, so
    /// it cannot be shown against the code. Such a thread is listed
    /// separately rather than pinned to the wrong line.
    pub is_outdated: bool,
    pub comments: Vec<PullRequestThreadComment>,
    /// Host-reported total, when this thread was read in pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<NonNegativeInt>,
    /// Opaque cursor for the next comment page. Absent once this thread is whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_comments_cursor: Option<TrimmedNonEmptyString>,
    /// The host's own answer to whether this reader may resolve this thread.
    /// It can only narrow the reader's repository-wide `resolve` permission,
    /// never widen it. Absent where the host doesn't say per thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_resolve: Option<bool>,
}

/// Whether a reviewer is a person or a group of them the host addresses as
/// one. GitHub is the only host here that takes a review request for a team;
/// the others name individuals only, so nothing they report is ever anything
/// but a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestReviewerKind {
    User,
    Team,
}

/// Somebody a review may be asked of. Carries the actor fields so the menu
/// shows the same face and handle the rest of the page does, and a team
/// wears them too — its slug stands in for a login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewerCandidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    pub login: TrimmedNonEmptyString,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    /// How the host addresses this reviewer when a review is requested,
    /// which is not always the handle it shows: GitHub takes a login or a
    /// team slug, GitLab a numeric user id, Bitbucket an account uuid.
    /// Opaque to the page, which sends back whatever the candidate arrived with.
    pub id: TrimmedNonEmptyString,
    pub kind: PullRequestReviewerKind,
    /// A review has already been asked of them, so pressing them takes the request back.
    pub is_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewerCandidateList {
    /// Never includes the author: nobody is asked to review their own change request.
    pub candidates: Vec<PullRequestReviewerCandidate>,
    /// The host has more people with access than the read asked for, so
    /// somebody missing from the menu may still be somebody this change
    /// request can be sent to. Nothing is wrong with the list; it is simply
    /// not all of it.
    pub truncated: bool,
}

/// A label the repository defines, with whether this change request already wears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLabelCandidate {
    pub name: TrimmedNonEmptyString,
    pub color: Option<String>,
    pub description: Option<String>,
    pub is_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLabelCandidateList {
    pub candidates: Vec<PullRequestLabelCandidate>,
    /// The repository defines more labels than the read asked for; the list is not all of them.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCommit {
    pub oid: TrimmedNonEmptyString,
    pub message_headline: String,
    pub committed_date: IsoDateTime,
    /// Per-commit line counts where the host can return them without a request per commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<NonNegativeInt>,
    /// Everyone the host attributes the commit to. Optional because older
    /// servers and hosts that do not expose commit authors still produce a
    /// useful timeline entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<PullRequestActor>>,
}

/// What a host can do with a review, which is where the four differ most.
/// GitLab has no way to say "changes requested", and Azure DevOps exposes no
/// diff through its CLI, so it has no lines to write against at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewCapabilities {
    /// A new comment can be anchored to a line of the diff.
    pub inline_comment: bool,
    /// An existing thread can be replied to.
    pub reply: bool,
    /// A thread can be marked resolved, and unresolved again.
    pub resolve: bool,
    /// The verdicts a submitted review can carry. Empty means reviews cannot be submitted.
    pub verdicts: Vec<PullRequestReviewVerdict>,
}

/// What a host lets be rewritten after it has been posted. The two are
/// separate because a host can take one without the other: Azure DevOps
/// takes a new title and description through the same command that closes a
/// pull request, and has no way to post a remark here at all — so nothing it
/// shows in a conversation can be rewritten either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestEditCapabilities {
    /// The change request's own title and description can be rewritten.
    pub change_request: bool,
    /// A remark can be rewritten by whoever wrote it.
    pub comment: bool,
}

/// What a host can do about who reviews. The two are independent: a host can
/// take a request without publishing who may receive one, which is Azure DevOps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewerCapabilities {
    /// A review can be asked of somebody, and the request taken back.
    pub request: bool,
    /// The people who may be asked can be listed. False where the host has
    /// no such list to give, which leaves the page to take a name rather
    /// than offer a menu — a guessed list would be worse, because a name
    /// missing from it reads as a name that cannot be asked.
    pub list_candidates: bool,
}

/// Who remembers which files a reader has cleared. `host` is the host's own
/// record, so the marks are the ones its web UI shows and a review can be
/// carried on from either side. `environment` is this server's record, for
/// a host that keeps none anything outside one browser can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestViewedFilesStore {
    Host,
    Environment,
}

/// What a provider can actually do, so a surface can hide what is missing
/// rather than offer an action that would fail. Every provider fills this in
/// for itself; nothing is assumed.
///
/// Hosts differ more than they look: Azure DevOps exposes no patch through
/// its CLI, and Bitbucket has no endpoint that reopens a declined pull
/// request. Both would otherwise be dead buttons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCapabilities {
    /// A unified patch can be fetched for the change request.
    pub diff: bool,
    /// A comment can be posted, and the conversation read back.
    pub comment: bool,
    /// The actions this host can carry out; anything absent is never offered.
    pub actions: Vec<PullRequestAction>,
    /// Merge strategies the provider itself offers, before repository settings narrow them.
    pub merge_methods: Vec<PullRequestMergeMethod>,
    /// How this host can bring a stale branch up to date. Absent where it
    /// cannot at all, which is every host that has not said otherwise — so a
    /// provider that says nothing offers nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_methods: Option<Vec<PullRequestUpdateMethod>>,
    /// The host can narrow a listing by free text. False means it answers
    /// unnarrowed and whoever asked has to do the narrowing — which is a
    /// different promise, so the page is told rather than left to show every
    /// change request on that host as a search result.
    pub search: bool,
    /// Reactions can be read from a remark, and added to one or taken back.
    /// One flag for both: neither host here reports reactions it will not
    /// also take, and a surface that could show a pill it may never press is
    /// a surface offering nothing. Optional for the same reason as
    /// `updateMethods` and `edit`: a server that says nothing about
    /// reactions has none, which is what every server before this field was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<bool>,
    /// Where the reader's own marks are kept, or absent where they are kept
    /// nowhere and the checkbox is not offered at all. Optional for the same
    /// reason as `reactions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewed_files: Option<PullRequestViewedFilesStore>,
    pub review: PullRequestReviewCapabilities,
    pub reviewers: PullRequestReviewerCapabilities,
    /// What can be rewritten after the fact. Optional so a page and a server
    /// of different ages still speak to each other: a server that says
    /// nothing about rewriting is one that cannot, which is what every
    /// server before this one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<PullRequestEditCapabilities>,
    /// The host keeps stacks of change requests as objects of its own, so a
    /// linked thread can show the stack the host shows. Absent means chains
    /// are only ever inferred from base branches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stacks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_actions: Option<bool>,
    /// The repository's labels can be listed, and one put on a change
    /// request or taken off it. Optional for the same reason `edit` is: a
    /// server that says nothing about labels has no way to change them,
    /// which is what every server before this field was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<bool>,
}

/// What the signed-in account may do with this change request, which is a
/// different question from `capabilities`: that says what the host is able
/// to do at all, and this says whether this viewer is allowed to ask for
/// it. A host that merges pull requests still will not let a stranger merge
/// one, so a control belongs on the page only where the two agree.
///
/// A permission the host reports nothing about is granted rather than
/// withheld. Hiding a control from someone who may in fact use it leaves
/// them no way through and no reason given, while offering one they may not
/// use ends in the host's own refusal — which at least says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestViewerPermissions {
    /// May request remote stack rebases, including when this layer is already current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_rebase: Option<bool>,
    /// Which of the actions this viewer may take; anything absent is theirs to look at only.
    pub actions: Vec<PullRequestAction>,
    /// This viewer may write a remark: a comment, a reply, or a note against a line.
    pub comment: bool,
    /// This viewer may mark a review conversation resolved, and unresolved again.
    pub resolve: bool,
    /// The verdicts this viewer may submit a review with. Empty means they may not review.
    pub verdicts: Vec<PullRequestReviewVerdict>,
    /// This viewer may ask somebody for a review, and take the request back again.
    pub request_reviewers: bool,
    /// The ways this viewer may bring the branch up to date, narrowed from
    /// what the host offers. Absent or empty means they may not, which is
    /// also what a host with no such action says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_methods: Option<Vec<PullRequestUpdateMethod>>,
    /// This viewer may put a label on the change request, and take one off.
    /// Absent is granted, like every permission here; the capability beside
    /// it is what decides whether a label can be changed on this host at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<bool>,
    /// This viewer may rewrite the change request's title and description.
    /// Absent where the host doesn't say, which leaves the page to guess
    /// from authorship and merge access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_change_request: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestMergeCapabilities {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackMembership {
    pub number: PositiveInt,
    pub position: PositiveInt,
    pub size: PositiveInt,
    pub base: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<PullRequestStackMembership>,
    pub provider: SourceControlProviderKind,
    /// The host below which `repository` is addressed, so the same provider
    /// kind can serve more than one account — github.com and a GitHub
    /// Enterprise install are different identities.
    pub host: TrimmedNonEmptyString,
    pub project_id: ProjectId,
    pub project_title: TrimmedNonEmptyString,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub author: Option<PullRequestActor>,
    pub head_branch: TrimmedNonEmptyString,
    pub base_branch: TrimmedNonEmptyString,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    /// Zero where the host has not been asked for the counts yet, which on
    /// GitHub is every row a listing hands over: they are read afterwards,
    /// through `PullRequestListStatsInput`. Zero is what a host that reports
    /// no counts at all has always sent, and what the page already draws as
    /// no stat rather than as an empty change.
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    /// Server epoch milliseconds when the provider read started; preserved on cache hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub observed_at: Option<f64>,
    pub viewer_review_requested: bool,
    pub labels: Vec<PullRequestLabel>,
    /// Absent where the host does not summarise its reviews, which is every host but GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<PullRequestReviewDecision>,
    /// Absent where the host reports no check rollup, or the change request has no checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_state: Option<PullRequestChecksState>,
}

/// Where each repository a listing already reached carries on from, keyed
/// `"<host> <repository>"` — which is how a listing tells two repositories
/// apart, since the same `owner/repo` exists on github.com and on an
/// Enterprise install at once.
///
/// Each value is opaque: only the provider that issued one knows what it
/// means, and the page hands back exactly what it was given rather than
/// composing one. (`Schema.Record`, so insertion order is preserved.)
pub type PullRequestListCursors = indexmap::IndexMap<TrimmedNonEmptyString, PullRequestListCursor>;

/// One continuation value: trimmed, non-empty, at most 4096 characters —
/// bounded because it arrives from the page and is unfolded into a host's
/// own filter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PullRequestListCursor(TrimmedNonEmptyString);

impl PullRequestListCursor {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        if js_len(value.as_str()) > 4096 {
            return Err(ContractError {
                expected: "a continuation of at most 4096 characters",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for PullRequestListCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for PullRequestListCursor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for PullRequestListCursor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

checked_field!(list_limit: i64, |v| {
    (!(1..=500).contains(v)).then_some("an integer between 1 and 500")
});
checked_field!(project_ids: Vec<ProjectId>, |v| {
    (v.len() > 100).then_some("at most 100 project ids")
});
checked_field!(list_query: TrimmedNonEmptyString, |v| {
    (js_len(v.as_str()) > 200).then_some("a search of at most 200 characters")
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListInput {
    pub state: PullRequestListState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub involvement: Option<PullRequestInvolvement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<PullRequestListFilters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// Only these projects, for a client that assigns each shared
    /// repository to one of its connections and asks the others to stay
    /// quiet about it. Absent means every project, which is what every
    /// listing asked for before there were several connections to spread
    /// across.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "project_ids::optional::deserialize"
    )]
    pub project_ids: Option<Vec<ProjectId>>,
    /// Narrows the listing to one host, named as the host itself rather
    /// than as its provider kind: github.com and a GitHub Enterprise install
    /// are two accounts, and a kind cannot tell them apart. Absent means
    /// every host the workspace has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    /// Rows to return per repository, which with a continuation is rows per slice.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "list_limit::optional::deserialize"
    )]
    pub limit: Option<i64>,
    /// Carry on from an answer already on the page rather than read the
    /// listing again. Only the repositories named here are read, one slice
    /// each — which is what makes a further page cost one slice rather than
    /// every repository over again at a larger size. A repository the page
    /// has enough of is simply left out.
    ///
    /// Absent asks for the listing from the top, which is what raising
    /// `limit` alone has always done and still does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursors: Option<PullRequestListCursors>,
    /// Free text the hosts themselves are asked to match, rather than a
    /// filter over the rows that have already arrived: a listing only ever
    /// holds a page per repository, so a search that never leaves the
    /// client can only find what happened to be loaded. What a match means
    /// is the host's own business — GitHub reads a body, GitLab a
    /// description — and a host with no text filter answers unnarrowed
    /// rather than pretending.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "list_query::optional::deserialize"
    )]
    pub query: Option<TrimmedNonEmptyString>,
}

/// A host the workspace has projects on, and whether it can be read right
/// now. Drives the host switcher and explains projects the list leaves out.
///
/// One per host rather than per provider kind, because signing in is a
/// question about the host: a workspace can hold repositories on github.com
/// and on a GitHub Enterprise install at once, and the two are separate
/// accounts that succeed and fail independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestProviderSummary {
    pub host: TrimmedNonEmptyString,
    pub kind: SourceControlProviderKind,
    /// False where a search has to be applied to the rows after they arrive.
    pub searches_on_host: bool,
    /// The actions this host can carry out, so a row can offer them before
    /// its detail is read. Absent from servers older than this field;
    /// clients then offer what they always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<PullRequestAction>>,
    pub project_count: PositiveInt,
    /// False when the provider's CLI or credentials are missing, with `detail` saying which.
    pub configured: bool,
    pub detail: Option<TrimmedNonEmptyString>,
}

/// One project whose repository could not be read; healthy projects still return entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListProjectError {
    pub project_id: ProjectId,
    pub project_title: TrimmedNonEmptyString,
    pub message: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListResult {
    /// The signed-in account per host, which is what involvement filtering
    /// compares. Keyed by host rather than by provider kind: two GitHub
    /// hosts are two accounts. A host that could not be read is absent
    /// rather than present-and-undefined, because an open-keyed record
    /// cannot carry an optional value through the JSON codec.
    pub viewers: indexmap::IndexMap<TrimmedNonEmptyString, TrimmedNonEmptyString>,
    pub providers: Vec<PullRequestProviderSummary>,
    /// By update, newest first, across every repository this answer covers.
    /// A page that appends a continuation sorts what it then holds the same
    /// way: a repository's next slice is older than that repository's last
    /// row, but can still be newer than another repository's.
    pub entries: Vec<PullRequestListEntry>,
    pub errors: Vec<PullRequestListProjectError>,
    /// At least one repository hit the per-repository listing cap.
    pub truncated: bool,
    /// Where each repository carries on, to be sent straight back as
    /// `cursors`. A repository is absent from this once it has nothing more
    /// to give, and also where its host could not be asked in an order a
    /// continuation means anything in — so an empty record beside
    /// `truncated` means more rows are only reachable by raising `limit`,
    /// the way they always were.
    pub next_cursors: PullRequestListCursors,
}

/// Addresses one pull request for reads and writes. `projectId` picks the
/// checkout the host CLI runs in and, when its repository matches, the
/// credentials; `host` lets the server route a pull request from another
/// repository through any project on the same host (a frontend project's
/// thread linking a backend PR). Absent `host` means "the project's own
/// host", which is every reference from before thread links became host-level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    /// Refuse a routed operation unless this GitHub account still owns the active credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    /// Let another environment answer when this one's cached response has expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
}

/// Account discovery never needs the originating project's private repository metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRoutingIdentityInput {
    pub host: TrimmedNonEmptyString,
}

string_literal!(
    /// Constant `"github"` provider of a routing identity result.
    GithubProvider,
    "github"
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRoutingIdentityResult {
    pub account_id: TrimmedNonEmptyString,
    pub host: TrimmedNonEmptyString,
    pub provider: GithubProvider,
    pub viewer: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRoutingResult {
    pub account_id: TrimmedNonEmptyString,
    pub host: TrimmedNonEmptyString,
    pub provider: SourceControlProviderKind,
    pub viewer: TrimmedNonEmptyString,
    pub project_title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLinkedThreadsEntry {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub archived_at: Option<IsoDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLinkedThreadsResult {
    pub threads: Vec<PullRequestLinkedThreadsEntry>,
}

/// The complete hover card, without checks, permissions, or branch comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestPreview {
    pub project_id: ProjectId,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub created_at: IsoDateTime,
}

/// The small live shape a linked thread needs. Keeping it separate from
/// detail means a sidebar status check never loads permissions, repository
/// settings, checks, or base comparison data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSummary {
    pub provider: SourceControlProviderKind,
    pub project_id: ProjectId,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub state: PullRequestState,
    /// Present when the host says the open pull request is still a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    pub head_branch: TrimmedNonEmptyString,
    pub base_branch: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub closed_at: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub merged_at: Option<Option<String>>,
    pub updated_at: IsoDateTime,
    /// Server epoch milliseconds when the provider read started; preserved on cache hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub observed_at: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub author: Option<Option<PullRequestActor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<NonNegativeInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub checks_state: Option<Option<PullRequestChecksState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeability: Option<PullRequestMergeability>,
    /// Null when the host says the pull request is in no stack; absent when the read did not ask.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub stack: Option<Option<PullRequestStackMembership>>,
}

/// A layer of the host-native stack a pull request belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackLayer {
    pub number: PositiveInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<TrimmedNonEmptyString>,
    pub head_branch: TrimmedNonEmptyString,
    pub state: PullRequestState,
}

/// The host-native stack a pull request belongs to, in the thread link's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStack {
    pub id: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub url: TrimmedNonEmptyString,
    pub base: TrimmedNonEmptyString,
    pub layers: Vec<PullRequestStackLayer>,
}

/// One row's line counts, read after the listing rather than inside it. On
/// GitHub the pair is 40-60% of the wall clock of the search that answers
/// the whole page — measured over twelve repositories, 7.1s with it and 4.0s
/// without — for two small numbers at the end of a row.
///
/// So the listing answers without them and the page asks for them next,
/// which is what puts the rows on screen at the speed of everything else on
/// them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffStat {
    pub project_id: ProjectId,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
}

checked_field!(stats_refs: Vec<PullRequestRef>, |v| {
    (v.len() > 500).then_some("at most 500 refs")
});

/// The rows whose line counts are wanted, which is the rows the page is showing.
///
/// Bounded at the largest listing a page can hold, since each ref becomes a
/// lookup of its own on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListStatsInput {
    #[serde(deserialize_with = "stats_refs::deserialize")]
    pub refs: Vec<PullRequestRef>,
}

/// Only the rows that could be answered for. A host with no batched stat
/// read of its own — every one but GitHub, which all report the counts in
/// the listing itself — is simply absent here, and the numbers its listing
/// already carried stand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListStatsResult {
    pub stats: Vec<PullRequestDiffStat>,
}

/// Forget what the server has cached, so the next read asks the host. With a
/// reference it forgets that one change request's detail and diff; without
/// one it forgets the listings. A separate request rather than a flag on
/// the reads, so an explicit "refresh" one person presses is the only thing
/// that spends host requests — every ordinary read shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestInvalidateInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<PullRequestRef>,
    /// Refresh review progress across routed environments without discarding the patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_viewed_only: Option<bool>,
}

/// The state a read routed to another environment saw, reported to the
/// environment the read was for so its thread links can catch up. A hint
/// only: that environment confirms with the host before writing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReportStateInput {
    pub reference: PullRequestRef,
    pub state: PullRequestState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDetail {
    pub provider: SourceControlProviderKind,
    pub capabilities: PullRequestCapabilities,
    /// What this viewer may do, which `capabilities` says nothing about. Both narrow the page.
    pub viewer_permissions: PullRequestViewerPermissions,
    pub project_id: ProjectId,
    pub project_title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub body: String,
    pub url: TrimmedNonEmptyString,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
    pub changed_files: NonNegativeInt,
    pub head_branch: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub head_repository_name_with_owner: Option<Option<TrimmedNonEmptyString>>,
    /// The head commit, where the host reports it with the detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<TrimmedNonEmptyString>,
    pub base_branch: TrimmedNonEmptyString,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    /// Server epoch milliseconds when the provider read started; preserved on cache hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub observed_at: Option<f64>,
    pub merged_at: Option<IsoDateTime>,
    pub closed_at: Option<IsoDateTime>,
    pub reviewers: Vec<PullRequestActor>,
    pub labels: Vec<PullRequestLabel>,
    pub checks: Vec<PullRequestCheck>,
    pub merge_capabilities: PullRequestMergeCapabilities,
    /// Who the host says the reader is, which is the one thing a
    /// conversation cannot be read without to tell the reader's own remarks
    /// from everybody else's — and rewriting a remark is offered only where
    /// the two names agree. Absent where the host could not say, which
    /// offers nothing rather than everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<TrimmedNonEmptyString>,
    /// Where the branch stands against its base. Optional so a host that
    /// cannot compare says nothing rather than claiming the branch is
    /// current — the page shows a banner only where the answer is "behind",
    /// and silence is not that answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_comparison: Option<PullRequestBaseComparison>,
    /// How many commits the base is ahead by, where the host counted them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind_by: Option<NonNegativeInt>,
    /// Whether the host is already armed to merge this on its own. Absent
    /// where the host does not report it, which is not the same as off: a
    /// page that reads silence as "not armed" offers to arm something that
    /// is already armed, and a second arming is a write nobody asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_enabled: Option<bool>,
    /// The strategy the host will use for an armed auto-merge, where it reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<PullRequestMergeMethod>,
    /// GitHub Actions runs on this head commit that are waiting for a maintainer's approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_approvals_required: Option<NonNegativeInt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestChecks {
    pub state: PullRequestState,
    pub checks: Vec<PullRequestCheck>,
}

/// The slower, conversation-shaped half of a change request. It is read
/// independently from the core detail so a host with a deeply paginated
/// review history cannot hold the title, body, checks, and actions off
/// screen. `author` and `reviewers` are optional enrichments: GitHub's
/// conversation query carries avatars and completed reviewers that its
/// basic detail does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActivity {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub author: Option<Option<PullRequestActor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewers: Option<Vec<PullRequestActor>>,
    pub comments: Vec<PullRequestComment>,
    /// How many remarks the host itself counts in the conversation, which
    /// is the number a surface showing a count has to show: `comments`
    /// carries what was read, and a reader who can see 225 on the host is
    /// not reassured by a page that says 10. Never less than `comments`
    /// holds, and equal to it wherever the host was read to the end.
    pub comment_count: NonNegativeInt,
    /// The read stopped at a bound of its own before the host ran out, so
    /// `comments` holds less than `commentCount` counts. Nothing else: a
    /// conversation read whole is never truncated, however long it is.
    pub comments_truncated: bool,
    /// Whether the thread listing itself is incomplete, apart from pages within a thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_threads_truncated: Option<bool>,
    pub review_threads: Vec<PullRequestReviewThread>,
    pub commits: Vec<PullRequestCommit>,
    /// The change request's own reactions — the ones on its description,
    /// which every host counts against the change request itself rather
    /// than against a remark in the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<PullRequestReaction>>,
}

/// The complete detail shape after the independently loaded activity has
/// been applied. A composed view always has the core identity fields, even
/// when the activity did not enrich them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDetailView {
    pub provider: SourceControlProviderKind,
    pub capabilities: PullRequestCapabilities,
    pub viewer_permissions: PullRequestViewerPermissions,
    pub project_id: ProjectId,
    pub project_title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub body: String,
    pub url: TrimmedNonEmptyString,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    pub additions: NonNegativeInt,
    pub deletions: NonNegativeInt,
    pub changed_files: NonNegativeInt,
    pub head_branch: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub head_repository_name_with_owner: Option<Option<TrimmedNonEmptyString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<TrimmedNonEmptyString>,
    pub base_branch: TrimmedNonEmptyString,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub observed_at: Option<f64>,
    pub merged_at: Option<IsoDateTime>,
    pub closed_at: Option<IsoDateTime>,
    pub reviewers: Vec<PullRequestActor>,
    pub labels: Vec<PullRequestLabel>,
    pub checks: Vec<PullRequestCheck>,
    pub merge_capabilities: PullRequestMergeCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_comparison: Option<PullRequestBaseComparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind_by: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<PullRequestMergeMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_approvals_required: Option<NonNegativeInt>,
    // Activity fields (PullRequestActivity spread), with the enriched
    // identity fields re-declared as required above.
    pub comments: Vec<PullRequestComment>,
    pub comment_count: NonNegativeInt,
    pub comments_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_threads_truncated: Option<bool>,
    pub review_threads: Vec<PullRequestReviewThread>,
    pub commits: Vec<PullRequestCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<PullRequestReaction>>,
}

/// A diff arrives a slice at a time, because a large change is more than any
/// host will hand over at once — GitHub refuses outright past 300 files —
/// and more than a viewer can lay out in one go. A slice is a whole number
/// of files, never a file cut in half, so each one parses on its own and the
/// reader can start on the first while the rest is still coming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    /// Where to carry on from. Absent asks for the first slice. Opaque to
    /// the reader: each host counts its files its own way, and only the
    /// provider that issued one knows what it means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<TrimmedNonEmptyString>,
    /// One commit of the change rather than the whole of it. A large pull
    /// request is easier to read a commit at a time, and the author's own
    /// commits are the boundaries they chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<TrimmedNonEmptyString>,
}

/// Real line counts for a file whose hunks the host withheld from the patch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestOmittedFileStat {
    pub path: TrimmedNonEmptyString,
    #[serde(with = "crate::base_schemas::js_number")]
    pub additions: f64,
    #[serde(with = "crate::base_schemas::js_number")]
    pub deletions: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffResult {
    pub patch: String,
    /// Something inside this slice could not be shown — a binary file, or a
    /// hunk the host declined to inline. Not the same as there being more
    /// slices, which `nextCursor` answers.
    pub truncated: bool,
    /// Where the next slice starts, or null once the diff is whole.
    pub next_cursor: Option<TrimmedNonEmptyString>,
    /// The host's own counts for the files whose hunks it withheld, so a
    /// file the patch cannot show still reports what changed instead of a
    /// zero the diff never had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_file_stats: Option<Vec<PullRequestOmittedFileStat>>,
}

/// The complete old and new files needed to open omitted context in a
/// host-backed patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffFileContentsInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    /// One commit's own comparison; absent means the whole change request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<TrimmedNonEmptyString>,
    pub change_type: PullRequestDiffFileChangeType,
    pub old_path: TrimmedNonEmptyString,
    pub new_path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestDiffFileChangeType {
    Change,
    RenamePure,
    RenameChanged,
    New,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDiffFileContentsResult {
    pub old_contents: String,
    pub new_contents: String,
}

// `FilePath`: non-empty, at most 4096 characters, deliberately NOT trimmed — a
// leading or trailing space is a legal part of a file's name, and the mark
// is keyed by the name the host gave. Bounded because a path arrives from a
// client rather than from the host.
checked_field!(file_path: String, |v| {
    if v.is_empty() || js_len(v) > 4096 {
        Some("file paths must be 1..=4096 characters and are not trimmed")
    } else {
        None
    }
});

/// Where one file of a change request stands with the person reading it.
/// `dismissed` is the file that was cleared and has since been pushed to,
/// which is worth telling a reader apart from `unviewed`: this file and not
/// the other forty is the one that moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestFileViewedState {
    Unviewed,
    Viewed,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestFileViewed {
    #[serde(deserialize_with = "file_path::deserialize")]
    pub path: String,
    pub state: PullRequestFileViewedState,
}

/// Which files of a change request the reader has cleared. Its own read
/// rather than a field on the patch: a patch changes when somebody pushes
/// and is cached by the minute, this changes on every press, so one read
/// would have to be wrong for the other to be right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestFilesViewedResult {
    /// Only the files the host reported a state for. A file missing from this list is unviewed.
    pub files: Vec<PullRequestFileViewed>,
    /// The host had more files than were read, so the count is short and says so.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSetFilesViewedFile {
    #[serde(deserialize_with = "file_path::deserialize")]
    pub path: String,
    pub viewed: bool,
}

/// Files to clear, or to put back. Several at once because a reader working
/// down a diff ticks boxes far faster than a host answers, so a burst is
/// gathered into one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSetFilesViewedInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    #[serde(deserialize_with = "set_files_viewed_field::deserialize")]
    pub files: Vec<PullRequestSetFilesViewedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackHead {
    pub number: PositiveInt,
    pub head_sha: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActionInput {
    /// Native stack scope; only send to environments advertising pullRequestStackActions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_number: Option<PositiveInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_stack_heads: Option<Vec<PullRequestStackHead>>,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub action: PullRequestAction,
    /// Which strategy the merge uses, read for `merge` and for
    /// `enable-auto-merge` alike — a merge the host performs later is still
    /// a merge, and the strategy is chosen when it is armed rather than at
    /// the moment it happens. Absent means the host's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_method: Option<PullRequestMergeMethod>,
    /// Only read for `update-branch`, where absent means the host's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_method: Option<PullRequestUpdateMethod>,
}

// How many presses one write carries (`MAX_FILES_VIEWED_PRESSES`); each
// element's path is validated by its own field attribute.
checked_field!(set_files_viewed_field: Vec<PullRequestSetFilesViewedFile>, |v| {
    (v.len() > 500).then_some("at most 500 files per write")
});

// `CommentBody`: non-empty and at most 65536 characters, not trimmed — the
// body is markdown, where leading spaces open a code block and two trailing
// spaces are a line break. A whitespace-only body is valid here; the service
// rejects it.
checked_field!(comment_body: String, |v| {
    if v.is_empty() || js_len(v) > 65_536 {
        Some("comment bodies must be 1..=65,536 characters and are not trimmed")
    } else {
        None
    }
});
// A body that may be empty (`Schema.String.check(isMaxLength(65_536))`).
checked_field!(markdown_body: String, |v| {
    (js_len(v) > 65_536).then_some("bodies must be at most 65,536 characters")
});
checked_field!(update_title: TrimmedNonEmptyString, |v| {
    (js_len(v.as_str()) > 1024).then_some("titles must be at most 1024 characters")
});
checked_field!(reviewer_requests: Vec<PullRequestReviewerRequestReviewer>, |v| {
    (v.is_empty() || v.len() > 25).then_some("between 1 and 25 reviewers")
});
checked_field!(label_names: Vec<TrimmedNonEmptyString>, |v| {
    (v.is_empty() || v.len() > 25).then_some("between 1 and 25 labels")
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCommentInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    #[serde(deserialize_with = "comment_body::deserialize")]
    pub body: String,
}

/// A change request's own words rewritten: its title, its description, or
/// both. Each is optional because the page rewrites them apart — a title
/// corrected on its own must not carry a description nobody opened — and
/// the service refuses a request that carries neither.
///
/// The title is bounded far past what any host takes, GitHub refusing one
/// past 256 characters, so an oversized one is turned away before it
/// reaches a subprocess. The body is not trimmed for the same reason a
/// comment is not: it is markdown, where leading spaces open a code block.
/// An empty one is allowed, which is how a description is cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestUpdateInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "update_title::optional::deserialize"
    )]
    pub title: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "markdown_body::optional::deserialize"
    )]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PullRequestCommentUpdateKind {
    #[serde(rename = "issue-comment")]
    IssueComment,
    #[serde(rename = "review-comment")]
    ReviewComment,
}

/// A remark already posted, rewritten by whoever wrote it. The kind travels
/// beside the id because an id alone does not say which endpoint addresses
/// it: GitHub rewrites a comment on the conversation and a comment on a
/// line through two different mutations.
///
/// A review's own summary is not among them. The hosts disagree about what
/// one even is — the entry Bitbucket shows is a vote this page renders as a
/// remark, with no words behind it to rewrite — and a control that worked
/// on one host and failed on another is worse than one that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCommentUpdateInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub comment_id: TrimmedNonEmptyString,
    pub kind: PullRequestCommentUpdateKind,
    #[serde(deserialize_with = "comment_body::deserialize")]
    pub body: String,
}

/// The coordinates of one line in a pull request diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PullRequestReviewPosition {
    #[serde(rename = "added")]
    Added { new_line: PositiveInt },
    #[serde(rename = "deleted")]
    Deleted { old_line: PositiveInt },
    #[serde(rename = "context")]
    Context {
        old_line: PositiveInt,
        new_line: PositiveInt,
        /// Which copy of an unchanged line the reviewer selected in a split diff.
        side: PullRequestDiffSide,
    },
}

/// One remark in a review that has not been sent yet, anchored to a line of the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewCommentDraft {
    pub path: TrimmedNonEmptyString,
    /// What the file was called before the change, sent only when it
    /// differs. GitLab resolves a position against both sides of the diff,
    /// so a comment on a renamed file needs both names; the hosts that
    /// address a comment by one path ignore this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<TrimmedNonEmptyString>,
    pub position: PullRequestReviewPosition,
    #[serde(deserialize_with = "comment_body::deserialize")]
    pub body: String,
}

/// A whole review, sent in one go. The line comments travel with the
/// verdict rather than being posted as they are written, so a half-finished
/// review is never visible to anyone else — and so hosts with no notion of a
/// pending review behave the same as the ones that have it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSubmitReviewInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub verdict: PullRequestReviewVerdict,
    /// The review's own words. May be empty, which is how an approval with no remarks is sent.
    #[serde(deserialize_with = "markdown_body::deserialize")]
    pub body: String,
    pub comments: Vec<PullRequestReviewCommentDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestThreadCommentsInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub thread_id: TrimmedNonEmptyString,
    pub cursor: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestThreadCommentsResult {
    pub comments: Vec<PullRequestThreadComment>,
    pub next_cursor: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestThreadReplyInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub thread_id: TrimmedNonEmptyString,
    #[serde(deserialize_with = "comment_body::deserialize")]
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestThreadResolutionInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub thread_id: TrimmedNonEmptyString,
    pub resolved: bool,
}

/// Reacting and taking the reaction back are one operation with `reacted`
/// turned around, which is what pressing the same pill twice is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReactionInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    /// Which remark to react to, as it arrived in the conversation. Absent
    /// reacts to the change request itself, which is where its
    /// description's reactions live — the id a host uses for that is its
    /// own to work out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<TrimmedNonEmptyString>,
    pub content: PullRequestReactionContent,
    pub reacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewerRequestReviewer {
    pub id: TrimmedNonEmptyString,
    pub kind: PullRequestReviewerKind,
}

/// Asking for a review and taking the request back are one operation with
/// `requested` turned around, and asking again somebody who has already
/// reviewed is what a re-request is — so that is the same operation once
/// more rather than a third one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewerRequestInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    /// Who, as the candidate list named them. Bounded because every host
    /// bounds it as well — GitHub refuses past fifteen — and because these
    /// travel into a request body a page composed.
    #[serde(deserialize_with = "reviewer_requests::deserialize")]
    pub reviewers: Vec<PullRequestReviewerRequestReviewer>,
    pub requested: bool,
}

/// Putting a label on and taking it off are one operation with `applied`
/// turned around, which is what pressing the same row in the menu twice is.
/// Named by the label's own name, which is how GitHub addresses one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLabelChangeInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    #[serde(deserialize_with = "label_names::deserialize")]
    pub labels: Vec<TrimmedNonEmptyString>,
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestUnavailableReason {
    CliMissing,
    CliUnauthenticated,
    ProviderUnsupported,
}

/// What each host needs before it can be read, so a failure names the fix
/// rather than the symptom. The reason names keep their `cli-` prefix for
/// wire compatibility; for GitHub they mean "no credential" and "a refused
/// credential", not a missing tool.
///
/// Only GitHub is kept; upstream's forgejo, gitlab, azure-devops and
/// bitbucket entries are cut with those hosts (see PORTING.md Deviations).
fn provider_requirement(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "github" => Some((
            "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`.",
            "GitHub has no working credential for this host. Run `gh auth login`, or check the account and hosts in Settings → Source Control.",
        )),
        _ => None,
    }
}

/// Enough of `RepositoryIdentity` for [`pull_request_host_of`], kept
/// structural so both the server and the page can call it. (Upstream also
/// reads `locator.remoteUrl`, but only for Forgejo, which is cut.)
pub struct RepositoryIdentityLike<'a> {
    pub canonical_key: Option<&'a str>,
}

/// The host a project's repository is addressed below. `canonicalKey` is the
/// normalized remote, `host/owner/repo`, so its first segment is the host;
/// the provider kind stands in when there is no key to read, which keeps one
/// bucket per kind for identities recorded before it existed.
///
/// Shared between the server and the page so both bucket a workspace the
/// same way — the page knows its hosts before the listing answers, and the
/// two must agree on what they are called.
pub fn pull_request_host_of(
    identity: Option<&RepositoryIdentityLike<'_>>,
    kind: &SourceControlProviderKind,
) -> String {
    let host = identity
        .and_then(|i| i.canonical_key)
        .and_then(|key| key.split('/').next())
        .map(js_trim);
    match host {
        Some(host) if !host.is_empty() => host.to_lowercase(),
        _ => kind.as_str().to_owned(),
    }
}

/// `author:me` names whoever is signed in to the host being read, GitHub's
/// `@me` spelled either way. A host's own search would answer it, but the
/// page and the server both re-check an author against the row's login, so
/// the name is resolved before either comparison rather than being matched
/// as the literal login "me".
///
/// Null viewer where the host has not said who the reader is; the filter
/// then stands as typed, which is the same answer a search for a user named
/// "me" would give.
///
/// Shared between the server and the page so both narrow by the same login.
pub fn resolve_pull_request_author_filter(author: &str, viewer: Option<&str>) -> String {
    let trimmed = js_trim(author);
    // /^@?me$/i
    let is_me = trimmed.eq_ignore_ascii_case("me") || trimmed.eq_ignore_ascii_case("@me");
    if !is_me {
        return trimmed.to_owned();
    }
    match viewer {
        Some(viewer) if !js_trim(viewer).is_empty() => viewer.to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// What a host needs before it can be read, as a sentence to show wherever
/// that host is reported as unusable — the whole page when nothing can be
/// read, and one entry in the host switcher when only that host cannot.
/// `None` when the reason is not about setting a host up.
pub fn pull_request_provider_requirement(
    provider: &SourceControlProviderKind,
    reason: PullRequestUnavailableReason,
) -> Option<&'static str> {
    let requirement = provider_requirement(provider.as_str())?;
    match reason {
        PullRequestUnavailableReason::CliMissing => Some(requirement.0),
        PullRequestUnavailableReason::CliUnauthenticated => Some(requirement.1),
        PullRequestUnavailableReason::ProviderUnsupported => None,
    }
}

/// `PullRequestUnavailableError` (Schema.TaggedError). The message is
/// derived from `reason` and the host rather than from whatever the CLI
/// printed, so it stays a stable sentence the UI can show as-is.
///
/// (Upstream also carries an `httpApiStatus: 503` annotation and an
/// `HttpServerRespondable` encoder; both are RPC-transport concerns Turin
/// cuts — see PORTING.md.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PullRequestUnavailableError {
    #[serde(rename = "PullRequestUnavailableError")]
    Unavailable {
        reason: PullRequestUnavailableReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<SourceControlProviderKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl std::fmt::Display for PullRequestUnavailableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Unavailable {
            reason, provider, ..
        } = self;
        let requirement = provider
            .as_ref()
            .and_then(|p| provider_requirement(p.as_str()));
        match reason {
            PullRequestUnavailableReason::CliMissing => f.write_str(
                requirement
                    .map(|r| r.0)
                    .unwrap_or("The tool this host is read through is not installed or set up."),
            ),
            PullRequestUnavailableReason::CliUnauthenticated => f.write_str(
                requirement
                    .map(|r| r.1)
                    .unwrap_or("This host has no working credentials."),
            ),
            PullRequestUnavailableReason::ProviderUnsupported => {
                f.write_str("Change requests cannot be browsed for this project's host yet.")
            }
        }
    }
}

impl std::error::Error for PullRequestUnavailableError {}

/// `PullRequestOperationError` (Schema.TaggedError; upstream's 502 HTTP
/// annotation is cut with the RPC transport).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PullRequestOperationError {
    #[serde(rename = "PullRequestOperationError")]
    #[error("Pull request operation {operation} failed: {detail}")]
    Operation {
        operation: String,
        detail: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<PullRequestOperationErrorReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestOperationErrorReason {
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_decode_each_narrowing() {
        let filters: PullRequestListFilters = serde_json::from_value(serde_json::json!({
            "draft": "only",
            "review": "changes-requested",
            "checks": "passing",
            "labels": [["size:S", "size:XS"], ["team:infra"]],
            "excludedLabels": ["wip"],
            "author": "me",
        }))
        .unwrap();
        assert_eq!(filters.draft, Some(PullRequestDraftFilter::Only));
        assert_eq!(
            filters.review,
            Some(PullRequestReviewFilter::ChangesRequested)
        );
        assert_eq!(filters.labels.as_ref().unwrap().len(), 2);
        assert_eq!(filters.author.as_ref().unwrap().as_str(), "me");

        let empty: PullRequestListFilters = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty.draft, None);
    }

    #[test]
    fn qualifier_values_are_bounded() {
        assert!(
            serde_json::from_value::<PullRequestListFilters>(serde_json::json!({
                "author": "x".repeat(201),
            }))
            .is_err()
        );
    }

    #[test]
    fn comment_bodies_reject_empty_and_oversized_but_never_trim() {
        let with_body = |body: serde_json::Value| {
            serde_json::from_value::<PullRequestCommentInput>(serde_json::json!({
                "projectId": "p1", "repository": "o/r", "number": 1, "body": body,
            }))
        };
        assert!(with_body(serde_json::json!("")).is_err());
        // `Schema.isNonEmpty` only checks length: whitespace-only decodes (the
        // service rejects it), so it must not be trimmed away either.
        assert_eq!(with_body(serde_json::json!("   ")).unwrap().body, "   ");
        assert!(with_body(serde_json::json!("x".repeat(65_537))).is_err());
        assert!(with_body(serde_json::json!("x".repeat(65_536))).is_ok());
        // Bodies are not trimmed: leading spaces survive.
        assert_eq!(
            with_body(serde_json::json!("  code block")).unwrap().body,
            "  code block"
        );
    }

    #[test]
    fn file_paths_are_not_trimmed() {
        let viewed: PullRequestFileViewed = serde_json::from_value(serde_json::json!({
            "path": " leading.txt", "state": "viewed",
        }))
        .unwrap();
        assert_eq!(viewed.path, " leading.txt");
        assert!(
            serde_json::from_value::<PullRequestFileViewed>(serde_json::json!({
                "path": "", "state": "viewed",
            }))
            .is_err()
        );
    }

    #[test]
    fn review_position_decodes_by_kind() {
        let added: PullRequestReviewPosition =
            serde_json::from_value(serde_json::json!({"kind": "added", "newLine": 10})).unwrap();
        assert!(matches!(added, PullRequestReviewPosition::Added { .. }));
        let context: PullRequestReviewPosition = serde_json::from_value(serde_json::json!({
            "kind": "context", "oldLine": 1, "newLine": 2, "side": "right"
        }))
        .unwrap();
        assert!(matches!(context, PullRequestReviewPosition::Context { .. }));
    }

    #[test]
    fn host_of_reads_canonical_key_first_segment() {
        let github = SourceControlProviderKind::new("github").unwrap();
        let key = |key| RepositoryIdentityLike {
            canonical_key: Some(key),
        };
        assert_eq!(
            pull_request_host_of(Some(&key("GitHub.com/pingdotgg/t3code")), &github),
            "github.com"
        );
        assert_eq!(
            pull_request_host_of(Some(&key("  github.acme.dev /org/repo")), &github),
            "github.acme.dev"
        );
        // No usable key: the provider kind stands in, once per kind.
        assert_eq!(pull_request_host_of(None, &github), "github");
        assert_eq!(
            pull_request_host_of(
                Some(&RepositoryIdentityLike {
                    canonical_key: None
                }),
                &github
            ),
            "github"
        );
        assert_eq!(pull_request_host_of(Some(&key("")), &github), "github");
        assert_eq!(
            pull_request_host_of(Some(&key("/owner/repo")), &github),
            "github"
        );
        assert_eq!(
            pull_request_host_of(Some(&key("   /owner/repo")), &github),
            "github"
        );
        // Other kinds bucket by their canonical host too.
        let gitlab = SourceControlProviderKind::new("gitlab").unwrap();
        assert_eq!(
            pull_request_host_of(Some(&key("forge.example/team/repo")), &gitlab),
            "forge.example"
        );
    }

    #[test]
    fn author_filter_resolves_me() {
        assert_eq!(
            resolve_pull_request_author_filter(" @Me ", Some("dev")),
            "dev"
        );
        assert_eq!(resolve_pull_request_author_filter("me", None), "me");
        assert_eq!(resolve_pull_request_author_filter("me", Some("  ")), "me");
        assert_eq!(resolve_pull_request_author_filter(" theo ", None), "theo");
    }

    #[test]
    fn provider_requirement_sentences() {
        let github = SourceControlProviderKind::new("github").unwrap();
        assert_eq!(
            pull_request_provider_requirement(&github, PullRequestUnavailableReason::CliMissing)
                .unwrap(),
            "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`."
        );
        assert_eq!(
            pull_request_provider_requirement(
                &github,
                PullRequestUnavailableReason::ProviderUnsupported
            ),
            None
        );
        let error = PullRequestUnavailableError::Unavailable {
            reason: PullRequestUnavailableReason::ProviderUnsupported,
            provider: None,
            cause: None,
        };
        assert_eq!(
            error.to_string(),
            "Change requests cannot be browsed for this project's host yet."
        );
    }

    #[test]
    fn unavailable_error_round_trips_with_tag() {
        let error = PullRequestUnavailableError::Unavailable {
            reason: PullRequestUnavailableReason::CliUnauthenticated,
            provider: Some(SourceControlProviderKind::new("github").unwrap()),
            cause: None,
        };
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["_tag"], "PullRequestUnavailableError");
        assert_eq!(json["reason"], "cli-unauthenticated");
        let back: PullRequestUnavailableError = serde_json::from_value(json).unwrap();
        assert_eq!(back, error);
    }

    // --- ported from pullRequest.test.ts --------------------------------

    use serde_json::{Value, json};

    fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn list_result() -> Value {
        json!({
            "viewers": { "github.com": "bilal", "gitlab.com": "bilal.hassan" },
            "providers": [
                {
                    "host": "github.com", "kind": "github", "searchesOnHost": true,
                    "projectCount": 1, "configured": true, "detail": null,
                },
                {
                    "host": "gitlab.com", "kind": "gitlab", "searchesOnHost": true,
                    "projectCount": 1, "configured": false, "detail": "glab is not installed.",
                },
            ],
            "entries": [{
                "provider": "github", "host": "github.com", "projectId": "project-1",
                "projectTitle": "t3code", "repository": "pingdotgg/t3code", "number": 1,
                "title": "Add a pull requests page",
                "url": "https://github.com/pingdotgg/t3code/pull/1",
                "author": { "login": "octocat", "name": null, "avatarUrl": null },
                "headBranch": "feat/page", "baseBranch": "main", "state": "open",
                "isDraft": false, "mergeability": "mergeable", "additions": 1, "deletions": 0,
                "createdAt": "2026-07-01T00:00:00Z", "updatedAt": "2026-07-02T00:00:00Z",
                "viewerReviewRequested": false,
                "labels": [{ "name": "backend", "color": null }],
            }],
            "errors": [],
            "truncated": false,
            "nextCursors": { "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|1|1" },
        })
    }

    #[test]
    fn list_result_round_trips_without_inventing_or_dropping_keys() {
        let decoded: PullRequestListResult = decode(list_result()).unwrap();
        // IsoDateTime is a passthrough string: not normalised to millis.
        assert_eq!(decoded.entries[0].created_at, "2026-07-01T00:00:00Z");
        assert_eq!(serde_json::to_value(&decoded).unwrap(), list_result());
        // Record keys keep their wire order.
        let keys: Vec<&str> = decoded.viewers.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["github.com", "gitlab.com"]);
    }

    #[test]
    fn viewer_is_keyed_by_host_so_two_hosts_of_one_kind_stay_separate() {
        let mut value = list_result();
        value["viewers"] = json!({ "github.com": "bilal", "github.acme.dev": "b.hassan" });
        let decoded: PullRequestListResult = decode(value).unwrap();
        let by_host = |host: &str| {
            decoded
                .viewers
                .get(&TrimmedNonEmptyString::new(host).unwrap())
                .map(|v| v.as_str().to_owned())
        };
        assert_eq!(by_host("github.com").as_deref(), Some("bilal"));
        assert_eq!(by_host("github.acme.dev").as_deref(), Some("b.hassan"));
    }

    #[test]
    fn list_input_trims_and_bounds_a_search() {
        let input: PullRequestListInput =
            decode(json!({"state": "open", "query": "  page  "})).unwrap();
        assert_eq!(input.query.unwrap().as_str(), "page");
        let none: PullRequestListInput = decode(json!({"state": "open"})).unwrap();
        assert!(none.query.is_none());
        let ok: PullRequestListInput =
            decode(json!({"state": "open", "query": "p".repeat(200)})).unwrap();
        assert_eq!(ok.query.unwrap().as_str().len(), 200);
        assert!(
            decode::<PullRequestListInput>(json!({"state": "open", "query": "p".repeat(201)}))
                .is_err()
        );
        // Blank after trimming is not a search either.
        assert!(decode::<PullRequestListInput>(json!({"state": "open", "query": "   "})).is_err());
        // `isMaxLength` counts UTF-16 code units: an emoji is two.
        assert!(
            decode::<PullRequestListInput>(json!({"state": "open", "query": "😀".repeat(100)}))
                .is_ok()
        );
        assert!(
            decode::<PullRequestListInput>(json!({"state": "open", "query": "😀".repeat(101)}))
                .is_err()
        );
    }

    #[test]
    fn list_input_cursors_round_trip_and_are_bounded() {
        let cursors = json!({ "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|99|1,2" });
        let input: PullRequestListInput =
            decode(json!({"state": "open", "cursors": cursors})).unwrap();
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({"state": "open", "cursors": cursors})
        );
        let none: PullRequestListInput = decode(json!({"state": "open"})).unwrap();
        assert!(none.cursors.is_none());
        let long = |length: usize| json!({"github.com acme/web": "c".repeat(length)});
        assert!(
            decode::<PullRequestListInput>(json!({"state": "open", "cursors": long(4096)})).is_ok()
        );
        assert!(
            decode::<PullRequestListInput>(json!({"state": "open", "cursors": long(4097)}))
                .is_err()
        );
        // The same bound applies to the cursors a result hands out.
        let mut result = list_result();
        result["nextCursors"] = long(4097);
        assert!(decode::<PullRequestListResult>(result).is_err());
    }

    #[test]
    fn list_input_bounds_limit_and_project_ids() {
        let with = |field: &str, value: Value| {
            decode::<PullRequestListInput>(json!({"state": "all", field: value}))
        };
        assert!(with("limit", json!(1)).is_ok());
        assert!(with("limit", json!(500)).is_ok());
        assert!(with("limit", json!(0)).is_err());
        assert!(with("limit", json!(501)).is_err());
        assert!(with("limit", json!(1.5)).is_err());
        let ids = |n: usize| json!((0..n).map(|i| format!("p{i}")).collect::<Vec<_>>());
        assert!(with("projectIds", ids(100)).is_ok());
        assert!(with("projectIds", ids(101)).is_err());
    }

    #[test]
    fn list_filters_bound_qualifier_values_and_groups() {
        let with =
            |field: &str, value: Value| decode::<PullRequestListFilters>(json!({field: value}));
        let values = |n: usize| json!((0..n).map(|i| format!("l{i}")).collect::<Vec<_>>());
        assert!(with("excludedLabels", values(10)).is_ok());
        assert!(with("excludedLabels", values(11)).is_err());
        assert!(
            with(
                "labels",
                json!((0..10).map(|_| values(10)).collect::<Vec<_>>())
            )
            .is_ok()
        );
        assert!(
            with(
                "labels",
                json!((0..11).map(|_| values(1)).collect::<Vec<_>>())
            )
            .is_err()
        );
        assert!(with("labels", json!([values(11)])).is_err());
        assert!(with("author", json!("x".repeat(200))).is_ok());
        assert!(with("author", json!("😀".repeat(101))).is_err());
        assert!(with("draft", json!("both")).is_err());
        // Qualifier values are trimmed.
        let filters = with("author", json!("  octocat ")).unwrap();
        assert_eq!(filters.author.unwrap().as_str(), "octocat");
    }

    #[test]
    fn reviewer_request_is_bounded_and_keeps_teams_apart() {
        let decode_request = |reviewers: Value, requested: bool| {
            decode::<PullRequestReviewerRequestInput>(json!({
                "projectId": "p1", "repository": "acme/web", "number": 1,
                "reviewers": reviewers, "requested": requested,
            }))
        };
        let many = |n: usize| {
            json!(
                (0..n)
                    .map(|i| json!({"id": format!("user{i}"), "kind": "user"}))
                    .collect::<Vec<_>>()
            )
        };
        for requested in [true, false] {
            assert_eq!(
                decode_request(json!([{"id": "octocat", "kind": "user"}]), requested)
                    .unwrap()
                    .requested,
                requested
            );
        }
        assert!(decode_request(json!([]), true).is_err());
        assert_eq!(decode_request(many(25), true).unwrap().reviewers.len(), 25);
        assert!(decode_request(many(26), true).is_err());
        let kinds: Vec<_> = decode_request(
            json!([{"id": "octocat", "kind": "user"}, {"id": "web-platform", "kind": "team"}]),
            true,
        )
        .unwrap()
        .reviewers
        .into_iter()
        .map(|r| r.kind)
        .collect();
        assert_eq!(
            kinds,
            [PullRequestReviewerKind::User, PullRequestReviewerKind::Team]
        );
    }

    #[test]
    fn label_change_is_bounded() {
        let with = |labels: Value| {
            decode::<PullRequestLabelChangeInput>(json!({
                "projectId": "p1", "repository": "acme/web", "number": 1,
                "labels": labels, "applied": true,
            }))
        };
        let names = |n: usize| json!((0..n).map(|i| format!("l{i}")).collect::<Vec<_>>());
        assert!(with(names(0)).is_err());
        assert!(with(names(1)).is_ok());
        assert!(with(names(25)).is_ok());
        assert!(with(names(26)).is_err());
    }

    #[test]
    fn actions_carry_their_own_strategy_options() {
        let base = json!({"projectId": "project-1", "repository": "acme/web", "number": 7});
        let with = |extra: Value| {
            let mut value = base.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            decode::<PullRequestActionInput>(value)
        };
        let rebase = with(json!({"action": "update-branch", "updateMethod": "rebase"})).unwrap();
        assert_eq!(rebase.action, PullRequestAction::UpdateBranch);
        assert_eq!(rebase.update_method, Some(PullRequestUpdateMethod::Rebase));
        assert_eq!(
            with(json!({"action": "update-branch"}))
                .unwrap()
                .update_method,
            None
        );
        // A way no host offers.
        assert!(with(json!({"action": "update-branch", "updateMethod": "squash"})).is_err());
        let armed = with(json!({"action": "enable-auto-merge", "mergeMethod": "squash"})).unwrap();
        assert_eq!(armed.action, PullRequestAction::EnableAutoMerge);
        assert_eq!(armed.merge_method, Some(PullRequestMergeMethod::Squash));
        assert_eq!(
            with(json!({"action": "disable-auto-merge"}))
                .unwrap()
                .merge_method,
            None
        );
        let revert = with(json!({"action": "revert"})).unwrap();
        assert_eq!(
            (revert.action, revert.merge_method),
            (PullRequestAction::Revert, None)
        );
        assert_eq!(
            with(json!({"action": "approve-workflows"})).unwrap().action,
            PullRequestAction::ApproveWorkflows
        );
        assert!(with(json!({"action": "explode"})).is_err());
    }

    #[test]
    fn capabilities_without_optional_keys_decode_as_a_server_with_none() {
        let base = json!({
            "diff": true, "comment": true, "actions": [], "mergeMethods": [], "search": true,
            "review": { "inlineComment": true, "reply": true, "resolve": true, "verdicts": [] },
            "reviewers": { "request": true, "listCandidates": true },
        });
        let capabilities: PullRequestCapabilities = decode(base.clone()).unwrap();
        assert_eq!(capabilities.reactions, None);
        assert_eq!(capabilities.update_methods, None);
        assert_eq!(capabilities.viewed_files, None);
        // Optional keys stay absent on the way back out.
        assert_eq!(serde_json::to_value(&capabilities).unwrap(), base);
    }

    #[test]
    fn files_viewed_keeps_path_spaces_and_bounds_the_batch() {
        let batch = |files: Value| {
            decode::<PullRequestSetFilesViewedInput>(json!({
                "projectId": "p1", "repository": "group/project", "number": 7, "files": files,
            }))
        };
        let kept = batch(json!([{"path": "docs/readme.md ", "viewed": true}])).unwrap();
        assert_eq!(kept.files[0].path, "docs/readme.md ");
        assert!(batch(json!([{"path": "", "viewed": true}])).is_err());
        let presses = |n: usize| {
            json!(
                (0..n)
                    .map(|i| json!({"path": format!("src/f{i}.ts"), "viewed": true}))
                    .collect::<Vec<_>>()
            )
        };
        assert!(batch(presses(500)).is_ok());
        assert!(batch(presses(501)).is_err());
        let path = |len: usize| json!([{"path": "a".repeat(len), "viewed": true}]);
        assert!(batch(path(4096)).is_ok());
        assert!(batch(path(4097)).is_err());
        // Missing `viewed` is a decode error, not a default.
        assert!(batch(json!([{"path": "a"}])).is_err());
        let reported: PullRequestFilesViewedResult = decode(json!({
            "files": [{"path": " leading.md", "state": "viewed"}], "truncated": false,
        }))
        .unwrap();
        assert_eq!(reported.files[0].path, " leading.md");
    }

    // --- round-trip tests for tricky fields ------------------------------

    fn comment() -> Value {
        json!({
            "id": "c1", "kind": "issue-comment", "author": null, "body": "hi",
            "createdAt": "2026-07-01T00:00:00Z", "url": null, "path": null, "reviewState": null,
        })
    }

    #[test]
    fn edited_at_distinguishes_absent_from_null() {
        let absent: PullRequestComment = decode(comment()).unwrap();
        assert_eq!(absent.edited_at, None);
        assert_eq!(serde_json::to_value(&absent).unwrap(), comment());

        let mut with_null = comment();
        with_null["editedAt"] = Value::Null;
        let decoded: PullRequestComment = decode(with_null.clone()).unwrap();
        assert_eq!(decoded.edited_at, Some(None));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), with_null);

        let mut with_value = comment();
        with_value["editedAt"] = json!("2026-07-02T00:00:00Z");
        let decoded: PullRequestComment = decode(with_value.clone()).unwrap();
        assert_eq!(
            decoded.edited_at,
            Some(Some("2026-07-02T00:00:00Z".to_owned()))
        );
        assert_eq!(serde_json::to_value(&decoded).unwrap(), with_value);
    }

    #[test]
    fn summary_keeps_null_apart_from_absent_for_every_nullable_optional() {
        let minimal = json!({
            "provider": "github", "projectId": "p1", "repository": "o/r", "number": 3,
            "title": "t", "url": "https://x", "state": "merged", "headBranch": "h",
            "baseBranch": "b", "updatedAt": "2026-07-01T00:00:00Z",
        });
        let absent: PullRequestSummary = decode(minimal.clone()).unwrap();
        assert_eq!(serde_json::to_value(&absent).unwrap(), minimal);

        let mut nulls = minimal.clone();
        for key in [
            "closedAt",
            "mergedAt",
            "author",
            "reviewDecision",
            "checksState",
            "stack",
        ] {
            nulls[key] = Value::Null;
        }
        let decoded: PullRequestSummary = decode(nulls.clone()).unwrap();
        assert_eq!(decoded.closed_at, Some(None));
        assert_eq!(decoded.author, Some(None));
        assert_eq!(decoded.review_decision, Some(None));
        assert_eq!(decoded.checks_state, Some(None));
        assert_eq!(decoded.stack, Some(None));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), nulls);
    }

    #[test]
    fn detail_head_repository_keeps_null_apart_from_absent() {
        let key = "headRepositoryNameWithOwner";
        let mut checked = 0;
        for (value, expected) in [
            (None, None),
            (Some(Value::Null), Some(None)),
            (Some(json!(" fork/repo ")), Some(Some("fork/repo"))),
        ] {
            let mut detail = detail_json();
            if let Some(value) = value {
                detail[key] = value;
            }
            let decoded: PullRequestDetail = decode(detail).unwrap();
            assert_eq!(
                decoded
                    .head_repository_name_with_owner
                    .as_ref()
                    .map(|o| o.as_ref().map(|s| s.as_str())),
                expected
            );
            checked += 1;
        }
        assert_eq!(checked, 3);
    }

    fn detail_json() -> Value {
        json!({
            "provider": "github",
            "capabilities": {
                "diff": true, "comment": true, "actions": ["merge"], "mergeMethods": ["squash"],
                "search": true,
                "review": { "inlineComment": true, "reply": true, "resolve": true, "verdicts": [] },
                "reviewers": { "request": true, "listCandidates": true },
            },
            "viewerPermissions": {
                "actions": [], "comment": true, "resolve": true, "verdicts": [],
                "requestReviewers": true,
            },
            "projectId": "p1", "projectTitle": "T", "workspaceRoot": "/w", "repository": "o/r",
            "number": 1, "title": "t", "body": "", "url": "https://x", "author": null,
            "state": "open", "isDraft": false, "mergeability": "unknown", "additions": 0,
            "deletions": 0, "changedFiles": 0, "headBranch": "h", "baseBranch": "b",
            "createdAt": "2026-07-01T00:00:00Z", "updatedAt": "2026-07-01T00:00:00Z",
            "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": true, "rebase": false },
        })
    }

    #[test]
    fn detail_round_trips_and_view_requires_activity() {
        let detail = detail_json();
        let decoded: PullRequestDetail = decode(detail.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), detail);

        let mut view = detail;
        view.as_object_mut().unwrap().extend(
            json!({
                "comments": [comment()], "commentCount": 1, "commentsTruncated": false,
                "reviewThreads": [], "commits": [],
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        let decoded: PullRequestDetailView = decode(view.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), view);
        // Activity's own keys are required on the composed view.
        view.as_object_mut().unwrap().remove("commentCount");
        assert!(decode::<PullRequestDetailView>(view).is_err());
        // The activity half alone may leave author/reviewers out or null.
        let activity: PullRequestActivity = decode(json!({
            "author": null, "comments": [], "commentCount": 0, "commentsTruncated": false,
            "reviewThreads": [], "commits": [],
        }))
        .unwrap();
        assert_eq!(activity.author, Some(None));
        assert_eq!(activity.reviewers, None);
    }

    #[test]
    fn positive_ints_reject_zero_and_review_thread_line_may_be_null() {
        let thread = |line: Value| {
            decode::<PullRequestReviewThread>(json!({
                "id": "t", "path": "a.rs", "line": line, "side": "left",
                "isResolved": false, "isOutdated": false, "comments": [],
            }))
        };
        assert_eq!(thread(Value::Null).unwrap().line, None);
        assert_eq!(
            thread(json!(4)).unwrap().line.map(PositiveInt::get),
            Some(4)
        );
        assert!(thread(json!(0)).is_err());
        assert!(
            decode::<PullRequestReaction>(json!({
                "content": "heart", "count": 0, "actors": [], "viewerHasReacted": false,
            }))
            .is_err()
        );
        assert!(
            decode::<PullRequestRef>(json!({
                "projectId": "p", "repository": "o/r", "number": 0,
            }))
            .is_err()
        );
    }

    #[test]
    fn reaction_and_enum_wire_names() {
        for (wire, expected) in [
            ("thumbs-up", PullRequestReactionContent::ThumbsUp),
            ("thumbs-down", PullRequestReactionContent::ThumbsDown),
            ("hooray", PullRequestReactionContent::Hooray),
            ("eyes", PullRequestReactionContent::Eyes),
        ] {
            assert_eq!(
                decode::<PullRequestReactionContent>(json!(wire)).unwrap(),
                expected
            );
            assert_eq!(serde_json::to_value(expected).unwrap(), json!(wire));
        }
        assert!(decode::<PullRequestReactionContent>(json!("tada")).is_err());
        for wire in [
            "merge",
            "ready",
            "draft",
            "close",
            "reopen",
            "update-branch",
            "enable-auto-merge",
            "disable-auto-merge",
            "revert",
            "approve-workflows",
        ] {
            let action: PullRequestAction = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(action).unwrap(), json!(wire));
        }
        for wire in ["changes-requested", "review-required", "approved"] {
            let decision: PullRequestReviewDecision = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(decision).unwrap(), json!(wire));
        }
        for wire in ["up-to-date", "behind", "unknown"] {
            let cmp: PullRequestBaseComparison = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(cmp).unwrap(), json!(wire));
        }
        for wire in [
            "pending",
            "action-required",
            "success",
            "failure",
            "skipped",
            "neutral",
            "cancelled",
        ] {
            let status: PullRequestCheckStatus = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
        }
        for wire in ["issue-comment", "review-comment", "review"] {
            let kind: PullRequestCommentKind = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
        }
        for wire in ["comment", "approve", "request-changes"] {
            let verdict: PullRequestReviewVerdict = decode(json!(wire)).unwrap();
            assert_eq!(serde_json::to_value(verdict).unwrap(), json!(wire));
        }
    }

    #[test]
    fn stack_has_no_kind_discriminator() {
        let stack: PullRequestStack = decode(json!({
            "id": "s1", "number": 1, "url": "https://x", "base": "main",
            "layers": [{ "number": 2, "headBranch": "h", "state": "open" }],
        }))
        .unwrap();
        let encoded = serde_json::to_value(&stack).unwrap();
        assert!(encoded.get("kind").is_none());
        assert_eq!(
            encoded["layers"][0],
            json!({"number": 2, "headBranch": "h", "state": "open"})
        );
    }

    #[test]
    fn update_and_review_inputs_bound_their_text() {
        let reference = json!({"projectId": "p", "repository": "o/r", "number": 1});
        let with = |extra: Value| {
            let mut value = reference.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            value
        };
        assert!(decode::<PullRequestUpdateInput>(with(json!({"title": "t".repeat(1024)}))).is_ok());
        assert!(
            decode::<PullRequestUpdateInput>(with(json!({"title": "t".repeat(1025)}))).is_err()
        );
        // An empty description is how one is cleared.
        assert_eq!(
            decode::<PullRequestUpdateInput>(with(json!({"body": ""})))
                .unwrap()
                .body,
            Some(String::new())
        );
        assert!(
            decode::<PullRequestUpdateInput>(with(json!({"body": "x".repeat(65_537)}))).is_err()
        );

        let review = |body: Value| {
            decode::<PullRequestSubmitReviewInput>(with(json!({
                "verdict": "approve", "body": body, "comments": [],
            })))
        };
        assert!(review(json!("")).is_ok());
        assert!(review(json!("😀".repeat(32_768))).is_ok());
        assert!(review(json!("😀".repeat(32_769))).is_err());
        // The body key is required.
        assert!(
            decode::<PullRequestSubmitReviewInput>(with(json!({
                "verdict": "approve", "comments": [],
            })))
            .is_err()
        );

        let draft = |body: Value| {
            decode::<PullRequestReviewCommentDraft>(json!({
                "path": "a.rs", "position": {"kind": "deleted", "oldLine": 3}, "body": body,
            }))
        };
        assert!(draft(json!("")).is_err());
        assert!(draft(json!(" ")).is_ok());

        let edit = |kind: &str| {
            decode::<PullRequestCommentUpdateInput>(with(json!({
                "commentId": "c", "kind": kind, "body": "b",
            })))
        };
        assert!(edit("issue-comment").is_ok());
        assert!(edit("review-comment").is_ok());
        assert!(edit("review").is_err());
    }

    #[test]
    fn review_position_rejects_unknown_kind_and_missing_fields() {
        assert!(
            decode::<PullRequestReviewPosition>(json!({"kind": "moved", "newLine": 1})).is_err()
        );
        assert!(decode::<PullRequestReviewPosition>(json!({"kind": "added"})).is_err());
        assert!(
            decode::<PullRequestReviewPosition>(
                json!({"kind": "context", "oldLine": 1, "newLine": 1})
            )
            .is_err()
        );
        assert!(
            decode::<PullRequestReviewPosition>(json!({"kind": "added", "newLine": 0})).is_err()
        );
        let deleted = PullRequestReviewPosition::Deleted {
            old_line: PositiveInt::new(3).unwrap(),
        };
        assert_eq!(
            serde_json::to_value(&deleted).unwrap(),
            json!({"kind": "deleted", "oldLine": 3})
        );
    }

    #[test]
    fn routing_identity_provider_is_always_github() {
        let ok = decode::<PullRequestRoutingIdentityResult>(json!({
            "accountId": "a", "host": "github.com", "provider": "github", "viewer": "v",
        }));
        assert!(ok.is_ok());
        assert!(
            decode::<PullRequestRoutingIdentityResult>(json!({
                "accountId": "a", "host": "github.com", "provider": "gitlab", "viewer": "v",
            }))
            .is_err()
        );
    }

    #[test]
    fn operation_error_round_trips_and_formats() {
        let wire = json!({
            "_tag": "PullRequestOperationError", "operation": "merge",
            "detail": "conflicts", "reason": "not-found",
        });
        let error: PullRequestOperationError = decode(wire.clone()).unwrap();
        assert_eq!(
            error.to_string(),
            "Pull request operation merge failed: conflicts"
        );
        assert_eq!(serde_json::to_value(&error).unwrap(), wire);
        assert!(
            decode::<PullRequestOperationError>(json!({
                "_tag": "PullRequestOperationError", "operation": "merge", "detail": "d",
                "reason": "gone",
            }))
            .is_err()
        );
    }

    #[test]
    fn unavailable_error_messages_fall_back_without_a_known_host() {
        let message = |reason, provider: Option<&str>| {
            PullRequestUnavailableError::Unavailable {
                reason,
                provider: provider.map(|p| SourceControlProviderKind::new(p).unwrap()),
                cause: None,
            }
            .to_string()
        };
        assert_eq!(
            message(PullRequestUnavailableReason::CliMissing, None),
            "The tool this host is read through is not installed or set up."
        );
        assert_eq!(
            message(
                PullRequestUnavailableReason::CliUnauthenticated,
                Some("gitlab")
            ),
            "This host has no working credentials."
        );
        assert!(
            message(
                PullRequestUnavailableReason::CliUnauthenticated,
                Some("github")
            )
            .starts_with("GitHub has no working credential")
        );
    }

    #[test]
    fn author_filter_matches_js_regex_semantics() {
        // U+FEFF is JS whitespace, so `trim()` strips it before the /^@?me$/i test.
        assert_eq!(
            resolve_pull_request_author_filter("\u{feff}me\u{feff}", Some("dev")),
            "dev"
        );
        assert_eq!(
            resolve_pull_request_author_filter("me-too", Some("dev")),
            "me-too"
        );
        assert_eq!(
            resolve_pull_request_author_filter("@@me", Some("dev")),
            "@@me"
        );
        // The viewer is returned as given (only its emptiness is judged after trimming).
        assert_eq!(
            resolve_pull_request_author_filter("me", Some(" dev ")),
            " dev "
        );
        // U+0085 is not JS whitespace, so a viewer of just that is not "empty".
        assert_eq!(
            resolve_pull_request_author_filter("me", Some("\u{85}")),
            "\u{85}"
        );
    }
}
