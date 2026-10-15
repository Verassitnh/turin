//! Port of upstream/packages/shared/src/threadPullRequests.ts
//!
//! GitHub only. The Forgejo authority branch and the Azure DevOps legacy-key branch are cut
//! (PORTING cut list), so keys are host + repository + number with no authority or Azure
//! canonicalisation. The Hermes array-method workaround is not needed here.
//!
//! `applyThreadPullRequestLinkUpdate` takes the thread's link list rather than the whole thread:
//! the orchestration thread type is ported with the orchestration module, and this only reads
//! and rewrites `pullRequests`.

use chrono::DateTime;
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use turin_contracts::base_schemas::{PositiveInt, ProjectId, TrimmedNonEmptyString, js_trim};
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::pull_request::PullRequestState;
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
};

use crate::source_control::{
    RepositorySelectorIdentity, canonical_repository_key, source_control_repository_selector,
};

/// The identity of a pull request as a thread link sees it: lower-cased host and repository, plus
/// the number. Two links with equal keys are the same pull request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// What [`thread_pull_request_keys_equal`] compares. Build it from a link or by hand.
#[derive(Debug, Clone, Copy)]
pub struct PullRequestRef<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub number: u64,
}

impl<'a> From<&'a ThreadPullRequestLink> for PullRequestRef<'a> {
    fn from(link: &'a ThreadPullRequestLink) -> Self {
        Self {
            host: link.host.as_str(),
            repository: link.repository.as_str(),
            number: link.number.get(),
        }
    }
}

/// Normalize a stored key: trim and lower-case host and repository, then canonicalise the
/// repository key the same way the change request URL parser does.
pub fn normalize_thread_pull_request_key(
    host: &str,
    repository: &str,
    number: u64,
) -> PullRequestKey {
    let canonical = canonical_repository_key(&format!(
        "{}/{}",
        js_trim(host).to_lowercase(),
        js_trim(repository).to_lowercase()
    ));
    let (host, repository) = canonical
        .split_once('/')
        .unwrap_or((canonical.as_str(), ""));
    PullRequestKey {
        host: host.to_owned(),
        repository: repository.to_owned(),
        number,
    }
}

/// Identity comparison for links: host-level, case-insensitive on host and repository.
pub fn thread_pull_request_keys_equal(left: PullRequestRef<'_>, right: PullRequestRef<'_>) -> bool {
    thread_pull_request_key_of(left) == thread_pull_request_key_of(right)
}

fn thread_pull_request_key_of(link: PullRequestRef<'_>) -> PullRequestKey {
    normalize_thread_pull_request_key(link.host, link.repository, link.number)
}

/// The host a legacy link was recorded under: the fallback host, else the URL's host, else
/// `unknown`. Upstream reads `new URL(linked.url).hostname`, which is the host name without port.
pub fn legacy_thread_pull_request_key(
    repository: &str,
    number: u64,
    url: &str,
    fallback_host: Option<&str>,
) -> PullRequestKey {
    let host = fallback_host.map(str::to_owned).unwrap_or_else(|| {
        url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned())
    });
    let host = js_trim(&host).to_lowercase();
    PullRequestKey {
        host: if host.is_empty() {
            "unknown".to_owned()
        } else {
            host
        },
        repository: js_trim(repository).to_lowercase(),
        number,
    }
}

/// Links a user should see. Tombstoned stack members stay in the list only so the sync reactor
/// does not re-add them.
pub fn visible_thread_pull_requests(
    links: &[ThreadPullRequestLink],
) -> Vec<&ThreadPullRequestLink> {
    links
        .iter()
        .filter(|link| link.source != ThreadPullRequestLinkSource::StackDismissed)
        .collect()
}

/// Unsynced links are treated as open: they were just linked, and hiding them behind a terminal
/// PR until the first sync would make the link look lost.
fn is_open(link: &ThreadPullRequestLink) -> bool {
    link.snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.state == PullRequestState::Open)
}

/// `Date.parse`, with NaN reported as 0. Link timestamps are validated ISO strings upstream.
fn parse_millis(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.timestamp_millis())
        .unwrap_or(0)
}

fn latest_updated_at(link: &ThreadPullRequestLink) -> i64 {
    let value = link
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.updated_at.as_deref())
        .unwrap_or(link.linked_at.as_str());
    parse_millis(value)
}

/// The single pull request a one-slot surface (sidebar badge, tab icon, copy link) shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadCurrentPullRequest<'a> {
    Single(&'a ThreadPullRequestLink),
    Stack {
        /// Open links, highest layer first within each chain.
        open: Vec<&'a ThreadPullRequestLink>,
        /// Highest layer of the open set; the one "View PR" and copy-link target.
        top: &'a ThreadPullRequestLink,
    },
}

