//! Port of upstream/apps/web/src/components/pullRequest/guide/PullRequestGuideRunCoordinator.tsx.
//!
//! Watches the guide runs the reader is waiting on and says when each ends, so the reader can
//! leave the Guide tab, or the pull request page, while one is written. It re-reads each watched
//! run every [`RUN_POLL_MS`] through the source, decides with [`evaluate_watch`], and raises a
//! "Review guide ready / failed" toast with an "Open guide" button.
//!
//! A reader who is not looking also gets a desktop notification, through the app's notifier
//! (`desktop_notification.rs`; logged until the app has one). Not ported: the completion / input
//! sounds and the `notificationMode` gate (settings are not read here). Upstream says nothing to a
//! reader who is not looking and has not granted desktop notifications; here the toast waits for
//! them as well.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Context, Entity, EventEmitter, Subscription};
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;

use super::desktop_notification::{DesktopNotification, show_desktop_notification};
use super::guide_run_store::{
    GuideRunStore, GuideRunWatch, RUN_POLL_MS, WatchDecision, evaluate_watch, guide_run_key,
};
use super::toast::{ToastAction, ToastKind, ToastQueue};
use super::view_model::PullRequestStore;

/// Raised when the reader asks, from a notice, to open a guide.
#[derive(Clone, Debug)]
pub enum GuideRunCoordinatorEvent {
    OpenGuide {
        environment_id: EnvironmentId,
        reference: PullRequestRef,
    },
}

pub struct GuideRunCoordinator {
    store: Entity<PullRequestStore>,
    runs: Entity<GuideRunStore>,
    /// `sawRunning`, by run key.
    saw_running: HashSet<String>,
    toasts: ToastQueue,
    /// The notice each run raised, so its button can take it away.
    notices: HashMap<String, u64>,
    polling: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GuideRunCoordinatorEvent> for GuideRunCoordinator {}

impl GuideRunCoordinator {
    pub fn new(store: Entity<PullRequestStore>, cx: &mut Context<Self>) -> Self {
        let runs = GuideRunStore::global(cx);
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.evaluate(cx)),
            cx.observe(&runs, |this, _, cx| this.evaluate(cx)),
        ];
        let mut this = Self {
            store,
            runs,
            saw_running: HashSet::new(),
            toasts: ToastQueue::default(),
            notices: HashMap::new(),
            polling: false,
            _subscriptions: subscriptions,
        };
        this.evaluate(cx);
        this
    }

    pub fn toasts(&self) -> &ToastQueue {
        &self.toasts
    }

    /// One pass over every watched run (the effect body of `GuideRunWatcher`).
    fn evaluate(&mut self, cx: &mut Context<Self>) {
        let watches: Vec<(String, GuideRunWatch)> = self
            .runs
            .read(cx)
            .watches()
            .iter()
            .map(|(key, watch)| (key.clone(), watch.clone()))
            .collect();
        for (key, watch) in watches {
            let decision = {
                let store = self.store.read(cx);
                let data = store
                    .detail(&watch.reference)
                    .and_then(|snapshot| snapshot.guide.ready());
                evaluate_watch(
                    data,
                    &watch,
                    self.saw_running.contains(&key),
                    crate::timestamp_format::now_ms(),
                )
            };
            match decision {
                WatchDecision::Wait => {}
                WatchDecision::Running => {
                    self.saw_running.insert(key);
                }
                WatchDecision::Settle => self.settle(&key, cx),
                WatchDecision::Notice { failure } => {
                    self.settle(&key, cx);
                    self.notify(&key, &watch, failure, cx);
                }
            }
        }
        self.ensure_polling(cx);
    }

    fn settle(&mut self, key: &str, cx: &mut Context<Self>) {
        self.saw_running.remove(key);
        self.runs.update(cx, |runs, cx| runs.settle(key, cx));
    }

    fn notify(
        &mut self,
        key: &str,
        watch: &GuideRunWatch,
        failure: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // The guide on screen right now needs no notice when it lands.
        let looking = cx.active_window().is_some();
        if looking && self.runs.read(cx).shown_key() == Some(key) {
            return;
        }
        let title = if failure.is_none() {
            "Review guide ready"
        } else {
            "Review guide failed"
        };
        let body = watch.title.clone().unwrap_or_else(|| {
            format!(
                "{} #{}",
                watch.reference.repository.as_str(),
                watch.reference.number.get()
            )
        });
        let description = match &failure {
            None => body,
            Some(failure) => format!("{body}: {failure}"),
        };
        let kind = if failure.is_none() {
            ToastKind::Success
        } else {
            ToastKind::Error
        };
        let coordinator = cx.entity().downgrade();
        let open_key = key.to_owned();
        let environment_id = watch.environment_id.clone();
        let reference = watch.reference.clone();
        let open_guide: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
            let open_key = open_key.clone();
            let environment_id = environment_id.clone();
            let reference = reference.clone();
            coordinator
                .update(cx, |this, cx| {
                    this.open(&open_key, environment_id, reference, cx)
                })
                .ok();
        });
        // A reader who is somewhere else is told outside the app too, and the click brings the
        // guide up (`new Notification(...)` with its `click` listener).
        if !looking {
            show_desktop_notification(
                DesktopNotification {
                    title: title.to_owned(),
                    body: description.clone(),
                    tag: format!("guide:{key}"),
                    on_click: Some(open_guide.clone()),
                },
                cx,
            );
        }
        let action = ToastAction {
            label: "Open guide".into(),
            on_click: open_guide,
        };
        let id = self.toasts.push_with_action(
            cx,
            |coordinator| &mut coordinator.toasts,
            kind,
            title,
            Some(description),
            Some(action),
        );
        self.notices.insert(key.to_owned(), id);
    }

    /// `open`: closes the notice and asks the host to show the guide.
    fn open(
        &mut self,
        key: &str,
        environment_id: EnvironmentId,
        reference: PullRequestRef,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.notices.remove(key) {
            self.toasts.dismiss(id);
        }
        let request = key.to_owned();
        self.runs
            .update(cx, |runs, cx| runs.request_open(Some(request), cx));
        cx.emit(GuideRunCoordinatorEvent::OpenGuide {
            environment_id,
            reference,
        });
        cx.notify();
    }

    /// The `setInterval(refresh, RUN_POLL_MS)` of each watcher, as one loop that runs while
    /// anything is watched.
    fn ensure_polling(&mut self, cx: &mut Context<Self>) {
        if self.polling || self.runs.read(cx).watches().is_empty() {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(RUN_POLL_MS))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        let references: Vec<PullRequestRef> = this
                            .runs
                            .read(cx)
                            .watches()
                            .values()
                            .map(|watch| watch.reference.clone())
                            .collect();
                        if references.is_empty() {
                            this.polling = false;
                            return false;
                        }
                        if let Some(source) = this.store.read(cx).source() {
                            for reference in &references {
                                source.refresh_guide(reference, cx);
                            }
                        }
                        // A watch can outlive its first read by the unseen window.
                        this.evaluate(cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }
}

/// The run key a reader's panel shows (`guideRunKey`), re-exported for the hosts.
pub fn run_key(environment_id: &EnvironmentId, reference: &PullRequestRef) -> String {
    guide_run_key(environment_id, reference)
}
