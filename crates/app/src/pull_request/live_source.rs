//! Port of upstream/packages/client-runtime/src/pullRequestEnvironment.ts.
//! In-process adapter: the existing GitHub services feed the PR view-model seam.

use super::{pull_request_list_logic::merge_pull_request_lists, view_model::*};
use crate::{
    app_runtime::write::{bus::CommandBus, commands::CommandError, runtime::WriteRuntime},
    state::{
        helpers::thread_pull_request_compatibility::ThreadPullRequestMutation,
        store::ClientStateHandle,
    },
};
use gpui_kit::{App, AppContext as _, Entity, WeakEntity};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use turin_contracts::{ThreadId, pull_request::*};

type Navigate = Arc<dyn Fn(&ThreadId, &mut App)>;

#[derive(Clone)]
pub struct LivePullRequestSource {
    store: WeakEntity<PullRequestStore>,
    bus: CommandBus,
    client: ClientStateHandle,
    navigate: Option<Navigate>,
    query: Rc<RefCell<PullRequestListInput>>,
    list_generation: Rc<Cell<u64>>,
    detail_generations: Rc<RefCell<HashMap<String, u64>>>,
    opened: Rc<RefCell<HashMap<String, PullRequestRef>>>,
}

pub fn start(bus: CommandBus, client: ClientStateHandle, cx: &mut App) -> Entity<PullRequestStore> {
    create(bus, client, None, cx)
}

pub fn start_with_navigation(
    bus: CommandBus,
    client: ClientStateHandle,
    navigate: Navigate,
    cx: &mut App,
) -> Entity<PullRequestStore> {
    create(bus, client, Some(navigate), cx)
}

fn create(
    bus: CommandBus,
    client: ClientStateHandle,
    navigate: Option<Navigate>,
    cx: &mut App,
) -> Entity<PullRequestStore> {
    let store = cx.new(|_| PullRequestStore::new());
    let source = Arc::new(LivePullRequestSource {
        store: store.downgrade(),
        bus,
        client,
        navigate,
        query: Rc::new(RefCell::new(PullRequestListInput {
            state: PullRequestListState::All,
            involvement: None,
            filters: None,
            project_id: None,
            project_ids: None,
            host: None,
            limit: Some(99),
            cursors: None,
            query: None,
        })),
        list_generation: Rc::new(Cell::new(0)),
        detail_generations: Rc::default(),
        opened: Rc::default(),
    });
    store.update(cx, |store, _| store.set_source(source.clone()));
    source.refresh_list(cx);
    // Retry startup and keep project/thread choices current without making renders do IO.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_secs(30))
                .await;
            if source.store.upgrade().is_none() {
                break;
            }
            let _ = cx.update(|cx| source.refresh_list(cx));
        }
    })
    .detach();
    store
}

