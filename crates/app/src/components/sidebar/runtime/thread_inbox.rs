//! Port of upstream/packages/client-runtime/src/state/threadInbox.ts
//!
//! The Working section beta: which inbox threads fold into "Working", and
//! the two time orders (return to the user, last send).
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use super::collate::locale_compare;
use crate::state::models::{EnvironmentThreadShell, InteractionMode, RunStatus, thread_runtime_is_active};
use super::thread_sort::to_sortable_timestamp;
use crate::timestamp_format::now_ms;

/// `isThreadWorking`: a running run, or one stopped with background work
/// that will wake it. Approvals, questions, plan prompts, and failures stay
/// in the inbox.
pub fn is_thread_working(thread: &EnvironmentThreadShell) -> bool {
    if thread.has_pending_approvals || thread.has_pending_user_input {
        return false;
    }
    if thread.pending_subagent_request.is_some() {
        return false;
    }
    let runtime = thread.runtime.as_ref();
    if !thread_runtime_is_active(runtime) && runtime.map(|r| r.status) != Some(RunStatus::Idle) {
        return false;
    }
    // A plan prompt outranks lingering background work: the user has to act on it.
    let run_settled = thread.latest_run.as_ref().is_some_and(|run| {
        !matches!(
            run.status,
            RunStatus::Preparing
                | RunStatus::Queued
                | RunStatus::Starting
                | RunStatus::Running
                | RunStatus::Waiting
        ) && runtime.and_then(|r| r.active_run_id.as_ref()) != Some(&run.run_id)
    });
    !(thread.interaction_mode == InteractionMode::Plan
        && thread.has_actionable_proposed_plan
        && run_settled)
}

fn sort_newest_first(
    threads: Vec<EnvironmentThreadShell>,
    timestamp: impl Fn(&EnvironmentThreadShell) -> i64,
) -> Vec<EnvironmentThreadShell> {
    let mut keyed: Vec<(i64, EnvironmentThreadShell)> = threads
        .into_iter()
        .map(|thread| (timestamp(&thread), thread))
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| locale_compare(left.id.as_str(), right.id.as_str()))
            .then_with(|| {
                locale_compare(left.environment_id.as_str(), right.environment_id.as_str())
            })
    });
    keyed.into_iter().map(|(_, thread)| thread).collect()
}

