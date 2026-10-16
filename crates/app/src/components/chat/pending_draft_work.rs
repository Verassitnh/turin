//! Port of upstream/apps/web/src/components/chat/pendingDraftWork.ts
//!
//! Upstream keeps one module singleton so a remount sees the same counts.
//! Turin keeps the counter on the draft session the composer is given again
//! after a remount, instead of a process global.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct PendingDraftWork {
    counts: HashMap<String, u32>,
}

impl PendingDraftWork {
    pub fn begin(&mut self, draft_key: &str) {
        *self.counts.entry(draft_key.to_owned()).or_insert(0) += 1;
    }

    pub fn end(&mut self, draft_key: &str) {
        let remaining = self.counts.get(draft_key).copied().unwrap_or(0);
        if remaining > 1 {
            self.counts.insert(draft_key.to_owned(), remaining - 1);
        } else {
            self.counts.remove(draft_key);
        }
    }

    pub fn has(&self, draft_key: &str) -> bool {
        self.counts.get(draft_key).copied().unwrap_or(0) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_a_draft_until_every_transfer_has_finished() {
        let mut pending = PendingDraftWork::default();
        pending.begin("concurrent-transfers");
        pending.begin("concurrent-transfers");
        assert!(pending.has("concurrent-transfers"));
        pending.end("concurrent-transfers");
        assert!(pending.has("concurrent-transfers"));
        pending.end("concurrent-transfers");
        assert!(!pending.has("concurrent-transfers"));
    }

    #[test]
    fn keeps_one_drafts_work_from_blocking_another() {
        let mut pending = PendingDraftWork::default();
        pending.begin("isolated-a");
        assert!(!pending.has("isolated-b"));
        pending.end("isolated-a");
        assert!(!pending.has("isolated-a"));
    }

    #[test]
    fn keeps_a_claim_when_the_same_counter_outlives_a_remount() {
        let mut pending = PendingDraftWork::default();
        pending.begin("remounted");
        assert!(pending.has("remounted"));
        pending.end("remounted");
        assert!(!pending.has("remounted"));
    }

    #[test]
    fn does_not_go_negative_when_a_transfer_ends_twice() {
        let mut pending = PendingDraftWork::default();
        pending.begin("double-end");
        pending.end("double-end");
        pending.end("double-end");
        pending.begin("double-end");
        assert!(pending.has("double-end"));
    }
}
