//! The seam between the pull request service and the pull request views.
//!
//! [`PullRequestStore`] is a GPUI model holding what the views read: the merged listing, one
//! [`PullRequestDetailSnapshot`] per opened pull request, and the thread ↔ pull request links.
//! Views observe it and never call the service. Whatever feeds the store implements
//! [`PullRequestSource`]: it publishes snapshots (through the store's `update_*` methods) and
//! receives the reader's commands — the `pullRequestEnvironment.*` commands the upstream
//! components call through `useAtomCommand`.
//!
//! [`super::mock_source::MockPullRequestSource`] feeds fixtures for screenshots and tests; T22
//! wires the live source-control service (T07) behind the same trait.

use std::collections::HashMap;
use std::sync::Arc;

use turin_contracts::pull_request::{
    PullRequestAction, PullRequestChecks, PullRequestCommentUpdateKind, PullRequestDetailView,
    PullRequestDiffResult, PullRequestFileViewed, PullRequestLabelCandidateList,
    PullRequestMergeMethod, PullRequestReactionContent, PullRequestRef,
    PullRequestReviewCommentDraft, PullRequestReviewVerdict, PullRequestReviewerCandidateList,
    PullRequestStack, PullRequestUpdateMethod,
};
use turin_contracts::pull_request_review_guide::PullRequestReviewGuideResult;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;
use turin_contracts::{EnvironmentId, ThreadId};

use gpui_kit::{App, Context};

use super::pull_request_list_logic::{EnvironmentPullRequestEntry, MergedPullRequestList};

/// A read that may not have answered yet, or may have failed. `stale` data is kept on screen
/// while a refresh is in flight, as the upstream atoms do (SWR).
#[derive(Debug, Clone, PartialEq)]
pub enum Loadable<T> {
    /// Nothing read yet: the view draws skeletons.
    Pending,
    Ready(T),
    /// The host refused or could not be reached; the message is the host's own sentence.
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(message) => Some(message),
            _ => None,
        }
    }
}

impl<T> Default for Loadable<T> {
    fn default() -> Self {
        Self::Pending
    }
}

/// A project the listing can be scoped to (`useProjects()` rows the page reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestProject {
    pub id: String,
    pub environment_id: EnvironmentId,
    pub title: String,
    pub repository: Option<String>,
}

/// What the list page reads (`pullRequestEnvironment.list` merged across environments, with the
/// page's own flags).
#[derive(Debug, Clone)]
pub struct PullRequestListSnapshot {
    /// `None` until the first answer lands: the page draws ghosts.
    pub merged: Option<MergedPullRequestList>,
    /// The failure of the last read, shown over the rows already held.
    pub error: Option<String>,
    /// A read is in flight (the refresh control turns).
    pub refreshing: bool,
    /// More rows are being fetched below the list.
    pub loading_more: bool,
    pub projects: Vec<PullRequestProject>,
    /// Whether every environment has said what projects it holds yet.
    pub projects_known: bool,
    /// Whether the servers advertise pull requests at all.
    pub supported: bool,
}

impl Default for PullRequestListSnapshot {
    fn default() -> Self {
        Self {
            merged: None,
            error: None,
            refreshing: false,
            loading_more: false,
            projects: Vec::new(),
            projects_known: false,
            supported: true,
        }
    }
}

/// Which code-tab diff the source answered, keyed by the commit it was scoped to
/// (`None` is the whole pull request).
pub type DiffsByCommit = HashMap<Option<String>, Loadable<PullRequestDiffResult>>;