/// Prefer open work and the highest open layer within a chain. A completed chain still points at
/// its top; unrelated terminal links use the most recently updated request.
pub fn resolve_thread_current_pull_request(
    links: &[ThreadPullRequestLink],
) -> Option<ThreadCurrentPullRequest<'_>> {
    let visible = visible_thread_pull_requests(links);
    if visible.is_empty() {
        return None;
    }
    let open: Vec<&ThreadPullRequestLink> = visible
        .iter()
        .copied()
        .filter(|link| is_open(link))
        .collect();
    if let [only] = open.as_slice() {
        return Some(ThreadCurrentPullRequest::Single(only));
    }
    let chains = resolve_thread_pull_request_chains(links);
    if open.len() > 1 {
        let mut open_chains: Vec<Vec<&ThreadPullRequestLink>> = chains
            .iter()
            .map(|chain| {
                chain
                    .layers
                    .iter()
                    .rev()
                    .copied()
                    .filter(|link| is_open(link))
                    .collect::<Vec<_>>()
            })
            .filter(|layers| !layers.is_empty())
            .collect();
        let newest_linked_at = |layers: &[&ThreadPullRequestLink]| {
            layers
                .iter()
                .map(|link| parse_millis(&link.linked_at))
                .max()
                .unwrap_or(0)
        };
        open_chains.sort_by_key(|layers| std::cmp::Reverse(newest_linked_at(layers)));
        let ordered: Vec<&ThreadPullRequestLink> = open_chains.into_iter().flatten().collect();
        let top = *ordered.first()?;
        return Some(ThreadCurrentPullRequest::Stack { open: ordered, top });
    }
    if let [only] = chains.as_slice() {
        return only
            .layers
            .last()
            .map(|link| ThreadCurrentPullRequest::Single(link));
    }
    let mut terminal = visible;
    terminal.sort_by_key(|link| std::cmp::Reverse(latest_updated_at(link)));
    terminal
        .first()
        .map(|link| ThreadCurrentPullRequest::Single(link))
}

/// The one link a legacy `linkedPullRequest` consumer should see, or None.
pub fn resolve_thread_current_pull_request_link(
    links: &[ThreadPullRequestLink],
) -> Option<&ThreadPullRequestLink> {
    match resolve_thread_current_pull_request(links)? {
        ThreadCurrentPullRequest::Single(link) => Some(link),
        ThreadCurrentPullRequest::Stack { top, .. } => Some(top),
    }
}

/// The legacy projection's host for a repository identity. A local-path remote has no host
/// segment and no provider, so there is no host to match.
fn repository_host_of(identity: &RepositoryIdentity) -> Option<String> {
    let segment = identity
        .canonical_key
        .as_str()
        .split('/')
        .next()
        .map(js_trim)
        .unwrap_or("");
    if !segment.is_empty() {
        return Some(segment.to_lowercase());
    }
    identity
        .provider
        .as_ref()
        .map(|provider| provider.as_str().to_owned())
}

/// Legacy clients can only route links belonging to the thread's own repository.
pub fn legacy_linked_pull_request_of(
    links: &[ThreadPullRequestLink],
    project_id: &ProjectId,
    identity: Option<&RepositoryIdentity>,
) -> Option<ThreadLinkedPullRequest> {
    let identity = identity?;
    let host = repository_host_of(identity)?;
    let repository =
        source_control_repository_selector(Some(RepositorySelectorIdentity::from(identity)))?;
    let matching: Vec<ThreadPullRequestLink> = links
        .iter()
        .filter(|link| {
            link.host.as_str().to_lowercase() == host.to_lowercase()
                && link.repository.as_str().to_lowercase() == repository.to_lowercase()
        })
        .cloned()
        .collect();
    let link = resolve_thread_current_pull_request_link(&matching)?;
    Some(ThreadLinkedPullRequest {
        project_id: project_id.clone(),
        repository: link.repository.clone(),
        number: link.number,
        url: link.url.clone(),
    })
}

/// A thread's links grouped into one stack or one unrelated pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestChainKind {
    Native,
    Derived,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadPullRequestChain<'a> {
    pub kind: ThreadPullRequestChainKind,
    /// Bottom to top.
    pub layers: Vec<&'a ThreadPullRequestLink>,
}

