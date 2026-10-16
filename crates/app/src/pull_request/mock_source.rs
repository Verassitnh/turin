//! A [`PullRequestSource`] that serves fixtures and applies the reader's commands to them, so the
//! views can be exercised (and photographed) without a code host. Same pattern as the thread
//! view's `MockTimelineSource` and the composer's `MockComposerSource`; T22 swaps in the live
//! source-control service.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Entity, WeakEntity};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestFileViewed, PullRequestFileViewedState, PullRequestMergeMethod,
    PullRequestReactionContent, PullRequestRef, PullRequestState, PullRequestUpdateMethod,
};
use turin_contracts::pull_request_review_guide::PullRequestReviewGuideRun;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;
use turin_contracts::{EnvironmentId, ThreadId};

use super::fixtures;
use super::pull_request_list_logic::merge_pull_request_lists;
use super::view_model::{
    LinkedThread, Loadable, PullRequestDetailSnapshot, PullRequestProject, PullRequestSource,
    PullRequestStore, ReviewSubmission,
};

/// The environment the fixtures claim to come from.
pub fn demo_environment_id() -> EnvironmentId {
    crate::state::local_environment_id()
}

pub struct MockPullRequestSource {
    store: WeakEntity<PullRequestStore>,
}

impl MockPullRequestSource {
    /// Creates the store, fills the listing and wires the source in.
    pub fn start(cx: &mut App) -> Entity<PullRequestStore> {
        let store = cx.new(|_| PullRequestStore::new());
        let source = Arc::new(Self {
            store: store.downgrade(),
        });
        store.update(cx, |store, cx| {
            store.set_source(source);
            let merged =
                merge_pull_request_lists(&[(demo_environment_id(), fixtures::list_result())]);
            store.set_thread_links(&demo_thread_id(), fixtures::thread_links(), cx);
            store.set_threads(fixtures::thread_choices(), cx);
            store.update_list(cx, |list| {
                list.merged = merged;
                list.projects = vec![PullRequestProject {
                    id: fixtures::DEMO_PROJECT_ID.to_owned(),
                    environment_id: demo_environment_id(),
                    title: fixtures::DEMO_PROJECT_TITLE.to_owned(),
                    repository: Some(fixtures::DEMO_REPOSITORY.to_owned()),
                }];
                list.projects_known = true;
            });
        });
        store
    }

    fn with_store(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut PullRequestStore, &mut gpui_kit::Context<PullRequestStore>),
    ) {
        if let Some(store) = self.store.upgrade() {
            store.update(cx, update);
        }
    }

    /// Runs `update` on the detail after `delay_ms`, as a host round trip would.
    fn later(
        &self,
        delay_ms: u64,
        cx: &mut App,
        reference: &PullRequestRef,
        update: impl FnOnce(&mut PullRequestDetailSnapshot) + 'static,
    ) {
        let store = self.store.clone();
        let reference = reference.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            if let Some(store) = store.upgrade() {
                store.update(cx, |store, cx| store.update_detail(&reference, cx, update));
            }
        })
        .detach();
    }

    fn list_entry(
        &self,
        reference: &PullRequestRef,
        cx: &App,
    ) -> Option<super::pull_request_list_logic::EnvironmentPullRequestEntry> {
        let store = self.store.upgrade()?;
        store
            .read(cx)
            .entries()
            .iter()
            .find(|entry| {
                entry.number.get() == reference.number.get()
                    && entry
                        .repository
                        .as_str()
                        .eq_ignore_ascii_case(reference.repository.as_str())
            })
            .cloned()
    }

    fn patch_list_state(
        &self,
        cx: &mut App,
        reference: &PullRequestRef,
        state: PullRequestState,
        is_draft: Option<bool>,
    ) {
        let number = reference.number.get();
        self.with_store(cx, |store, cx| {
            store.update_list(cx, |list| {
                if let Some(merged) = list.merged.as_mut() {
                    for entry in &mut merged.entries {
                        if entry.number.get() == number {
                            entry.state = state;
                            if let Some(is_draft) = is_draft {
                                entry.is_draft = is_draft;
                            }
                            entry.updated_at = fixtures::ago(0);
                        }
                    }
                }
            });
        });
    }
}

