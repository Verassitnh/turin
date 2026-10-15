//! Port of upstream/packages/shared/src/threadPullRequests.ts (GitHub only;
//! the Azure selector paths are kept because they are pure string handling).

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use crate::state::contracts::{
    OrchestrationV2AppThread, PullRequestState, RepositoryIdentity, SourceControlProviderKind,
    ThreadLinkedPullRequest, ThreadPullRequestKey, ThreadPullRequestLink,
    ThreadPullRequestLinkSource, TrimmedNonEmptyString, js_trim,
};
use regex::Regex;

use super::change_request_url::parse_change_request_url;
use crate::state::time::parse_ms;

/// `canonicalRepositoryKey` (packages/shared/src/sourceControl.ts).
pub fn canonical_repository_key(key: &str) -> String {
    static SSH_AZURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$",
        )
        .expect("static regex")
    });
    static LEGACY_AZURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
            .expect("static regex")
    });
    let key = SSH_AZURE.replace(key, "dev.azure.com/$1/$2/_git/$3");
    LEGACY_AZURE
        .replace(&key, "dev.azure.com/$1/$2/_git/$3")
        .into_owned()
}

/// `sourceControlRepositorySelector` (packages/shared/src/sourceControl.ts).
pub fn source_control_repository_selector(identity: &RepositoryIdentity) -> Option<String> {
    if identity.provider.as_ref().map(|p| p.as_str()) == Some("azure-devops") {
        let display = identity
            .display_name
            .as_ref()
            .map(|name| name.as_str())
            .unwrap_or("");
        let last = display.split('/').rfind(|part| *part != "_git");
        return identity
            .name
            .as_ref()
            .map(|name| name.as_str().to_owned())
            .or_else(|| last.filter(|s| !s.is_empty()).map(str::to_owned));
    }
    if let Some(display) = &identity.display_name {
        return Some(display.as_str().to_owned());
    }
    match (&identity.owner, &identity.name) {
        (Some(owner), Some(name)) => Some(format!("{}/{}", owner.as_str(), name.as_str())),
        _ => None,
    }
}

/// `ThreadPullRequestKey & { authority?, url? }`.
#[derive(Clone, Debug)]
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

/// A normalized `ThreadPullRequestKey` over plain strings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedPullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// Normalize stored links, recovering Forgejo HTTP ports from old links' URLs.
pub fn normalize_thread_pull_request_key(
    key: &ThreadPullRequestKeySource<'_>,
) -> NormalizedPullRequestKey {
    let parsed = key.url.and_then(parse_change_request_url);
    let authority = key.authority.map(str::to_owned).or_else(|| {
        parsed.as_ref().and_then(|parsed| {
            if parsed.repository == js_trim(key.repository).to_lowercase()
                && parsed.number == key.number
            {
                parsed.authority.clone()
            } else {
                None
            }
        })
    });
    let canonical = canonical_repository_key(&format!(
        "{}/{}",
        js_trim(authority.as_deref().unwrap_or(key.host)).to_lowercase(),
        js_trim(key.repository).to_lowercase()
    ));
    let separator = canonical.find('/').unwrap_or(canonical.len());
    NormalizedPullRequestKey {
        host: canonical[..separator].to_owned(),
        repository: canonical.get(separator + 1..).unwrap_or("").to_owned(),
        number: key.number,
    }
}

/// Legacy Azure selectors omit the organization and project; recover those from the PR URL.
pub fn legacy_thread_pull_request_key(
    linked: &ThreadLinkedPullRequest,
    fallback_host: Option<&str>,
) -> NormalizedPullRequestKey {
    if let Some(parsed) = parse_change_request_url(linked.url.as_str())
        && parsed.number == linked.number.get()
    {
        let canonical = canonical_repository_key(&format!("{}/{}", parsed.host, parsed.repository));
        if parsed.authority.is_some() || canonical.starts_with("dev.azure.com/") {
            return normalize_thread_pull_request_key(&ThreadPullRequestKeySource {
                host: &parsed.host,
                repository: &parsed.repository,
                number: parsed.number,
                authority: parsed.authority.as_deref(),
                url: None,
            });
        }
    }
    let host = match fallback_host {
        Some(host) => host.to_owned(),
        None => url::Url::parse(linked.url.as_str())
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    let host = js_trim(&host).to_lowercase();
    NormalizedPullRequestKey {
        host: if host.is_empty() {
            "unknown".to_owned()
        } else {
            host
        },
        repository: js_trim(linked.repository.as_str()).to_lowercase(),
        number: linked.number.get(),
    }
}

/// Identity comparison for links: host-level, case-insensitive on host and repository.
pub fn thread_pull_request_keys_equal(
    left: &ThreadPullRequestKeySource<'_>,
    right: &ThreadPullRequestKeySource<'_>,
) -> bool {
    thread_pull_request_key_of(left) == thread_pull_request_key_of(right)
}

pub fn thread_pull_request_key_of(key: &ThreadPullRequestKeySource<'_>) -> String {
    let normalized = normalize_thread_pull_request_key(key);
    format!(
        "{}/{}#{}",
        normalized.host, normalized.repository, normalized.number
    )
}

fn link_key_of(link: &ThreadPullRequestLink) -> String {
    thread_pull_request_key_of(&link.into())
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
    link.snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.state == PullRequestState::Open)
}