/// `sortInboxThreadsByReturn`: newest first by when each last came back to
/// the user. `observed_return_at` adds returns the server does not stamp.
pub fn sort_inbox_threads_by_return(
    threads: Vec<EnvironmentThreadShell>,
    observed_return_at: impl Fn(&EnvironmentThreadShell) -> Option<i64>,
) -> Vec<EnvironmentThreadShell> {
    sort_newest_first(threads, |thread| {
        let run = thread.latest_run.as_ref();
        [
            to_sortable_timestamp(Some(&thread.created_at)).unwrap_or(0),
            to_sortable_timestamp(thread.unsettled_at.as_deref()).unwrap_or(0),
            to_sortable_timestamp(run.and_then(|r| r.requested_at.as_deref())).unwrap_or(0),
            to_sortable_timestamp(run.and_then(|r| r.completed_at.as_deref())).unwrap_or(0),
            observed_return_at(thread).unwrap_or(0),
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    })
}

/// `sortWorkingThreadsBySend`: newest first by the last message the user
/// sent; servers without the authored stamp fall back to the latest run's
/// request time.
pub fn sort_working_threads_by_send(
    threads: Vec<EnvironmentThreadShell>,
) -> Vec<EnvironmentThreadShell> {
    sort_newest_first(threads, |thread| {
        let sent = match &thread.latest_user_authored_message_at {
            None => thread
                .latest_run
                .as_ref()
                .and_then(|r| r.requested_at.as_deref()),
            Some(authored) => authored.as_deref(),
        };
        to_sortable_timestamp(Some(&thread.created_at))
            .unwrap_or(0)
            .max(to_sortable_timestamp(sent).unwrap_or(0))
    })
}

/// `createInboxReturnTracker`: remembers when this client saw each thread
/// leave the Working section. The first observation only takes a baseline,
/// so mounting never reshuffles the inbox.
#[derive(Debug, Default)]
pub struct InboxReturnTracker {
    last_working_keys: Option<HashSet<String>>,
    returns: HashMap<String, i64>,
}

impl InboxReturnTracker {
    /// `observe(threads)`; `None` resets while the beta is off.
    pub fn observe(&mut self, threads: Option<&[EnvironmentThreadShell]>) {
        self.observe_at(threads, now_ms());
    }

    fn observe_at(&mut self, threads: Option<&[EnvironmentThreadShell]>, at: i64) {
        let Some(threads) = threads else {
            self.last_working_keys = None;
            self.returns.clear();
            return;
        };
        let mut working = HashSet::new();
        let mut present = HashSet::new();
        for thread in threads {
            let key = thread.key();
            if is_thread_working(thread) {
                working.insert(key.clone());
            }
            present.insert(key);
        }
        // Drop deleted threads so the map stays bounded by the live thread list.
        self.returns.retain(|key, _| present.contains(key));
        for key in self.last_working_keys.iter().flatten() {
            if present.contains(key) && !working.contains(key) {
                self.returns.insert(key.clone(), at);
            }
        }
        self.last_working_keys = Some(working);
    }

    /// `returnedAt(thread)`.
    pub fn returned_at(&self, thread: &EnvironmentThreadShell) -> Option<i64> {
        self.returns.get(&thread.key()).copied()
    }
}

#[cfg(test)]
mod tests {
    use crate::state::models::ThreadRunSummary;
    use crate::state::models::fixtures::{run_id, runtime, shell};
    use super::*;

    fn thread(id: &str, working: bool) -> EnvironmentThreadShell {
        let mut thread = shell(id);
        thread.created_at = "2026-06-01T00:00:00.000Z".into();
        thread.runtime = working.then(|| runtime(RunStatus::Running));
        thread
    }

    // describe("createInboxReturnTracker")
    #[test]
    fn stamps_a_thread_when_it_stops_working_but_never_on_the_first_observation() {
        let mut tracker = InboxReturnTracker::default();
        tracker.observe(Some(&[thread("a", true), thread("b", false)]));
        assert_eq!(tracker.returned_at(&thread("a", true)), None);
        assert_eq!(tracker.returned_at(&thread("b", false)), None);
        tracker.observe(Some(&[thread("a", false), thread("b", false)]));
        assert!(tracker.returned_at(&thread("a", false)).is_some());
        assert_eq!(tracker.returned_at(&thread("b", false)), None);
    }

    #[test]
    fn forgets_deleted_threads_and_resets_when_the_beta_turns_off() {
        let mut tracker = InboxReturnTracker::default();
        tracker.observe(Some(&[thread("a", true), thread("b", true)]));
        tracker.observe(Some(&[thread("a", false), thread("b", false)]));
        tracker.observe(Some(&[thread("b", false)]));
        assert_eq!(tracker.returned_at(&thread("a", false)), None);
        assert!(tracker.returned_at(&thread("b", false)).is_some());
        tracker.observe(None);
        assert_eq!(tracker.returned_at(&thread("b", false)), None);
        tracker.observe(Some(&[thread("b", true)]));
        tracker.observe(Some(&[thread("b", false)]));
        assert!(tracker.returned_at(&thread("b", false)).is_some());
    }

    // describe("sortWorkingThreadsBySend")
    #[test]
    fn orders_by_the_last_message_the_user_sent_not_by_later_runs() {
        let mut sent_first = thread("sent-first", true);
        sent_first.latest_user_authored_message_at = Some(Some("2026-06-01T01:00:00.000Z".into()));
        sent_first.latest_run = Some(ThreadRunSummary {
            run_id: run_id("run:wake"),
            status: RunStatus::Running,
            requested_at: Some("2026-06-01T04:00:00.000Z".into()),
            started_at: Some("2026-06-01T04:00:00.000Z".into()),
            completed_at: None,
        });
        let mut sent_last = thread("sent-last", true);
        sent_last.latest_user_authored_message_at = Some(Some("2026-06-01T02:00:00.000Z".into()));
        let mut launched = thread("launched", true);
        launched.latest_user_authored_message_at = Some(None);
        let sorted = sort_working_threads_by_send(vec![launched, sent_first, sent_last]);
        assert_eq!(
            sorted.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["sent-last", "sent-first", "launched"]
        );
    }
}
