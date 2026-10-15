//! Port of upstream/packages/shared/src/threadPullRequests.ts
//!
//! GitHub only: upstream's Azure DevOps selector recovery (`azureKey` in
//! `legacyLinkedPullRequestOf`, the `dev.azure.com` branch of `legacyThreadPullRequestKey`) is
//! cut with that host. The Forgejo `authority` hooks are kept; `authority` is always `None`
//! while Forgejo is cut. Chains, current-PR resolution and badges borrow the caller's links
//! instead of returning the same objects (`&'a ThreadPullRequestLink`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use reqwest::Url;
use turin_contracts::base_schemas::{PositiveInt, ProjectId, TrimmedNonEmptyString};
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_v2::OrchestrationV2AppThread;
use turin_contracts::pull_request::{RepositoryIdentityLike, pull_request_host_of};
use turin_contracts::source_control::SourceControlProviderKind;
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestKey, ThreadPullRequestLink,
    ThreadPullRequestLinkSource, ThreadPullRequestSnapshot,
};

use crate::shared::change_request_url::{ChangeRequestLink, parse_change_request_url};
use crate::shared::js::js_trim;
use crate::shared::source_control::{canonical_repository_key, source_control_repository_selector};

/// `ThreadPullRequestKey & { authority?, url? }`: the identity of a link plus the optional extras
/// that let old stored links recover a Forgejo HTTP port from their URL.
#[derive(Debug, Clone, Copy)]
pub struct ThreadPullRequestKeySource<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub number: u64,
    pub authority: Option<&'a str>,
    pub url: Option<&'a str>,
}

impl<'a> From<&'a ThreadPullRequestLink> for ThreadPullRequestKeySource<'a> {
    fn from(link: &'a ThreadPullRequestLink) -> Self {
        Self {
            host: link.host.as_str(),
            repository: link.repository.as_str(),
            number: link.number.get(),
            authority: None,
            url: Some(link.url.as_str()),
        }
    }
}

impl<'a> From<&'a ThreadPullRequestKey> for ThreadPullRequestKeySource<'a> {
    fn from(key: &'a ThreadPullRequestKey) -> Self {
        Self {
            host: key.host.as_str(),
            repository: key.repository.as_str(),
            number: key.number.get(),
            authority: None,
            url: None,
        }
    }
}

impl<'a> From<&'a ChangeRequestLink> for ThreadPullRequestKeySource<'a> {
    fn from(link: &'a ChangeRequestLink) -> Self {
        Self {
            host: link.host.as_str(),
            repository: link.repository.as_str(),
            number: link.number,
            authority: link.authority.as_deref(),
            url: None,
        }
    }
}

/// The parts of a [`ThreadLinkedPullRequest`] (or a [`ThreadPullRequestLink`]) that
/// [`legacy_thread_pull_request_key`] reads: `Pick<ThreadLinkedPullRequest, "repository" | "number" | "url">`.
#[derive(Debug, Clone, Copy)]
pub struct LinkedPullRequestRef<'a> {
    pub repository: &'a str,
    pub number: u64,
    pub url: &'a str,
}

impl<'a> From<&'a ThreadLinkedPullRequest> for LinkedPullRequestRef<'a> {
    fn from(linked: &'a ThreadLinkedPullRequest) -> Self {
        Self {
            repository: linked.repository.as_str(),
            number: linked.number.get(),
            url: linked.url.as_str(),
        }
    }
}

impl<'a> From<&'a ThreadPullRequestLink> for LinkedPullRequestRef<'a> {
    fn from(link: &'a ThreadPullRequestLink) -> Self {
        Self {
            repository: link.repository.as_str(),
            number: link.number.get(),
            url: link.url.as_str(),
        }
    }
}

fn lower_trimmed(value: &str) -> String {
    js_trim(value).to_lowercase()
}

fn positive(number: u64) -> PositiveInt {
    PositiveInt::new(number).expect("thread pull request numbers are positive")
}

fn trimmed_non_empty(value: &str) -> TrimmedNonEmptyString {
    // Keys are built from already-validated non-blank parts; "unknown" is the same stand-in
    // `legacyThreadPullRequestKey` uses when a host cannot be read.
    TrimmedNonEmptyString::new(value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("non-blank literal"))
}

/// Lower-cased `(host, repository)` of a key, after Azure spelling canonicalisation.
fn normalized_parts(key: &ThreadPullRequestKeySource<'_>) -> (String, String) {
    let parsed = key.url.and_then(parse_change_request_url);
    let authority: Option<String> = key.authority.map(str::to_owned).or_else(|| {
        parsed.as_ref().and_then(|parsed| {
            (parsed.repository == lower_trimmed(key.repository) && parsed.number == key.number)
                .then(|| parsed.authority.clone())
                .flatten()
        })
    });
    let canonical = canonical_repository_key(&format!(
        "{}/{}",
        lower_trimmed(authority.as_deref().unwrap_or(key.host)),
        lower_trimmed(key.repository)
    ));
    match canonical.split_once('/') {
        Some((host, repository)) => (host.to_owned(), repository.to_owned()),
        None => (canonical, String::new()),
    }
}

/// Normalize stored links, recovering Forgejo HTTP ports from old links' URLs.
pub fn normalize_thread_pull_request_key<'a>(
    key: impl Into<ThreadPullRequestKeySource<'a>>,
) -> ThreadPullRequestKey {
    let key = key.into();
    let (host, repository) = normalized_parts(&key);
    ThreadPullRequestKey {
        host: trimmed_non_empty(&host),
        repository: trimmed_non_empty(&repository),
        number: positive(key.number),
    }
}