fn latest_updated_at(link: &ThreadPullRequestLink) -> i64 {
    let value = link
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.updated_at.as_deref())
        .unwrap_or(&link.linked_at);
    parse_ms(value).unwrap_or(0)
}

/// The single pull request a one-slot surface (sidebar badge, tab icon, copy link) shows.
#[derive(Clone, Debug, PartialEq)]
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
    let open: Vec<&ThreadPullRequestLink> = visible
        .iter()
        .copied()
        .filter(|link| is_open(link))
        .collect();
    if open.len() == 1 {
        return Some(ThreadCurrentPullRequest::Single { link: open[0] });
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
        let newest_linked = |layers: &Vec<&ThreadPullRequestLink>| {
            layers
                .iter()
                .map(|link| parse_ms(&link.linked_at))
                .fold(None::<i64>, |max, value| match (max, value) {
                    // `Math.max` over a NaN poisons the result; keep the sort
                    // stable by treating an unparseable stamp as the oldest.
                    (Some(max), Some(value)) => Some(max.max(value)),
                    (None, Some(value)) => Some(value),
                    (max, None) => max,
                })
                .unwrap_or(i64::MIN)
        };
        // Newest chain first (stable for ties, like Array#sort).
        open_chains.sort_by_key(|layers| std::cmp::Reverse(newest_linked(layers)));
        let ordered: Vec<&ThreadPullRequestLink> = open_chains.into_iter().flatten().collect();
        let top = ordered[0];
        return Some(ThreadCurrentPullRequest::Stack { open: ordered, top });
    }
    if chains.len() == 1 {
        return Some(ThreadCurrentPullRequest::Single {
            link: chains[0].layers.last().expect("a chain has layers"),
        });
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

/// Legacy clients can only route links belonging to the thread's own repository.
pub fn legacy_linked_pull_request_of(
    links: &[ThreadPullRequestLink],
    project_id: &crate::state::contracts::ProjectId,
    identity: Option<&RepositoryIdentity>,
) -> Option<ThreadLinkedPullRequest> {
    let identity = identity?;
    let provider = SourceControlProviderKind::new(
        identity
            .provider
            .as_ref()
            .map(|provider| provider.as_str())
            .unwrap_or("unknown"),
    )
    .ok()?;
    // A local-path remote has no host segment and no provider, so there is no host to match.
    let host = crate::state::contracts::pull_request_host_of(
        Some(&crate::state::contracts::RepositoryIdentityLike {
            canonical_key: Some(identity.canonical_key.as_str()),
            remote_url: Some(identity.locator.remote_url.as_str()),
        }),
        provider,
    );
    let repository = source_control_repository_selector(identity)?;
    let azure_key = if identity.provider.as_ref().map(|p| p.as_str()) == Some("azure-devops") {
        Some(canonical_repository_key(
            &identity.canonical_key.as_str().to_lowercase(),
        ))
    } else {
        None
    };
    let matching: Vec<ThreadPullRequestLink> = links
        .iter()
        .filter(|link| {
            if let Some(azure_key) = &azure_key {
                let key = legacy_thread_pull_request_key_for(link, Some(link.host.as_str()));
                return &canonical_repository_key(&format!("{}/{}", key.host, key.repository))
                    == azure_key;
            }
            let parsed = parse_change_request_url(link.url.as_str());
            if let Some(authority) = parsed.as_ref().and_then(|p| p.authority.as_deref()) {
                if let Ok(remote) = url::Url::parse(identity.locator.remote_url.as_str())
                    && (remote.scheme() == "http" || remote.scheme() == "https")
                {
                    let remote_host = remote_host_with_port(&remote);
                    return authority == remote_host
                        && parsed
                            .as_ref()
                            .is_some_and(|p| p.repository == repository.to_lowercase());
                }
                return parsed
                    .as_ref()
                    .is_some_and(|p| p.host == host && p.repository == repository.to_lowercase());
            }
            link.host.as_str().to_lowercase() == host.to_lowercase()
                && link.repository.as_str().to_lowercase() == repository.to_lowercase()
        })
        .cloned()
        .collect();
    let link = resolve_thread_current_pull_request_link(&matching)?;
    Some(ThreadLinkedPullRequest {
        project_id: project_id.clone(),
        repository: if azure_key.is_none() {
            link.repository.clone()
        } else {
            TrimmedNonEmptyString::new(&repository).ok()?
        },
        number: link.number,
        url: link.url.clone(),
    })
}

fn remote_host_with_port(url: &url::Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        _ => String::new(),
    }
}

