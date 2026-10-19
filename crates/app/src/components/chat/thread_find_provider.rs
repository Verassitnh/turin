//! Port of upstream/apps/web/src/components/chat/ThreadFindProvider.tsx, with the hook
//! it wraps, `useThreadFind.ts` (the pure rules live in `thread_find_state.rs`).
//!
//! `ThreadFind` owns the find for one open thread: the query field, the debounced
//! search against the orchestrator (`search_thread_stream`, so the early frame shows
//! while the count finishes), the local stepping, the live refresh while the thread
//! streams, and what the timeline paints and scrolls to. Upstream's React context
//! splits that between a provider and a bar; one entity does both here, and the chat
//! view mounts it over the canvas.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, div,
};
use turin_contracts::ThreadId;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::orchestration_v2::{
    OrchestrationV2SearchQuery, OrchestrationV2SearchThreadInput,
    OrchestrationV2SearchThreadResult, OrchestrationV2SearchThreadSkill,
};

use super::messages_timeline::{MessagesTimeline, TimelineFind, TimelineFindMatch};
use super::thread_find::ThreadFindStart;
use super::thread_find_action_bus::subscribe_thread_find_open;
use super::thread_find_bar::{THREAD_FIND_QUERY_MAX_LENGTH, ThreadFindBar};
use super::thread_find_state::{
    THREAD_FIND_LIVE_REFRESH_MS, THREAD_FIND_QUERY_DEBOUNCE_MS, ThreadFindLiveRefresh,
    ThreadFindOpenOutcome, ThreadFindRemote, ThreadFindResultMemo, ThreadFindState, ThreadFindView,
    thread_find_is_pending, thread_find_search_eligible, thread_find_search_key,
};
use super::timeline::source::ThreadTimelineStore;
use crate::app_runtime::write::bus::CommandBus;
use crate::state::local_environment_id;

