//! Port of upstream/packages/shared/src/composerPullRequestMatches.ts

use std::collections::{HashMap, HashSet};

use turin_contracts::pull_request::PullRequestState;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

use super::thread_pull_requests::visible_thread_pull_requests;

/// What the matcher needs of a suggestion row.
pub trait ComposerPullRequestMatch {
    fn number(&self) -> u64;
    fn project_id(&self) -> &str;
    fn repository(&self) -> &str;
    fn updated_at(&self) -> &str;
}

/// A suggestion row built from a thread's synced link snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposerLinkedPullRequest {
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

impl ComposerPullRequestMatch for ComposerLinkedPullRequest {
    fn number(&self) -> u64 {
        self.number
    }
    fn project_id(&self) -> &str {
        &self.project_id
    }
    fn repository(&self) -> &str {
        &self.repository
    }
    fn updated_at(&self) -> &str {
        &self.updated_at
    }
}

/// Either a project listing row or a linked row (upstream's `Entry | Linked` union).
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerMatch<Entry, Linked> {
    Entry(Entry),
    Linked(Linked),
}

impl<E: ComposerPullRequestMatch, L: ComposerPullRequestMatch> ComposerPullRequestMatch
    for ComposerMatch<E, L>
{
    fn number(&self) -> u64 {
        match self {
            Self::Entry(e) => e.number(),
            Self::Linked(l) => l.number(),
        }
    }
    fn project_id(&self) -> &str {
        match self {
            Self::Entry(e) => e.project_id(),
            Self::Linked(l) => l.project_id(),
        }
    }
    fn repository(&self) -> &str {
        match self {
            Self::Entry(e) => e.repository(),
            Self::Linked(l) => l.repository(),
        }
    }
    fn updated_at(&self) -> &str {
        match self {
            Self::Entry(e) => e.updated_at(),
            Self::Linked(l) => l.updated_at(),
        }
    }
}

/// Suggestion rows for the pull requests a thread links, from any repository. Built from the
/// synced snapshot, so a link the server has not read yet is left out until it has one.
pub fn composer_pull_requests_from_links(
    links: &[ThreadPullRequestLink],
    project_id: &str,
) -> Vec<ComposerLinkedPullRequest> {
    visible_thread_pull_requests(links)
        .into_iter()
        .filter_map(|link| {
            let snapshot = link.snapshot.as_ref()?;
            Some(ComposerLinkedPullRequest {
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
                    .map_or_else(|| link.linked_at.to_string(), |at| at.to_string()),
            })
        })
        .collect()
}

pub struct FilterComposerPullRequestMatchesInput<'a, Entry, Linked> {
    pub entries: &'a [Entry],
    pub linked: &'a [Linked],
    pub project_id: &'a str,
    pub repository: &'a str,
    pub query: &'a str,
    pub limit: usize,
}

fn key_of(entry: &impl ComposerPullRequestMatch) -> String {
    format!(
        "{}#{}",
        entry.repository().trim().to_lowercase(),
        entry.number()
    )
}

/// JS `localeCompare` for ISO timestamps: plain code-point order is what the strings need.
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.cmp(right)
}