/// Legacy Azure selectors omit the organization and project; recover those from the PR URL.
/// (Only the Forgejo `authority` recovery remains while Azure DevOps is cut.)
pub fn legacy_thread_pull_request_key<'a>(
    linked: impl Into<LinkedPullRequestRef<'a>>,
    fallback_host: Option<&str>,
) -> ThreadPullRequestKey {
    let linked = linked.into();
    if let Some(parsed) = parse_change_request_url(linked.url)
        && parsed.number == linked.number
    {
        let canonical = canonical_repository_key(&format!("{}/{}", parsed.host, parsed.repository));
        if parsed.authority.is_some() || canonical.starts_with("dev.azure.com/") {
            return normalize_thread_pull_request_key(&parsed);
        }
    }
    let host = match fallback_host {
        Some(host) => host.to_owned(),
        None => Url::parse(linked.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    let host = lower_trimmed(&host);
    ThreadPullRequestKey {
        host: trimmed_non_empty(if host.is_empty() { "unknown" } else { &host }),
        repository: trimmed_non_empty(&lower_trimmed(linked.repository)),
        number: positive(linked.number),
    }
}

/// Identity comparison for links: host-level, case-insensitive on host and repository.
pub fn thread_pull_request_keys_equal<'a, 'b>(
    left: impl Into<ThreadPullRequestKeySource<'a>>,
    right: impl Into<ThreadPullRequestKeySource<'b>>,
) -> bool {
    thread_pull_request_key_of(left) == thread_pull_request_key_of(right)
}

pub fn thread_pull_request_key_of<'a>(key: impl Into<ThreadPullRequestKeySource<'a>>) -> String {
    let key = key.into();
    let (host, repository) = normalized_parts(&key);
    format!("{host}/{repository}#{}", key.number)
}

/// Links a user should see. Tombstoned stack members stay in the array only so the
/// sync reactor does not re-add them.
pub fn visible_thread_pull_requests(
    links: &[ThreadPullRequestLink],
) -> Vec<&ThreadPullRequestLink> {
    links
        .iter()
        .filter(|link| link.source != ThreadPullRequestLinkSource::StackDismissed)
        .collect()
}

fn is_open(link: &ThreadPullRequestLink) -> bool {
    // Unsynced links are treated as open: they were just linked, and hiding them
    // behind a terminal PR until the first sync would make the link look lost.
    link.snapshot.as_ref().is_none_or(|snapshot| {
        snapshot.state == turin_contracts::pull_request::PullRequestState::Open
    })
}

/// `Date.parse` for the ISO timestamps links carry; `None` is JS's `NaN`.
fn parse_date_ms(value: &str) -> Option<i64> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(parsed.timestamp_millis());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().timestamp_millis())
}

fn latest_updated_at(link: &ThreadPullRequestLink) -> i64 {
    let value = link
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.updated_at.as_deref())
        .unwrap_or(link.linked_at.as_str());
    parse_date_ms(value).unwrap_or(0)
}

/// The single pull request a one-slot surface (sidebar badge, tab icon, copy link) shows.
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadCurrentPullRequest<'a> {
    Single {
        link: &'a ThreadPullRequestLink,
    },
    Stack {
        open: Vec<&'a ThreadPullRequestLink>,
        /// Highest layer of the open set; the one "View PR" and copy-link target.
        top: &'a ThreadPullRequestLink,
    },
}

/// Prefer open work and the highest open layer within a chain. A completed chain still
/// points at its top; unrelated terminal links use the most recently updated request.
pub fn resolve_thread_current_pull_request(
    links: &[ThreadPullRequestLink],
) -> Option<ThreadCurrentPullRequest<'_>> {
    let visible = visible_thread_pull_requests(links);
    if visible.is_empty() {
        return None;
    }
    let open: Vec<&ThreadPullRequestLink> =
        visible.iter().copied().filter(|l| is_open(l)).collect();
    if open.len() == 1 {
        return Some(ThreadCurrentPullRequest::Single { link: open[0] });
    }
    let chains = chains_of(&visible);
    if open.len() > 1 {
        // Greatest `linkedAt` per chain; any unparseable one makes the whole max `NaN`, and a
        // `NaN` comparator result sorts as "equal".
        let max_linked_at = |layers: &[&ThreadPullRequestLink]| -> Option<i64> {
            let mut max: Option<i64> = None;
            for link in layers {
                let ms = parse_date_ms(link.linked_at.as_str())?;
                max = Some(max.map_or(ms, |m| m.max(ms)));
            }
            max
        };
        let mut open_chains: Vec<Vec<&ThreadPullRequestLink>> = chains
            .iter()
            .map(|chain| {
                chain
                    .layers
                    .iter()
                    .rev()
                    .copied()
                    .filter(|l| is_open(l))
                    .collect::<Vec<_>>()
            })
            .filter(|layers| !layers.is_empty())
            .collect();
        open_chains.sort_by(
            |left, right| match (max_linked_at(right), max_linked_at(left)) {
                (Some(r), Some(l)) => r.cmp(&l),
                _ => Ordering::Equal,
            },
        );
        let ordered: Vec<&ThreadPullRequestLink> = open_chains.into_iter().flatten().collect();
        let top = ordered[0];
        return Some(ThreadCurrentPullRequest::Stack { open: ordered, top });
    }
    if chains.len() == 1 {
        let link = *chains[0].layers.last().expect("chains are never empty");
        return Some(ThreadCurrentPullRequest::Single { link });
    }
    let mut terminal = visible;
    terminal.sort_by_key(|link| std::cmp::Reverse(latest_updated_at(link)));
    Some(ThreadCurrentPullRequest::Single { link: terminal[0] })
}

/// The one link a legacy `linkedPullRequest` consumer should see, or `None`.
pub fn resolve_thread_current_pull_request_link(
    links: &[ThreadPullRequestLink],
) -> Option<&ThreadPullRequestLink> {
    match resolve_thread_current_pull_request(links)? {
        ThreadCurrentPullRequest::Single { link } => Some(link),
        ThreadCurrentPullRequest::Stack { top, .. } => Some(top),
    }
}

