//! Live [`PullRequestSource`] (`pullRequestEnvironment.*`). Reads/writes go through
//! `Commands::runtime().pull_requests` / `.review_guides` once WIRE-runtime exposes them.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Entity, WeakEntity};
use turin_contracts::ThreadId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestListInput, PullRequestListState, PullRequestMergeMethod,
    PullRequestRef, PullRequestUpdateMethod,
};

use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;
use crate::app_runtime::write::runtime::WriteRuntime;
use crate::state::store::ClientStateHandle;

use super::pull_request_list_logic::merge_pull_request_lists;
use super::view_model::{
    PullRequestProject, PullRequestSource, PullRequestStore, ReviewSubmission, ThreadChoice,
};

pub type OpenThreadCallback = Arc<dyn Fn(&ThreadId, &mut App)>;

#[derive(Clone)]
struct LivePullRequestSource {
    store: WeakEntity<PullRequestStore>,
    bus: CommandBus,
    client: ClientStateHandle,
    navigate: OpenThreadCallback,
    query: Rc<RefCell<PullRequestListInput>>,
    list_generation: Rc<Cell<u64>>,
}

pub fn start(bus: CommandBus, client: ClientStateHandle, cx: &mut App) -> Entity<PullRequestStore> {
    let navigation_client = client.clone();
    start_with_navigation(
        bus,
        client,
        Arc::new(move |id, _| navigation_client.open_thread(id.clone())),
        cx,
    )
}

pub fn start_with_navigation(
    bus: CommandBus,
    client: ClientStateHandle,
    navigate: OpenThreadCallback,
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
    });
    store.update(cx, |store, _| store.set_source(source.clone()));
    source.refresh_list(cx);
    let periodic = source.clone();
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(Duration::from_secs(30)).await;
        if periodic.store.upgrade().is_none() {
            break;
        }
        let _ = cx.update(|cx| periodic.refresh_list(cx));
    })
    .detach();
    store
}

fn backend_unavailable() -> CommandError {
    CommandError::new(
        "Pull request services are not available yet (waiting on WriteRuntime.pull_requests).",
    )
}

async fn list_pull_requests(
    _rt: Arc<WriteRuntime>,
    _query: &PullRequestListInput,
) -> Result<turin_contracts::pull_request::PullRequestListResult, CommandError> {
    Err(backend_unavailable())
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
        let task = self.bus.task(cx, move |commands| command(commands.runtime()));
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
                    .and_then(|p| p.repository_identity.as_ref())
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
            for thread in shell
                .threads
                .iter()
                .chain(shell.archived_threads.iter())
            {
                if let Some(source) = thread.source.as_ref() {
                    store.set_thread_links(
                        &thread.id,
                        source.pull_requests.clone().unwrap_or_default(),
                        cx,
                    );
                }
            }
        });
    }

    fn read_list(&self, more: bool, cx: &mut App) {
        self.sync_choices(cx);
        let busy = self.store.upgrade().is_some_and(|s| {
            let list = s.read(cx).list();
            list.refreshing || list.loading_more
        });
        if busy {
            return;
        }
        let query = self.query.borrow().clone();
        let generation = self.list_generation.get() + 1;
        self.list_generation.set(generation);
        self.with_store(cx, |store, cx| {
            store.update_list(cx, |list| {
                list.refreshing = !more;
                list.loading_more = more;
                list.error = None;
            });
        });
        let environment = self.client.snapshot().environment_id;
        let generations = self.list_generation.clone();
        self.task(
            cx,
            move |rt| async move { list_pull_requests(rt, &query).await },
            move |store, cx, result| {
                if generations.get() != generation {
                    return;
                }
                store.update_list(cx, |list| {
                    list.refreshing = false;
                    list.loading_more = false;
                    match result {
                        Ok(answer) => {
                            list.merged = merge_pull_request_lists(&[(environment.clone(), answer)]);
                        }
                        Err(error) => list.error = Some(error.message),
                    }
                });
            },
        );
    }
}

impl PullRequestSource for LivePullRequestSource {
    fn refresh_list(&self, cx: &mut App) {
        self.read_list(false, cx);
    }

    fn load_more(&self, cx: &mut App) {
        self.read_list(true, cx);
    }

    fn open_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        let reference = reference.clone();
        self.with_store(cx, |store, cx| {
            store.update_detail(&reference, cx, |snapshot| {
                snapshot.activity_pending = true;
                snapshot.action_error = Some(backend_unavailable().message);
            });
        });
    }

    fn refresh_detail(&self, reference: &PullRequestRef, cx: &mut App) {
        self.open_detail(reference, cx);
    }

    fn run_action(
        &self,
        reference: &PullRequestRef,
        _: PullRequestAction,
        _: Option<PullRequestMergeMethod>,
        _: Option<PullRequestUpdateMethod>,
        cx: &mut App,
    ) {
        self.with_store(cx, |store, cx| {
            store.update_detail(reference, cx, |snapshot| {
                snapshot.action_error = Some(backend_unavailable().message);
            });
        });
    }

    fn post_comment(&self, reference: &PullRequestRef, _: String, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn update_title(&self, reference: &PullRequestRef, _: String, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn update_body(&self, reference: &PullRequestRef, _: String, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn submit_review(&self, reference: &PullRequestRef, _: ReviewSubmission, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn reply_to_thread(
        &self,
        reference: &PullRequestRef,
        _: &str,
        _: String,
        cx: &mut App,
    ) {
        self.post_comment(reference, String::new(), cx);
    }

    fn set_thread_resolved(
        &self,
        reference: &PullRequestRef,
        _: &str,
        _: bool,
        cx: &mut App,
    ) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn toggle_reaction(
        &self,
        reference: &PullRequestRef,
        _: &str,
        _: turin_contracts::pull_request::PullRequestReactionContent,
        cx: &mut App,
    ) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn load_diff(&self, reference: &PullRequestRef, _: Option<String>, cx: &mut App) {
        self.open_detail(reference, cx);
    }

    fn set_files_viewed(
        &self,
        reference: &PullRequestRef,
        _: Vec<(String, bool)>,
        cx: &mut App,
    ) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn request_reviewers(&self, reference: &PullRequestRef, _: Vec<String>, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn set_labels(&self, reference: &PullRequestRef, _: Vec<String>, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn generate_guide(&self, reference: &PullRequestRef, cx: &mut App) {
        self.run_action(reference, PullRequestAction::Merge, None, None, cx);
    }

    fn link_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        let _ = (thread_id, reference, cx);
    }

    fn unlink_thread(&self, thread_id: &ThreadId, reference: &PullRequestRef, cx: &mut App) {
        let _ = (thread_id, reference, cx);
    }

    fn open_thread(&self, thread_id: &ThreadId, cx: &mut App) {
        (self.navigate)(thread_id, cx);
    }
}
