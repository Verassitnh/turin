//! Port of the running-thread logic in upstream/apps/web/src/state/threads.ts
//! (`createRunningThreadKeepAliveAtom`, `isRunning`, `isDetailDone`).
//!
//! The rest of that file is Effect-Atom wiring (`threadEnvironment`,
//! `environmentThreadDetails`, `environmentThreadShells`, `useEnvironmentThread`)
//! that has no counterpart here: the GPUI app owns the orchestrator in-process
//! (PORTING.md "No client/server split"), so a thread's state is read from the
//! store rather than through a per-thread atom family.
//!
//! What is kept is the policy. Upstream keeps the detail stream of every
//! running thread mounted, and keeps a stopped thread's mounted until its own
//! detail is live and shows the stop too, because the shell and detail streams
//! are independent and the shell can report a stop before the detail catches
//! up. Then the stream closes and the settled state is saved to disk.
//! [`RunningThreadKeepAlive`] holds the set of threads that must stay mounted;
//! the caller mounts and unmounts to match, and calls
//! [`RunningThreadKeepAlive::update`] again whenever the shell list changes or
//! a watched detail ([`RunningThreadKeepAlive::awaiting_detail`]) becomes done.
//! Atom `get.once` / `get.subscribe` / `get.mount` are the caller's.
//!
//! Single environment, so upstream's per-environment map is one set.
//!
//! Port of upstream/apps/web/src/state/threads.ts (running-thread logic).

use std::collections::HashSet;
use std::hash::Hash;

use turin_contracts::orchestration_v2::{
    OrchestrationV2RunStatus, OrchestrationV2ShellThreadStatus,
};

use crate::state::thread_state::{EnvironmentThreadState, EnvironmentThreadStatus};

/// `isRunning`: `preparing`, `queued`, `starting` or `running`.
pub fn is_running(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running")
}

/// `isRunning` over a sidebar row's status.
pub fn is_shell_status_running(status: OrchestrationV2ShellThreadStatus) -> bool {
    matches!(
        status,
        OrchestrationV2ShellThreadStatus::Preparing
            | OrchestrationV2ShellThreadStatus::Queued
            | OrchestrationV2ShellThreadStatus::Starting
            | OrchestrationV2ShellThreadStatus::Running
    )
}

/// `isRunning` over a run's status.
pub fn is_run_status_running(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Queued
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
    )
}

/// True once a thread's own stream no longer needs to stay open: it is in sync
/// and shows a settled session, or it cannot progress (deleted or failed). A
/// stream that is still loading or reconnecting keeps waiting for the stop.
///
/// `None` is an `AsyncResult` that is not a success (initial, waiting or
/// failed), which is done: there is nothing to wait for.
pub fn is_detail_done(state: Option<&EnvironmentThreadState>) -> bool {
    let Some(state) = state else {
        return true;
    };
    if state.status == EnvironmentThreadStatus::Deleted || state.error.is_some() {
        return true;
    }
    state.status == EnvironmentThreadStatus::Live
        && !state.data.as_ref().is_some_and(|thread| {
            thread
                .runs
                .iter()
                .any(|run| is_run_status_running(run.status))
        })
}

/// `runningThreadIdsAtom`: the running threads' ids, keeping their identity
/// until a thread starts or stops, so ordinary shell updates do not rebuild
/// the keep-alive set.
#[derive(Debug, Clone)]
pub struct RunningThreadIds<T> {
    previous: Vec<T>,
}

impl<T> Default for RunningThreadIds<T> {
    fn default() -> Self {
        Self {
            previous: Vec::new(),
        }
    }
}

impl<T: Clone + PartialEq> RunningThreadIds<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the running ids and whether they differ from the last call
    /// (`arrayElementsEqual`).
    pub fn update(&mut self, threads: impl IntoIterator<Item = (T, bool)>) -> (&[T], bool) {
        let running: Vec<T> = threads
            .into_iter()
            .filter_map(|(id, running)| running.then_some(id))
            .collect();
        let changed = self.previous != running;
        if changed {
            self.previous = running;
        }
        (&self.previous, changed)
    }
}

/// `createRunningThreadKeepAliveAtom`: the threads whose detail must stay
/// mounted.
#[derive(Debug, Clone)]
pub struct RunningThreadKeepAlive<T> {
    kept: HashSet<T>,
    running: HashSet<T>,
}