/// Pull requests matching the numeric fragment typed after `#`, de-duplicated. `entries` are kept
/// to the project's repository; `linked` rows (the thread's own pull requests) may come from any
/// repository. An exact number match always outranks a substring match so the result limit can
/// never drop the pull request the user typed in full; then linked rows lead, newest first.
pub fn filter_composer_pull_request_matches<Entry, Linked>(
    input: FilterComposerPullRequestMatchesInput<'_, Entry, Linked>,
) -> Vec<ComposerMatch<Entry, Linked>>
where
    Entry: ComposerPullRequestMatch + Clone,
    Linked: ComposerPullRequestMatch + Clone,
{
    let repository = input.repository.trim().to_lowercase();
    let contains_query = |number: u64| number.to_string().contains(input.query);
    let linked: Vec<&Linked> = input
        .linked
        .iter()
        .filter(|e| contains_query(e.number()))
        .collect();
    let linked_keys: HashSet<String> = linked.iter().map(|e| key_of(*e)).collect();
    let matching_entries = input.entries.iter().filter(|entry| {
        entry.project_id() == input.project_id
            && entry.repository().trim().to_lowercase() == repository
            && contains_query(entry.number())
    });
    // Project rows go first so a linked pull request the listing also has keeps the fresher row.
    let mut unique: Vec<ComposerMatch<Entry, Linked>> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    let candidates = matching_entries
        .map(|e| ComposerMatch::Entry(e.clone()))
        .chain(linked.into_iter().map(|l| ComposerMatch::Linked(l.clone())));
    for candidate in candidates {
        if seen.insert(key_of(&candidate), ()).is_none() {
            unique.push(candidate);
        }
    }
    let is_exact = |entry: &ComposerMatch<Entry, Linked>| entry.number().to_string() == input.query;
    let is_linked = |entry: &ComposerMatch<Entry, Linked>| linked_keys.contains(&key_of(entry));
    // Stable sort, like JS `Array.prototype.sort`.
    unique.sort_by(|left, right| {
        is_exact(right)
            .cmp(&is_exact(left))
            .then(is_linked(right).cmp(&is_linked(left)))
            .then_with(|| locale_compare(right.updated_at(), left.updated_at()))
    });
    unique.truncate(input.limit);
    unique
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::thread_pull_request::{
        ThreadPullRequestLinkSource, ThreadPullRequestSnapshot,
    };

    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        number: u64,
        project_id: String,
        repository: String,
        updated_at: String,
    }
    impl ComposerPullRequestMatch for Row {
        fn number(&self) -> u64 {
            self.number
        }
        fn project_id(&self) -> &str {
            &self.project_id
        }
        fn repository(&self) -> &str {
            &self.repository
        }
        fn updated_at(&self) -> &str {
            &self.updated_at
        }
    }

    fn entry(number: u64, updated_at: &str) -> Row {
        Row {
            number,
            project_id: "p1".into(),
            repository: "owner/repo".into(),
            updated_at: updated_at.into(),
        }
    }

    fn filter(
        entries: &[Row],
        linked: &[ComposerLinkedPullRequest],
        query: &str,
        limit: usize,
    ) -> Vec<ComposerMatch<Row, ComposerLinkedPullRequest>> {
        filter_composer_pull_request_matches(FilterComposerPullRequestMatchesInput {
            entries,
            linked,
            project_id: "p1",
            repository: "owner/repo",
            query,
            limit,
        })
    }

    #[test]
    fn ranks_the_exact_number_above_newer_substring_matches() {
        let result = filter(
            &[
                entry(1234, "2026-01-03"),
                entry(123, "2026-01-01"),
                entry(1230, "2026-01-02"),
            ],
            &[],
            "123",
            10,
        );
        assert_eq!(
            result.iter().map(|m| m.number()).collect::<Vec<_>>(),
            vec![123, 1234, 1230]
        );
    }

    #[test]
    fn leaves_the_callers_slice_untouched() {
        let entries = vec![entry(2, "2026-01-01"), entry(1, "2026-01-02")];
        let snapshot = entries.clone();
        filter(&entries, &[], "", 10);
        assert_eq!(entries, snapshot);
    }

    #[test]
    fn de_duplicates_and_honours_the_limit() {
        let result = filter(
            &[
                entry(7, "2026-01-02"),
                entry(7, "2026-01-01"),
                entry(8, "2026-01-03"),
            ],
            &[],
            "",
            1,
        );
        assert_eq!(result.len(), 1);
    }

    fn link(
        repository: &str,
        number: u64,
        source: ThreadPullRequestLinkSource,
        synced: bool,
    ) -> ThreadPullRequestLink {
        serde_json::from_value(serde_json::json!({
            "host": "github.com",
            "repository": repository,
            "number": number,
            "url": format!("https://github.com/{repository}/pull/{number}"),
            "source": serde_json::to_value(source).unwrap(),
            "linkedAt": "2026-01-01T00:00:00.000Z",
            "snapshot": if synced { serde_json::to_value(ThreadPullRequestSnapshot {
                state: PullRequestState::Open,
                title: turin_contracts::TrimmedNonEmptyString::new(format!("{repository} {number}")).unwrap(),
                head_branch: turin_contracts::TrimmedNonEmptyString::new("feature").unwrap(),
                base_branch: turin_contracts::TrimmedNonEmptyString::new("main").unwrap(),
                is_draft: false,
                updated_at: Some("2026-01-02T00:00:00.000Z".parse().unwrap()),
                synced_at: "2026-01-02T00:00:00.000Z".parse().unwrap(),
                closed_at: None,
                merged_at: None,
                author: None,
                additions: None,
                deletions: None,
                changed_files: None,
                review_decision: None,
                checks_state: None,
                mergeability: None,
            }).unwrap() } else { serde_json::Value::Null },
            "stack": null,
        }))
        .unwrap()
    }

    #[test]
    fn offers_a_linked_pull_request_from_another_repository_above_the_projects_own() {
        let other = Row {
            repository: "owner/other".into(),
            ..entry(4, "2026-01-04")
        };
        let result = filter_composer_pull_request_matches(FilterComposerPullRequestMatchesInput {
            entries: &[entry(4, "2026-01-03"), other],
            linked: &composer_pull_requests_from_links(
                &[link(
                    "owner/repo-b",
                    4,
                    ThreadPullRequestLinkSource::Agent,
                    true,
                )],
                "p1",
            ),
            project_id: "p1",
            repository: "owner/repo",
            query: "4",
            limit: 10,
        });
        assert_eq!(
            result
                .iter()
                .map(|m| format!("{}#{}", m.repository(), m.number()))
                .collect::<Vec<_>>(),
            vec!["owner/repo-b#4", "owner/repo#4"]
        );
    }

    #[test]
    fn keeps_one_row_for_a_linked_pull_request_the_project_listing_already_has() {
        let result = filter(
            &[entry(4, "2026-01-03")],
            &composer_pull_requests_from_links(
                &[link(
                    "Owner/Repo",
                    4,
                    ThreadPullRequestLinkSource::Agent,
                    true,
                )],
                "p1",
            ),
            "",
            10,
        );
        assert_eq!(result, vec![ComposerMatch::Entry(entry(4, "2026-01-03"))]);
    }

    #[test]
    fn skips_links_that_are_not_synced_yet_or_were_dismissed() {
        assert!(
            composer_pull_requests_from_links(
                &[
                    link("owner/repo-b", 4, ThreadPullRequestLinkSource::Agent, false),
                    link(
                        "owner/repo-b",
                        5,
                        ThreadPullRequestLinkSource::StackDismissed,
                        true
                    ),
                ],
                "p1"
            )
            .is_empty()
        );
    }
}
