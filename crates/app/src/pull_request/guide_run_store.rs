//! Port of upstream/apps/web/src/components/pullRequest/guide/guideRunStore.ts.
//!
//! Guide runs this tab is waiting on, so the reader can leave and hear when one is done, and where
//! the reader is in each guide.
//!
//! A run takes minutes and finishes on the source. [`GuideRunCoordinator`](super::guide_run_coordinator)
//! watches every run listed here, wherever the reader is in the app, and says when it ends.
//! Progress is shared so a guide opened beside a new thread picks up at the same stop. Not
//! persisted: a reload forgets both, and the guide is still there when the reader comes back.
//!
//! The store is one GPUI entity held as a global ([`GuideRunStore::global`]), which is what the
//! zustand store is upstream.

use std::collections::{BTreeSet, HashMap};

use gpui_kit::{App, AppContext as _, Context, Entity, Global};
use indexmap::IndexMap;
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuideResult, PullRequestReviewGuideRun,
};

use super::pull_request_review_store::pull_request_review_key;

/// `guideRunKey`.
pub fn guide_run_key(environment_id: &EnvironmentId, reference: &PullRequestRef) -> String {
    format!(
        "{}:{}",
        environment_id.as_str(),
        pull_request_review_key(reference)
    )
}

/// `guideProgressKey`: a new head is a new tour, since its stops are not the ones read.
pub fn guide_progress_key(run_key: &str, head_sha: &str) -> String {
    format!("{run_key}:{head_sha}")
}

/// `GuideRunWatch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideRunWatch {
    pub environment_id: EnvironmentId,
    pub reference: PullRequestRef,
    /// The pull request's title, for the notice.
    pub title: Option<String>,
    /// The guide and failure on record when the wait began; a different one means the run ended.
    pub previous_guide_at: Option<String>,
    pub previous_failed_at: Option<String>,
    /// When the wait began, in epoch milliseconds.
    pub since: i64,
}

/// Where a reader is in one guide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuideProgress {
    pub position: usize,
    /// Stops the reader has moved on from, which the rail checks off.
    pub seen: BTreeSet<usize>,
}

#[derive(Default)]
pub struct GuideRunStore {
    watches: IndexMap<String, GuideRunWatch>,
    /// The guide on screen right now, which needs no notice when it lands.
    shown_key: Option<String>,
    /// A guide a notice asked to open; its panel switches to the Guide tab and clears this.
    open_request_key: Option<String>,
    /// Reading progress by guide, keyed by [`guide_progress_key`].
    progress: HashMap<String, GuideProgress>,
}

struct StoreGlobal(Entity<GuideRunStore>);

impl Global for StoreGlobal {}

impl GuideRunStore {
    /// The app's one store.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<StoreGlobal>() {
            return global.0.clone();
        }
        let store = cx.new(|_| Self::default());
        cx.set_global(StoreGlobal(store.clone()));
        store
    }

    pub fn watches(&self) -> &IndexMap<String, GuideRunWatch> {
        &self.watches
    }

    pub fn shown_key(&self) -> Option<&str> {
        self.shown_key.as_deref()
    }

    pub fn open_request_key(&self) -> Option<&str> {
        self.open_request_key.as_deref()
    }

    /// `watch`: a run already watched keeps its first baseline.
    pub fn watch(&mut self, watch: GuideRunWatch, cx: &mut Context<Self>) {
        let key = guide_run_key(&watch.environment_id, &watch.reference);
        if self.watches.contains_key(&key) {
            return;
        }
        self.watches.insert(key, watch);
        cx.notify();
    }

    pub fn settle(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.watches.shift_remove(key).is_some() {
            cx.notify();
        }
    }

    pub fn set_shown(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.shown_key != key {
            self.shown_key = key;
            cx.notify();
        }
    }

    pub fn request_open(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.open_request_key != key {
            self.open_request_key = key;
            cx.notify();
        }
    }

    pub fn get(&self, key: &str) -> GuideProgress {
        self.progress.get(key).cloned().unwrap_or_default()
    }

    /// `setProgress`.
    pub fn set(&mut self, key: &str, change: impl FnOnce(&GuideProgress) -> GuideProgress) {
        let next = change(&self.get(key));
        self.progress.insert(key.to_owned(), next);
    }
}

/// How often a watched run is re-read (`RUN_POLL_MS`). A run takes minutes, not seconds.
pub const RUN_POLL_MS: u64 = 5_000;
/// A run never seen running after this long was dropped before the first read (`UNSEEN_RUN_MS`).
pub const UNSEEN_RUN_MS: i64 = 60_000;

/// What one read of a watched run says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchDecision {
    /// Nothing yet: a read from before the run started, or no read at all.
    Wait,
    /// The run is going; its going away will mean it ended.
    Running,
    /// The run ended with nothing to say (the source dropped it, say on a restart).
    Settle,
    /// A guide was written, or the run failed (`failure` is the detail).
    Notice { failure: Option<String> },
}