/// `legacyThreadPullRequestKey` for a full link (it satisfies the `Pick`).
fn legacy_thread_pull_request_key_for(
    link: &ThreadPullRequestLink,
    fallback_host: Option<&str>,
) -> NormalizedPullRequestKey {
    legacy_thread_pull_request_key(
        &ThreadLinkedPullRequest {
            // Only repository, number and url are read.
            project_id: crate::state::contracts::ProjectId::new("unused").expect("static id"),
            repository: link.repository.clone(),
            number: link.number,
            url: link.url.clone(),
        },
        fallback_host,
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadPullRequestChain<'a> {
    pub kind: ChainKind,
    /// Bottom to top.
    pub layers: Vec<&'a ThreadPullRequestLink>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainKind {
    Native,
    Derived,
}

/// Groups a thread's links into stacks. Native stacks come from the host and win; the rest
/// are chained by matching one link's base branch to another's head branch within the same
/// repository. A link that chains to nothing is a one-layer chain.
pub fn resolve_thread_pull_request_chains(
    links: &[ThreadPullRequestLink],
) -> Vec<ThreadPullRequestChain<'_>> {
    let visible = visible_thread_pull_requests(links);
    let mut chains: Vec<ThreadPullRequestChain<'_>> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();

    // Insertion-ordered like a JS Map.
    let mut native_order: Vec<String> = Vec::new();
    let mut native_stacks: HashMap<String, Vec<&ThreadPullRequestLink>> = HashMap::new();
    for link in &visible {
        let Some(stack) = &link.stack else { continue };
        let key = normalize_thread_pull_request_key(&(*link).into());
        let stack_key = format!(
            "{}/{}#stack:{}",
            key.host,
            key.repository,
            stack.id.as_str()
        );
        if !native_stacks.contains_key(&stack_key) {
            native_order.push(stack_key.clone());
        }
        native_stacks.entry(stack_key).or_default().push(link);
    }
    for stack_key in native_order {
        let mut members = native_stacks
            .remove(&stack_key)
            .expect("ordered key exists");
        let order: HashMap<u64, usize> = members[0]
            .stack
            .as_ref()
            .expect("native members have stacks")
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| (layer.number.get(), index))
            .collect();
        members.sort_by_key(|member| order.get(&member.number.get()).copied().unwrap_or(0));
        for member in &members {
            placed.insert(link_key_of(member));
        }
        chains.push(ThreadPullRequestChain {
            kind: ChainKind::Native,
            layers: members,
        });
    }

    let remaining: Vec<&ThreadPullRequestLink> = visible
        .iter()
        .copied()
        .filter(|link| !placed.contains(&link_key_of(link)))
        .collect();
    let branch_key = |link: &ThreadPullRequestLink, branch: &str| {
        let key = normalize_thread_pull_request_key(&link.into());
        format!("{}/{}:{}", key.host, key.repository, branch)
    };
    // Reused head names cannot identify a parent unambiguously.
    let mut by_head: HashMap<String, Option<&ThreadPullRequestLink>> = HashMap::new();
    for link in &remaining {
        let Some(snapshot) = &link.snapshot else {
            continue;
        };
        let key = branch_key(link, snapshot.head_branch.as_str());
        let value = if by_head.contains_key(&key) {
            None
        } else {
            Some(*link)
        };
        by_head.insert(key, value);
    }
    let mut has_child: HashSet<String> = HashSet::new();
    for link in &remaining {
        let Some(snapshot) = &link.snapshot else {
            continue;
        };
        if let Some(Some(parent)) = by_head.get(&branch_key(link, snapshot.base_branch.as_str()))
            && !std::ptr::eq(*parent, *link)
        {
            has_child.insert(link_key_of(parent));
        }
    }
    // Walk from each top (a link nothing builds on) down its base chain.
    for top in &remaining {
        if has_child.contains(&link_key_of(top)) {
            continue;
        }
        let mut layers: Vec<&ThreadPullRequestLink> = Vec::new();
        let mut cursor: Option<&ThreadPullRequestLink> = Some(top);
        while let Some(current) = cursor {
            if placed.contains(&link_key_of(current)) {
                break;
            }
            placed.insert(link_key_of(current));
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
                kind: ChainKind::Derived,
                layers,
            });
        }
    }
    // Cycles have no top. Keep those links visible without inventing a stack order.
    for link in &remaining {
        if !placed.contains(&link_key_of(link)) {
            chains.push(ThreadPullRequestChain {
                kind: ChainKind::Derived,
                layers: vec![link],
            });
        }
    }
    chains
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestBadgeState {
    Open,
    Closed,
    Merged,
    Draft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPullRequestBadge {
    Stack {
        state: PullRequestBadgeState,
        layers: usize,
    },
    PullRequest {
        state: PullRequestBadgeState,
        others: usize,
    },
}

/// Aggregate visible links' state for both stacks and unrelated linked counts.
pub fn resolve_thread_pull_request_badge(
    pull_requests: Option<&[ThreadPullRequestLink]>,
) -> Option<ThreadPullRequestBadge> {
    let links = pull_requests.unwrap_or(&[]);
    let visible = visible_thread_pull_requests(links);
    if visible.is_empty() {
        return None;
    }
    let states: Vec<PullRequestState> = visible
        .iter()
        .map(|link| {
            link.snapshot
                .as_ref()
                .map_or(PullRequestState::Open, |snapshot| snapshot.state)
        })
        .collect();
    let all_open_drafts = visible.iter().all(|link| {
        link.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.state == PullRequestState::Open && snapshot.is_draft)
    });
    let state = if all_open_drafts {
        PullRequestBadgeState::Draft
    } else if states.contains(&PullRequestState::Open) {
        PullRequestBadgeState::Open
    } else if states
        .iter()
        .all(|state| *state == PullRequestState::Merged)
    {
        PullRequestBadgeState::Merged
    } else {
        PullRequestBadgeState::Closed
    };
    let chains = resolve_thread_pull_request_chains(links);
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
                format!("#{}", branch.number),
                format!("{}#{}", branch.repository.as_str(), branch.number),
                branch.url.as_str().to_owned(),
            ]
        })
        .unwrap_or_default();
    if let Some(links) = pull_requests
        && !links.is_empty()
    {
        for link in visible_thread_pull_requests(links) {
            terms.push(format!("#{}", link.number));
            terms.push(format!("{}#{}", link.repository.as_str(), link.number));
            terms.push(link.url.as_str().to_owned());
            terms.push(
                link.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.title.as_str().to_owned())
                    .unwrap_or_default(),
            );
        }
        return terms;
    }
    if let Some(legacy) = linked_pull_request {
        terms.push(format!("#{}", legacy.number));
        terms.push(format!("{}#{}", legacy.repository.as_str(), legacy.number));
        terms.push(legacy.url.as_str().to_owned());
    }
    terms
}