impl PullRequestSource for MockPullRequestSource {
    fn refresh_list(&self, cx: &mut App) {
        self.with_store(cx, |store, cx| {
            store.update_list(cx, |list| list.refreshing = true)
        });
        let store = self.store.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            if let Some(store) = store.upgrade() {
                store.update(cx, |store, cx| {
                    store.update_list(cx, |list| list.refreshing = false)
                });
            }
        })
        .detach();
    }

    fn load_more(&self, _cx: &mut App) {}

    fn open_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        let entry = self.list_entry(reference, cx);
        let is_demo = reference.number.get() == fixtures::DEMO_DETAIL_NUMBER;
        let detail = match &entry {
            Some(entry) => fixtures::detail_view_for(entry),
            None => fixtures::detail_view(),
        };
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if !snapshot.detail.is_pending() {
                    return;
                }
                snapshot.can_write = true;
                snapshot.activity_pending = false;
                snapshot.detail = Loadable::Ready(detail);
                snapshot.stack_loaded = true;
                snapshot.checks = Loadable::Ready(if is_demo {
                    fixtures::checks()
                } else {
                    turin_contracts::pull_request::PullRequestChecks {
                        state: PullRequestState::Open,
                        checks: Vec::new(),
                    }
                });
                if is_demo {
                    snapshot.stack = Some(fixtures::stack());
                    snapshot.files_viewed = fixtures::files_viewed().files;
                    snapshot.linked_threads = fixtures::linked_threads()
                        .threads
                        .into_iter()
                        .map(|thread| LinkedThread {
                            id: thread.id,
                            project_id: thread.project_id.as_str().to_owned(),
                            title: thread.title,
                            archived: thread.archived_at.is_some(),
                        })
                        .collect();
                    snapshot.guide = Loadable::Ready(fixtures::guide_result());
                } else {
                    snapshot.guide = Loadable::Ready(
                        turin_contracts::pull_request_review_guide::PullRequestReviewGuideResult {
                            guide: None,
                            run: None,
                            current_head_sha: None,
                        },
                    );
                }
            });
        });
    }

    fn refresh_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        let _ = (reference, cx);
    }

    fn run_action(
        &self,
        reference: &PullRequestRef,
        action: PullRequestAction,
        _merge_method: Option<PullRequestMergeMethod>,
        _update_method: Option<PullRequestUpdateMethod>,
        cx: &mut App,
    ) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                snapshot.pending_action = Some(action);
                snapshot.action_error = None;
            });
        });
        let list_state = match action {
            PullRequestAction::Close => Some((PullRequestState::Closed, None)),
            PullRequestAction::Reopen => Some((PullRequestState::Open, None)),
            PullRequestAction::Merge => Some((PullRequestState::Merged, None)),
            PullRequestAction::Draft => Some((PullRequestState::Open, Some(true))),
            PullRequestAction::Ready => Some((PullRequestState::Open, Some(false))),
            _ => None,
        };
        if let Some((state, is_draft)) = list_state {
            self.patch_list_state(cx, reference, state, is_draft);
        }
        self.later(450, cx, reference, move |snapshot| {
            snapshot.pending_action = None;
            if let Loadable::Ready(detail) = &mut snapshot.detail {
                match action {
                    PullRequestAction::Close => {
                        detail.state = PullRequestState::Closed;
                        detail.closed_at = Some(fixtures::ago(0));
                    }
                    PullRequestAction::Reopen => {
                        detail.state = PullRequestState::Open;
                        detail.closed_at = None;
                    }
                    PullRequestAction::Merge => {
                        detail.state = PullRequestState::Merged;
                        detail.merged_at = Some(fixtures::ago(0));
                        detail.closed_at = Some(fixtures::ago(0));
                    }
                    PullRequestAction::Draft => detail.is_draft = true,
                    PullRequestAction::Ready => detail.is_draft = false,
                    PullRequestAction::EnableAutoMerge => detail.auto_merge_enabled = Some(true),
                    PullRequestAction::DisableAutoMerge => detail.auto_merge_enabled = Some(false),
                    PullRequestAction::UpdateBranch => {
                        detail.base_comparison = Some(
                            turin_contracts::pull_request::PullRequestBaseComparison::UpToDate,
                        );
                        detail.behind_by = None;
                    }
                    _ => {}
                }
                detail.updated_at = fixtures::ago(0);
            }
        });
    }

    fn post_comment(&self, reference: &PullRequestRef, body: String, cx: &mut App) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail {
                    let id = format!("IC_new_{}", detail.comments.len());
                    detail.comments.push(fixtures::new_comment(&id, &body));
                    detail.comment_count += 1;
                }
            });
        });
    }

    fn update_comment(
        &self,
        reference: &PullRequestRef,
        comment_id: &str,
        _kind: turin_contracts::pull_request::PullRequestCommentUpdateKind,
        body: String,
        cx: &mut App,
    ) {
        let comment_id = comment_id.to_owned();
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail {
                    for comment in detail
                        .comments
                        .iter_mut()
                        .filter(|c| c.id.as_str() == comment_id)
                    {
                        comment.body = body.clone();
                        comment.edited_at = Some(Some(fixtures::ago(0)));
                    }
                    for thread in &mut detail.review_threads {
                        for comment in thread
                            .comments
                            .iter_mut()
                            .filter(|c| c.id.as_str() == comment_id)
                        {
                            comment.body = body.clone();
                            comment.edited_at = Some(Some(fixtures::ago(0)));
                        }
                    }
                }
            });
        });
    }

    fn update_title(&self, reference: &PullRequestRef, title: String, cx: &mut App) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail
                    && let Ok(title) = turin_contracts::TrimmedNonEmptyString::new(title)
                {
                    detail.title = title;
                }
            });
        });
    }

    fn update_body(&self, reference: &PullRequestRef, body: String, cx: &mut App) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail {
                    detail.body = body;
                }
            });
        });
    }

    fn submit_review(&self, reference: &PullRequestRef, review: ReviewSubmission, cx: &mut App) {
        let body = review.body;
        self.post_comment(reference, body, cx);
    }

    fn reply_to_thread(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        body: String,
        cx: &mut App,
    ) {
        let thread_id = thread_id.to_owned();
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail
                    && let Some(thread) = detail
                        .review_threads
                        .iter_mut()
                        .find(|t| t.id.as_str() == thread_id)
                {
                    let comment = fixtures::new_thread_comment(
                        &format!("RC_new_{}", thread.comments.len()),
                        &body,
                    );
                    thread.comments.push(comment);
                }
            });
        });
    }

    fn set_thread_resolved(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        resolved: bool,
        cx: &mut App,
    ) {
        let thread_id = thread_id.to_owned();
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail
                    && let Some(thread) = detail
                        .review_threads
                        .iter_mut()
                        .find(|t| t.id.as_str() == thread_id)
                {
                    thread.is_resolved = resolved;
                }
            });
        });
    }

    fn toggle_reaction(
        &self,
        reference: &PullRequestRef,
        _subject_id: &str,
        content: PullRequestReactionContent,
        cx: &mut App,
    ) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                if let Loadable::Ready(detail) = &mut snapshot.detail
                    && let Some(reactions) = detail.reactions.as_mut()
                    && let Some(reaction) = reactions.iter_mut().find(|r| r.content == content)
                {
                    reaction.viewer_has_reacted = !reaction.viewer_has_reacted;
                }
            });
        });
    }

    fn load_diff(&self, reference: &PullRequestRef, commit: Option<String>, cx: &mut App) {
        let key = commit.clone();
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                snapshot
                    .diffs
                    .entry(key.clone())
                    .or_insert(Loadable::Pending);
            });
        });
        self.later(250, cx, reference, move |snapshot| {
            snapshot
                .diffs
                .insert(commit, Loadable::Ready(fixtures::diff_result()));
        });
    }

    fn load_diff_file_contents(
        &self,
        reference: &PullRequestRef,
        _commit: Option<String>,
        _change_type: turin_contracts::pull_request::PullRequestDiffFileChangeType,
        _old_path: &str,
        new_path: &str,
        cx: &mut App,
    ) {
        let key = new_path.to_owned();
        let contents = fixtures::diff_file_contents(new_path);
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                snapshot
                    .file_contents
                    .entry(key.clone())
                    .or_insert(Loadable::Pending);
            });
        });
        self.later(250, cx, reference, move |snapshot| {
            snapshot.file_contents.insert(
                key,
                match contents {
                    Some(contents) => Loadable::Ready(contents),
                    None => Loadable::Failed("The host has no contents for this file.".into()),
                },
            );
        });
    }

    fn set_files_viewed(
        &self,
        reference: &PullRequestRef,
        files: Vec<(String, bool)>,
        cx: &mut App,
    ) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                for (path, viewed) in files {
                    let state = if viewed {
                        PullRequestFileViewedState::Viewed
                    } else {
                        PullRequestFileViewedState::Unviewed
                    };
                    match snapshot.files_viewed.iter_mut().find(|f| f.path == path) {
                        Some(file) => file.state = state,
                        None => snapshot
                            .files_viewed
                            .push(PullRequestFileViewed { path, state }),
                    }
                }
            });
        });
    }

    fn request_reviewers(&self, _reference: &PullRequestRef, _logins: Vec<String>, _cx: &mut App) {}

    fn set_labels(&self, _reference: &PullRequestRef, _labels: Vec<String>, _cx: &mut App) {}

    fn generate_guide(&self, reference: &PullRequestRef, cx: &mut App) {
        // Dev hook for screenshots: the model is not set up, so no run starts.
        if std::env::var("TURIN_PR_GUIDE_FAIL").is_ok_and(|value| value == "1") {
            self.with_store(cx, |store, cx| {
                store.update_detail(reference, cx, |snapshot| {
                    snapshot.guide_start_error = Some("No model is selected.".into());
                });
            });
            return;
        }
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| snapshot.guide_start_error = None);
        });
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                let head = fixtures::guide_result().current_head_sha;
                if let (Some(head_sha), Loadable::Ready(result)) = (head, &mut snapshot.guide) {
                    result.run = Some(PullRequestReviewGuideRun::Running {
                        head_sha,
                        started_at: fixtures::ago(0),
                    });
                }
            });
        });
        self.later(1800, cx, reference, |snapshot| {
            let mut result = fixtures::guide_result();
            // A new guide is stamped when it is written, which is how the coordinator knows it ended.
            if let Some(guide) = result.guide.as_mut() {
                guide.generated_at = fixtures::ago(0);
            }
            snapshot.guide = Loadable::Ready(result);
        });
    }

    fn link_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        let Some(entry) = self.list_entry(reference, cx) else {
            return;
        };
        let link = fixtures::link_for_entry(&entry);
        let thread_id = thread_id.clone();
        self.with_store(cx, |store, cx| {
            let mut links = store.thread_links(&thread_id).to_vec();
            if links.iter().all(|existing| existing.number != link.number) {
                links.push(link);
            }
            store.set_thread_links(&thread_id, links, cx);
        });
    }

    fn change_thread_link(
        &self,
        mutation: crate::state::helpers::thread_pull_request_compatibility::ThreadPullRequestMutation,
        cx: &mut App,
    ) -> Result<(), String> {
        use crate::state::helpers::thread_pull_request_compatibility::ThreadPullRequestMutation as Mutation;
        let (thread_id, host, repository, number, linked) = match mutation {
            Mutation::Link(input) => (
                input.thread_id,
                input.host,
                input.repository,
                input.number,
                true,
            ),
            Mutation::Unlink(input) => (
                input.thread_id,
                input.host,
                input.repository,
                input.number,
                false,
            ),
            Mutation::MetaUpdate(_) => {
                return Err(crate::hooks::use_pull_request_linking::LINKING_UNSUPPORTED.to_owned());
            }
        };
        let reference = turin_contracts::PositiveInt::new(number)
            .and_then(|number| reference_for(&host, &repository, number))
            .ok_or_else(|| {
                crate::hooks::use_pull_request_linking::NOT_AVAILABLE_IN_ENVIRONMENT.to_owned()
            })?;
        if linked {
            self.link_thread(&thread_id, &reference, cx);
        } else {
            self.unlink_thread(&thread_id, &reference, cx);
        }
        Ok(())
    }

    fn unlink_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        let thread_id = thread_id.clone();
        let number = reference.number;
        self.with_store(cx, |store, cx| {
            let links: Vec<_> = store
                .thread_links(&thread_id)
                .iter()
                .filter(|link| link.number != number)
                .cloned()
                .collect();
            store.set_thread_links(&thread_id, links, cx);
        });
    }

    fn open_thread(&self, _thread_id: &ThreadId, _cx: &mut App) {}
}