fn failure(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(error.to_string())
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, CommandError> {
    serde_json::from_value(value).map_err(failure)
}

/// Build existing typed service inputs, preserving routing/account fields and their validation.
fn input(reference: &PullRequestRef, extra: Value) -> Value {
    let mut value = serde_json::to_value(reference).expect("PullRequestRef serializes");
    if let (Some(value), Value::Object(extra)) = (value.as_object_mut(), extra) {
        value.extend(extra);
    }
    value
}

fn detail_view(detail: PullRequestDetail) -> Result<PullRequestDetailView, CommandError> {
    let mut value = serde_json::to_value(detail).map_err(failure)?;
    value.as_object_mut().expect("detail is an object").extend(
        json!({"comments":[], "commentCount":0, "commentsTruncated":false, "reviewThreads":[], "commits":[]})
            .as_object().expect("object").clone());
    decode(value)
}

fn apply_activity(detail: &mut PullRequestDetailView, activity: PullRequestActivity) {
    if let Some(author) = activity.author {
        detail.author = author;
    }
    if let Some(reviewers) = activity.reviewers {
        detail.reviewers = reviewers;
    }
    detail.comments = activity.comments;
    detail.comment_count = activity.comment_count;
    detail.comments_truncated = activity.comments_truncated;
    detail.review_threads_truncated = activity.review_threads_truncated;
    detail.review_threads = activity.review_threads;
    detail.commits = activity.commits;
    detail.reactions = activity.reactions;
}

impl LivePullRequestSource {
    fn with_store(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut PullRequestStore, &mut gpui_kit::Context<PullRequestStore>),
    ) {
        if let Some(store) = self.store.upgrade() {
            store.update(cx, update);
        }
    }

    fn task<T, F, Fut>(
        &self,
        cx: &mut App,
        command: F,
        done: impl FnOnce(
            &mut PullRequestStore,
            &mut gpui_kit::Context<PullRequestStore>,
            Result<T, CommandError>,
        ) + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(Arc<WriteRuntime>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, CommandError>> + Send + 'static,
    {
        let task = self
            .bus
            .task(cx, move |commands| command(commands.runtime().clone()));
        let store = self.store.clone();
        cx.spawn(async move |cx| {
            let result = task.await;
            if let Some(store) = store.upgrade() {
                let _ = store.update(cx, |store, cx| done(store, cx, result));
            }
        })
        .detach();
    }

    fn sync_choices(&self, cx: &mut App) {
        let state = self.client.snapshot();
        let Some(shell) = state.shell_view.as_ref() else {
            return;
        };
        let projects = shell
            .projects
            .iter()
            .map(|project| PullRequestProject {
                id: project.id.as_str().to_owned(),
                environment_id: state.environment_id.clone(),
                title: project.title.clone(),
                repository: project
                    .source
                    .as_ref()
                    .and_then(|p| p.repository_identity.as_ref().and_then(Option::as_ref))
                    .and_then(|r| r.display_name.as_ref())
                    .map(|r| r.as_str().to_owned()),
            })
            .collect();
        let threads = shell
            .navigation_threads
            .iter()
            .map(|thread| ThreadChoice {
                id: thread.id.clone(),
                title: thread.title.clone(),
                updated_at: thread.updated_at.clone(),
                project_title: shell
                    .project(&thread.project_id)
                    .map_or_else(String::new, |p| p.title.clone()),
            })
            .collect();
        self.with_store(cx, |store, cx| {
            store.update_list(cx, |list| {
                list.projects = projects;
                list.projects_known = true;
            });
            store.set_threads(threads, cx);
            for thread in shell.threads.iter().chain(shell.archived_threads.iter()) {
                if let Some(source) = thread.source.as_ref() {
                    store.set_thread_links(
                        &thread.id,
                        crate::state::support::thread_pull_requests::thread_pull_requests_of(
                            source.pull_requests.as_deref(),
                            source.linked_pull_request.as_ref(),
                        ),
                        cx,
                    );
                }
            }
        });
    }

    fn read_list(&self, more: bool, cx: &mut App) {
        self.sync_choices(cx);
        let busy = self
            .store
            .upgrade()
            .is_some_and(|s| s.read(cx).list().refreshing || s.read(cx).list().loading_more);
        if busy {
            return;
        }
        let mut query = self.query.borrow().clone();
        if more {
            let Some(store) = self.store.upgrade() else {
                return;
            };
            let Some(merged) = store.read(cx).list().merged.as_ref() else {
                return;
            };
            let environment = self.client.snapshot().environment_id.clone();
            query.cursors = merged.next_cursors.get(environment.as_str()).cloned();
            if query.cursors.as_ref().is_none_or(|c| c.is_empty()) {
                if !merged.truncated {
                    return;
                }
                if query.limit == Some(500) {
                    self.with_store(cx, |store, cx| store.update_list(cx, |list| list.error = Some("The host returned its 500-row limit without a continuation cursor.".into())));
                    return;
                }
                query.limit = Some((query.limit.unwrap_or(99) * 2).min(500));
                self.query.borrow_mut().limit = query.limit;
            }
        }
        let generation = self.list_generation.get() + 1;
        self.list_generation.set(generation);
        self.with_store(cx, |store, cx| {
            store.update_list(cx, |list| {
                list.refreshing = !more;
                list.loading_more = more;
                list.error = None;
            })
        });
        let environment = self.client.snapshot().environment_id.clone();
        let generations = self.list_generation.clone();
        self.task(
            cx,
            move |rt| async move {
                if !more {
                    rt.pull_requests
                        .invalidate(
                            &PullRequestInvalidateInput {
                                reference: None,
                                files_viewed_only: None,
                            },
                            false,
                        )
                        .await;
                }
                rt.pull_requests.list(&query).await.map_err(failure)
            },
            move |store, cx, result| {
                if generations.get() != generation {
                    return;
                }
                store.update_list(cx, |list| {
                    list.refreshing = false;
                    list.loading_more = false;
                    match result {
                        Ok(answer) => {
                            let mut merged = merge_pull_request_lists(&[(environment, answer)]);
                            if more && let (Some(old), Some(new)) = (&list.merged, &mut merged) {
                                let keys: std::collections::HashSet<_> = new
                                    .entries
                                    .iter()
                                    .map(|e| (e.project_id.clone(), e.repository.clone(), e.number))
                                    .collect();
                                new.entries.extend(
                                    old.entries
                                        .iter()
                                        .filter(|e| {
                                            !keys.contains(&(
                                                e.project_id.clone(),
                                                e.repository.clone(),
                                                e.number,
                                            ))
                                        })
                                        .cloned(),
                                );
                                new.entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                            }
                            list.merged = merged;
                        }
                        Err(error) => list.error = Some(error.message),
                    }
                });
            },
        );
    }

    fn read_detail(&self, reference: &PullRequestRef, refresh: bool, cx: &mut App) {
        self.sync_choices(cx);
        let key = pull_request_key(reference);
        self.opened
            .borrow_mut()
            .insert(key.clone(), reference.clone());
        let generation = {
            let mut gens = self.detail_generations.borrow_mut();
            let next = gens.get(&key).copied().unwrap_or(0) + 1;
            gens.insert(key.clone(), next);
            next
        };
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |s| {
                s.activity_pending = true;
                s.action_error = None;
            })
        });
        let r = reference.clone();
        let reference = reference.clone();
        let source = self.clone();
        let generations = self.detail_generations.clone();
        self.task(
            cx,
            move |rt| async move {
                if refresh {
                    rt.pull_requests
                        .invalidate(
                            &PullRequestInvalidateInput {
                                reference: Some(r.clone()),
                                files_viewed_only: None,
                            },
                            false,
                        )
                        .await;
                }
                rt.pull_requests
                    .detail(&r)
                    .await
                    .map_err(failure)
                    .and_then(detail_view)
            },
            move |store, cx, result| {
                if generations.borrow().get(&key) != Some(&generation) {
                    return;
                }
                let succeeded = result.is_ok();
                store.update_detail(&reference, cx, |s| match result {
                    Ok(detail) => {
                        s.can_write = true;
                        s.detail = Loadable::Ready(detail);
                    }
                    Err(error) => {
                        s.detail = Loadable::Failed(error.message);
                        s.activity_pending = false;
                    }
                });
                if succeeded {
                    source.read_activity(&reference, generation, cx);
                    cx.defer(move |cx| source.read_aux(&reference, cx));
                }
            },
        );
    }

    fn read_activity(&self, reference: &PullRequestRef, generation: u64, cx: &mut App) {
        let r = reference.clone();
        let reference = reference.clone();
        let generations = self.detail_generations.clone();
        self.task(
            cx,
            move |rt| async move { rt.pull_requests.activity(&r).await.map_err(failure) },
            move |store, cx, result| {
                if generations.borrow().get(&pull_request_key(&reference)) != Some(&generation) {
                    return;
                }
                store.update_detail(&reference, cx, |s| {
                    s.activity_pending = false;
                    match result {
                        Ok(activity) => {
                            if let Loadable::Ready(detail) = &mut s.detail {
                                apply_activity(detail, activity);
                            }
                        }
                        Err(error) => s.action_error = Some(format!("Activity: {}", error.message)),
                    }
                });
            },
        );
    }
}

