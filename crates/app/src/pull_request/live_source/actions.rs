//! Port of upstream/packages/client-runtime/src/state/pullRequests.ts commands.
use super::*;

impl PullRequestSource for LivePullRequestSource {
    fn refresh_list(&self, cx: &mut App) {
        self.read_list(false, true, cx);
    }
    fn load_more(&self, cx: &mut App) {
        self.read_list(true, false, cx);
    }
    fn open_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        self.read_detail(reference, false, cx);
    }
    fn refresh_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        self.read_detail(reference, true, cx);
    }

    fn run_action(
        &self,
        reference: &PullRequestRef,
        action: PullRequestAction,
        merge_method: Option<PullRequestMergeMethod>,
        update_method: Option<PullRequestUpdateMethod>,
        cx: &mut App,
    ) {
        self.mutate(
            reference,
            Some(action),
            json!({"action":action,"mergeMethod":merge_method,"updateMethod":update_method}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .run_action(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn post_comment(&self, reference: &PullRequestRef, body: String, cx: &mut App) {
        self.mutate(
            reference,
            None,
            json!({"body":body}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .comment(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn update_title(&self, reference: &PullRequestRef, title: String, cx: &mut App) {
        self.mutate(
            reference,
            None,
            json!({"title":title}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .update(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn update_body(&self, reference: &PullRequestRef, body: String, cx: &mut App) {
        self.mutate(
            reference,
            None,
            json!({"body":body}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .update(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn update_comment(
        &self,
        reference: &PullRequestRef,
        comment_id: &str,
        kind: PullRequestCommentUpdateKind,
        body: String,
        cx: &mut App,
    ) {
        self.mutate(
            reference,
            None,
            json!({"commentId":comment_id,"kind":kind,"body":body}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .update_comment(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn submit_review(&self, reference: &PullRequestRef, review: ReviewSubmission, cx: &mut App) {
        self.submit_review_with_comments(reference, review, Vec::new(), cx);
    }
    fn submit_review_with_comments(
        &self,
        reference: &PullRequestRef,
        review: ReviewSubmission,
        comments: Vec<PullRequestReviewCommentDraft>,
        cx: &mut App,
    ) {
        self.mutate(
            reference,
            None,
            json!({"verdict":review.verdict,"body":review.body,"comments":comments}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .submit_review(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn reply_to_thread(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        body: String,
        cx: &mut App,
    ) {
        self.mutate(
            reference,
            None,
            json!({"threadId":thread_id,"body":body}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .reply_to_thread(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn set_thread_resolved(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        resolved: bool,
        cx: &mut App,
    ) {
        self.mutate(
            reference,
            None,
            json!({"threadId":thread_id,"resolved":resolved}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .set_thread_resolution(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn load_thread_comments(
        &self,
        reference: &PullRequestRef,
        thread_id: &str,
        cursor: &str,
        cx: &mut App,
    ) {
        let value = input(reference, json!({"threadId":thread_id,"cursor":cursor}));
        let reference = reference.clone();
        let thread_id = thread_id.to_owned();
        let cursor = cursor.to_owned();
        self.task(
            cx,
            move |rt| async move {
                rt.pull_requests
                    .thread_comments(&decode(value)?)
                    .await
                    .map_err(failure)
            },
            move |store, cx, result| {
                store.update_detail(&reference, cx, |s| match result {
                    Ok(page) => {
                        if let Loadable::Ready(detail) = &mut s.detail
                            && let Some(thread) = detail
                                .review_threads
                                .iter_mut()
                                .find(|t| t.id.as_str() == thread_id)
                            && thread
                                .next_comments_cursor
                                .as_ref()
                                .is_some_and(|c| c.as_str() == cursor)
                        {
                            for comment in page.comments {
                                if !thread.comments.iter().any(|c| c.id == comment.id) {
                                    thread.comments.push(comment);
                                }
                            }
                            thread.next_comments_cursor = page.next_cursor;
                        }
                    }
                    Err(error) => s.action_error = Some(error.message),
                });
            },
        );
    }
    fn toggle_reaction(
        &self,
        reference: &PullRequestRef,
        subject_id: &str,
        content: PullRequestReactionContent,
        cx: &mut App,
    ) {
        let reacted = self
            .store
            .upgrade()
            .and_then(|store| {
                store
                    .read(cx)
                    .detail(reference)
                    .and_then(|s| s.detail.ready())
                    .map(|detail| {
                        let reactions = if subject_id.is_empty() {
                            detail.reactions.as_ref()
                        } else {
                            detail
                                .comments
                                .iter()
                                .find(|c| c.id.as_str() == subject_id)
                                .and_then(|c| c.reactions.as_ref())
                                .or_else(|| {
                                    detail
                                        .review_threads
                                        .iter()
                                        .flat_map(|t| t.comments.iter())
                                        .find(|c| c.id.as_str() == subject_id)
                                        .and_then(|c| c.reactions.as_ref())
                                })
                        };
                        !reactions.is_some_and(|rs| {
                            rs.iter()
                                .any(|r| r.content == content && r.viewer_has_reacted)
                        })
                    })
            })
            .unwrap_or(true);
        self.mutate(reference, None, json!({"subjectId":(!subject_id.is_empty()).then_some(subject_id),"content":content,"reacted":reacted}), cx,
            |rt, value| async move { rt.pull_requests.set_reaction(&decode(value)?).await.map_err(failure) });
    }
    fn load_diff(&self, reference: &PullRequestRef, commit: Option<String>, cx: &mut App) {
        self.read_diff(reference, commit, None, cx);
    }
    fn load_diff_page(
        &self,
        reference: &PullRequestRef,
        commit: Option<String>,
        cursor: &str,
        cx: &mut App,
    ) {
        self.read_diff(reference, commit, Some(cursor.to_owned()), cx);
    }
    fn load_diff_file_contents(
        &self,
        reference: &PullRequestRef,
        commit: Option<String>,
        change_type: PullRequestDiffFileChangeType,
        old_path: &str,
        new_path: &str,
        cx: &mut App,
    ) {
        let value = input(
            reference,
            json!({"commit":commit,"changeType":change_type,"oldPath":old_path,"newPath":new_path}),
        );
        let path = new_path.to_owned();
        let reference = reference.clone();
        self.with_store(cx, |store, cx| {
            store.update_detail(&reference, cx, |s| {
                s.file_contents.insert(path.clone(), Loadable::Pending);
            })
        });
        self.task(
            cx,
            move |rt| async move {
                rt.pull_requests
                    .diff_file_contents(&decode(value)?)
                    .await
                    .map_err(failure)
            },
            move |store, cx, result| {
                store.update_detail(&reference, cx, |s| {
                    s.file_contents.insert(
                        path,
                        match result {
                            Ok(value) => Loadable::Ready(value),
                            Err(error) => Loadable::Failed(error.message),
                        },
                    );
                });
            },
        );
    }
    fn set_files_viewed(
        &self,
        reference: &PullRequestRef,
        files: Vec<(String, bool)>,
        cx: &mut App,
    ) {
        let files: Vec<_> = files
            .into_iter()
            .map(|(path, viewed)| PullRequestSetFilesViewedFile { path, viewed })
            .collect();
        self.mutate(
            reference,
            None,
            json!({"files":files}),
            cx,
            |rt, value| async move {
                rt.pull_requests
                    .set_files_viewed(&decode(value)?)
                    .await
                    .map_err(failure)
            },
        );
    }
    fn request_reviewers(&self, reference: &PullRequestRef, logins: Vec<String>, cx: &mut App) {
        let candidates = self.store.upgrade().and_then(|store| {
            store
                .read(cx)
                .detail(reference)
                .and_then(|s| s.reviewer_candidates.clone())
        });
        let Some(candidates) = candidates else {
            self.with_store(cx, |store, cx| {
                store.update_detail(reference, cx, |s| {
                    s.action_error =
                        Some("Reviewer candidates have not loaded. Refresh and try again.".into())
                })
            });
            return;
        };
        let changes: Vec<_> = candidates
            .candidates
            .into_iter()
            .filter_map(|candidate| {
                let requested = logins.iter().any(|login| {
                    candidate.login.as_str() == login || candidate.id.as_str() == login
                });
                (requested != candidate.is_requested).then_some((candidate, requested))
            })
            .collect();
        self.mutate(
            reference,
            None,
            json!({}),
            cx,
            move |rt, value| async move {
                for requested in [true, false] {
                    let reviewers: Vec<_> = changes
                        .iter()
                        .filter(|(_, state)| *state == requested)
                        .map(|(c, _)| json!({"id":c.id,"kind":c.kind}))
                        .collect();
                    if reviewers.is_empty() {
                        continue;
                    }
                    let mut value = value.clone();
                    value.as_object_mut().expect("object").extend(
                        json!({"reviewers":reviewers,"requested":requested})
                            .as_object()
                            .expect("object")
                            .clone(),
                    );
                    rt.pull_requests
                        .request_reviewers(&decode(value)?)
                        .await
                        .map_err(failure)?;
                }
                Ok(())
            },
        );
    }
    fn set_labels(&self, reference: &PullRequestRef, labels: Vec<String>, cx: &mut App) {
        let candidates = self.store.upgrade().and_then(|store| {
            store
                .read(cx)
                .detail(reference)
                .and_then(|s| s.label_candidates.clone())
        });
        let Some(candidates) = candidates else {
            self.with_store(cx, |store, cx| {
                store.update_detail(reference, cx, |s| {
                    s.action_error =
                        Some("Label candidates have not loaded. Refresh and try again.".into())
                })
            });
            return;
        };
        let changes: Vec<_> = candidates
            .candidates
            .into_iter()
            .filter_map(|candidate| {
                let applied = labels.iter().any(|name| candidate.name.as_str() == name);
                (applied != candidate.is_applied).then_some((candidate.name, applied))
            })
            .collect();
        self.mutate(
            reference,
            None,
            json!({}),
            cx,
            move |rt, value| async move {
                for applied in [true, false] {
                    let labels: Vec<_> = changes
                        .iter()
                        .filter(|(_, state)| *state == applied)
                        .map(|(name, _)| name)
                        .collect();
                    if labels.is_empty() {
                        continue;
                    }
                    let mut value = value.clone();
                    value.as_object_mut().expect("object").extend(
                        json!({"labels":labels,"applied":applied})
                            .as_object()
                            .expect("object")
                            .clone(),
                    );
                    rt.pull_requests
                        .set_labels(&decode(value)?)
                        .await
                        .map_err(failure)?;
                }
                Ok(())
            },
        );
    }
    fn refresh_guide(&self, reference: &PullRequestRef, cx: &mut App) {
        let r = reference.clone();
        let reference = reference.clone();
        self.task(
            cx,
            move |rt| async move { rt.review_guides.get(&r).await.map_err(failure) },
            move |store, cx, result| {
                store.update_detail(&reference, cx, |s| {
                    s.guide = match result {
                        Ok(guide) => Loadable::Ready(guide),
                        Err(error) => Loadable::Failed(error.message),
                    }
                });
            },
        );
    }
    fn generate_guide(&self, reference: &PullRequestRef, cx: &mut App) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |s| s.guide_start_error = None)
        });
        let r = reference.clone();
        let reference = reference.clone();
        let source = self.clone();
        self.task(
            cx,
            move |rt| async move { rt.review_guides.generate(&r).await.map_err(failure) },
            move |store, cx, result| match result {
                Ok(_) => source.refresh_guide(&reference, cx),
                Err(error) => store.update_detail(&reference, cx, |s| {
                    s.guide_start_error = Some(error.message)
                }),
            },
        );
    }
    fn link_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        let url = self
            .store
            .upgrade()
            .and_then(|store| {
                store
                    .read(cx)
                    .detail(reference)
                    .and_then(|s| s.detail.ready())
                    .map(|d| d.url.as_str().to_owned())
            })
            .unwrap_or_else(|| {
                format!(
                    "https://{}/{}/pull/{}",
                    reference.host.as_ref().map_or("github.com", |h| h.as_str()),
                    reference.repository,
                    reference.number.get()
                )
            });
        self.link_command(thread_id.clone(), "thread.pull-request.link", json!({"host":reference.host.as_ref().map_or("github.com", |h| h.as_str()),"repository":reference.repository,"number":reference.number,"url":url,"source":"manual"}), cx);
    }
    fn unlink_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        self.link_command(thread_id.clone(), "thread.pull-request.unlink", json!({"host":reference.host.as_ref().map_or("github.com", |h| h.as_str()),"repository":reference.repository,"number":reference.number}), cx);
    }
    fn change_thread_link(
        &self,
        mutation: ThreadPullRequestMutation,
        cx: &mut App,
    ) -> Result<(), String> {
        // Validate readiness synchronously, then dispatch once; host/commit failures go to the store.
        self.bus.commands().map_err(|e| e.message)?;
        match mutation {
            ThreadPullRequestMutation::Link(v) => self.link_command(v.thread_id, "thread.pull-request.link", json!({"host":v.host,"repository":v.repository,"number":v.number,"url":v.url,"source":"manual"}), cx),
            ThreadPullRequestMutation::Unlink(v) => self.link_command(v.thread_id, "thread.pull-request.unlink", json!({"host":v.host,"repository":v.repository,"number":v.number}), cx),
            ThreadPullRequestMutation::MetaUpdate(v) => self.link_command(v.thread_id, "thread.meta.update", json!({"linkedPullRequest":v.linked_pull_request.map(|p| json!({"projectId":p.project_id,"repository":p.repository,"number":p.number,"url":p.url}))}), cx),
        }
        Ok(())
    }
    fn open_thread(&self, thread_id: &ThreadId, cx: &mut App) {
        if let Some(navigate) = &self.navigate {
            navigate(thread_id, cx);
        } else {
            self.with_store(cx, |store, cx| store.update_list(cx, |list| list.error = Some("Thread navigation is unavailable. The shell must mount start_with_navigation.".into())));
        }
    }
}

impl LivePullRequestSource {
    fn read_diff(
        &self,
        reference: &PullRequestRef,
        commit: Option<String>,
        cursor: Option<String>,
        cx: &mut App,
    ) {
        let value = input(reference, json!({"commit":commit,"cursor":cursor}));
        let reference = reference.clone();
        self.with_store(cx, |store, cx| {
            store.update_detail(&reference, cx, |s| {
                s.diffs.entry(commit.clone()).or_insert(Loadable::Pending);
            })
        });
        self.task(
            cx,
            move |rt| async move {
                rt.pull_requests
                    .diff(&decode(value)?)
                    .await
                    .map_err(failure)
            },
            move |store, cx, result| {
                store.update_detail(&reference, cx, |s| match result {
                    Ok(mut page) => {
                        if let Some(cursor) = cursor {
                            let Some(Loadable::Ready(old)) = s.diffs.get(&commit) else {
                                return;
                            };
                            if old
                                .next_cursor
                                .as_ref()
                                .is_none_or(|c| c.as_str() != cursor)
                            {
                                return;
                            }
                            page.patch = format!("{}{}", old.patch, page.patch);
                            page.truncated |= old.truncated;
                            let mut stats = old.omitted_file_stats.clone().unwrap_or_default();
                            stats.extend(page.omitted_file_stats.take().unwrap_or_default());
                            page.omitted_file_stats = (!stats.is_empty()).then_some(stats);
                        }
                        s.diffs.insert(commit, Loadable::Ready(page));
                    }
                    Err(error) => {
                        if cursor.is_some() {
                            s.action_error = Some(error.message);
                        } else {
                            s.diffs.insert(commit, Loadable::Failed(error.message));
                        }
                    }
                });
            },
        );
    }

    fn link_command(&self, thread_id: ThreadId, kind: &'static str, extra: Value, cx: &mut App) {
        let id = thread_id.clone();
        let source = self.clone();
        let task = self.bus.task(cx, move |commands| async move {
            commands.thread_command(kind, &id, extra).await?;
            commands
                .runtime()
                .stores
                .projections
                .get_thread_shell(&id)
                .await
                .map_err(failure)
        });
        let store = self.store.clone();
        cx.spawn(async move |cx| {
            let result = task.await;
            if let Some(store) = store.upgrade() {
                store.update(cx, |store, cx| match result {
                    Ok(Some(thread)) => {
                        store.set_thread_links(
                            &thread_id,
                            crate::state::support::thread_pull_requests::thread_pull_requests_of(
                                thread.pull_requests.as_deref(),
                                thread
                                    .linked_pull_request
                                    .as_ref()
                                    .and_then(|linked| linked.as_ref()),
                            ),
                            cx,
                        );

                        let references: Vec<_> = source.opened.borrow().values().cloned().collect();
                        for reference in references {
                            source.read_links(&reference, cx);
                        }
                    }
                    Ok(None) => store.update_list(cx, |list| {
                        list.error = Some("The linked thread could not be reloaded.".into())
                    }),
                    Err(error) => store.update_list(cx, |list| list.error = Some(error.message)),
                });
            }
        })
        .detach();
    }
}