/// Everything one pull request's panel reads. Mirrors the atoms `PullRequestDetailPanel` opens:
/// detail + activity (already merged into [`PullRequestDetailView`]), checks, the host's native
/// stack, the diff, viewed marks, linked threads and the review guide.
#[derive(Debug, Clone, Default)]
pub struct PullRequestDetailSnapshot {
    pub detail: Loadable<PullRequestDetailView>,
    /// The activity read (comments, threads, commits) is still in flight; the detail is shown
    /// without them meanwhile.
    pub activity_pending: bool,
    pub checks: Loadable<PullRequestChecks>,
    pub stack: Option<PullRequestStack>,
    pub diffs: DiffsByCommit,
    pub files_viewed: Vec<PullRequestFileViewed>,
    pub linked_threads: Vec<LinkedThread>,
    pub guide: Loadable<PullRequestReviewGuideResult>,
    pub reviewer_candidates: Option<PullRequestReviewerCandidateList>,
    pub label_candidates: Option<PullRequestLabelCandidateList>,
    /// The action in flight, if any (every control is disabled while one runs).
    pub pending_action: Option<PullRequestAction>,
    /// The last action's failure, as a toast would show it.
    pub action_error: Option<String>,
    /// Whether the viewer may write to the host (`AuthSourceControlWriteScope`).
    pub can_write: bool,
}

/// An active thread the reader can link a pull request to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadChoice {
    pub id: ThreadId,
    pub title: String,
    pub project_title: String,
    pub updated_at: String,
}

/// A thread linked to this pull request (`pullRequests.linkedThreads`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedThread {
    pub id: ThreadId,
    pub project_id: String,
    pub title: String,
    pub archived: bool,
}

/// Stable key of an opened pull request: `projectId:host:repository#number`
/// (`pullRequestKey` in the panel).
pub fn pull_request_key(reference: &PullRequestRef) -> String {
    format!(
        "{}:{}:{}#{}",
        reference.project_id.as_str(),
        reference.host.as_ref().map_or("", |host| host.as_str()),
        reference.repository.as_str().to_lowercase(),
        reference.number.get()
    )
}

/// A comment or review the reader submits.
#[derive(Debug, Clone)]
pub struct ReviewSubmission {
    pub verdict: PullRequestReviewVerdict,
    pub body: String,
}

/// The reader's commands. Every method returns immediately; the source publishes the outcome
/// through the store (`PullRequestStore::update_detail` etc.) when the host has answered.
pub trait PullRequestSource: 'static {
    /// The refresh control: ask every environment again.
    fn refresh_list(&self, cx: &mut App);
    /// "Load more pull requests".
    fn load_more(&self, cx: &mut App);
    /// Open a pull request: start reading its detail, activity, checks and stack.
    fn open_detail(&self, reference: &PullRequestRef, cx: &mut App);
    /// The refresh control in the panel header: bypass the host cache and read again.
    fn refresh_detail(&self, reference: &PullRequestRef, cx: &mut App);
    /// A host action (merge, close, ready, update branch, ...).
    fn run_action(
        &self,
        reference: &PullRequestRef,
        action: PullRequestAction,
        merge_method: Option<PullRequestMergeMethod>,
        update_method: Option<PullRequestUpdateMethod>,
        cx: &mut App,
    );
    fn post_comment(&self, reference: &PullRequestRef, body: String, cx: &mut App);
    fn update_title(&self, reference: &PullRequestRef, title: String, cx: &mut App);
    fn update_body(&self, reference: &PullRequestRef, body: String, cx: &mut App);
    fn submit_review(&self, reference: &PullRequestRef, review: ReviewSubmission, cx: &mut App);
    /// A whole review with the line comments the reader drafted in the Code tab
    /// (`pullRequestEnvironment.submitReview` carries them in one request). Sources that cannot
    /// post line comments fall back to the verdict and the words.
    fn submit_review_with_comments(
        &self,
        reference: &PullRequestRef,
        review: ReviewSubmission,
        comments: Vec<PullRequestReviewCommentDraft>,
        cx: &mut App,
    ) {
        let _ = comments;
        self.submit_review(reference, review, cx);
    }
    /// Rewrite one of the reader's own remarks (`pullRequestEnvironment.updateComment`).
    fn update_comment(
        &self,
        reference: &PullRequestRef,
        comment_id: &str,
        kind: PullRequestCommentUpdateKind,
        body: String,
        cx: &mut App,
    ) {
        let _ = (reference, comment_id, kind, body, cx);
    }
    /// "Show more replies" on a review thread read in pages (`threadComments`).
    fn load_thread_comments(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        cursor: &str,
        cx: &mut App,
    ) {
        let _ = (reference, thread_id, cursor, cx);
    }
    fn reply_to_thread(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        body: String,
        cx: &mut App,
    );
    fn set_thread_resolved(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        resolved: bool,
        cx: &mut App,
    );
    fn toggle_reaction(
        &self,
        reference: &PullRequestRef,
        subject_id: &str,
        content: PullRequestReactionContent,
        cx: &mut App,
    );
    /// The Code tab: read the diff, scoped to one commit or the whole pull request.
    fn load_diff(&self, reference: &PullRequestRef, commit: Option<String>, cx: &mut App);
    /// "Viewed" checkboxes.
    fn set_files_viewed(
        &self,
        reference: &PullRequestRef,
        files: Vec<(String, bool)>,
        cx: &mut App,
    );
    fn request_reviewers(&self, reference: &PullRequestRef, logins: Vec<String>, cx: &mut App);
    fn set_labels(&self, reference: &PullRequestRef, labels: Vec<String>, cx: &mut App);
    /// The Guide tab: write (or rewrite) the review guide.
    fn generate_guide(&self, reference: &PullRequestRef, cx: &mut App);
    /// Thread ↔ pull request linking.
    fn link_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App);
    fn unlink_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App);
    /// Open a thread in the app (the linked-threads row).
    fn open_thread(&self, thread_id: &ThreadId, cx: &mut App);
    /// Open a URL in the system browser.
    fn open_external(&self, url: &str, cx: &mut App) {
        cx.open_url(url);
    }
}