impl LivePullRequestSource {
    fn read_aux(&self, reference: &PullRequestRef, cx: &mut App) {
        let r = reference.clone();
        let reference_checks = reference.clone();
        self.task(
            cx,
            move |rt| async move { rt.pull_requests.checks(&r).await.map_err(failure) },
            move |store, cx, result| {
                store.update_detail(&reference_checks, cx, |s| {
                    s.checks = match result {
                        Ok(Some(checks)) => Loadable::Ready(checks),
                        Ok(None) => Loadable::Failed("The host did not return checks.".into()),
                        Err(error) => Loadable::Failed(error.message),
                    }
                });
            },
        );
        let r = reference.clone();
        let reference_stack = reference.clone();
        self.task(
            cx,
            move |rt| async move {
                rt.pull_requests
                    .stack(&r, Some(true))
                    .await
                    .map_err(failure)
            },
            move |store, cx, result| {
                store.update_detail(&reference_stack, cx, |s| match result {
                    Ok(stack) => {
                        s.stack = stack;
                        s.stack_loaded = true;
                        s.stack_error = None;
                    }
                    Err(error) => {
                        s.stack_loaded = false;
                        s.stack_error = Some(error.message);
                    }
                });
            },
        );
        self.read_files_viewed(reference, cx);
        self.read_links(reference, cx);
        self.refresh_guide(reference, cx);
        let r = reference.clone();
        let reference_candidates = reference.clone();
        self.task(
            cx,
            move |rt| async move {
                Ok(tokio::join!(
                    rt.pull_requests.reviewer_candidates(&r),
                    rt.pull_requests.label_candidates(&r)
                ))
            },
            move |store, cx, result| {
                store.update_detail(&reference_candidates, cx, |s| match result {
                    Ok((reviewers, labels)) => {
                        match reviewers {
                            Ok(value) => s.reviewer_candidates = Some(value),
                            Err(error) => s.action_error = Some(error.to_string()),
                        }
                        match labels {
                            Ok(value) => s.label_candidates = Some(value),
                            Err(error) => s.action_error = Some(error.to_string()),
                        }
                    }
                    Err(error) => s.action_error = Some(error.message),
                });
            },
        );
    }