/// Groups a thread's links into stacks. Native stacks come from the host and win; the rest are
/// chained by matching one link's base branch to another's head branch within the same
/// repository. A link that chains to nothing is a one-layer chain.
pub fn resolve_thread_pull_request_chains(
    links: &[ThreadPullRequestLink],
) -> Vec<ThreadPullRequestChain<'_>> {
    let visible = visible_thread_pull_requests(links);
    let mut chains = Vec::new();
    let mut placed: HashSet<PullRequestKey> = HashSet::new();

    let mut native_stacks: IndexMap<String, Vec<&ThreadPullRequestLink>> = IndexMap::new();
    for link in &visible {
        let Some(stack) = link.stack.as_ref() else {
            continue;
        };
        let key = thread_pull_request_key_of(PullRequestRef::from(*link));
        let stack_key = format!("{}/{}#stack:{}", key.host, key.repository, stack.id);
        native_stacks.entry(stack_key).or_default().push(link);
    }
    for mut members in native_stacks.into_values() {
        let Some(stack) = members[0].stack.as_ref() else {
            continue;
        };
        let order: HashMap<u64, usize> = stack
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| (layer.number.get(), index))
            .collect();
        members.sort_by_key(|link| order.get(&link.number.get()).copied().unwrap_or(0));
        for member in &members {
            placed.insert(thread_pull_request_key_of(PullRequestRef::from(*member)));
        }
        chains.push(ThreadPullRequestChain {
            kind: ThreadPullRequestChainKind::Native,
            layers: members,
        });
    }

    let remaining: Vec<&ThreadPullRequestLink> = visible
        .iter()
        .copied()
        .filter(|link| !placed.contains(&thread_pull_request_key_of(PullRequestRef::from(*link))))
        .collect();
    let branch_key = |link: &ThreadPullRequestLink, branch: &str| {
        let key = thread_pull_request_key_of(PullRequestRef::from(link));
        format!("{}/{}:{}", key.host, key.repository, branch)
    };
    // Reused head names cannot identify a parent unambiguously. `None` marks such a branch.
    let mut by_head: HashMap<String, Option<usize>> = HashMap::new();
    for (index, link) in remaining.iter().enumerate() {
        let Some(snapshot) = link.snapshot.as_ref() else {
            continue;
        };
        let key = branch_key(link, snapshot.head_branch.as_str());
        let entry = if by_head.contains_key(&key) {
            None
        } else {
            Some(index)
        };
        by_head.insert(key, entry);
    }
    let parent_of = |index: usize| -> Option<usize> {
        let link = remaining[index];
        let snapshot = link.snapshot.as_ref()?;
        by_head
            .get(&branch_key(link, snapshot.base_branch.as_str()))
            .copied()
            .flatten()
    };
    let mut has_child: HashSet<usize> = HashSet::new();
    for index in 0..remaining.len() {
        if let Some(parent) = parent_of(index).filter(|parent| *parent != index) {
            has_child.insert(parent);
        }
    }
    // Walk from each top (a link nothing builds on) down its base chain.
    for top in 0..remaining.len() {
        if has_child.contains(&top) {
            continue;
        }
        let mut layers: Vec<&ThreadPullRequestLink> = Vec::new();
        let mut cursor = Some(top);
        while let Some(index) = cursor {
            let key = thread_pull_request_key_of(PullRequestRef::from(remaining[index]));
            if placed.contains(&key) {
                break;
            }
            placed.insert(key);
            layers.insert(0, remaining[index]);
            cursor = parent_of(index);
        }
        if !layers.is_empty() {
            chains.push(ThreadPullRequestChain {
                kind: ThreadPullRequestChainKind::Derived,
                layers,
            });
        }
    }
    // Cycles have no top. Keep those links visible without inventing a stack order.
    for link in &remaining {
        if !placed.contains(&thread_pull_request_key_of(PullRequestRef::from(*link))) {
            chains.push(ThreadPullRequestChain {
                kind: ThreadPullRequestChainKind::Derived,
                layers: vec![link],
            });
        }
    }
    chains
}

/// Aggregate state of a thread's pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestBadgeState {
    Open,
    Closed,
    Merged,
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestBadge {
    Stack {
        layers: usize,
        state: ThreadPullRequestBadgeState,
    },
    PullRequest {
        others: usize,
        state: ThreadPullRequestBadgeState,
    },
}

/// Aggregate visible links' state for both stacks and unrelated linked counts.
pub fn resolve_thread_pull_request_badge(
    pull_requests: &[ThreadPullRequestLink],
) -> Option<ThreadPullRequestBadge> {
    let visible = visible_thread_pull_requests(pull_requests);
    if visible.is_empty() {
        return None;
    }
    let state_of = |link: &ThreadPullRequestLink| {
        link.snapshot
            .as_ref()
            .map_or(PullRequestState::Open, |snapshot| snapshot.state)
    };
    let all_open_drafts = visible.iter().all(|link| {
        link.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.state == PullRequestState::Open && snapshot.is_draft)
    });
    let states: Vec<PullRequestState> = visible.iter().map(|link| state_of(link)).collect();
    let state = if all_open_drafts {
        ThreadPullRequestBadgeState::Draft
    } else if states.contains(&PullRequestState::Open) {
        ThreadPullRequestBadgeState::Open
    } else if states
        .iter()
        .all(|state| *state == PullRequestState::Merged)
    {
        ThreadPullRequestBadgeState::Merged
    } else {
        ThreadPullRequestBadgeState::Closed
    };
    let chains = resolve_thread_pull_request_chains(pull_requests);
    if visible.len() > 1 && chains.len() == 1 {
        return Some(ThreadPullRequestBadge::Stack {
            layers: visible.len(),
            state,
        });
    }
    Some(ThreadPullRequestBadge::PullRequest {
        others: visible.len() - 1,
        state,
    })
}

