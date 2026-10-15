//! Port of upstream/packages/shared/src/composerPullRequestMatches.ts
//!
//! Upstream types the entry and linked lists separately (a union of two row types). Both are the
//! same shape in practice, so this takes one generic `M: ComposerPullRequestMatch` for both.

use std::collections::HashSet;

use turin_contracts::base_schemas::js_trim;
use turin_contracts::pull_request::PullRequestState;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

use crate::thread_pull_requests::visible_thread_pull_requests;

/// The fields the composer's `#` suggestions read.
pub trait ComposerPullRequestMatch {
    fn project_id(&self) -> &str;
    fn repository(&self) -> &str;
    fn number(&self) -> u64;
    fn updated_at(&self) -> &str;
}

/// A suggestion row built from a thread's link snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerPullRequestRow {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub updated_at: String,
}

impl ComposerPullRequestMatch for ComposerPullRequestRow {
    fn project_id(&self) -> &str {
        &self.project_id
    }

    fn repository(&self) -> &str {
        &self.repository
    }

    fn number(&self) -> u64 {
        self.number
    }

    fn updated_at(&self) -> &str {
        &self.updated_at
    }
}

/// Suggestion rows for the pull requests a thread links, from any repository. Built from the
/// synced snapshot, so a link the server has not read yet is left out until it has one.
pub fn composer_pull_requests_from_links(
    links: &[ThreadPullRequestLink],
    project_id: &str,
) -> Vec<ComposerPullRequestRow> {
    visible_thread_pull_requests(links)
        .into_iter()
        .filter_map(|link| {
            let snapshot = link.snapshot.as_ref()?;
            Some(ComposerPullRequestRow {
                project_id: project_id.to_owned(),
                repository: link.repository.as_str().to_owned(),
                number: link.number.get(),
                url: link.url.as_str().to_owned(),
                title: snapshot.title.as_str().to_owned(),
                head_branch: snapshot.head_branch.as_str().to_owned(),
                base_branch: snapshot.base_branch.as_str().to_owned(),
                state: snapshot.state,
                is_draft: snapshot.is_draft,
                updated_at: snapshot
                    .updated_at
                    .clone()
                    .unwrap_or_else(|| link.linked_at.clone()),
            })
        })
        .collect()
}

