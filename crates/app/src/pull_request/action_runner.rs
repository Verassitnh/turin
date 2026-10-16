//! Port of upstream/apps/web/src/components/pullRequest/usePullRequestActions.ts
//! (`usePullRequestActionRunner`, `usePullRequestCloseBatch`, and the merge-method resolution
//! `PullRequestSpeedActions` hands the runner).
//!
//! One host action against a pull request, with the toasts every surface says the same way, and
//! the close sweep's batch of them. The commands go through the store's
//! [`PullRequestSource`](super::view_model::PullRequestSource); the outcome comes back the way the
//! detail panel reads it, as the detail snapshot's `pending_action` going out and `action_error`
//! staying or not. The runner watches that for the actions it started.
//!
//! Not ported: `usePullRequestHandoffs` (the "ask in a thread" and checkout hand-offs need thread
//! launch, which hooks replace), and `squashAtomCommandFailure` (a failure is already the host's
//! sentence by the time the source publishes it).

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Context, Entity, EventEmitter, Subscription};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::ProjectId;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestDetailView, PullRequestMergeMethod, PullRequestRef,
    PullRequestState,
};

use super::detail_panel::core_detail;
use super::pull_request_detail_logic::{
    allowed_pull_request_merge_methods, readable_failure, resolve_pull_request_merge_method,
};
use super::pull_request_list_logic::{EnvironmentPullRequestEntry, pull_request_entry_key};
use super::toast::{ToastKind, ToastQueue};
use super::view_model::{Loadable, PullRequestStore, pull_request_key};
use crate::hooks::use_pull_request_actions::{
    pull_request_action_failure_hint, pull_request_action_failure_label,
    pull_request_action_success_label,
};

/// A source that never sets `pending_action` for an action is given this long to start before the
/// runner stops watching for it (the row would otherwise spin for good).
const UNSEEN_GIVE_UP_MS: u64 = 10_000;

/// What `resolveMergeMethod` in `PullRequestSpeedActions` reads: the repository's own default
/// for the project, then the last method the reader picked in the panel.
#[derive(Clone)]
pub struct MergeMethodSettings {
    /// `usePullRequestDefaultMergeMethodResolver`, resolved on the click rather than per row.
    pub project_default: Rc<dyn Fn(&EnvironmentId, &ProjectId) -> Option<PullRequestMergeMethod>>,
    /// `useUiStateStore.getState().pullRequestMergeMethod`.
    pub last_selected: Rc<dyn Fn() -> PullRequestMergeMethod>,
}

impl Default for MergeMethodSettings {
    fn default() -> Self {
        Self {
            project_default: Rc::new(|_, _| None),
            last_selected: Rc::new(|| PullRequestMergeMethod::Merge),
        }
    }
}

/// The reference a listed row opens its detail with.
pub fn reference_of_entry(entry: &EnvironmentPullRequestEntry) -> PullRequestRef {
    PullRequestRef {
        project_id: entry.project_id.clone(),
        host: Some(entry.host.clone()),
        expected_account_id: None,
        allow_stale: None,
        repository: entry.repository.clone(),
        number: entry.number,
    }
}

/// The row key the runner and the list share (`pullRequestEntryKey`).
pub fn entry_key(entry: &EnvironmentPullRequestEntry) -> String {
    pull_request_entry_key(&entry.entry, Some(entry.environment_id.as_str()))
}

/// `resolveMergeMethod` of `PullRequestSpeedActions`: what the repository allows, narrowed to the
/// project default, then the reader's last pick. An error is the toast's description.
pub fn resolve_quick_merge_method(
    detail: &PullRequestDetailView,
    project_default: Option<PullRequestMergeMethod>,
    last_selected: PullRequestMergeMethod,
) -> Result<PullRequestMergeMethod, String> {
    let allowed = allowed_pull_request_merge_methods(Some(&core_detail(detail)));
    if allowed.is_empty() {
        return Err("No merge method is available for this repository.".to_owned());
    }
    Ok(resolve_pull_request_merge_method(
        &allowed,
        None,
        project_default,
        last_selected,
    ))
}

/// What the close sweep reports once every row has answered.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchTally {
    pub total: usize,
    pub closed: usize,
    pub failures: Vec<String>,
}

impl BatchTally {
    /// A row the source never reported closing: counted as a failure so the sweep can finish.
    pub fn record_did_not_start(&mut self, number: u64) {
        self.failures
            .push(format!("#{number}: The close did not start."));
    }

    pub fn is_error(&self) -> bool {
        !self.failures.is_empty()
    }