    fn read_files_viewed(&self, reference: &PullRequestRef, cx: &mut App) {
        let r = reference.clone();
        let reference = reference.clone();
        self.task(
            cx,
            move |rt| async move { rt.pull_requests.files_viewed(&r).await.map_err(failure) },
            move |store, cx, result| {
                store.update_detail(&reference, cx, |s| match result {
                    Ok(value) => {
                        s.files_viewed = value.files;
                        if value.truncated {
                            s.action_error =
                                Some("The host returned only part of the viewed-file list.".into());
                        }
                    }
                    Err(error) => s.action_error = Some(error.message),
                });
            },
        );
    }

    fn read_links(&self, reference: &PullRequestRef, cx: &mut App) {
        let r = reference.clone();
        let reference = reference.clone();
        self.task(cx, move |rt| async move {
            let key = decode(input(&r, json!({"host": r.host.as_ref().map_or("github.com", |h| h.as_str())})))?;
            turin_source_control::pull_request::linked_threads::list_linked_pull_request_threads(&rt.stores.db, &key).await.map_err(failure)
        }, move |store, cx, result| {
            store.update_detail(&reference, cx, |s| match result {
                Ok(value) => s.linked_threads = value.threads.into_iter().map(|t| LinkedThread { id: t.id, project_id: t.project_id.as_str().to_owned(), title: t.title, archived: t.archived_at.is_some() }).collect(),
                Err(error) => s.action_error = Some(error.message),
            });
        });
    }

    fn mutate<F, Fut>(
        &self,
        reference: &PullRequestRef,
        action: Option<PullRequestAction>,
        extra: Value,
        cx: &mut App,
        command: F,
    ) where
        F: FnOnce(Arc<WriteRuntime>, Value) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CommandError>> + Send + 'static,
    {
        // Never retry a write implicitly. Refresh failures are separate from a successful write.
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |s| {
                s.action_error = None;
                s.pending_action = action;
            })
        });
        let value = input(reference, extra);
        let reference = reference.clone();
        let source = self.clone();
        self.task(
            cx,
            move |rt| command(rt, value),
            move |store, cx, result| {
                let succeeded = result.is_ok();
                store.update_detail(&reference, cx, |s| {
                    s.pending_action = None;
                    s.action_error = result.err().map(|e| e.message);
                });
                if succeeded {
                    cx.defer(move |cx| {
                        source.read_detail(&reference, false, cx);
                        source.read_list(false, cx);
                    });
                }
            },
        );
    }
}

mod actions;