/// Pull requests matching the numeric fragment typed after `#`, de-duplicated. `entries` are kept
/// to the project's repository; `linked` rows (the thread's own pull requests) may come from any
/// repository. An exact number match always outranks a substring match so the result limit can
/// never drop the pull request the user typed in full; then linked rows lead, newest first.
pub fn filter_composer_pull_request_matches<'a, M: ComposerPullRequestMatch>(
    entries: &'a [M],
    linked: &'a [M],
    project_id: &str,
    repository: &str,
    query: &str,
    limit: usize,
) -> Vec<&'a M> {
    let repository = js_trim(repository).to_lowercase();
    let key_of = |entry: &M| {
        format!(
            "{}#{}",
            js_trim(entry.repository()).to_lowercase(),
            entry.number()
        )
    };
    let linked: Vec<&M> = linked
        .iter()
        .filter(|entry| entry.number().to_string().contains(query))
        .collect();
    let linked_keys: HashSet<String> = linked.iter().map(|entry| key_of(entry)).collect();
    let matching_entries = entries.iter().filter(|entry| {
        entry.project_id() == project_id
            && js_trim(entry.repository()).to_lowercase() == repository
            && entry.number().to_string().contains(query)
    });
    // Project rows go first so a linked pull request the listing also has keeps the fresher row.
    let mut seen = HashSet::new();
    let mut unique: Vec<&M> = Vec::new();
    for entry in matching_entries.chain(linked.iter().copied()) {
        if seen.insert(key_of(entry)) {
            unique.push(entry);
        }
    }
    let is_exact = |entry: &M| entry.number().to_string() == query;
    let is_linked = |entry: &M| linked_keys.contains(&key_of(entry));
    // Stable sort on a copy, as upstream's `.sort()` does.
    unique.sort_by(|left, right| {
        is_exact(right)
            .cmp(&is_exact(left))
            .then_with(|| is_linked(right).cmp(&is_linked(left)))
            .then_with(|| right.updated_at().cmp(left.updated_at()))
    });
    unique.truncate(limit);
    unique
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString as Tne};
    use turin_contracts::thread_pull_request::{
        ThreadPullRequestLinkSource, ThreadPullRequestSnapshot,
    };

    fn entry(number: u64, updated_at: &str) -> ComposerPullRequestRow {
        ComposerPullRequestRow {
            project_id: "p1".to_owned(),
            repository: "owner/repo".to_owned(),
            number,
            url: format!("https://github.com/owner/repo/pull/{number}"),
            title: "Change".to_owned(),
            head_branch: "feature".to_owned(),
            base_branch: "main".to_owned(),
            state: PullRequestState::Open,
            is_draft: false,
            updated_at: updated_at.to_owned(),
        }
    }

    fn with_repository(
        mut row: ComposerPullRequestRow,
        repository: &str,
    ) -> ComposerPullRequestRow {
        row.repository = repository.to_owned();
        row
    }

    fn link(repository: &str, number: u64, snapshot: Option<&str>) -> ThreadPullRequestLink {
        ThreadPullRequestLink {
            host: Tne::new("github.com").unwrap(),
            repository: Tne::new(repository).unwrap(),
            number: PositiveInt::new(number).unwrap(),
            url: Tne::new(format!("https://github.com/{repository}/pull/{number}")).unwrap(),
            source: ThreadPullRequestLinkSource::Agent,
            linked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            snapshot: snapshot.map(|updated_at| ThreadPullRequestSnapshot {
                state: PullRequestState::Open,
                title: Tne::new(format!("{repository} {number}")).unwrap(),
                head_branch: Tne::new("feature").unwrap(),
                base_branch: Tne::new("main").unwrap(),
                is_draft: false,
                updated_at: Some(updated_at.to_owned()),
                synced_at: "2026-01-02T00:00:00.000Z".to_owned(),
                closed_at: None,
                merged_at: None,
                author: None,
                additions: None,
                deletions: None,
                changed_files: None,
                review_decision: None,
                checks_state: None,
                mergeability: None,
            }),
            stack: None,
            watch: None,
        }
    }

    fn numbers(rows: &[&ComposerPullRequestRow]) -> Vec<u64> {
        rows.iter().map(|row| row.number).collect()
    }

    #[test]
    fn ranks_the_exact_number_above_newer_substring_matches() {
        let entries = [
            entry(1234, "2026-01-03"),
            entry(123, "2026-01-01"),
            entry(1230, "2026-01-02"),
        ];
        let result =
            filter_composer_pull_request_matches(&entries, &[], "p1", "owner/repo", "123", 10);
        assert_eq!(numbers(&result), vec![123, 1234, 1230]);
    }

    #[test]
    fn leaves_the_callers_slice_untouched() {
        let entries = [entry(2, "2026-01-01"), entry(1, "2026-01-02")];
        let _ = filter_composer_pull_request_matches(&entries, &[], "p1", "owner/repo", "", 10);
        assert_eq!(entries[0].number, 2);
        assert_eq!(entries[1].number, 1);
    }

    #[test]
    fn de_duplicates_and_honours_the_limit() {
        let entries = [
            entry(7, "2026-01-02"),
            entry(7, "2026-01-01"),
            entry(8, "2026-01-03"),
        ];
        let result = filter_composer_pull_request_matches(&entries, &[], "p1", "owner/repo", "", 1);
        assert_eq!(numbers(&result), vec![8]);
    }

    #[test]
    fn offers_a_linked_pull_request_from_another_repository_above_the_projects_own() {
        let entries = [
            entry(4, "2026-01-03"),
            with_repository(entry(4, "2026-01-04"), "owner/other"),
        ];
        let linked = composer_pull_requests_from_links(
            &[link("owner/repo-b", 4, Some("2026-01-02T00:00:00.000Z"))],
            "p1",
        );
        let result =
            filter_composer_pull_request_matches(&entries, &linked, "p1", "owner/repo", "4", 10);
        let keys: Vec<String> = result
            .iter()
            .map(|row| format!("{}#{}", row.repository, row.number))
            .collect();
        assert_eq!(keys, vec!["owner/repo-b#4", "owner/repo#4"]);
    }

    #[test]
    fn keeps_one_row_for_a_linked_pull_request_the_project_listing_already_has() {
        let entries = [entry(4, "2026-01-03")];
        let linked = composer_pull_requests_from_links(
            &[link("Owner/Repo", 4, Some("2026-01-02T00:00:00.000Z"))],
            "p1",
        );
        let result =
            filter_composer_pull_request_matches(&entries, &linked, "p1", "owner/repo", "", 10);
        assert_eq!(result, vec![&entries[0]]);
    }

    #[test]
    fn skips_links_that_are_not_synced_yet_or_were_dismissed() {
        let mut dismissed = link("owner/repo-b", 5, Some("2026-01-02T00:00:00.000Z"));
        dismissed.source = ThreadPullRequestLinkSource::StackDismissed;
        let rows =
            composer_pull_requests_from_links(&[link("owner/repo-b", 4, None), dismissed], "p1");
        assert!(rows.is_empty());
    }
}