/// `new URL(x).host`: lower-cased host plus any non-default port.
fn url_host_with_port(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// `pullRequestHostOf(identity, identity.provider as SourceControlProviderKind)` for an identity
/// whose provider may be missing: `None` where upstream's `undefined` kind stands in for a host
/// that cannot be read (a local-path remote has no host segment and no provider).
pub fn pull_request_host_of_identity(identity: &RepositoryIdentity) -> Option<String> {
    let like = RepositoryIdentityLike {
        canonical_key: Some(identity.canonical_key.as_str()),
    };
    match identity.provider.as_ref() {
        Some(provider) => Some(pull_request_host_of(
            Some(&like),
            &SourceControlProviderKind::new(provider.as_str()).ok()?,
        )),
        None => {
            let first = identity
                .canonical_key
                .as_str()
                .split('/')
                .next()
                .map(js_trim)?;
            (!first.is_empty()).then(|| first.to_lowercase())
        }
    }
}

/// Legacy clients can only route links belonging to the thread's own repository.
pub fn legacy_linked_pull_request_of(
    links: &[ThreadPullRequestLink],
    project_id: &ProjectId,
    identity: Option<&RepositoryIdentity>,
) -> Option<ThreadLinkedPullRequest> {
    let identity = identity?;
    // A local-path remote has no host segment and no provider, so there is no host to match.
    let host = pull_request_host_of_identity(identity)?;
    let repository = source_control_repository_selector(Some(identity))?;
    let repository_lower = repository.to_lowercase();
    let matching: Vec<ThreadPullRequestLink> = links
        .iter()
        .filter(|link| {
            let parsed = parse_change_request_url(link.url.as_str());
            if let Some(authority) = parsed
                .as_ref()
                .and_then(|parsed| parsed.authority.as_deref())
            {
                let parsed = parsed.as_ref().expect("authority implies a parse");
                // SSH web ports are resolved by the provider's configured login.
                if let Ok(remote) = Url::parse(identity.locator.remote_url.as_str())
                    && matches!(remote.scheme(), "http" | "https")
                {
                    return Some(authority) == url_host_with_port(&remote).as_deref()
                        && parsed.repository == repository_lower;
                }
                return parsed.host == host && parsed.repository == repository_lower;
            }
            link.host.as_str().to_lowercase() == host.to_lowercase()
                && link.repository.as_str().to_lowercase() == repository_lower
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

/// Groups a thread's links into stacks. Native stacks come from the host and win; the rest
/// are chained by matching one link's base branch to another's head branch within the same
/// repository. A link that chains to nothing is a one-layer chain.
pub fn resolve_thread_pull_request_chains(
    links: &[ThreadPullRequestLink],
) -> Vec<ThreadPullRequestChain<'_>> {
    chains_of(&visible_thread_pull_requests(links))
}

fn chains_of<'a>(visible: &[&'a ThreadPullRequestLink]) -> Vec<ThreadPullRequestChain<'a>> {
    let mut chains: Vec<ThreadPullRequestChain<'a>> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();

    let mut native_stacks: IndexMap<String, Vec<&'a ThreadPullRequestLink>> = IndexMap::new();
    for &link in visible {
        let Some(stack) = link.stack.as_ref() else {
            continue;
        };
        let (host, repository) = normalized_parts(&ThreadPullRequestKeySource::from(link));
        let stack_key = format!("{host}/{repository}#stack:{}", stack.id.as_str());
        native_stacks.entry(stack_key).or_default().push(link);
    }
    for (_, mut members) in native_stacks {
        // `new Map(layers.map((layer, index) => [layer.number, index]))`: a repeated number
        // keeps its last index.
        let mut order: HashMap<u64, usize> = HashMap::new();
        if let Some(stack) = members[0].stack.as_ref() {
            for (index, layer) in stack.layers.iter().enumerate() {
                order.insert(layer.number.get(), index);
            }
        }
        members.sort_by_key(|member| order.get(&member.number.get()).copied().unwrap_or(0));
        for member in &members {
            placed.insert(thread_pull_request_key_of(*member));
        }
        chains.push(ThreadPullRequestChain {
            kind: ThreadPullRequestChainKind::Native,
            layers: members,
        });
    }

    let remaining: Vec<&'a ThreadPullRequestLink> = visible
        .iter()
        .copied()
        .filter(|link| !placed.contains(&thread_pull_request_key_of(*link)))
        .collect();
    let branch_key = |link: &ThreadPullRequestLink, branch: &str| {
        let (host, repository) = normalized_parts(&ThreadPullRequestKeySource::from(link));
        format!("{host}/{repository}:{branch}")
    };
    // Reused head names cannot identify a parent unambiguously.
    let mut by_head: HashMap<String, Option<&'a ThreadPullRequestLink>> = HashMap::new();
    for &link in &remaining {
        let Some(snapshot) = link.snapshot.as_ref() else {
            continue;
        };
        let key = branch_key(link, snapshot.head_branch.as_str());
        let value = if by_head.contains_key(&key) {
            None
        } else {
            Some(link)
        };
        by_head.insert(key, value);
    }
    let mut has_child: HashSet<String> = HashSet::new();
    for &link in &remaining {
        let Some(snapshot) = link.snapshot.as_ref() else {
            continue;
        };
        let parent = by_head
            .get(&branch_key(link, snapshot.base_branch.as_str()))
            .copied()
            .flatten();
        if let Some(parent) = parent
            && !std::ptr::eq(parent, link)
        {
            has_child.insert(thread_pull_request_key_of(parent));
        }
    }
    // Walk from each top (a link nothing builds on) down its base chain.
    for &top in &remaining {
        if has_child.contains(&thread_pull_request_key_of(top)) {
            continue;
        }
        let mut layers: Vec<&'a ThreadPullRequestLink> = Vec::new();
        let mut cursor = Some(top);
        while let Some(current) = cursor {
            let key = thread_pull_request_key_of(current);
            if placed.contains(&key) {
                break;
            }
            placed.insert(key);
            layers.insert(0, current);
            cursor = current.snapshot.as_ref().and_then(|snapshot| {
                by_head
                    .get(&branch_key(current, snapshot.base_branch.as_str()))
                    .copied()
                    .flatten()
            });
        }
        if !layers.is_empty() {
            chains.push(ThreadPullRequestChain {
                kind: ThreadPullRequestChainKind::Derived,
                layers,
            });
        }
    }
    // Cycles have no top. Keep those links visible without inventing a stack order.
    for &link in &remaining {
        if !placed.contains(&thread_pull_request_key_of(link)) {
            chains.push(ThreadPullRequestChain {
                kind: ThreadPullRequestChainKind::Derived,
                layers: vec![link],
            });
        }
    }
    chains
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestBadgeState {
    Open,
    Closed,
    Merged,
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestBadgeKind {
    Stack { layers: usize },
    PullRequest { others: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadPullRequestBadge {
    pub state: ThreadPullRequestBadgeState,
    pub kind: ThreadPullRequestBadgeKind,
}

/// Aggregate visible links' state for both stacks and unrelated linked counts.
pub fn resolve_thread_pull_request_badge(
    pull_requests: Option<&[ThreadPullRequestLink]>,
) -> Option<ThreadPullRequestBadge> {
    use turin_contracts::pull_request::PullRequestState;
    let visible = visible_thread_pull_requests(pull_requests.unwrap_or(&[]));
    if visible.is_empty() {
        return None;
    }
    let states: Vec<PullRequestState> = visible
        .iter()
        .map(|link| {
            link.snapshot
                .as_ref()
                .map_or(PullRequestState::Open, |s| s.state)
        })
        .collect();
    let state = if visible.iter().all(|link| {
        link.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.state == PullRequestState::Open && snapshot.is_draft)
    }) {
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
    let chains = chains_of(&visible);
    let kind = if visible.len() > 1 && chains.len() == 1 {
        ThreadPullRequestBadgeKind::Stack {
            layers: visible.len(),
        }
    } else {
        ThreadPullRequestBadgeKind::PullRequest {
            others: visible.len() - 1,
        }
    };
    Some(ThreadPullRequestBadge { state, kind })
}

/// The thread fields [`thread_pull_request_search_terms`] reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadPullRequestSearchSource<'a> {
    pub pull_requests: Option<&'a [ThreadPullRequestLink]>,
    pub linked_pull_request: Option<&'a ThreadLinkedPullRequest>,
    pub branch_pull_request: Option<&'a ThreadLinkedPullRequest>,
}

/// Search terms for visible PR links, the legacy projection, and the branch PR.
pub fn thread_pull_request_search_terms(thread: ThreadPullRequestSearchSource<'_>) -> Vec<String> {
    let mut terms: Vec<String> = match thread.branch_pull_request {
        Some(branch) => vec![
            format!("#{}", branch.number),
            format!("{}#{}", branch.repository, branch.number),
            branch.url.to_string(),
        ],
        None => Vec::new(),
    };
    if let Some(pull_requests) = thread.pull_requests.filter(|links| !links.is_empty()) {
        for link in visible_thread_pull_requests(pull_requests) {
            terms.push(format!("#{}", link.number));
            terms.push(format!("{}#{}", link.repository, link.number));
            terms.push(link.url.to_string());
            terms.push(
                link.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.title.to_string())
                    .unwrap_or_default(),
            );
        }
        return terms;
    }
    if let Some(legacy) = thread.linked_pull_request {
        terms.push(format!("#{}", legacy.number));
        terms.push(format!("{}#{}", legacy.repository, legacy.number));
        terms.push(legacy.url.to_string());
    }
    terms
}

/// Older V2 event payloads stored one link; an explicit empty array means it was unlinked.
pub fn thread_pull_requests_of(
    pull_requests: Option<&[ThreadPullRequestLink]>,
    linked_pull_request: Option<&ThreadLinkedPullRequest>,
) -> Vec<ThreadPullRequestLink> {
    if let Some(pull_requests) = pull_requests {
        return pull_requests.to_vec();
    }
    match linked_pull_request {
        None => Vec::new(),
        Some(linked) => {
            let key = legacy_thread_pull_request_key(linked, None);
            vec![ThreadPullRequestLink {
                host: key.host,
                repository: key.repository,
                number: key.number,
                url: linked.url.clone(),
                source: ThreadPullRequestLinkSource::Manual,
                linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
                snapshot: None::<ThreadPullRequestSnapshot>,
                stack: None,
                watch: None,
            }]
        }
    }
}

/// Apply a single-link sync without copying unrelated PRs into the event history.
pub fn apply_thread_pull_request_link_update(
    thread: &OrchestrationV2AppThread,
    updated: &ThreadPullRequestLink,
) -> OrchestrationV2AppThread {
    let linked = thread
        .linked_pull_request
        .as_ref()
        .and_then(|linked| linked.as_ref());
    let current = thread_pull_requests_of(thread.pull_requests.as_deref(), linked);
    let mut next = thread.clone();
    next.pull_requests = Some(
        current
            .into_iter()
            .map(|link| {
                if thread_pull_request_keys_equal(&link, updated) {
                    updated.clone()
                } else {
                    link
                }
            })
            .collect(),
    );
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::pull_request::PullRequestState;
    use turin_contracts::thread_pull_request::{
        ThreadNativeStackKind, ThreadPullRequestStack, ThreadPullRequestStackLayer,
    };

    fn t(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn snapshot(edit: impl FnOnce(&mut ThreadPullRequestSnapshot)) -> ThreadPullRequestSnapshot {
        let mut snapshot = ThreadPullRequestSnapshot {
            state: PullRequestState::Open,
            title: t("Change"),
            head_branch: t("feature"),
            base_branch: t("main"),
            is_draft: false,
            updated_at: Some("2026-01-01T00:00:00.000Z".to_owned()),
            synced_at: "2026-01-01T00:00:00.000Z".to_owned(),
            closed_at: None,
            merged_at: None,
            author: None,
            additions: None,
            deletions: None,
            changed_files: None,
            review_decision: None,
            checks_state: None,
            mergeability: None,
        };
        edit(&mut snapshot);
        snapshot
    }

    fn snap(state: PullRequestState, head: &str, base: &str) -> ThreadPullRequestSnapshot {
        snapshot(|s| {
            s.state = state;
            s.head_branch = t(head);
            s.base_branch = t(base);
        })
    }

    fn open_snap(head: &str, base: &str) -> ThreadPullRequestSnapshot {
        snap(PullRequestState::Open, head, base)
    }

    fn link(number: u64) -> ThreadPullRequestLink {
        ThreadPullRequestLink {
            host: t("github.com"),
            repository: t("pingdotgg/t3code"),
            number: PositiveInt::new(number).unwrap(),
            url: t(&format!(
                "https://github.com/pingdotgg/t3code/pull/{number}"
            )),
            source: ThreadPullRequestLinkSource::Manual,
            linked_at: format!("2026-01-01T00:00:{number:02}.000Z"),
            snapshot: None,
            stack: None,
            watch: None,
        }
    }

    fn with(
        mut link: ThreadPullRequestLink,
        edit: impl FnOnce(&mut ThreadPullRequestLink),
    ) -> ThreadPullRequestLink {
        edit(&mut link);
        link
    }

    fn snapped(number: u64, snapshot: ThreadPullRequestSnapshot) -> ThreadPullRequestLink {
        with(link(number), |l| l.snapshot = Some(snapshot))
    }

    fn dismissed(number: u64) -> ThreadPullRequestLink {
        with(link(number), |l| {
            l.source = ThreadPullRequestLinkSource::StackDismissed
        })
    }

    fn numbers(links: &[&ThreadPullRequestLink]) -> Vec<u64> {
        links.iter().map(|l| l.number.get()).collect()
    }

    fn key(host: &str, repository: &str, number: u64) -> ThreadPullRequestKey {
        ThreadPullRequestKey {
            host: t(host),
            repository: t(repository),
            number: PositiveInt::new(number).unwrap(),
        }
    }

    fn native_stack(layers: &[(u64, &str, PullRequestState)]) -> ThreadPullRequestStack {
        ThreadPullRequestStack {
            kind: ThreadNativeStackKind,
            id: t("s1"),
            number: PositiveInt::new(1).unwrap(),
            url: t("https://github.com/pingdotgg/t3code/stacks/1"),
            base: t("main"),
            layers: layers
                .iter()
                .map(|(number, head, state)| ThreadPullRequestStackLayer {
                    number: PositiveInt::new(*number).unwrap(),
                    head_branch: t(head),
                    state: *state,
                })
                .collect(),
        }
    }

    // Upstream's "recovers Forgejo ports from old stored URLs" half of this test is dropped with
    // Forgejo; the case-insensitivity half is kept.
    #[test]
    fn keys_equal_ignores_host_and_repository_case() {
        assert!(thread_pull_request_keys_equal(
            &key("GitHub.com", "PingDotGG/t3code", 1),
            &key("github.com", "pingdotgg/t3code", 1)
        ));
        assert!(!thread_pull_request_keys_equal(
            &key("github.com", "pingdotgg/t3code", 1),
            &key("gitlab.com", "pingdotgg/t3code", 1)
        ));
    }

    #[test]
    fn normalizes_azure_spellings_in_keys() {
        assert!(thread_pull_request_keys_equal(
            &key("dev.azure.com", "org-a/project/_git/web", 7),
            &key(
                "org-a.visualstudio.com",
                "DefaultCollection/project/_git/web",
                7
            )
        ));
    }

    #[test]
    fn current_is_none_with_no_visible_links() {
        assert_eq!(resolve_thread_current_pull_request(&[]), None);
        assert_eq!(resolve_thread_current_pull_request(&[dismissed(1)]), None);
    }

    #[test]
    fn current_treats_an_unsynced_link_as_open() {
        let links = [link(1)];
        let current = resolve_thread_current_pull_request(&links).unwrap();
        assert!(
            matches!(current, ThreadCurrentPullRequest::Single { link } if link.number.get() == 1)
        );
    }

    #[test]
    fn current_prefers_the_single_open_link_over_terminal_ones() {
        let links = [
            snapped(1, snap(PullRequestState::Merged, "feature", "main")),
            snapped(2, open_snap("feature", "main")),
            snapped(3, snap(PullRequestState::Closed, "feature", "main")),
        ];
        let current = resolve_thread_current_pull_request(&links).unwrap();
        assert!(
            matches!(current, ThreadCurrentPullRequest::Single { link } if link.number.get() == 2)
        );
    }

    #[test]
    fn current_reports_a_stack_when_several_links_are_open_with_the_highest_layer_on_top() {
        let stack = native_stack(&[
            (10, "a", PullRequestState::Open),
            (11, "b", PullRequestState::Open),
        ]);
        let links = [
            with(snapped(11, snapshot(|_| {})), |l| {
                l.stack = Some(stack.clone())
            }),
            with(snapped(10, snapshot(|_| {})), |l| {
                l.stack = Some(stack.clone())
            }),
        ];
        match resolve_thread_current_pull_request(&links).unwrap() {
            ThreadCurrentPullRequest::Stack { open, top } => {
                assert_eq!(top.number.get(), 11);
                assert_eq!(numbers(&open), vec![11, 10]);
            }
            other => panic!("expected stack, got {other:?}"),
        }
    }

    #[test]
    fn current_orders_an_open_set_without_stack_data_by_most_recent_link() {
        let links = [link(1), link(2)];
        match resolve_thread_current_pull_request(&links).unwrap() {
            ThreadCurrentPullRequest::Stack { top, .. } => assert_eq!(top.number.get(), 2),
            other => panic!("expected stack, got {other:?}"),
        }
    }

    #[test]
    fn current_falls_back_to_the_most_recently_updated_terminal_link() {
        let links = [
            snapped(
                1,
                snapshot(|s| {
                    s.state = PullRequestState::Merged;
                    s.updated_at = Some("2026-01-03T00:00:00.000Z".to_owned());
                }),
            ),
            snapped(
                2,
                snapshot(|s| {
                    s.state = PullRequestState::Closed;
                    s.updated_at = Some("2026-01-02T00:00:00.000Z".to_owned());
                }),
            ),
        ];
        let current = resolve_thread_current_pull_request(&links).unwrap();
        assert!(
            matches!(current, ThreadCurrentPullRequest::Single { link } if link.number.get() == 1)
        );
    }

    fn identity() -> RepositoryIdentity {
        serde_json::from_value(serde_json::json!({
            "canonicalKey": "github.com/pingdotgg/t3code",
            "provider": "github",
            "displayName": "pingdotgg/t3code",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "https://github.com/pingdotgg/t3code.git",
            },
        }))
        .unwrap()
    }

    fn project() -> ProjectId {
        ProjectId::new("project-1").unwrap()
    }

    #[test]
    fn legacy_projects_only_links_the_owning_project_can_route_without_a_host() {
        let projected =
            legacy_linked_pull_request_of(&[link(7)], &project(), Some(&identity())).unwrap();
        assert_eq!(projected.project_id, project());
        assert_eq!(projected.repository.as_str(), "pingdotgg/t3code");
        assert_eq!(projected.number.get(), 7);
        assert_eq!(
            projected.url.as_str(),
            "https://github.com/pingdotgg/t3code/pull/7"
        );
        assert_eq!(
            legacy_linked_pull_request_of(&[], &project(), Some(&identity())),
            None
        );
    }

    #[test]
    fn legacy_omits_an_unsafe_route() {
        let foreign_host = with(link(7), |l| {
            l.host = t("github.enterprise.test");
            l.url = t("https://github.enterprise.test/pingdotgg/t3code/pull/7");
        });
        let foreign_repository = with(link(7), |l| {
            l.repository = t("acme/other");
            l.url = t("https://github.com/acme/other/pull/7");
        });
        for foreign in [foreign_host, foreign_repository] {
            assert_eq!(
                legacy_linked_pull_request_of(
                    std::slice::from_ref(&foreign),
                    &project(),
                    Some(&identity())
                ),
                None
            );
            let found =
                legacy_linked_pull_request_of(&[foreign, link(8)], &project(), Some(&identity()));
            assert_eq!(found.map(|l| l.number.get()), Some(8));
        }
    }

    #[test]
    fn legacy_does_not_guess_when_the_project_identity_is_unavailable() {
        assert_eq!(
            legacy_linked_pull_request_of(&[link(7)], &project(), None),
            None
        );
    }

    #[test]
    fn legacy_does_not_route_links_for_a_local_path_remote_with_no_host_or_provider() {
        let local: RepositoryIdentity = serde_json::from_value(serde_json::json!({
            "canonicalKey": "/tmp/r/remote",
            "displayName": "remote",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "/tmp/r/remote.git",
            },
        }))
        .unwrap();
        assert_eq!(
            legacy_linked_pull_request_of(&[link(7)], &project(), Some(&local)),
            None
        );
    }

    #[test]
    fn legacy_key_falls_back_to_the_url_host() {
        let linked: ThreadLinkedPullRequest = serde_json::from_value(serde_json::json!({
            "projectId": "p",
            "repository": "Acme/Web",
            "number": 3,
            "url": "https://example.test/Acme/Web/merge/3",
        }))
        .unwrap();
        assert_eq!(
            legacy_thread_pull_request_key(&linked, None),
            key("example.test", "acme/web", 3)
        );
        assert_eq!(
            legacy_thread_pull_request_key(&linked, Some(" GitHub.com ")),
            key("github.com", "acme/web", 3)
        );
    }

    #[test]
    fn chains_by_base_to_head_within_a_repository_bottom_to_top() {
        let links = [
            snapped(3, open_snap("c", "b")),
            snapped(1, open_snap("a", "main")),
            snapped(2, open_snap("b", "a")),
            snapped(9, open_snap("solo", "main")),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let shape: Vec<_> = chains
            .iter()
            .map(|c| (c.kind, numbers(&c.layers)))
            .collect();
        assert_eq!(
            shape,
            vec![
                (ThreadPullRequestChainKind::Derived, vec![1, 2, 3]),
                (ThreadPullRequestChainKind::Derived, vec![9]),
            ]
        );
    }

    #[test]
    fn chains_use_the_native_stack_order_when_the_host_provides_one() {
        let stack = native_stack(&[
            (5, "a", PullRequestState::Merged),
            (6, "b", PullRequestState::Open),
        ]);
        let links = [
            with(snapped(6, open_snap("b", "main")), |l| {
                l.stack = Some(stack.clone())
            }),
            with(
                snapped(5, snap(PullRequestState::Merged, "a", "main")),
                |l| l.stack = Some(stack.clone()),
            ),
            link(8),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let shape: Vec<_> = chains
            .iter()
            .map(|c| (c.kind, numbers(&c.layers)))
            .collect();
        assert_eq!(
            shape,
            vec![
                (ThreadPullRequestChainKind::Native, vec![5, 6]),
                (ThreadPullRequestChainKind::Derived, vec![8]),
            ]
        );
    }

    #[test]
    fn badge_aggregates_state_for_stacked_and_unstacked_links() {
        use PullRequestState::{Closed, Merged, Open};
        use ThreadPullRequestBadgeState as B;
        let cases = [
            (Open, false, Open, false, B::Open),
            (Closed, false, Closed, false, B::Closed),
            (Open, true, Open, true, B::Draft),
            (Open, false, Open, true, B::Open),
            (Closed, false, Open, true, B::Open),
            (Merged, false, Merged, false, B::Merged),
            (Merged, false, Closed, true, B::Closed),
        ];
        for (first_state, first_draft, second_state, second_draft, state) in cases {
            for stacked in [false, true] {
                let links = [
                    snapped(
                        1,
                        snapshot(|s| {
                            s.state = first_state;
                            s.is_draft = first_draft;
                            s.head_branch = t("base");
                        }),
                    ),
                    snapped(
                        2,
                        snapshot(|s| {
                            s.state = second_state;
                            s.is_draft = second_draft;
                            s.base_branch = t(if stacked { "base" } else { "main" });
                        }),
                    ),
                    dismissed(3),
                ];
                let expected = ThreadPullRequestBadge {
                    state,
                    kind: if stacked {
                        ThreadPullRequestBadgeKind::Stack { layers: 2 }
                    } else {
                        ThreadPullRequestBadgeKind::PullRequest { others: 1 }
                    },
                };
                assert_eq!(
                    resolve_thread_pull_request_badge(Some(&links)),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn targets_the_top_of_a_derived_chain_despite_a_later_bottom_update_and_link() {
        for state in [
            PullRequestState::Open,
            PullRequestState::Merged,
            PullRequestState::Closed,
        ] {
            let bottom = snapped(
                2,
                snapshot(|s| {
                    s.state = state;
                    s.head_branch = t("base");
                    s.updated_at = Some("2026-02-01T00:00:00.000Z".to_owned());
                }),
            );
            let top = snapped(1, snap(state, "top", "base"));
            let links = [bottom, top];
            let current = resolve_thread_current_pull_request(&links).unwrap();
            if state == PullRequestState::Open {
                match current {
                    ThreadCurrentPullRequest::Stack { top, open } => {
                        assert_eq!(top.number.get(), 1);
                        assert_eq!(numbers(&open), vec![1, 2]);
                    }
                    other => panic!("expected stack, got {other:?}"),
                }
            } else {
                assert!(
                    matches!(current, ThreadCurrentPullRequest::Single { link } if link.number.get() == 1)
                );
            }
            let badge = resolve_thread_pull_request_badge(Some(&links)).unwrap();
            assert_eq!(badge.kind, ThreadPullRequestBadgeKind::Stack { layers: 2 });
            let expected = match state {
                PullRequestState::Open => ThreadPullRequestBadgeState::Open,
                PullRequestState::Merged => ThreadPullRequestBadgeState::Merged,
                PullRequestState::Closed => ThreadPullRequestBadgeState::Closed,
            };
            assert_eq!(badge.state, expected);
        }
    }

    #[test]
    fn keeps_the_top_native_layer_after_completion_and_treats_an_unsynced_layer_as_open() {
        let stack = native_stack(&[
            (2, "base", PullRequestState::Merged),
            (1, "top", PullRequestState::Merged),
        ]);
        let bottom = with(
            snapped(
                2,
                snapshot(|s| {
                    s.state = PullRequestState::Merged;
                    s.updated_at = Some("2026-02-01T00:00:00.000Z".to_owned());
                }),
            ),
            |l| l.stack = Some(stack.clone()),
        );
        let top = with(
            snapped(1, snap(PullRequestState::Merged, "feature", "main")),
            |l| l.stack = Some(stack.clone()),
        );
        let links = [bottom.clone(), top.clone()];
        let current = resolve_thread_current_pull_request(&links).unwrap();
        assert!(
            matches!(current, ThreadCurrentPullRequest::Single { link } if link.number.get() == 1)
        );
        let unsynced_top = with(top, |l| l.snapshot = None);
        assert_eq!(
            resolve_thread_pull_request_badge(Some(&[bottom, unsynced_top])),
            Some(ThreadPullRequestBadge {
                state: ThreadPullRequestBadgeState::Open,
                kind: ThreadPullRequestBadgeKind::Stack { layers: 2 },
            })
        );
    }

    #[test]
    fn badge_uses_aggregate_state_and_distinguishes_unrelated_work_from_a_stack() {
        let bottom = snapped(1, snap(PullRequestState::Merged, "base", "main"));
        let top = snapped(2, snap(PullRequestState::Closed, "feature", "base"));
        assert_eq!(
            resolve_thread_pull_request_badge(Some(&[bottom.clone(), top.clone()])),
            Some(ThreadPullRequestBadge {
                state: ThreadPullRequestBadgeState::Closed,
                kind: ThreadPullRequestBadgeKind::Stack { layers: 2 },
            })
        );
        assert_eq!(
            resolve_thread_pull_request_badge(Some(&[bottom, top, link(3)])),
            Some(ThreadPullRequestBadge {
                state: ThreadPullRequestBadgeState::Open,
                kind: ThreadPullRequestBadgeKind::PullRequest { others: 2 },
            })
        );
        assert_eq!(
            resolve_thread_pull_request_badge(Some(&[dismissed(3)])),
            None
        );
        assert_eq!(resolve_thread_pull_request_badge(None), None);
    }

    #[test]
    fn keeps_branch_matching_case_sensitive_while_ignoring_repository_case() {
        let bottom = with(snapped(1, snapshot(|s| s.head_branch = t("Base"))), |l| {
            l.repository = t("PingDotGG/T3code")
        });
        let top = snapped(2, open_snap("top", "base"));
        assert_eq!(
            resolve_thread_pull_request_chains(&[bottom.clone(), top]).len(),
            2
        );
        let top = snapped(2, open_snap("top", "Base"));
        let links = [bottom.clone(), top.clone()];
        let chains = resolve_thread_pull_request_chains(&links);
        assert_eq!(chains[0].layers, vec![&bottom, &top]);
    }

    #[test]
    fn preserves_cyclic_links_without_presenting_a_guessed_stack_order() {
        let links = [
            snapped(1, open_snap("a", "b")),
            snapped(2, open_snap("b", "a")),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let layers: Vec<Vec<u64>> = chains.iter().map(|c| numbers(&c.layers)).collect();
        assert_eq!(layers, vec![vec![1], vec![2]]);
        match resolve_thread_current_pull_request(&links).unwrap() {
            ThreadCurrentPullRequest::Stack { top, .. } => assert_eq!(top.number.get(), 2),
            other => panic!("expected stack, got {other:?}"),
        }
        assert_eq!(
            resolve_thread_pull_request_badge(Some(&links)),
            Some(ThreadPullRequestBadge {
                state: ThreadPullRequestBadgeState::Open,
                kind: ThreadPullRequestBadgeKind::PullRequest { others: 1 },
            })
        );
    }

    #[test]
    fn does_not_guess_a_parent_when_a_head_branch_was_reused() {
        let links = [
            snapped(1, snap(PullRequestState::Merged, "reused", "main")),
            snapped(2, open_snap("reused", "main")),
            snapped(3, open_snap("top", "reused")),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let layers: Vec<Vec<u64>> = chains.iter().map(|c| numbers(&c.layers)).collect();
        assert_eq!(layers, vec![vec![1], vec![2], vec![3]]);
    }

    fn branch_pull_request() -> ThreadLinkedPullRequest {
        serde_json::from_value(serde_json::json!({
            "projectId": "project",
            "repository": "pingdotgg/t3code",
            "number": 11809,
            "url": "https://github.com/pingdotgg/t3code/pull/11809",
        }))
        .unwrap()
    }

    #[test]
    fn search_terms_include_the_branch_pr_alongside_existing_links() {
        let branch = branch_pull_request();
        let linked = ThreadLinkedPullRequest {
            number: PositiveInt::new(12).unwrap(),
            url: t("https://github.com/pingdotgg/t3code/pull/12"),
            ..branch.clone()
        };
        let with_links = [link(34)];
        let cases: [(Option<&[ThreadPullRequestLink]>, &str); 3] = [
            (None, "#12"),
            (Some(&[]), "#12"),
            (Some(&with_links), "#34"),
        ];
        for (pull_requests, expected) in cases {
            let terms = thread_pull_request_search_terms(ThreadPullRequestSearchSource {
                pull_requests,
                branch_pull_request: Some(&branch),
                linked_pull_request: Some(&linked),
            });
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
            assert!(
                terms.iter().any(|term| term == expected),
                "{expected} in {terms:?}"
            );
        }
    }

    #[test]
    fn search_terms_include_completed_and_unsynced_links_but_exclude_dismissed_links() {
        let links = [
            snapped(
                12,
                snapshot(|s| {
                    s.title = t("Fix login");
                    s.state = PullRequestState::Merged;
                }),
            ),
            link(34),
            dismissed(56),
        ];
        let terms = thread_pull_request_search_terms(ThreadPullRequestSearchSource {
            pull_requests: Some(&links),
            ..Default::default()
        });
        for expected in [
            "#12",
            "pingdotgg/t3code#12",
            "https://github.com/pingdotgg/t3code/pull/12",
            "Fix login",
            "#34",
        ] {
            assert!(terms.iter().any(|term| term == expected), "{expected}");
        }
        assert!(!terms.join(" ").contains("56"));
    }

    #[test]
    fn search_terms_use_the_legacy_projection_when_old_environments_decode_to_an_empty_links_list()
    {
        let linked = branch_pull_request();
        let linked = ThreadLinkedPullRequest {
            number: PositiveInt::new(12).unwrap(),
            url: t("https://github.com/pingdotgg/t3code/pull/12"),
            ..linked
        };
        let empty = ThreadPullRequestSearchSource {
            pull_requests: Some(&[]),
            linked_pull_request: Some(&linked),
            branch_pull_request: None,
        };
        assert!(thread_pull_request_search_terms(empty).contains(&"#12".to_owned()));
        let links = [link(34)];
        let with_links = ThreadPullRequestSearchSource {
            pull_requests: Some(&links),
            ..empty
        };
        assert!(!thread_pull_request_search_terms(with_links).contains(&"#12".to_owned()));
    }

    #[test]
    fn thread_pull_requests_of_synthesises_a_manual_link_from_the_legacy_projection() {
        let linked = branch_pull_request();
        let links = thread_pull_requests_of(None, Some(&linked));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].host.as_str(), "github.com");
        assert_eq!(links[0].repository.as_str(), "pingdotgg/t3code");
        assert_eq!(links[0].source, ThreadPullRequestLinkSource::Manual);
        assert_eq!(links[0].linked_at, "1970-01-01T00:00:00.000Z");
        assert!(thread_pull_requests_of(Some(&[]), Some(&linked)).is_empty());
        assert!(thread_pull_requests_of(None, None).is_empty());
    }
}