    pub fn title(&self) -> String {
        if self.is_error() {
            format!("Closed {} of {} pull requests", self.closed, self.total)
        } else {
            format!(
                "Closed {} pull request{}",
                self.closed,
                if self.closed == 1 { "" } else { "s" }
            )
        }
    }

    /// The first three failures, one a line.
    pub fn description(&self) -> Option<String> {
        self.is_error().then(|| {
            self.failures
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Single,
    Batch(u64),
}

#[derive(Clone)]
struct InFlight {
    entry: EnvironmentPullRequestEntry,
    reference: PullRequestRef,
    action: PullRequestAction,
    origin: Origin,
    /// The source has reported the action running; only then does its going away mean it ended.
    seen_pending: bool,
    /// Which start this is, so a timer set for an earlier action on the same row cannot take a
    /// newer one away.
    token: u64,
}

/// What the detail snapshot says about the action in flight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ActionSnapshot {
    pending: Option<PullRequestAction>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Succeeded,
    Failed(String),
}

/// The actions a runner has started and not yet seen end.
#[derive(Default)]
struct Tracker {
    in_flight: IndexMap<String, InFlight>,
    next_token: u64,
}

impl Tracker {
    fn begin(
        &mut self,
        entry: EnvironmentPullRequestEntry,
        action: PullRequestAction,
        origin: Origin,
    ) -> u64 {
        let key = entry_key(&entry);
        let reference = reference_of_entry(&entry);
        let token = self.take_token();
        self.in_flight.insert(
            key,
            InFlight {
                entry,
                reference,
                action,
                origin,
                seen_pending: false,
                token,
            },
        );
        token
    }

    fn take_token(&mut self) -> u64 {
        let token = self.next_token;
        self.next_token += 1;
        token
    }

    /// Moves every action whose snapshot shows it ended out of the tracker, with how it ended.
    fn settle(
        &mut self,
        snapshot_of: impl Fn(&PullRequestRef) -> Option<ActionSnapshot>,
    ) -> Vec<(InFlight, Outcome)> {
        let mut ended = Vec::new();
        for (key, flight) in self.in_flight.iter_mut() {
            let Some(snapshot) = snapshot_of(&flight.reference) else {
                continue;
            };
            if snapshot.pending.is_some() {
                flight.seen_pending = true;
            } else if flight.seen_pending {
                let outcome = match snapshot.error {
                    Some(error) => Outcome::Failed(error),
                    None => Outcome::Succeeded,
                };
                ended.push((key.clone(), outcome));
            }
        }
        ended
            .into_iter()
            .filter_map(|(key, outcome)| {
                self.in_flight
                    .shift_remove(&key)
                    .map(|flight| (flight, outcome))
            })
            .collect()
    }

    /// Stops watching an action the source never reported running.
    fn give_up_unseen(&mut self, key: &str, token: u64) -> Option<InFlight> {
        if self
            .in_flight
            .get(key)
            .is_some_and(|flight| !flight.seen_pending && flight.token == token)
        {
            self.in_flight.shift_remove(key)
        } else {
            None
        }
    }
}

/// Raised for each action the host accepted.
#[derive(Clone, Debug)]
pub enum PullRequestActionRunnerEvent {
    Acted {
        entry: EnvironmentPullRequestEntry,
        action: PullRequestAction,
    },
}

pub struct PullRequestActionRunner {
    store: Entity<PullRequestStore>,
    settings: MergeMethodSettings,
    tracker: Tracker,
    /// A merge clicked before the row's detail was read: sent once it lands.
    awaiting_detail: IndexMap<String, (u64, EnvironmentPullRequestEntry)>,
    batches: HashMap<u64, BatchTally>,
    next_batch: u64,
    toasts: ToastQueue,
    _store_subscription: Subscription,
}

impl EventEmitter<PullRequestActionRunnerEvent> for PullRequestActionRunner {}

impl PullRequestActionRunner {
    pub fn new(
        store: Entity<PullRequestStore>,
        settings: MergeMethodSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&store, |this, _, cx| this.on_store(cx));
        Self {
            store,
            settings,
            tracker: Tracker::default(),
            awaiting_detail: IndexMap::new(),
            batches: HashMap::new(),
            next_batch: 0,
            toasts: ToastQueue::default(),
            _store_subscription: subscription,
        }
    }

    pub fn toasts(&self) -> &ToastQueue {
        &self.toasts
    }

    /// `actionPending` for one row.
    pub fn action_pending(&self, key: &str) -> bool {
        self.tracker
            .in_flight
            .get(key)
            .is_some_and(|flight| flight.origin == Origin::Single)
            || self.awaiting_detail.contains_key(key)
    }

    /// `closingKeys.has(key)`.
    pub fn closing(&self, key: &str) -> bool {
        self.tracker
            .in_flight
            .get(key)
            .is_some_and(|flight| matches!(flight.origin, Origin::Batch(_)))
    }

    /// `perform(action)` of `usePullRequestActionRunner`, for a listed row.
    pub fn perform(
        &mut self,
        entry: &EnvironmentPullRequestEntry,
        action: PullRequestAction,
        cx: &mut Context<Self>,
    ) {
        let key = entry_key(entry);
        if self.tracker.in_flight.contains_key(&key) || self.awaiting_detail.contains_key(&key) {
            return;
        }
        if action != PullRequestAction::Merge {
            self.start(entry.clone(), action, None, Origin::Single, cx);
            return;
        }
        // Detail is read only for a merge: it is what says which methods the repository allows.
        let reference = reference_of_entry(entry);
        let held = self
            .store
            .read(cx)
            .detail(&reference)
            .and_then(|snapshot| snapshot.detail.ready().cloned());
        match held {
            Some(detail) => self.merge_with(entry.clone(), &detail, cx),
            None => {
                if let Some(source) = self.store.read(cx).source() {
                    source.open_detail(&reference, cx);
                }
                let token = self.tracker.take_token();
                self.awaiting_detail
                    .insert(key.clone(), (token, entry.clone()));
                // A detail that never answers must not leave the row spinning.
                let runner = cx.entity().downgrade();
                cx.spawn(async move |_, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(UNSEEN_GIVE_UP_MS))
                        .await;
                    runner
                        .update(cx, |runner, cx| {
                            if runner
                                .awaiting_detail
                                .get(&key)
                                .is_some_and(|(awaited, _)| *awaited == token)
                            {
                                runner.awaiting_detail.shift_remove(&key);
                                runner.fail(
                                    PullRequestAction::Merge,
                                    Some("The pull request did not load."),
                                    cx,
                                );
                                cx.notify();
                            }
                        })
                        .ok();
                })
                .detach();
                cx.notify();
            }
        }
    }

    fn merge_with(
        &mut self,
        entry: EnvironmentPullRequestEntry,
        detail: &PullRequestDetailView,
        cx: &mut Context<Self>,
    ) {
        let project_default =
            (self.settings.project_default)(&entry.environment_id, &entry.project_id);
        let last_selected = (self.settings.last_selected)();
        match resolve_quick_merge_method(detail, project_default, last_selected) {
            Ok(method) => self.start(
                entry,
                PullRequestAction::Merge,
                Some(method),
                Origin::Single,
                cx,
            ),
            Err(message) => self.fail(PullRequestAction::Merge, Some(&message), cx),
        }
    }

    fn start(
        &mut self,
        entry: EnvironmentPullRequestEntry,
        action: PullRequestAction,
        method: Option<PullRequestMergeMethod>,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let key = entry_key(&entry);
        let reference = reference_of_entry(&entry);
        let token = self.tracker.begin(entry, action, origin);
        if let Some(source) = self.store.read(cx).source() {
            source.run_action(&reference, action, method, None, cx);
        }
        // The source may already have marked the action running.
        self.on_store(cx);
        let runner = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(UNSEEN_GIVE_UP_MS))
                .await;
            runner
                .update(cx, |runner, cx| runner.give_up(&key, token, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// The give-up timer's end: stop watching an action the source never reported running. A row
    /// of a close sweep that never started is counted against it, so the sweep still reports.
    fn give_up(&mut self, key: &str, token: u64, cx: &mut Context<Self>) {
        let Some(flight) = self.tracker.give_up_unseen(key, token) else {
            return;
        };
        if let Origin::Batch(id) = flight.origin {
            if let Some(tally) = self.batches.get_mut(&id) {
                tally.record_did_not_start(flight.entry.number.get());
            }
            self.finish_batch_if_done(id, cx);
        }
        cx.notify();
    }

    /// `usePullRequestCloseBatch().close`: queue a close sweep through the same lanes as
    /// individual actions.
    pub fn close_batch(&mut self, entries: &[EnvironmentPullRequestEntry], cx: &mut Context<Self>) {
        // The sweep only gathers rows whose host can close them, so state is all that is left.
        let batch: Vec<EnvironmentPullRequestEntry> = entries
            .iter()
            .filter(|entry| {
                entry.state == PullRequestState::Open
                    && !self.tracker.in_flight.contains_key(&entry_key(entry))
            })
            .cloned()
            .collect();
        if batch.is_empty() {
            return;
        }
        let id = self.next_batch;
        self.next_batch += 1;
        self.batches.insert(
            id,
            BatchTally {
                total: batch.len(),
                ..BatchTally::default()
            },
        );
        for entry in batch {
            self.start(entry, PullRequestAction::Close, None, Origin::Batch(id), cx);
        }
    }

    fn on_store(&mut self, cx: &mut Context<Self>) {
        let ready_merges: Vec<(
            String,
            EnvironmentPullRequestEntry,
            Loadable<PullRequestDetailView>,
        )> = self
            .awaiting_detail
            .iter()
            .filter_map(|(key, (_, entry))| {
                let load = self
                    .store
                    .read(cx)
                    .detail(&reference_of_entry(entry))
                    .map(|snapshot| snapshot.detail.clone())?;
                (!load.is_pending()).then(|| (key.clone(), entry.clone(), load))
            })
            .collect();
        for (key, entry, load) in ready_merges {
            self.awaiting_detail.shift_remove(&key);
            match load {
                Loadable::Ready(detail) => self.merge_with(entry, &detail, cx),
                Loadable::Failed(message) => {
                    self.fail(PullRequestAction::Merge, Some(&message), cx)
                }
                Loadable::Pending => {}
            }
        }

        let store = self.store.clone();
        let ended = self.tracker.settle(|reference| {
            store
                .read(cx)
                .detail(reference)
                .map(|snapshot| ActionSnapshot {
                    pending: snapshot.pending_action,
                    error: snapshot.action_error.clone(),
                })
        });
        for (flight, outcome) in ended {
            match (flight.origin, outcome) {
                (Origin::Single, Outcome::Succeeded) => {
                    self.toasts.push(
                        cx,
                        |runner| &mut runner.toasts,
                        ToastKind::Success,
                        pull_request_action_success_label(flight.action),
                        None,
                    );
                    cx.emit(PullRequestActionRunnerEvent::Acted {
                        entry: flight.entry,
                        action: flight.action,
                    });
                }
                (Origin::Single, Outcome::Failed(message)) => {
                    self.fail(flight.action, Some(&message), cx);
                }
                (Origin::Batch(id), outcome) => {
                    let number = flight.entry.number;
                    let acted = outcome == Outcome::Succeeded;
                    if let Some(tally) = self.batches.get_mut(&id) {
                        match outcome {
                            Outcome::Succeeded => tally.closed += 1,
                            Outcome::Failed(message) => tally.failures.push(format!(
                                "#{number}: {}",
                                readable_failure(
                                    Some(&message),
                                    pull_request_action_failure_hint(PullRequestAction::Close)
                                )
                            )),
                        }
                    }
                    if acted {
                        cx.emit(PullRequestActionRunnerEvent::Acted {
                            entry: flight.entry,
                            action: PullRequestAction::Close,
                        });
                    }
                    self.finish_batch_if_done(id, cx);
                }
            }
        }
        cx.notify();
    }

    fn finish_batch_if_done(&mut self, id: u64, cx: &mut Context<Self>) {
        let still_running = self
            .tracker
            .in_flight
            .values()
            .any(|flight| flight.origin == Origin::Batch(id));
        if still_running {
            return;
        }
        let Some(tally) = self.batches.remove(&id) else {
            return;
        };
        let kind = if tally.is_error() {
            ToastKind::Error
        } else {
            ToastKind::Success
        };
        self.toasts.push(
            cx,
            |runner| &mut runner.toasts,
            kind,
            tally.title(),
            tally.description(),
        );
    }

    /// The failure toast: the thing that did not happen, then the host's sentence or what to try.
    fn fail(&mut self, action: PullRequestAction, failure: Option<&str>, cx: &mut Context<Self>) {
        self.toasts.push(
            cx,
            |runner| &mut runner.toasts,
            ToastKind::Error,
            pull_request_action_failure_label(action),
            Some(readable_failure(
                failure,
                pull_request_action_failure_hint(action),
            )),
        );
    }
}

/// The identity a sweep or a row uses for a reference (`pullRequestKey`).
pub fn reference_key(reference: &PullRequestRef) -> String {
    pull_request_key(reference)
}

/// `App`-level handle used by views that only hold the runner entity.
pub fn perform_on(
    runner: &Entity<PullRequestActionRunner>,
    entry: &EnvironmentPullRequestEntry,
    action: PullRequestAction,
    cx: &mut App,
) {
    runner.update(cx, |runner, cx| runner.perform(entry, action, cx));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_entry() -> EnvironmentPullRequestEntry {
        EnvironmentPullRequestEntry {
            entry: crate::pull_request::fixtures::list_entries()
                .into_iter()
                .next()
                .expect("a fixture row"),
            environment_id: EnvironmentId::new("local").expect("an environment id"),
        }
    }

    fn snapshot(pending: Option<PullRequestAction>, error: Option<&str>) -> ActionSnapshot {
        ActionSnapshot {
            pending,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn batch_titles_count_what_closed() {
        let mut tally = BatchTally {
            total: 1,
            closed: 1,
            failures: Vec::new(),
        };
        assert_eq!(tally.title(), "Closed 1 pull request");
        tally.total = 3;
        tally.closed = 3;
        assert_eq!(tally.title(), "Closed 3 pull requests");
        assert_eq!(tally.description(), None);
    }

    #[test]
    fn a_partial_close_says_how_many_and_lists_three_failures() {
        let tally = BatchTally {
            total: 5,
            closed: 1,
            failures: (1..=4).map(|n| format!("#{n}: refused")).collect(),
        };
        assert!(tally.is_error());
        assert_eq!(tally.title(), "Closed 1 of 5 pull requests");
        assert_eq!(
            tally.description().as_deref(),
            Some("#1: refused\n#2: refused\n#3: refused")
        );
    }

    #[test]
    fn an_action_ends_when_the_source_stops_reporting_it() {
        let entry = fixture_entry();
        let mut tracker = Tracker::default();
        tracker.begin(entry.clone(), PullRequestAction::Close, Origin::Single);
        // Not reported yet: still waiting, not ended.
        assert!(tracker.settle(|_| Some(snapshot(None, None))).is_empty());
        assert!(
            tracker
                .settle(|_| Some(snapshot(Some(PullRequestAction::Close), None)))
                .is_empty()
        );
        let ended = tracker.settle(|_| Some(snapshot(None, None)));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1, Outcome::Succeeded);
        assert!(tracker.in_flight.is_empty());
    }

    #[test]
    fn a_failed_action_carries_the_hosts_sentence() {
        let entry = fixture_entry();
        let mut tracker = Tracker::default();
        tracker.begin(entry, PullRequestAction::Merge, Origin::Single);
        tracker.settle(|_| Some(snapshot(Some(PullRequestAction::Merge), None)));
        let ended = tracker.settle(|_| Some(snapshot(None, Some("Branch is protected"))));
        assert_eq!(
            ended[0].1,
            Outcome::Failed("Branch is protected".to_owned())
        );
    }

    #[test]
    fn an_action_the_source_never_reports_is_given_up_on() {
        let entry = fixture_entry();
        let key = entry_key(&entry);
        let mut tracker = Tracker::default();
        tracker.begin(entry.clone(), PullRequestAction::Close, Origin::Single);
        let token = tracker.begin(entry.clone(), PullRequestAction::Close, Origin::Single);
        assert!(tracker.give_up_unseen(&key, token).is_some());
        let token = tracker.begin(entry, PullRequestAction::Close, Origin::Single);
        tracker.settle(|_| Some(snapshot(Some(PullRequestAction::Close), None)));
        assert!(tracker.give_up_unseen(&key, token).is_none());
    }

    #[test]
    fn a_timer_for_an_earlier_action_does_not_take_a_newer_one_on_the_same_row() {
        let entry = fixture_entry();
        let key = entry_key(&entry);
        let mut tracker = Tracker::default();
        let first = tracker.begin(entry.clone(), PullRequestAction::Close, Origin::Single);
        // The first ended and the row was acted on again before its timer fired.
        tracker.in_flight.shift_remove(&key);
        let second = tracker.begin(entry, PullRequestAction::Reopen, Origin::Single);
        assert_ne!(first, second);
        assert!(tracker.give_up_unseen(&key, first).is_none());
        assert!(tracker.in_flight.contains_key(&key));
        assert!(tracker.give_up_unseen(&key, second).is_some());
    }

    #[test]
    fn a_close_that_never_started_still_lets_the_sweep_report() {
        let mut tally = BatchTally {
            total: 3,
            closed: 2,
            failures: Vec::new(),
        };
        tally.record_did_not_start(41);
        assert!(tally.is_error());
        assert_eq!(tally.title(), "Closed 2 of 3 pull requests");
        assert_eq!(
            tally.description().as_deref(),
            Some("#41: The close did not start.")
        );
    }
}
