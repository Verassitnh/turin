//! The state half of upstream/apps/web/src/components/pullRequest/usePullRequestFilesViewed.ts:
//! presses show immediately and are held over the host's answer until a read that could have
//! seen them comes back, so the checkbox never waits on a round trip and never outlasts the
//! record behind it. The React hook's timers and atoms are the Code tab's job; this holds the
//! pure transitions over `pull_request_files_viewed_logic`.
//!
//! Port of upstream/apps/web/src/components/pullRequest/usePullRequestFilesViewed.ts (state half).

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use turin_contracts::pull_request::PullRequestFileViewed;

use turin_contracts::pull_request::PullRequestSetFilesViewedFile;

use crate::pull_request::pull_request_files_viewed_logic::{
    FileViewedOverlay, FileViewedStates, answered_paths_after_read, count_viewed_files,
    is_file_viewed, is_file_viewed_stale, revert_file_viewed_overlay, settle_file_viewed_overlay,
};

#[derive(Default)]
pub struct FilesViewed {
    overlay: FileViewedOverlay,
    /// Presses waiting for the next flush.
    queued: IndexMap<String, bool>,
    /// Paths a request is carrying, with that request's number.
    sent_by: HashMap<String, u64>,
    requests: u64,
    /// The host's answer as it stood when a write was acknowledged, per path (by version).
    answered_from: HashMap<String, usize>,
    states: FileViewedStates,
    /// Bumped every time the host's answer changes.
    version: usize,
    last: Vec<PullRequestFileViewed>,
    seen: bool,
    inflight: Option<Vec<(String, bool)>>,
}

impl FilesViewed {
    /// Takes the host's newest answer. Retires presses a read could have seen.
    pub fn sync(&mut self, files: &[PullRequestFileViewed]) {
        if self.seen && self.last == files {
            return;
        }
        self.seen = true;
        self.last = files.to_vec();
        self.version += 1;
        self.states = files
            .iter()
            .map(|file| (file.path.clone(), file.state))
            .collect();
        let pending = self.pending();
        let answered = answered_paths_after_read(&self.answered_from, self.version, &pending);
        for path in &answered {
            self.answered_from.remove(path);
        }
        self.overlay =
            settle_file_viewed_overlay(&self.overlay, Some(&self.states), &pending, &answered);
    }

    fn pending(&self) -> HashSet<String> {
        self.queued
            .keys()
            .chain(self.sent_by.keys())
            .cloned()
            .collect()
    }

    pub fn is_viewed(&self, path: &str) -> bool {
        is_file_viewed(path, Some(&self.states), &self.overlay)
    }

    pub fn is_stale(&self, path: &str) -> bool {
        is_file_viewed_stale(path, Some(&self.states), &self.overlay)
    }

    pub fn viewed_count(&self, paths: &[String]) -> usize {
        count_viewed_files(paths, Some(&self.states), &self.overlay)
    }

    /// A press: shown at once, sent with the next flush.
    pub fn press(&mut self, path: &str, viewed: bool) {
        self.overlay.insert(path.to_owned(), viewed);
        self.queued.insert(path.to_owned(), viewed);
    }

    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// The queued presses as one request; the paths are answered for from the next read on.
    pub fn flush(&mut self) -> Vec<(String, bool)> {
        let batch: Vec<(String, bool)> = std::mem::take(&mut self.queued).into_iter().collect();
        if batch.is_empty() {
            return batch;
        }
        self.requests += 1;
        for (path, _) in &batch {
            self.sent_by.insert(path.clone(), self.requests);
        }
        batch
    }

    /// The request that carried `batch` was accepted: its paths wait for the next read.
    pub fn acknowledge(&mut self, batch: &[(String, bool)]) {
        for (path, _) in batch {
            self.sent_by.remove(path);
            self.answered_from.insert(path.clone(), self.version);
        }
    }

    pub fn has_inflight(&self) -> bool {
        self.inflight.is_some()
    }

    pub fn mark_inflight(&mut self, batch: Vec<(String, bool)>) {
        self.inflight = Some(batch);
    }

    pub fn take_inflight(&mut self) -> Option<Vec<(String, bool)>> {
        self.inflight.take()
    }

    /// A failed write only rolls back presses this batch still owns.
    pub fn revert_inflight(&mut self) {
        let Some(batch) = self.inflight.take() else {
            return;
        };
        let owned: HashSet<String> = batch.iter().map(|(path, _)| path.clone()).collect();
        let payload: Vec<PullRequestSetFilesViewedFile> = batch
            .iter()
            .map(|(path, viewed)| PullRequestSetFilesViewedFile {
                path: path.clone(),
                viewed: *viewed,
            })
            .collect();
        self.overlay = revert_file_viewed_overlay(&self.overlay, &payload, &owned);
        for (path, _) in batch {
            self.sent_by.remove(&path);
            self.answered_from.remove(&path);
        }
    }

    /// A new pull request: nothing held here belongs to it.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::pull_request::PullRequestFileViewedState as State;

    fn file(path: &str, state: State) -> PullRequestFileViewed {
        PullRequestFileViewed {
            path: path.to_owned(),
            state,
        }
    }

    #[test]
    fn a_press_shows_before_the_host_answers() {
        let mut viewed = FilesViewed::default();
        viewed.sync(&[]);
        assert!(!viewed.is_viewed("a.rs"));
        viewed.press("a.rs", true);
        assert!(viewed.is_viewed("a.rs"));
        assert_eq!(viewed.viewed_count(&["a.rs".into(), "b.rs".into()]), 1);
    }

    #[test]
    fn the_first_read_after_the_write_retires_the_press_even_if_it_disagrees() {
        let mut viewed = FilesViewed::default();
        viewed.sync(&[]);
        viewed.press("a.rs", true);
        let batch = viewed.flush();
        assert_eq!(batch, vec![("a.rs".to_owned(), true)]);
        viewed.acknowledge(&batch);
        // A push landed between the write and the read, so the host says `dismissed`.
        viewed.sync(&[file("a.rs", State::Dismissed)]);
        assert!(!viewed.is_viewed("a.rs"));
        assert!(viewed.is_stale("a.rs"));
    }

    #[test]
    fn a_press_waiting_to_be_sent_survives_a_read() {
        let mut viewed = FilesViewed::default();
        viewed.sync(&[]);
        viewed.press("a.rs", true);
        viewed.sync(&[file("b.rs", State::Viewed)]);
        assert!(viewed.is_viewed("a.rs"));
        assert!(viewed.has_queued());
    }

    #[test]
    fn unpressing_a_viewed_file_clears_it() {
        let mut viewed = FilesViewed::default();
        viewed.sync(&[file("a.rs", State::Viewed)]);
        assert!(viewed.is_viewed("a.rs"));
        viewed.press("a.rs", false);
        assert!(!viewed.is_viewed("a.rs"));
    }
}