/// Search terms for visible PR links, the legacy projection, and the branch PR.
pub fn thread_pull_request_search_terms(
    pull_requests: Option<&[ThreadPullRequestLink]>,
    linked_pull_request: Option<&ThreadLinkedPullRequest>,
    branch_pull_request: Option<&ThreadLinkedPullRequest>,
) -> Vec<String> {
    let mut terms: Vec<String> = branch_pull_request
        .map(|branch| {
            vec![
                format!("#{}", branch.number.get()),
                format!("{}#{}", branch.repository, branch.number.get()),
                branch.url.as_str().to_owned(),
            ]
        })
        .unwrap_or_default();
    if let Some(pull_requests) = pull_requests.filter(|links| !links.is_empty()) {
        for link in visible_thread_pull_requests(pull_requests) {
            terms.extend([
                format!("#{}", link.number.get()),
                format!("{}#{}", link.repository, link.number.get()),
                link.url.as_str().to_owned(),
                link.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.title.as_str().to_owned())
                    .unwrap_or_default(),
            ]);
        }
        return terms;
    }
    if let Some(legacy) = linked_pull_request {
        terms.extend([
            format!("#{}", legacy.number.get()),
            format!("{}#{}", legacy.repository, legacy.number.get()),
            legacy.url.as_str().to_owned(),
        ]);
    }
    terms
}

/// Older V2 event payloads stored one link; an explicit empty list means it was unlinked.
pub fn thread_pull_requests_of(
    pull_requests: Option<&[ThreadPullRequestLink]>,
    linked_pull_request: Option<&ThreadLinkedPullRequest>,
) -> Vec<ThreadPullRequestLink> {
    if let Some(pull_requests) = pull_requests {
        return pull_requests.to_vec();
    }
    let Some(linked) = linked_pull_request else {
        return Vec::new();
    };
    let key = legacy_thread_pull_request_key(
        linked.repository.as_str(),
        linked.number.get(),
        linked.url.as_str(),
        None,
    );
    let (Some(host), Some(repository)) = (
        TrimmedNonEmptyString::new(&key.host).ok(),
        TrimmedNonEmptyString::new(&key.repository).ok(),
    ) else {
        return Vec::new();
    };
    let (Some(number), Some(url)) = (
        PositiveInt::new(key.number),
        TrimmedNonEmptyString::new(linked.url.as_str()).ok(),
    ) else {
        return Vec::new();
    };
    vec![ThreadPullRequestLink {
        host,
        repository,
        number,
        url,
        source: ThreadPullRequestLinkSource::Manual,
        linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
        snapshot: None,
        stack: None,
        watch: None,
    }]
}