/// The observable holder the views read.
pub struct PullRequestStore {
    list: PullRequestListSnapshot,
    details: HashMap<String, PullRequestDetailSnapshot>,
    thread_links: HashMap<String, Vec<ThreadPullRequestLink>>,
    threads: Vec<ThreadChoice>,
    source: Option<Arc<dyn PullRequestSource>>,
}

impl PullRequestStore {
    pub fn new() -> Self {
        Self {
            list: PullRequestListSnapshot::default(),
            details: HashMap::new(),
            thread_links: HashMap::new(),
            threads: Vec::new(),
            source: None,
        }
    }

    pub fn set_source(&mut self, source: Arc<dyn PullRequestSource>) {
        self.source = Some(source);
    }

    pub fn source(&self) -> Option<Arc<dyn PullRequestSource>> {
        self.source.clone()
    }

    pub fn list(&self) -> &PullRequestListSnapshot {
        &self.list
    }

    pub fn entries(&self) -> &[EnvironmentPullRequestEntry] {
        self.list
            .merged
            .as_ref()
            .map_or(&[], |merged| merged.entries.as_slice())
    }

    pub fn detail(&self, reference: &PullRequestRef) -> Option<&PullRequestDetailSnapshot> {
        self.details.get(&pull_request_key(reference))
    }

    pub fn thread_links(&self, thread_id: &ThreadId) -> &[ThreadPullRequestLink] {
        self.thread_links
            .get(thread_id.as_str())
            .map_or(&[], Vec::as_slice)
    }

    /// The active threads the thread picker offers (`useThreadShells`).
    pub fn threads(&self) -> &[ThreadChoice] {
        &self.threads
    }

    pub fn set_threads(&mut self, threads: Vec<ThreadChoice>, cx: &mut Context<Self>) {
        self.threads = threads;
        cx.notify();
    }

    pub fn update_list(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut PullRequestListSnapshot),
    ) {
        update(&mut self.list);
        cx.notify();
    }

    pub fn update_detail(
        &mut self,
        reference: &PullRequestRef,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut PullRequestDetailSnapshot),
    ) {
        update(self.details.entry(pull_request_key(reference)).or_default());
        cx.notify();
    }

    pub fn set_thread_links(
        &mut self,
        thread_id: &ThreadId,
        links: Vec<ThreadPullRequestLink>,
        cx: &mut Context<Self>,
    ) {
        self.thread_links
            .insert(thread_id.as_str().to_owned(), links);
        cx.notify();
    }
}

impl Default for PullRequestStore {
    fn default() -> Self {
        Self::new()
    }
}
