//! Port of upstream/apps/web/src/components/pullRequest/pullRequestReviewStore.ts.
//!
//! The Zustand hooks become a plain, tab-lifetime store. No global store or React hook is ported.

use std::collections::BTreeMap;
use turin_contracts::pull_request::{PullRequestRef, PullRequestReviewCommentDraft};

/// A review being written, held until it is sent. Nothing here reaches the host: a review is
/// one request carrying every line comment and the verdict together, so a half-written one is
/// invisible to everyone else. The draft deliberately is not persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReviewComment {
    pub id: String,
    pub draft: PullRequestReviewCommentDraft,
}

/// A counter rather than anything derived from the comment: two remarks on one line can have
/// the same length, and an id built from the draft's contents can collide with a removed one.
#[derive(Debug, Clone, Default)]
pub struct PullRequestReviewStore {
    pub drafts: BTreeMap<String, Vec<PendingReviewComment>>,
    pub summaries: BTreeMap<String, String>,
    pending_comment_sequence: u64,
}

/// A project's thread can review the same repository path and number on different hosts.
pub fn pull_request_review_key(reference: &PullRequestRef) -> String {
    serde_json::json!([
        reference.project_id.as_str(),
        reference
            .host
            .as_ref()
            .map(|host| host.as_str().to_lowercase()),
        reference.repository.as_str().to_lowercase(),
        reference.number.get(),
    ])
    .to_string()
}

impl PullRequestReviewStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_pending_review_comment_id(&mut self) -> String {
        self.pending_comment_sequence += 1;
        format!("pending-review-comment-{}", self.pending_comment_sequence)
    }

    /// The comments a pull request's draft holds.
    pub fn pending_review_comments(&self, reference: &PullRequestRef) -> &[PendingReviewComment] {
        self.drafts
            .get(&pull_request_review_key(reference))
            .map_or(&[], Vec::as_slice)
    }

    pub fn add_comment(&mut self, key: &str, comment: PendingReviewComment) {
        self.drafts.entry(key.to_owned()).or_default().push(comment);
    }

    pub fn remove_comment(&mut self, key: &str, comment_id: &str) {
        self.remove_comments(key, &[comment_id]);
    }

    pub fn remove_comments(&mut self, key: &str, comment_ids: &[&str]) {
        if let Some(entries) = self.drafts.get_mut(key) {
            entries.retain(|entry| !comment_ids.contains(&entry.id.as_str()));
            if entries.is_empty() {
                self.drafts.remove(key);
            }
        }
    }

    pub fn clear(&mut self, key: &str) {
        self.drafts.remove(key);
    }

    pub fn set_summary(&mut self, key: &str, body: &str) {
        self.summaries.insert(key.to_owned(), body.to_owned());
    }

    pub fn clear_summary(&mut self, key: &str, submitted_body: &str) {
        // The textarea stays editable while the request is in flight. Only remove the exact
        // draft the host accepted; a revised summary for the same PR is new work.
        if self
            .summaries
            .get(key)
            .is_some_and(|body| body == submitted_body)
        {
            self.summaries.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn comment(id: &str) -> PendingReviewComment {
        PendingReviewComment {
            id: id.into(),
            draft: serde_json::from_value(json!({
                "body": id, "path": "src/app.ts", "position": {"kind": "added", "newLine": 1}
            }))
            .unwrap(),
        }
    }
    fn reference(host: &str) -> PullRequestRef {
        serde_json::from_value(json!({"projectId":"project-a", "host":host,
            "repository":"owner/repo", "number":7}))
        .unwrap()
    }
    #[test]
    fn removes_only_line_comments_in_submitted_snapshot() {
        let mut store = PullRequestReviewStore::new();
        store.add_comment("review-a", comment("submitted"));
        let submitted_ids: Vec<String> = store.drafts["review-a"]
            .iter()
            .map(|e| e.id.clone())
            .collect();
        store.add_comment("review-a", comment("added-in-flight"));
        store.remove_comments(
            "review-a",
            &submitted_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(store.drafts["review-a"], vec![comment("added-in-flight")]);
    }
    #[test]
    fn keeps_summary_bodies_isolated_by_review_key() {
        let mut store = PullRequestReviewStore::new();
        store.set_summary("review-a", "Summary A");
        store.set_summary("review-b", "Summary B");
        store.clear_summary("review-a", "Summary A");
        assert_eq!(
            store.summaries,
            BTreeMap::from([("review-b".into(), "Summary B".into())])
        );
    }
    #[test]
    fn keeps_drafts_on_different_hosts_separate() {
        let public = pull_request_review_key(&reference("github.com"));
        let enterprise = pull_request_review_key(&reference("github.example.com"));
        let mut store = PullRequestReviewStore::new();
        store.add_comment(&public, comment("public"));
        store.set_summary(&public, "Public review");
        assert!(!store.drafts.contains_key(&enterprise));
        assert!(!store.summaries.contains_key(&enterprise));
        store.add_comment(&enterprise, comment("enterprise"));
        store.set_summary(&enterprise, "Enterprise review");
        store.clear(&enterprise);
        store.clear_summary(&enterprise, "Enterprise review");
        assert_eq!(store.drafts[&public], vec![comment("public")]);
        assert_eq!(store.summaries[&public], "Public review");
    }
    #[test]
    fn does_not_clear_a_summary_revised_during_submission() {
        let mut store = PullRequestReviewStore::new();
        store.set_summary("review-a", "Submitted body");
        store.set_summary("review-a", "Revised body");
        store.clear_summary("review-a", "Submitted body");
        assert_eq!(store.summaries["review-a"], "Revised body");
    }
}