/// A reference to a pull request of the demo repository.
pub fn reference_for(
    host: &str,
    repository: &str,
    number: turin_contracts::PositiveInt,
) -> Option<PullRequestRef> {
    serde_json::from_value(serde_json::json!({
        "projectId": fixtures::DEMO_PROJECT_ID,
        "host": host,
        "repository": repository,
        "number": number.get(),
    }))
    .ok()
}

/// The thread whose links the demo thread panel shows.
pub fn demo_thread_id() -> ThreadId {
    ThreadId::new("thread-pr-views").expect("static id is non-blank")
}

/// Thread links of the demo thread, for the thread panel.
pub fn demo_thread_links() -> Vec<ThreadPullRequestLink> {
    fixtures::thread_links()
}

/// The reference the detail fixtures answer.
pub fn demo_reference() -> PullRequestRef {
    serde_json::from_value(serde_json::json!({
        "projectId": fixtures::DEMO_PROJECT_ID,
        "host": fixtures::DEMO_HOST,
        "repository": fixtures::DEMO_REPOSITORY,
        "number": fixtures::DEMO_DETAIL_NUMBER,
    }))
    .expect("demo reference matches the contract")
}

/// The thread's own project for the link dialog: bare numbers resolve against this repository.
pub fn demo_link_project() -> super::link_dialog::LinkDialogProject {
    super::link_dialog::LinkDialogProject {
        host: fixtures::DEMO_HOST.to_owned(),
        repository: fixtures::DEMO_REPOSITORY.to_owned(),
        readable_repositories: vec![
            (
                fixtures::DEMO_HOST.to_owned(),
                fixtures::DEMO_REPOSITORY.to_owned(),
            ),
            (
                fixtures::DEMO_HOST.to_owned(),
                "pingdotgg/t3code".to_owned(),
            ),
        ],
    }
}