/// Older V2 event payloads stored one link; an explicit empty array means it was unlinked.
pub fn thread_pull_requests_of(
    pull_requests: Option<&[ThreadPullRequestLink]>,
    linked_pull_request: Option<&ThreadLinkedPullRequest>,
) -> Vec<ThreadPullRequestLink> {
    if let Some(links) = pull_requests {
        return links.to_vec();
    }
    let Some(linked) = linked_pull_request else {
        return Vec::new();
    };
    let key = legacy_thread_pull_request_key(linked, None);
    let (Ok(host), Ok(repository)) = (
        TrimmedNonEmptyString::new(&key.host),
        TrimmedNonEmptyString::new(&key.repository),
    ) else {
        return Vec::new();
    };
    let Some(number) = crate::state::contracts::PositiveInt::new(key.number) else {
        return Vec::new();
    };
    vec![ThreadPullRequestLink {
        host,
        repository,
        number,
        url: linked.url.clone(),
        source: ThreadPullRequestLinkSource::Manual,
        linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
        snapshot: None,
        stack: None,
        watch: None,
    }]
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
    let updated_source: ThreadPullRequestKeySource<'_> = updated.into();
    let pull_requests = thread_pull_requests_of(thread.pull_requests.as_deref(), linked)
        .into_iter()
        .map(|link| {
            if thread_pull_request_keys_equal(&(&link).into(), &updated_source) {
                updated.clone()
            } else {
                link
            }
        })
        .collect();
    OrchestrationV2AppThread {
        pull_requests: Some(pull_requests),
        ..thread.clone()
    }
}

#[allow(dead_code)]
type KeyDocs = ThreadPullRequestKey;