impl<T> Default for RunningThreadKeepAlive<T> {
    fn default() -> Self {
        Self {
            kept: HashSet::new(),
            running: HashSet::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> RunningThreadKeepAlive<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the kept set: every running thread, plus every thread kept
    /// last time that stopped but whose detail is not done yet.
    /// `detail_done` is read once per such thread (upstream's `get.once`, so a
    /// stopped thread does not hold its stream open by being observed).
    pub fn update(
        &mut self,
        running_thread_ids: &[T],
        detail_done: impl Fn(&T) -> bool,
    ) -> &HashSet<T> {
        let mut thread_ids: HashSet<T> = running_thread_ids.iter().cloned().collect();
        for thread_id in &self.kept {
            if thread_ids.contains(thread_id) {
                continue;
            }
            if detail_done(thread_id) {
                continue;
            }
            thread_ids.insert(thread_id.clone());
        }
        self.running = running_thread_ids.iter().cloned().collect();
        self.kept = thread_ids;
        &self.kept
    }

    pub fn kept(&self) -> &HashSet<T> {
        &self.kept
    }

    /// Stopped threads kept only until their detail is done. The caller
    /// watches these and calls [`Self::update`] when one is done ("rebuild
    /// when this detail is done, not on each update").
    pub fn awaiting_detail(&self) -> impl Iterator<Item = &T> {
        self.kept
            .iter()
            .filter(|thread_id| !self.running.contains(*thread_id))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn running_covers_preparing_queued_starting_and_running_only() {
        for status in ["preparing", "queued", "starting", "running"] {
            assert!(is_running(status), "{status}");
        }
        for status in [
            "idle",
            "waiting",
            "completed",
            "interrupted",
            "failed",
            "cancelled",
            "rolled_back",
            "",
        ] {
            assert!(!is_running(status), "{status}");
        }
    }

    #[test]
    fn typed_statuses_agree_with_the_string_predicate() {
        use OrchestrationV2ShellThreadStatus as Shell;
        for (status, name) in [
            (Shell::Idle, "idle"),
            (Shell::Preparing, "preparing"),
            (Shell::Queued, "queued"),
            (Shell::Starting, "starting"),
            (Shell::Running, "running"),
            (Shell::Waiting, "waiting"),
            (Shell::Completed, "completed"),
            (Shell::Interrupted, "interrupted"),
            (Shell::Failed, "failed"),
            (Shell::Cancelled, "cancelled"),
            (Shell::RolledBack, "rolled_back"),
        ] {
            assert_eq!(is_shell_status_running(status), is_running(name), "{name}");
        }
        use OrchestrationV2RunStatus as Run;
        for (status, name) in [
            (Run::Preparing, "preparing"),
            (Run::Queued, "queued"),
            (Run::Starting, "starting"),
            (Run::Running, "running"),
            (Run::Waiting, "waiting"),
            (Run::Completed, "completed"),
            (Run::Interrupted, "interrupted"),
            (Run::Failed, "failed"),
            (Run::Cancelled, "cancelled"),
            (Run::RolledBack, "rolled_back"),
        ] {
            assert_eq!(is_run_status_running(status), is_running(name), "{name}");
        }
    }

    fn state(status: EnvironmentThreadStatus) -> EnvironmentThreadState {
        let mut state = EnvironmentThreadState::default();
        state.status = status;
        state
    }

    #[test]
    fn a_detail_that_is_not_a_success_is_done() {
        assert!(is_detail_done(None));
    }

    #[test]
    fn a_deleted_or_failed_detail_is_done() {
        assert!(is_detail_done(Some(&state(
            EnvironmentThreadStatus::Deleted
        ))));
        let mut failed = state(EnvironmentThreadStatus::Synchronizing);
        failed.error = Some("Could not synchronize the thread.".to_owned());
        assert!(is_detail_done(Some(&failed)));
    }

    #[test]
    fn a_detail_that_is_still_loading_or_reconnecting_is_not_done() {
        for status in [
            EnvironmentThreadStatus::Empty,
            EnvironmentThreadStatus::Cached,
            EnvironmentThreadStatus::Synchronizing,
        ] {
            assert!(!is_detail_done(Some(&state(status))), "{status:?}");
        }
    }

    #[test]
    fn a_live_detail_without_a_running_run_is_done() {
        assert!(is_detail_done(Some(&state(EnvironmentThreadStatus::Live))));
    }

    #[test]
    fn running_ids_keep_their_identity_until_a_thread_starts_or_stops() {
        let mut ids = RunningThreadIds::new();
        let (running, changed) = ids.update([("a", true), ("b", false)]);
        assert_eq!(running, ["a"]);
        assert!(changed);

        let (running, changed) = ids.update([("a", true), ("b", false)]);
        assert_eq!(running, ["a"]);
        assert!(!changed);

        let (running, changed) = ids.update([("a", true), ("b", true)]);
        assert_eq!(running, ["a", "b"]);
        assert!(changed);

        let (running, changed) = ids.update([("a", false), ("b", true)]);
        assert_eq!(running, ["b"]);
        assert!(changed);
    }

    #[test]
    fn a_stopped_thread_stays_kept_until_its_own_detail_is_done() {
        let details: RefCell<HashMap<&str, bool>> = RefCell::new(HashMap::new());
        let done = |id: &&str| details.borrow().get(id).copied().unwrap_or(true);
        let mut keep_alive = RunningThreadKeepAlive::new();

        let kept = keep_alive.update(&["a", "b"], done);
        assert_eq!(kept.len(), 2);

        // The shell reports `a` stopped before its detail catches up.
        details.borrow_mut().insert("a", false);
        let kept = keep_alive.update(&["b"], done);
        assert!(kept.contains("a") && kept.contains("b"));
        assert_eq!(keep_alive.awaiting_detail().collect::<Vec<_>>(), [&"a"]);

        // Still not done on an unrelated rebuild: stays.
        let kept = keep_alive.update(&["b"], done);
        assert!(kept.contains("a"));

        // The detail shows the stop: the stream can close.
        details.borrow_mut().insert("a", true);
        let kept = keep_alive.update(&["b"], done);
        assert!(!kept.contains("a") && kept.contains("b"));
        assert_eq!(keep_alive.awaiting_detail().count(), 0);
    }

    #[test]
    fn a_thread_never_kept_is_not_resurrected_by_a_pending_detail() {
        let mut keep_alive = RunningThreadKeepAlive::new();
        keep_alive.update(&["a"], |_| false);
        keep_alive.update(&[], |_| true);
        // `a` was dropped; a later rebuild with a not-done detail must not
        // bring it back, because only the previous set is consulted.
        let kept = keep_alive.update(&[], |_| false);
        assert!(kept.is_empty());
    }
}