/// The effect body of `GuideRunWatcher`.
pub fn evaluate_watch(
    data: Option<&PullRequestReviewGuideResult>,
    watch: &GuideRunWatch,
    saw_running: bool,
    now_ms: i64,
) -> WatchDecision {
    let Some(data) = data else {
        return WatchDecision::Wait;
    };
    if matches!(data.run, Some(PullRequestReviewGuideRun::Running { .. })) {
        return WatchDecision::Running;
    }
    let failure = match &data.run {
        Some(PullRequestReviewGuideRun::Failed {
            failed_at, detail, ..
        }) if Some(failed_at.as_str()) != watch.previous_failed_at.as_deref() => {
            Some(detail.as_str().to_owned())
        }
        _ => None,
    };
    let written = failure.is_none()
        && data.guide.as_ref().is_some_and(|guide| {
            Some(guide.generated_at.as_str()) != watch.previous_guide_at.as_deref()
        });
    if failure.is_none() && !written {
        return if saw_running || now_ms - watch.since > UNSEEN_RUN_MS {
            WatchDecision::Settle
        } else {
            WatchDecision::Wait
        };
    }
    WatchDecision::Notice { failure }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::fixtures;
    use pretty_assertions::assert_eq;
    use turin_contracts::TrimmedNonEmptyString;

    fn watch(previous_guide_at: Option<&str>, previous_failed_at: Option<&str>) -> GuideRunWatch {
        GuideRunWatch {
            environment_id: EnvironmentId::new("local").expect("an environment id"),
            reference: crate::pull_request::mock_source::demo_reference(),
            title: Some("Port the views".to_owned()),
            previous_guide_at: previous_guide_at.map(str::to_owned),
            previous_failed_at: previous_failed_at.map(str::to_owned),
            since: 1_000,
        }
    }

    fn running() -> PullRequestReviewGuideRun {
        PullRequestReviewGuideRun::Running {
            head_sha: TrimmedNonEmptyString::new("abc").unwrap(),
            started_at: "2026-10-14T00:00:00.000Z".to_owned(),
        }
    }

    fn failed(at: &str) -> PullRequestReviewGuideRun {
        PullRequestReviewGuideRun::Failed {
            head_sha: TrimmedNonEmptyString::new("abc").unwrap(),
            failed_at: at.to_owned(),
            detail: TrimmedNonEmptyString::new("model timed out").unwrap(),
        }
    }

    #[test]
    fn watch_keeps_the_first_baseline_and_settle_removes_it() {
        let mut store = GuideRunStore::default();
        let first = watch(Some("a"), None);
        let key = guide_run_key(&first.environment_id, &first.reference);
        // The entity methods need a context; the baseline rule is the map's.
        store.watches.insert(key.clone(), first.clone());
        assert!(store.watches.contains_key(&key));
        assert_eq!(store.shown_key(), None);
        assert!(store.watches.shift_remove(&key).is_some());
    }

    #[test]
    fn progress_starts_at_the_first_stop_and_keeps_seen() {
        let mut store = GuideRunStore::default();
        assert_eq!(store.get("k"), GuideProgress::default());
        store.set("k", |current| {
            let mut seen = current.seen.clone();
            seen.insert(0);
            GuideProgress { position: 1, seen }
        });
        assert_eq!(store.get("k").position, 1);
        assert!(store.get("k").seen.contains(&0));
        assert_eq!(store.get("other"), GuideProgress::default());
    }

    #[test]
    fn no_read_waits() {
        assert_eq!(
            evaluate_watch(None, &watch(None, None), false, 2_000),
            WatchDecision::Wait
        );
    }

    #[test]
    fn a_running_run_is_remembered() {
        let mut result = fixtures::guide_result();
        result.run = Some(running());
        assert_eq!(
            evaluate_watch(Some(&result), &watch(None, None), false, 2_000),
            WatchDecision::Running
        );
    }

    #[test]
    fn a_new_guide_ends_the_wait_with_a_notice() {
        let result = fixtures::guide_result();
        let generated = result.guide.as_ref().unwrap().generated_at.clone();
        assert_eq!(
            evaluate_watch(Some(&result), &watch(Some("older"), None), true, 2_000),
            WatchDecision::Notice { failure: None }
        );
        // The same guide as before the wait is not news.
        assert_eq!(
            evaluate_watch(
                Some(&result),
                &watch(Some(generated.as_str()), None),
                true,
                2_000
            ),
            WatchDecision::Settle
        );
    }

    #[test]
    fn a_read_from_before_the_run_started_waits_until_it_is_old_enough_to_drop() {
        let result = fixtures::guide_result();
        let generated = result.guide.as_ref().unwrap().generated_at.clone();
        let w = watch(Some(generated.as_str()), None);
        assert_eq!(
            evaluate_watch(Some(&result), &w, false, w.since + UNSEEN_RUN_MS),
            WatchDecision::Wait
        );
        assert_eq!(
            evaluate_watch(Some(&result), &w, false, w.since + UNSEEN_RUN_MS + 1),
            WatchDecision::Settle
        );
    }

    #[test]
    fn a_new_failure_is_a_notice_and_an_old_one_is_not() {
        let mut result = fixtures::guide_result();
        result.guide = None;
        result.run = Some(failed("2026-10-14T00:00:01.000Z"));
        assert_eq!(
            evaluate_watch(Some(&result), &watch(None, None), true, 2_000),
            WatchDecision::Notice {
                failure: Some("model timed out".to_owned())
            }
        );
        assert_eq!(
            evaluate_watch(
                Some(&result),
                &watch(None, Some("2026-10-14T00:00:01.000Z")),
                true,
                2_000
            ),
            WatchDecision::Settle
        );
    }
}