/// Apply a single-link sync without copying unrelated PRs into the event history. `current` is
/// the thread's links as [`thread_pull_requests_of`] reads them.
pub fn apply_thread_pull_request_link_update(
    current: &[ThreadPullRequestLink],
    updated: &ThreadPullRequestLink,
) -> Vec<ThreadPullRequestLink> {
    current
        .iter()
        .map(|link| {
            if thread_pull_request_keys_equal(
                PullRequestRef::from(link),
                PullRequestRef::from(updated),
            ) {
                updated.clone()
            } else {
                link.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use PullRequestState::{Closed, Merged, Open};
    use turin_contracts::base_schemas::TrimmedNonEmptyString as Tne;
    use turin_contracts::environment::{GitRemoteSource, RepositoryIdentityLocator};
    use turin_contracts::thread_pull_request::{
        ThreadNativeStackKind, ThreadPullRequestSnapshot, ThreadPullRequestStack,
        ThreadPullRequestStackLayer,
    };

    const CREATED: &str = "2026-01-01T00:00:00.000Z";

    fn snapshot(state: PullRequestState, head: &str, base: &str) -> ThreadPullRequestSnapshot {
        snapshot_at(state, false, head, base, CREATED)
    }

    fn snapshot_at(
        state: PullRequestState,
        is_draft: bool,
        head: &str,
        base: &str,
        updated_at: &str,
    ) -> ThreadPullRequestSnapshot {
        ThreadPullRequestSnapshot {
            state,
            title: Tne::new("Change").unwrap(),
            head_branch: Tne::new(head).unwrap(),
            base_branch: Tne::new(base).unwrap(),
            is_draft,
            updated_at: Some(updated_at.to_owned()),
            synced_at: CREATED.to_owned(),
            closed_at: None,
            merged_at: None,
            author: None,
            additions: None,
            deletions: None,
            changed_files: None,
            review_decision: None,
            checks_state: None,
            mergeability: None,
        }
    }

    fn link(number: u64) -> ThreadPullRequestLink {
        ThreadPullRequestLink {
            host: Tne::new("github.com").unwrap(),
            repository: Tne::new("pingdotgg/t3code").unwrap(),
            number: PositiveInt::new(number).unwrap(),
            url: Tne::new(format!("https://github.com/pingdotgg/t3code/pull/{number}")).unwrap(),
            source: ThreadPullRequestLinkSource::Manual,
            linked_at: format!("2026-01-01T00:00:{number:02}.000Z"),
            snapshot: None,
            stack: None,
            watch: None,
        }
    }

    fn synced(
        mut link: ThreadPullRequestLink,
        snapshot: ThreadPullRequestSnapshot,
    ) -> ThreadPullRequestLink {
        link.snapshot = Some(snapshot);
        link
    }

    fn in_stack(
        mut link: ThreadPullRequestLink,
        layers: &[(u64, PullRequestState)],
    ) -> ThreadPullRequestLink {
        link.stack = Some(ThreadPullRequestStack {
            kind: ThreadNativeStackKind,
            id: Tne::new("s1").unwrap(),
            number: PositiveInt::new(1).unwrap(),
            url: Tne::new("https://github.com/pingdotgg/t3code/stacks/1").unwrap(),
            base: Tne::new("main").unwrap(),
            layers: layers
                .iter()
                .map(|(number, state)| ThreadPullRequestStackLayer {
                    number: PositiveInt::new(*number).unwrap(),
                    head_branch: Tne::new(format!("b{number}")).unwrap(),
                    state: *state,
                })
                .collect(),
        });
        link
    }

    fn dismissed(mut link: ThreadPullRequestLink) -> ThreadPullRequestLink {
        link.source = ThreadPullRequestLinkSource::StackDismissed;
        link
    }

    /// Numbers of a current selection: `(open, top)` for a stack, `(vec![n], n)` for a single.
    #[derive(Debug, PartialEq)]
    enum Current {
        Single(u64),
        Stack { open: Vec<u64>, top: u64 },
    }

    fn current(links: &[ThreadPullRequestLink]) -> Option<Current> {
        resolve_thread_current_pull_request(links).map(|selection| match selection {
            ThreadCurrentPullRequest::Single(link) => Current::Single(link.number.get()),
            ThreadCurrentPullRequest::Stack { open, top } => Current::Stack {
                open: open.iter().map(|link| link.number.get()).collect(),
                top: top.number.get(),
            },
        })
    }

    fn chain_numbers(
        links: &[ThreadPullRequestLink],
    ) -> Vec<(ThreadPullRequestChainKind, Vec<u64>)> {
        resolve_thread_pull_request_chains(links)
            .iter()
            .map(|chain| {
                (
                    chain.kind,
                    chain.layers.iter().map(|link| link.number.get()).collect(),
                )
            })
            .collect()
    }

    fn identity(
        canonical_key: &str,
        provider: Option<&str>,
        display_name: Option<&str>,
    ) -> RepositoryIdentity {
        RepositoryIdentity {
            canonical_key: Tne::new(canonical_key).unwrap(),
            locator: RepositoryIdentityLocator {
                source: GitRemoteSource,
                remote_name: Tne::new("origin").unwrap(),
                remote_url: Tne::new("https://github.com/pingdotgg/t3code.git").unwrap(),
            },
            web_url: None,
            root_path: None,
            display_name: display_name.map(|name| Tne::new(name).unwrap()),
            provider: provider.map(|name| Tne::new(name).unwrap()),
            owner: None,
            name: None,
            origin: None,
        }
    }

    fn legacy(number: u64) -> ThreadLinkedPullRequest {
        ThreadLinkedPullRequest {
            project_id: ProjectId::new("project-1").unwrap(),
            repository: Tne::new("pingdotgg/t3code").unwrap(),
            number: PositiveInt::new(number).unwrap(),
            url: Tne::new(format!("https://github.com/pingdotgg/t3code/pull/{number}")).unwrap(),
        }
    }

    #[test]
    fn keys_ignore_host_and_repository_case() {
        assert!(thread_pull_request_keys_equal(
            PullRequestRef {
                host: "GitHub.com",
                repository: "PingDotGG/t3code",
                number: 1
            },
            PullRequestRef {
                host: "github.com",
                repository: "pingdotgg/t3code",
                number: 1
            },
        ));
        assert!(!thread_pull_request_keys_equal(
            PullRequestRef {
                host: "github.com",
                repository: "pingdotgg/t3code",
                number: 1
            },
            PullRequestRef {
                host: "gitlab.com",
                repository: "pingdotgg/t3code",
                number: 1
            },
        ));
    }

    #[test]
    fn current_is_none_without_visible_links() {
        assert_eq!(current(&[]), None);
        assert_eq!(current(&[dismissed(link(1))]), None);
    }

    #[test]
    fn current_treats_an_unsynced_link_as_open() {
        assert_eq!(current(&[link(1)]), Some(Current::Single(1)));
    }

    #[test]
    fn current_prefers_the_single_open_link_over_terminal_ones() {
        let links = [
            synced(link(1), snapshot(Merged, "a", "main")),
            synced(link(2), snapshot(Open, "b", "main")),
            synced(link(3), snapshot(Closed, "c", "main")),
        ];
        assert_eq!(current(&links), Some(Current::Single(2)));
    }

    #[test]
    fn current_reports_a_native_stack_with_the_highest_layer_on_top() {
        let stack = [(10, Open), (11, Open)];
        let links = [
            in_stack(synced(link(11), snapshot(Open, "b", "a")), &stack),
            in_stack(synced(link(10), snapshot(Open, "a", "main")), &stack),
        ];
        assert_eq!(
            current(&links),
            Some(Current::Stack {
                open: vec![11, 10],
                top: 11
            })
        );
    }

    #[test]
    fn current_orders_an_open_set_without_stack_data_by_most_recent_link() {
        assert_eq!(
            current(&[link(1), link(2)]),
            Some(Current::Stack {
                open: vec![2, 1],
                top: 2
            })
        );
    }

    #[test]
    fn current_falls_back_to_the_most_recently_updated_terminal_link() {
        let links = [
            synced(
                link(1),
                snapshot_at(Merged, false, "a", "main", "2026-01-03T00:00:00.000Z"),
            ),
            synced(
                link(2),
                snapshot_at(Closed, false, "b", "main", "2026-01-02T00:00:00.000Z"),
            ),
        ];
        assert_eq!(current(&links), Some(Current::Single(1)));
    }

    #[test]
    fn legacy_projects_only_links_the_owning_project_can_route() {
        let github = identity(
            "github.com/pingdotgg/t3code",
            Some("github"),
            Some("pingdotgg/t3code"),
        );
        let project = ProjectId::new("project-1").unwrap();
        let projected = legacy_linked_pull_request_of(&[link(7)], &project, Some(&github)).unwrap();
        assert_eq!(projected.number.get(), 7);
        assert_eq!(projected.repository.as_str(), "pingdotgg/t3code");
        assert_eq!(
            projected.url.as_str(),
            "https://github.com/pingdotgg/t3code/pull/7"
        );
        assert_eq!(
            legacy_linked_pull_request_of(&[], &project, Some(&github)),
            None
        );
    }

    #[test]
    fn legacy_omits_an_unsafe_route_but_keeps_a_routable_one() {
        let github = identity(
            "github.com/pingdotgg/t3code",
            Some("github"),
            Some("pingdotgg/t3code"),
        );
        let project = ProjectId::new("project-1").unwrap();
        let mut foreign_host = link(7);
        foreign_host.host = Tne::new("github.enterprise.test").unwrap();
        let mut foreign_repository = link(7);
        foreign_repository.repository = Tne::new("acme/other").unwrap();
        for foreign in [foreign_host, foreign_repository] {
            assert_eq!(
                legacy_linked_pull_request_of(
                    std::slice::from_ref(&foreign),
                    &project,
                    Some(&github)
                ),
                None
            );
            assert_eq!(
                legacy_linked_pull_request_of(&[foreign, link(8)], &project, Some(&github))
                    .map(|linked| linked.number.get()),
                Some(8)
            );
        }
    }

    #[test]
    fn legacy_does_not_guess_without_an_identity_or_for_a_local_path_remote() {
        let project = ProjectId::new("project-1").unwrap();
        assert_eq!(
            legacy_linked_pull_request_of(&[link(7)], &project, None),
            None
        );
        let local = identity("/tmp/r/remote", None, Some("remote"));
        assert_eq!(
            legacy_linked_pull_request_of(&[link(7)], &project, Some(&local)),
            None
        );
    }

    #[test]
    fn legacy_key_takes_the_fallback_host_or_the_url_host() {
        assert_eq!(
            legacy_thread_pull_request_key(
                "PingDotGG/T3code",
                7,
                "https://github.com/x/y/pull/7",
                None
            ),
            PullRequestKey {
                host: "github.com".to_owned(),
                repository: "pingdotgg/t3code".to_owned(),
                number: 7,
            }
        );
        assert_eq!(
            legacy_thread_pull_request_key("a/b", 1, "not a url", Some(" Forge.Example ")).host,
            "forge.example"
        );
        assert_eq!(
            legacy_thread_pull_request_key("a/b", 1, "not a url", None).host,
            "unknown"
        );
    }

    #[test]
    fn legacy_projection_round_trips_through_thread_pull_requests_of() {
        let linked = legacy(12);
        let links = thread_pull_requests_of(None, Some(&linked));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source, ThreadPullRequestLinkSource::Manual);
        assert_eq!(links[0].linked_at, "1970-01-01T00:00:00.000Z");
        assert_eq!(links[0].number.get(), 12);
        assert!(thread_pull_requests_of(Some(&[]), Some(&linked)).is_empty());
    }

    #[test]
    fn apply_link_update_replaces_only_the_matching_pull_request() {
        let current = [link(1), link(2)];
        let updated = synced(link(2), snapshot(Merged, "x", "main"));
        let applied = apply_thread_pull_request_link_update(&current, &updated);
        assert_eq!(applied[0].snapshot, None);
        assert_eq!(applied[1].snapshot, updated.snapshot);
    }

    #[test]
    fn chains_links_by_base_to_head_bottom_to_top() {
        let links = [
            synced(link(3), snapshot(Open, "c", "b")),
            synced(link(1), snapshot(Open, "a", "main")),
            synced(link(2), snapshot(Open, "b", "a")),
            synced(link(9), snapshot(Open, "solo", "main")),
        ];
        assert_eq!(
            chain_numbers(&links),
            vec![
                (ThreadPullRequestChainKind::Derived, vec![1, 2, 3]),
                (ThreadPullRequestChainKind::Derived, vec![9]),
            ]
        );
    }

    #[test]
    fn uses_the_native_stack_order_when_the_host_provides_one() {
        let stack = [(5, Merged), (6, Open)];
        let links = [
            in_stack(synced(link(6), snapshot(Open, "b", "main")), &stack),
            in_stack(synced(link(5), snapshot(Merged, "a", "main")), &stack),
            link(8),
        ];
        assert_eq!(
            chain_numbers(&links),
            vec![
                (ThreadPullRequestChainKind::Native, vec![5, 6]),
                (ThreadPullRequestChainKind::Derived, vec![8]),
            ]
        );
    }

    #[test]
    fn aggregates_stack_and_pull_request_badge_state() {
        let cases = [
            (Open, false, Open, false, ThreadPullRequestBadgeState::Open),
            (
                Closed,
                false,
                Closed,
                false,
                ThreadPullRequestBadgeState::Closed,
            ),
            (Open, true, Open, true, ThreadPullRequestBadgeState::Draft),
            (Open, false, Open, true, ThreadPullRequestBadgeState::Open),
            (Closed, false, Open, true, ThreadPullRequestBadgeState::Open),
            (
                Merged,
                false,
                Merged,
                false,
                ThreadPullRequestBadgeState::Merged,
            ),
            (
                Merged,
                false,
                Closed,
                true,
                ThreadPullRequestBadgeState::Closed,
            ),
        ];
        for (first_state, first_draft, second_state, second_draft, expected) in cases {
            for stacked in [false, true] {
                let links = [
                    synced(
                        link(1),
                        snapshot_at(first_state, first_draft, "base", "main", CREATED),
                    ),
                    synced(
                        link(2),
                        snapshot_at(
                            second_state,
                            second_draft,
                            "feature",
                            if stacked { "base" } else { "main" },
                            CREATED,
                        ),
                    ),
                    dismissed(link(3)),
                ];
                let expected_badge = if stacked {
                    ThreadPullRequestBadge::Stack {
                        layers: 2,
                        state: expected,
                    }
                } else {
                    ThreadPullRequestBadge::PullRequest {
                        others: 1,
                        state: expected,
                    }
                };
                assert_eq!(
                    resolve_thread_pull_request_badge(&links),
                    Some(expected_badge),
                    "{first_state:?}/{second_state:?} stacked={stacked}"
                );
            }
        }
    }

    #[test]
    fn targets_the_top_of_a_derived_chain_despite_a_later_bottom_update() {
        for state in [Open, Merged, Closed] {
            let bottom = synced(
                link(2),
                snapshot_at(state, false, "base", "main", "2026-02-01T00:00:00.000Z"),
            );
            let top = synced(link(1), snapshot(state, "top", "base"));
            let links = [bottom, top];
            let expected = if state == Open {
                Current::Stack {
                    open: vec![1, 2],
                    top: 1,
                }
            } else {
                Current::Single(1)
            };
            assert_eq!(current(&links), Some(expected), "{state:?}");
            assert_eq!(
                resolve_thread_pull_request_badge(&links),
                Some(ThreadPullRequestBadge::Stack {
                    layers: 2,
                    state: match state {
                        Open => ThreadPullRequestBadgeState::Open,
                        Merged => ThreadPullRequestBadgeState::Merged,
                        _ => ThreadPullRequestBadgeState::Closed,
                    },
                })
            );
        }
    }

    #[test]
    fn keeps_the_top_native_layer_after_completion_and_treats_an_unsynced_layer_as_open() {
        let stack = [(2, Merged), (1, Merged)];
        let bottom = in_stack(
            synced(
                link(2),
                snapshot_at(Merged, false, "base", "main", "2026-02-01T00:00:00.000Z"),
            ),
            &stack,
        );
        let top = in_stack(synced(link(1), snapshot(Merged, "top", "base")), &stack);
        let mut unsynced_top = top.clone();
        unsynced_top.snapshot = None;
        assert_eq!(current(&[bottom.clone(), top]), Some(Current::Single(1)));
        assert_eq!(
            resolve_thread_pull_request_badge(&[bottom, unsynced_top]),
            Some(ThreadPullRequestBadge::Stack {
                layers: 2,
                state: ThreadPullRequestBadgeState::Open,
            })
        );
    }

    #[test]
    fn uses_aggregate_state_and_separates_unrelated_work_from_a_stack() {
        let bottom = synced(link(1), snapshot(Merged, "base", "main"));
        let top = synced(link(2), snapshot(Closed, "feature", "base"));
        assert_eq!(
            resolve_thread_pull_request_badge(&[bottom.clone(), top.clone()]),
            Some(ThreadPullRequestBadge::Stack {
                layers: 2,
                state: ThreadPullRequestBadgeState::Closed,
            })
        );
        assert_eq!(
            resolve_thread_pull_request_badge(&[bottom.clone(), top.clone(), link(3)]),
            Some(ThreadPullRequestBadge::PullRequest {
                others: 2,
                state: ThreadPullRequestBadgeState::Open,
            })
        );
        assert_eq!(
            resolve_thread_pull_request_badge(&[dismissed(link(3))]),
            None
        );
    }

    #[test]
    fn keeps_branch_matching_case_sensitive_while_ignoring_repository_case() {
        let mut bottom = synced(link(1), snapshot(Open, "Base", "main"));
        bottom.repository = Tne::new("PingDotGG/T3code").unwrap();
        let top = synced(link(2), snapshot(Open, "top", "base"));
        assert_eq!(
            resolve_thread_pull_request_chains(&[bottom.clone(), top.clone()]).len(),
            2
        );
        let matched = synced(link(2), snapshot(Open, "top", "Base"));
        assert_eq!(
            chain_numbers(&[bottom, matched]),
            vec![(ThreadPullRequestChainKind::Derived, vec![1, 2])]
        );
    }

    #[test]
    fn preserves_cyclic_links_without_presenting_a_guessed_stack_order() {
        let links = [
            synced(link(1), snapshot(Open, "a", "b")),
            synced(link(2), snapshot(Open, "b", "a")),
        ];
        assert_eq!(
            chain_numbers(&links),
            vec![
                (ThreadPullRequestChainKind::Derived, vec![1]),
                (ThreadPullRequestChainKind::Derived, vec![2]),
            ]
        );
        assert_eq!(
            current(&links),
            Some(Current::Stack {
                open: vec![2, 1],
                top: 2
            })
        );
        assert_eq!(
            resolve_thread_pull_request_badge(&links),
            Some(ThreadPullRequestBadge::PullRequest {
                others: 1,
                state: ThreadPullRequestBadgeState::Open,
            })
        );
    }

    #[test]
    fn does_not_guess_a_parent_when_a_head_branch_was_reused() {
        let links = [
            synced(link(1), snapshot(Merged, "reused", "main")),
            synced(link(2), snapshot(Open, "reused", "main")),
            synced(link(3), snapshot(Open, "top", "reused")),
        ];
        assert_eq!(
            chain_numbers(&links),
            vec![
                (ThreadPullRequestChainKind::Derived, vec![1]),
                (ThreadPullRequestChainKind::Derived, vec![2]),
                (ThreadPullRequestChainKind::Derived, vec![3]),
            ]
        );
    }

    #[test]
    fn search_terms_include_the_branch_pull_request_alongside_existing_links() {
        let project = ProjectId::new("project").unwrap();
        let branch = ThreadLinkedPullRequest {
            project_id: project.clone(),
            repository: Tne::new("pingdotgg/t3code").unwrap(),
            number: PositiveInt::new(11809).unwrap(),
            url: Tne::new("https://github.com/pingdotgg/t3code/pull/11809").unwrap(),
        };
        let linked = ThreadLinkedPullRequest {
            number: PositiveInt::new(12).unwrap(),
            url: Tne::new("https://github.com/pingdotgg/t3code/pull/12").unwrap(),
            ..branch.clone()
        };
        let existing = [link(34)];
        for pull_requests in [None, Some(&[][..]), Some(&existing[..])] {
            let terms =
                thread_pull_request_search_terms(pull_requests, Some(&linked), Some(&branch));
            for query in [
                "11809",
                "#11809",
                "pingdotgg/t3code#11809",
                branch.url.as_str(),
            ] {
                assert!(
                    terms.iter().any(|term| term.contains(query)),
                    "{query} in {terms:?}"
                );
            }
            let expected = if pull_requests.is_some_and(|links| !links.is_empty()) {
                "#34"
            } else {
                "#12"
            };
            assert!(
                terms.iter().any(|term| term == expected),
                "{expected} in {terms:?}"
            );
        }
    }

    #[test]
    fn search_terms_include_completed_and_unsynced_links_but_exclude_dismissed_ones() {
        let links = [
            synced(link(12), snapshot_at(Merged, false, "a", "main", CREATED)),
            link(34),
            dismissed(link(56)),
        ];
        let terms = thread_pull_request_search_terms(Some(&links), None, None);
        for expected in [
            "#12",
            "pingdotgg/t3code#12",
            "https://github.com/pingdotgg/t3code/pull/12",
            "Change",
            "#34",
        ] {
            assert!(
                terms.iter().any(|term| term == expected),
                "{expected} in {terms:?}"
            );
        }
        assert!(!terms.join(" ").contains("56"));
    }

    #[test]
    fn search_falls_back_to_the_legacy_projection_when_links_are_empty() {
        let linked = legacy(12);
        let terms = thread_pull_request_search_terms(Some(&[]), Some(&linked), None);
        assert!(terms.iter().any(|term| term == "#12"));
        let existing = [link(34)];
        let terms = thread_pull_request_search_terms(Some(&existing), Some(&linked), None);
        assert!(!terms.iter().any(|term| term == "#12"));
    }
}