type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The arguments of one search (`useServerResults`' atom input). A change starts a new
/// search; the same arguments are only ever refreshed.
#[derive(Debug, Clone, PartialEq)]
struct SearchArgs {
    query: String,
    start: Option<ThreadFindStart>,
    offset: i64,
    skills: Vec<OrchestrationV2SearchThreadSkill>,
}

/// One frame of a running search.
enum SearchFrame {
    Result(Box<OrchestrationV2SearchThreadResult>),
    Done,
    Failed,
}

/// The atom's state for the current arguments.
#[derive(Default)]
struct Remote {
    data: Option<Arc<OrchestrationV2SearchThreadResult>>,
    in_flight: bool,
    error: bool,
}

pub struct ThreadFind {
    thread_id: ThreadId,
    thread_key: String,
    environment: String,
    bus: CommandBus,
    timeline: Entity<MessagesTimeline>,
    store: Entity<ThreadTimelineStore>,
    input: Entity<InputState>,
    on_close: CloseHandler,
    state: ThreadFindState,
    memo: ThreadFindResultMemo,
    view: ThreadFindView,
    remote: Remote,
    /// `useDebouncedValue(query.trim(), 100)`.
    debounced_query: String,
    debounce_task: Option<Task<()>>,
    args: Option<SearchArgs>,
    search_task: Option<Task<()>>,
    /// The 300 ms live-refresh timer (`refreshTimerRef`).
    refresh_timer: Option<Task<()>>,
    /// A refresh that waits for the running search to settle (`refreshAfterSearchRef`).
    refresh_after_search: bool,
    _subscriptions: Vec<Subscription>,
}

impl ThreadFind {
    pub fn new(
        thread_id: ThreadId,
        bus: CommandBus,
        timeline: Entity<MessagesTimeline>,
        store: Entity<ThreadTimelineStore>,
        on_close: CloseHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let environment = local_environment_id();
        let thread_key = crate::state::models::scoped_thread_key(&environment, &thread_id);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        this.on_query_change(&text, window, cx);
                    }
                    // Enter steps to the next match; Shift+Enter to the previous.
                    InputEvent::PressEnter { shift, .. } => {
                        this.step(if *shift { -1 } else { 1 }, cx);
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
            // `requestThreadFindOpen` from the shortcut and the command palette.
            subscribe_thread_find_open(window, cx, |this, window, cx| this.open(window, cx)),
            // A content change refreshes the open search, at most once per 300 ms.
            cx.observe(&store, |this, _, cx| this.on_content_changed(cx)),
        ];
        Self {
            thread_id,
            thread_key,
            environment: environment.to_string(),
            bus,
            timeline,
            store,
            input,
            on_close,
            state: ThreadFindState::closed(),
            memo: ThreadFindResultMemo::default(),
            view: ThreadFindView {
                status: None,
                count: 0,
                counting: false,
                active_index: 0,
                navigation_id: 0,
                r#match: None,
                find_expanded: false,
                find_query: String::new(),
            },
            remote: Remote::default(),
            debounced_query: String::new(),
            debounce_task: None,
            args: None,
            search_task: None,
            refresh_timer: None,
            refresh_after_search: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state.is_open(true, Some(&self.thread_key))
    }

    /// `open`: shows the bar and puts the caret in it, selecting the previous query.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.timeline.read(cx).read_find_position();
        let outcome = self
            .state
            .open(Some(&self.thread_key), true, |_| start.clone());
        if outcome != ThreadFindOpenOutcome::Opened {
            return;
        }
        // The previous query returns with its text.
        let text = self.state.query.clone();
        if self.input.read(cx).value().as_ref() != text.as_str() {
            self.input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.sync(cx);
    }

    /// `close`: forgets the query and the results and hands focus back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.close();
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.debounced_query.clear();
        self.debounce_task = None;
        self.refresh_timer = None;
        self.refresh_after_search = false;
        self.memo = ThreadFindResultMemo::default();
        self.remote = Remote::default();
        self.args = None;
        self.search_task = None;
        self.sync(cx);
        (self.on_close)(window, cx);
    }

    /// `onQueryChange`: the new query re-anchors on the rows in view and searches after
    /// the debounce.
    fn on_query_change(&mut self, text: &str, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_open() || text == self.state.query {
            return;
        }
        let query: String = text.chars().take(THREAD_FIND_QUERY_MAX_LENGTH).collect();
        let start = (!js_trim(&query).is_empty())
            .then(|| self.timeline.read(cx).read_find_position())
            .flatten();
        self.state.set_query(&query, start);
        self.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(THREAD_FIND_QUERY_DEBOUNCE_MS))
                .await;
            this.update(cx, |this, cx| {
                this.debounced_query = js_trim(&this.state.query).to_owned();
                this.debounce_task = None;
                this.sync(cx);
            })
            .ok();
        }));
        self.sync(cx);
    }

    /// `step(delta)`: previous or next match, locally when the server's navigation covers it.
    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let open = self.is_open();
        let key = self.search_key(open, cx);
        let remote = Self::remote_view(
            &self.memo,
            key.as_deref(),
            &self.remote,
            thread_find_is_pending(
                open,
                &self.state.query,
                &self.debounced_query,
                self.remote.in_flight,
            ),
        );
        self.state
            .step(delta, remote, true, self.refresh_timer.is_some());
        self.sync(cx);
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.start_search(false, cx);
        self.sync(cx);
    }

    fn skills(&self, cx: &App) -> Vec<OrchestrationV2SearchThreadSkill> {
        self.store
            .read(cx)
            .snapshot()
            .skills
            .iter()
            .map(|skill| OrchestrationV2SearchThreadSkill {
                name: skill.name.clone(),
                display_name: skill.display_name.clone(),
            })
            .collect()
    }

    fn search_key(&self, open: bool, cx: &App) -> Option<String> {
        let thread = open.then_some((self.environment.as_str(), self.thread_id.as_str()));
        thread_find_search_key(thread, &self.state.query, &self.skills(cx))
    }

    fn remote_view<'a>(
        memo: &'a ThreadFindResultMemo,
        key: Option<&str>,
        remote: &Remote,
        is_pending: bool,
    ) -> ThreadFindRemote<'a> {
        ThreadFindRemote {
            data: memo.shown(key, remote.error).map(|data| &**data),
            navigation_id: memo.navigation_id,
            is_pending,
            error: remote.error,
        }
    }

    /// Recomputes what the find shows from its state and the latest search, starts a
    /// search when its arguments changed, and tells the timeline.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let open = self.is_open();
        // The search arguments, as `useServerResults` builds them: only once the debounced
        // query has caught up with what is typed.
        let thread = open.then_some((self.environment.as_str(), self.thread_id.as_str()));
        let args = thread_find_search_eligible(thread, &self.state.query, &self.debounced_query)
            .then(|| SearchArgs {
                query: self.debounced_query.clone(),
                start: self.state.start.clone(),
                offset: self.state.offset,
                skills: self.skills(cx),
            });
        if args != self.args {
            self.args = args;
            self.start_search(true, cx);
        }
        let key = self.search_key(open, cx);
        self.memo.update(
            key.clone(),
            self.remote.data.as_ref(),
            self.remote.in_flight,
            self.state.navigation_id,
            true,
        );
        let is_pending = thread_find_is_pending(
            open,
            &self.state.query,
            &self.debounced_query,
            self.remote.in_flight,
        );
        let remote = Self::remote_view(&self.memo, key.as_deref(), &self.remote, is_pending);
        self.view = self.state.view(remote, open);
        self.push_to_timeline(cx);
        cx.notify();
    }

    /// `timelineProps`: the query to paint and the active match to reveal.
    fn push_to_timeline(&self, cx: &mut Context<Self>) {
        let find = TimelineFind {
            query: js_trim(&self.view.find_query).to_owned(),
            active: self.view.r#match.as_ref().map(|found| TimelineFindMatch {
                entry_id: found.entry_id.to_string(),
                run_id: found.run_id.as_ref().map(ToString::to_string),
                occurrence: found.occurrence,
            }),
            navigation_id: self.view.navigation_id,
        };
        self.timeline
            .update(cx, |timeline, cx| timeline.set_find(find, cx));
    }

    /// Starts the search for the current arguments. `reset` drops the previous result
    /// (new arguments); a refresh keeps it on screen until the new frames arrive.
    fn start_search(&mut self, reset: bool, cx: &mut Context<Self>) {
        self.search_task = None;
        if reset {
            self.remote = Remote::default();
        }
        let Some(args) = self.args.clone() else {
            return;
        };
        let Ok(query) = OrchestrationV2SearchQuery::new(&args.query) else {
            return;
        };
        self.remote.in_flight = true;
        self.remote.error = false;
        let input = OrchestrationV2SearchThreadInput {
            thread_id: self.thread_id.clone(),
            skills: Some(args.skills.clone()),
            query,
            index: None,
            offset: (args.offset != 0).then_some(args.offset),
            start: args.start.clone(),
        };
        let (frames, mut receiver) = futures::channel::mpsc::unbounded::<SearchFrame>();
        let commands = self.bus.commands();
        self.bus.handle().spawn(async move {
            let stream = match commands {
                Ok(commands) => commands.search_thread_stream(input).await,
                Err(error) => Err(error),
            };
            match stream {
                Ok(mut stream) => {
                    while let Some(frame) = stream.next().await {
                        if frames
                            .unbounded_send(SearchFrame::Result(Box::new(frame)))
                            .is_err()
                        {
                            return;
                        }
                    }
                    let _ = frames.unbounded_send(SearchFrame::Done);
                }
                Err(error) => {
                    tracing::warn!(%error, "thread find search failed");
                    let _ = frames.unbounded_send(SearchFrame::Failed);
                }
            }
        });
        // Dropping the task drops the receiver, which ends the search above.
        self.search_task = Some(cx.spawn(async move |this, cx| {
            while let Some(frame) = receiver.next().await {
                let alive = this
                    .update(cx, |this, cx| this.on_search_frame(frame, cx))
                    .is_ok();
                if !alive {
                    return;
                }
            }
        }));
    }

    fn on_search_frame(&mut self, frame: SearchFrame, cx: &mut Context<Self>) {
        match frame {
            SearchFrame::Result(result) => {
                self.remote.data = Some(Arc::new(*result));
                self.remote.error = false;
            }
            SearchFrame::Done => self.remote.in_flight = false,
            SearchFrame::Failed => {
                self.remote.in_flight = false;
                self.remote.error = true;
            }
        }
        self.sync(cx);
        // A refresh that arrived mid-search runs once it settles.
        if self.refresh_after_search && !self.remote.in_flight {
            self.refresh_after_search = false;
            if !self.remote.error {
                self.refresh_live_results(cx);
            }
        }
    }

    /// The thread's content changed: refresh the open search, at most once per 300 ms,
    /// even while tokens keep arriving.
    fn on_content_changed(&mut self, cx: &mut Context<Self>) {
        if !self.state.content_refresh_due(self.is_open()) || self.refresh_timer.is_some() {
            return;
        }
        self.refresh_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(THREAD_FIND_LIVE_REFRESH_MS))
                .await;
            this.update(cx, |this, cx| {
                this.refresh_timer = None;
                this.refresh_live_results(cx);
            })
            .ok();
        }));
    }

    /// `refreshLiveResultsRef`: an identity anchor keeps newly inserted matches from
    /// shifting the selection.
    fn refresh_live_results(&mut self, cx: &mut Context<Self>) {
        let open = self.is_open();
        let key = self.search_key(open, cx);
        let is_pending = thread_find_is_pending(
            open,
            &self.state.query,
            &self.debounced_query,
            self.remote.in_flight,
        );
        let remote = Self::remote_view(&self.memo, key.as_deref(), &self.remote, is_pending);
        let selected = self.view.r#match.clone();
        let decision = self
            .state
            .live_refresh(open, true, remote, selected.as_ref());
        match decision {
            ThreadFindLiveRefresh::Ignore => {}
            ThreadFindLiveRefresh::DeferUntilSettled => self.refresh_after_search = true,
            ThreadFindLiveRefresh::RemoteRefresh => {
                self.start_search(false, cx);
                self.sync(cx);
            }
            ThreadFindLiveRefresh::Rebase(start) => {
                self.state.apply_rebase(start);
                self.sync(cx);
            }
        }
    }
}

impl Render for ThreadFind {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().into_any_element();
        }
        let entity = cx.entity();
        let action = |run: fn(&mut ThreadFind, &mut Window, &mut Context<ThreadFind>)| {
            let entity = entity.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| run(this, window, cx));
            }) as Rc<dyn Fn(&mut Window, &mut App)>
        };
        let escape = {
            let close = action(|this, window, cx| this.close(window, cx));
            move |event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut App| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    close(window, cx);
                }
            }
        };
        div()
            .on_key_down(escape)
            .child(ThreadFindBar {
                input: self.input.clone(),
                query: self.state.query.clone(),
                match_count: self.view.count,
                counting: self.view.counting,
                status: self.view.status,
                active_index: self.view.active_index,
                on_retry: action(|this, _, cx| this.retry(cx)),
                on_next: action(|this, _, cx| this.step(1, cx)),
                on_previous: action(|this, _, cx| this.step(-1, cx)),
                on_close: action(|this, window, cx| this.close(window, cx)),
            })
            .into_any_element()
    }
}
