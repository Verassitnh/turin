//! Port of upstream/apps/web/src/components/pullRequest/pullRequestList.logic.ts
//!
//! The list's pure logic: merging each environment's answer, reading the typed query, narrowing by
//! involvement and filters, grouping and ranking, line-count batching, optimistic overrides and the
//! stored snapshot. The React page and its hooks are not ported; the GPUI list page calls these
//! functions directly.
//!
//! Entry-generic helpers take [`ListEntry`] so the same body serves a plain
//! `PullRequestListEntry` and an `EnvironmentPullRequestEntry`, as upstream's `ScopedEntry` does.

use super::pull_request_detail_logic::SnapshotStorage;
use super::pull_request_list_preferences::PullRequestListSort;
use crate::state::support::collate::locale_compare;
use crate::state::time::{parse_ms, to_iso_string};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActor, PullRequestChecksFilter, PullRequestChecksState,
    PullRequestDiffStat, PullRequestDraftFilter, PullRequestInvolvement, PullRequestLabel,
    PullRequestListCursors, PullRequestListEntry, PullRequestListFilters,
    PullRequestListProjectError, PullRequestListResult, PullRequestListState,
    PullRequestListStatsInput, PullRequestMergeability, PullRequestProviderSummary,
    PullRequestQualifierValue, PullRequestRef, PullRequestReviewDecision, PullRequestReviewFilter,
    PullRequestState, resolve_pull_request_author_filter,
};
use turin_contracts::{EnvironmentId, PositiveInt, js_trim};

/// A listed change request with the environment that read it. Nothing on a row says which machine
/// it came from, and the page unions every connected one, so acting on a row, refreshing it, or
/// opening its detail all need the tag the listing itself does not carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPullRequestEntry {
    #[serde(flatten)]
    pub entry: PullRequestListEntry,
    pub environment_id: EnvironmentId,
}

impl Deref for EnvironmentPullRequestEntry {
    type Target = PullRequestListEntry;
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl DerefMut for EnvironmentPullRequestEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entry
    }
}

/// A line-count read tagged with the environment it came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPullRequestStat {
    #[serde(flatten)]
    pub stat: PullRequestDiffStat,
    pub environment_id: EnvironmentId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPullRequestError {
    #[serde(flatten)]
    pub error: PullRequestListProjectError,
    pub environment_id: EnvironmentId,
}

/// The signed-in login per host. Keyed `"<environmentId> <host>"` once a listing spans more than
/// one environment: two machines can both reach github.com signed in as different people, and a
/// single host-keyed record would let whichever answered last decide who "I" am, which is how
/// every row of somebody's own work came to be filed under Others.
///
/// Ordered, because it is persisted in the list snapshot.
pub type PullRequestViewers = IndexMap<String, String>;

/// A row the list helpers read: a listed change request, plus the environment that read it when
/// the caller has one. Upstream's `ScopedEntry` is `PullRequestListEntry & { environmentId? }`;
/// a plain row reports no environment and keys without the `"<environmentId>:"` prefix.
pub trait ListEntry: Clone {
    fn listing(&self) -> &PullRequestListEntry;
    fn listing_mut(&mut self) -> &mut PullRequestListEntry;
    fn environment_id(&self) -> Option<&str> {
        None
    }
}

impl ListEntry for PullRequestListEntry {
    fn listing(&self) -> &PullRequestListEntry {
        self
    }
    fn listing_mut(&mut self) -> &mut PullRequestListEntry {
        self
    }
}

impl ListEntry for EnvironmentPullRequestEntry {
    fn listing(&self) -> &PullRequestListEntry {
        &self.entry
    }
    fn listing_mut(&mut self) -> &mut PullRequestListEntry {
        &mut self.entry
    }
    fn environment_id(&self) -> Option<&str> {
        Some(self.environment_id.as_str())
    }
}

/// `pullRequestLabelColor`: a six-digit hex color with its `#`, or nothing.
pub fn pull_request_label_color(color: Option<&str>) -> Option<String> {
    let raw = js_trim(color?);
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    (hex.len() == 6 && hex.bytes().all(|ch| ch.is_ascii_hexdigit())).then(|| format!("#{hex}"))
}

/// Lowercased and trimmed; empty counts as absent.
fn normalize(value: Option<&str>) -> Option<String> {
    let trimmed = value
        .map(|value| js_trim(value).to_lowercase())
        .unwrap_or_default();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// `state === "all" || entry.state === state`: the list's state tab against a row's own state.
fn list_state_matches(list: PullRequestListState, state: PullRequestState) -> bool {
    list == PullRequestListState::All || list_state_to_state(list) == state
}

/// The row state a concrete list state asks for. `All` has no row state of its own, so callers
/// test it first; it maps to `Open` here only because the return type must be one.
pub fn list_state_to_state(state: PullRequestListState) -> PullRequestState {
    match state {
        PullRequestListState::All | PullRequestListState::Open => PullRequestState::Open,
        PullRequestListState::Closed => PullRequestState::Closed,
        PullRequestListState::Merged => PullRequestState::Merged,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestAuthorFacet {
    pub actor: PullRequestActor,
    pub count: usize,
    pub merged_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestLabelFacet {
    pub label: PullRequestLabel,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestListFacets {
    pub authors: Vec<PullRequestAuthorFacet>,
    pub labels: Vec<PullRequestLabelFacet>,
}

/// Authors and labels the loaded rows carry. An author is counted over the rows the state tab
/// shows, and its merged count over every row, so a merged count stays meaningful under the open
/// tab. Labels are counted only over the rows the state tab shows.
pub fn collect_pull_request_list_facets<E: ListEntry>(
    entries: &[E],
    state: PullRequestListState,
) -> PullRequestListFacets {
    let mut authors: IndexMap<String, PullRequestAuthorFacet> = IndexMap::new();
    let mut labels: IndexMap<String, PullRequestLabelFacet> = IndexMap::new();
    let mut unique: IndexMap<String, &E> = IndexMap::new();
    for entry in entries {
        unique.insert(pull_request_entry_key_of(entry), entry);
    }
    for entry in unique.values() {
        let listing = entry.listing();
        let in_state = list_state_matches(state, listing.state);
        if let Some(author) = &listing.author
            && let Some(key) = normalize(Some(author.login.as_str()))
        {
            let held = authors.get(&key);
            let next = PullRequestAuthorFacet {
                actor: held.map_or_else(|| author.clone(), |held| held.actor.clone()),
                count: held.map_or(0, |held| held.count) + usize::from(in_state),
                merged_count: held.map_or(0, |held| held.merged_count)
                    + usize::from(listing.state == PullRequestState::Merged),
            };
            authors.insert(key, next);
        }
        if !in_state {
            continue;
        }
        for label in &listing.labels {
            let Some(key) = normalize(Some(label.name.as_str())) else {
                continue;
            };
            let held = labels.get(&key);
            let next = PullRequestLabelFacet {
                label: PullRequestLabel {
                    name: held.map_or_else(|| label.name.clone(), |held| held.label.name.clone()),
                    color: held
                        .and_then(|held| held.label.color.clone())
                        .or_else(|| label.color.clone()),
                },
                count: held.map_or(0, |held| held.count) + 1,
            };
            labels.insert(key, next);
        }
    }
    let mut authors: Vec<PullRequestAuthorFacet> = authors
        .into_values()
        .filter(|author| author.count > 0)
        .collect();
    authors.sort_by(|left, right| {
        right
            .merged_count
            .cmp(&left.merged_count)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| locale_compare(left.actor.login.as_str(), right.actor.login.as_str()))
    });
    let mut labels: Vec<PullRequestLabelFacet> = labels.into_values().collect();
    labels.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| locale_compare(left.label.name.as_str(), right.label.name.as_str()))
    });
    PullRequestListFacets { authors, labels }
}

/// The signed-in login for the host a row came from, or null where none was given. Shared by
/// authorship matching here and by `author:me` resolution wherever a row's own viewer is needed.
pub fn pull_request_entry_viewer<E: ListEntry>(
    entry: &E,
    viewers: &PullRequestViewers,
) -> Option<String> {
    // The environment's own answer first; a plain host key is what a single-environment listing
    // still writes, and what the snapshot from one carries.
    let listing = entry.listing();
    let scoped_key = format!(
        "{} {}",
        entry.environment_id().unwrap_or(""),
        listing.host.as_str()
    );
    normalize(
        viewers
            .get(scoped_key.as_str())
            .or_else(|| viewers.get(listing.host.as_str()))
            .map(String::as_str),
    )
}

/// Authorship is per host, not per provider kind: the same list can hold change requests from
/// GitHub, GitLab and a GitHub Enterprise install, and the account that owns one says nothing
/// about the others.
fn is_authored_by_viewer<E: ListEntry>(entry: &E, viewers: &PullRequestViewers) -> bool {
    let Some(viewer) = pull_request_entry_viewer(entry, viewers) else {
        return false;
    };
    normalize(
        entry
            .listing()
            .author
            .as_ref()
            .map(|author| author.login.as_str()),
    )
    .is_some_and(|login| login == viewer)
}

/// `review:` takes GitHub's spelling and the contract's.
fn review_value(value: &str) -> Option<PullRequestReviewFilter> {
    Some(match value {
        "approved" => PullRequestReviewFilter::Approved,
        "changes_requested" | "changes-requested" => PullRequestReviewFilter::ChangesRequested,
        "required" | "review-required" => PullRequestReviewFilter::ReviewRequired,
        "none" => PullRequestReviewFilter::None,
        _ => return None,
    })
}

/// `status:` and `checks:`.
fn checks_value(value: &str) -> Option<PullRequestChecksFilter> {
    match value {
        "success" | "passing" => Some(PullRequestChecksFilter::Passing),
        "failure" | "failing" => Some(PullRequestChecksFilter::Failing),
        _ => None,
    }
}

/// JS `\s` for one code point: exactly the set `js_trim` strips.
fn is_js_space(c: char) -> bool {
    let mut buf = [0u8; 4];
    js_trim(c.encode_utf8(&mut buf)).is_empty()
}

/// The first `units` UTF-16 code units of `value`, as JS `slice(0, units)` gives them. A character
/// that straddles the cut is left out whole, where JS would keep half of a surrogate pair.
fn utf16_prefix(value: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, ch) in value.char_indices() {
        let width = ch.len_utf16();
        if used + width > units {
            return &value[..index];
        }
        used += width;
    }
    value
}

/// The contract's own ceilings on a qualifier: this many names, each this long.
const MAX_QUALIFIER_VALUES: usize = 10;
const MAX_QUALIFIER_LENGTH: usize = 200;

/// A qualifier's value with its quotes removed and whitespace trimmed.
fn unquote_qualifier(raw: &str) -> String {
    js_trim(&raw.replace('"', "")).to_owned()
}

/// A qualifier value the contract accepts, or nothing for an empty or over-long one.
pub fn qualifier_value(raw: &str) -> Option<PullRequestQualifierValue> {
    PullRequestQualifierValue::new(unquote_qualifier(raw)).ok()
}

/// A qualifier's value as the list it may be: split on commas, each name unquoted on its own.
fn split_qualifier_list(raw: &str) -> Vec<String> {
    // Quoting names one whole label however many commas it holds: `label:"needs,triage"` asks for
    // the label written that way, not for either half of it.
    let trimmed = js_trim(raw);
    let quoted_whole = trimmed.len() >= 2
        && trimmed.starts_with('"')
        && trimmed.ends_with('"')
        && !trimmed[1..trimmed.len() - 1].contains('"');
    if quoted_whole {
        let whole = unquote_qualifier(raw);
        return if whole.is_empty() {
            vec![]
        } else {
            vec![whole]
        };
    }
    raw.split(',')
        .map(unquote_qualifier)
        .filter(|part| !part.is_empty())
        .collect()
}

/// A typed query is written by hand, and the contract bounds what a qualifier may carry, so what
/// is typed past those bounds is cut here rather than refused by the request that carries it.
fn bounded_names(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .take(MAX_QUALIFIER_VALUES)
        .map(|name| js_trim(utf16_prefix(&name, MAX_QUALIFIER_LENGTH)).to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// One token of a typed query: a run of non-space characters in which a quoted stretch counts as
/// part of the token, so `label:"needs design"` stays whole. An unbalanced quote is dropped rather
/// than swallowing the rest of the line.
///
/// Mirrors `/(?:[^\s"]|"[^"]*")+/g`: a quote opens a stretch only when a closing quote follows.
fn query_tokens(raw: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = raw.char_indices().collect();
    let byte_at = |index: usize| chars.get(index).map_or(raw.len(), |(at, _)| *at);
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        while end < chars.len() {
            let ch = chars[end].1;
            if is_js_space(ch) {
                break;
            }
            if ch == '"' {
                match chars[end + 1..].iter().position(|(_, next)| *next == '"') {
                    Some(offset) => end = end + 1 + offset + 1,
                    None => break,
                }
            } else {
                end += 1;
            }
        }
        if end > start {
            tokens.push(&raw[byte_at(start)..byte_at(end)]);
            start = end;
        } else {
            start += 1;
        }
    }
    tokens
}

/// `^(-?)([A-Za-z][A-Za-z0-9_-]*):(.*)$`, without a regex. `.` does not cross a line terminator,
/// so a token whose value spans a newline is not a qualifier.
struct Qualifier<'a> {
    negated: bool,
    key: &'a str,
    rest: &'a str,
}

fn parse_qualifier(token: &str) -> Option<Qualifier<'_>> {
    let (negated, body) = match token.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, token),
    };
    let key_len = body
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
        .count();
    let (key, after) = body.split_at(key_len);
    if !key.as_bytes().first()?.is_ascii_alphabetic() {
        return None;
    }
    let rest = after.strip_prefix(':')?;
    if rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some(Qualifier { negated, key, rest })
}

/// The qualifiers a typed query has taken so far, filled one token at a time.
#[derive(Default)]
struct QueryFilters {
    labels: Vec<Vec<String>>,
    excluded_labels: Vec<String>,
    author: Option<String>,
    draft: Option<PullRequestDraftFilter>,
    review: Option<PullRequestReviewFilter>,
    checks: Option<PullRequestChecksFilter>,
}

impl QueryFilters {
    /// Reads one token as a qualifier. `false` leaves the token as plain text.
    fn take_qualifier(&mut self, token: &str) -> bool {
        let Some(qualifier) = parse_qualifier(token) else {
            return false;
        };
        let value = unquote_qualifier(qualifier.rest);
        if value.is_empty() {
            return false;
        }
        let lowered = value.to_lowercase();
        match qualifier.key.to_lowercase().as_str() {
            "label" => {
                // GitHub's own OR: `label:a,b` is one qualifier satisfied by either name. Negated,
                // the comma excludes each: a row carrying any of them goes.
                let names = bounded_names(split_qualifier_list(qualifier.rest));
                if names.is_empty() {
                    return false;
                }
                if qualifier.negated {
                    self.excluded_labels.extend(names);
                } else {
                    self.labels.push(names);
                }
            }
            "author" => {
                if qualifier.negated {
                    return false;
                }
                self.author = Some(js_trim(utf16_prefix(&value, MAX_QUALIFIER_LENGTH)).to_owned());
            }
            "draft" => {
                if qualifier.negated || (lowered != "true" && lowered != "false") {
                    return false;
                }
                self.draft = Some(if lowered == "true" {
                    PullRequestDraftFilter::Only
                } else {
                    PullRequestDraftFilter::Hide
                });
            }
            "review" => {
                if qualifier.negated {
                    return false;
                }
                let Some(decision) = review_value(&lowered) else {
                    return false;
                };
                self.review = Some(decision);
            }
            "status" | "checks" => {
                if qualifier.negated {
                    return false;
                }
                let Some(state) = checks_value(&lowered) else {
                    return false;
                };
                self.checks = Some(state);
            }
            _ => {
                // An unknown key, read as the namespaced label it almost always is. A pasted link
                // is not one: `https://...` would otherwise become a label named after its scheme.
                if value.starts_with('/') {
                    return false;
                }
                // The key names the namespace, so the bare parts of `size:S,XS` are both sizes. A
                // part that already carries a colon names its whole label: `size:S,size:XS` is the
                // same pair written out, and prefixing it again would ask for `size:size:XS`.
                let names = bounded_names(
                    split_qualifier_list(qualifier.rest)
                        .into_iter()
                        .map(|name| {
                            if name.contains(':') {
                                name
                            } else {
                                format!("{}:{name}", qualifier.key)
                            }
                        })
                        .collect(),
                );
                if names.is_empty() {
                    return false;
                }
                if qualifier.negated {
                    self.excluded_labels.extend(names);
                } else {
                    self.labels.push(names);
                }
            }
        }
        true
    }

    fn into_filters(self) -> PullRequestListFilters {
        let labels: Vec<Vec<PullRequestQualifierValue>> = self
            .labels
            .into_iter()
            .take(MAX_QUALIFIER_VALUES)
            .map(|group| {
                group
                    .iter()
                    .filter_map(|name| qualifier_value(name))
                    .collect()
            })
            .collect();
        let excluded_labels: Vec<PullRequestQualifierValue> = self
            .excluded_labels
            .iter()
            .take(MAX_QUALIFIER_VALUES)
            .filter_map(|name| qualifier_value(name))
            .collect();
        PullRequestListFilters {
            labels: (!labels.is_empty()).then_some(labels),
            excluded_labels: (!excluded_labels.is_empty()).then_some(excluded_labels),
            author: self.author.as_deref().and_then(qualifier_value),
            draft: self.draft,
            review: self.review,
            checks: self.checks,
        }
    }
}

/// A typed query split into the qualifiers the hosts can act on and the text that is left.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPullRequestQuery {
    pub text: String,
    pub filters: PullRequestListFilters,
}

/// A typed query split into the qualifiers the hosts can act on and the text that is left. Written
/// GitHub's way (`label:foo`, `-label:"needs design"`, `author:octocat`, `draft:true`,
/// `review:approved`, `status:success`), because a project's labels are its own and a menu cannot
/// list them.
///
/// A key this does not know is read as a label of that whole name: repositories namespace their
/// labels with a colon (`size:XXL`, `area:web`, `vouch:trusted`), and someone typing one means the
/// label, not a description that happens to contain it. `-size:XXL` excludes the same way.
///
/// Quoting is the way back to plain text: `"size:XXL"` is searched for as written, which is what
/// makes a literal search for a colon still possible. A known key whose value it does not take is
/// text too, so a search for "status:" itself is still findable.
pub fn parse_pull_request_query(raw: &str) -> ParsedPullRequestQuery {
    let mut text = Vec::new();
    let mut filters = QueryFilters::default();
    for token in query_tokens(raw) {
        if !filters.take_qualifier(token) {
            text.push(token);
        }
    }
    ParsedPullRequestQuery {
        text: text.join(" "),
        filters: filters.into_filters(),
    }
}

/// Free-text filter over the fields a row actually shows, plus `#123` / `123`.
pub fn matches_pull_request_query<E: ListEntry>(entry: &E, query: &str) -> bool {
    let needle = js_trim(query).to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let listing = entry.listing();
    let haystack = format!(
        "#{} {} {} {} {}",
        listing.number.get(),
        listing.title.as_str(),
        listing.repository.as_str(),
        listing.head_branch.as_str(),
        listing
            .author
            .as_ref()
            .map_or("", |author| author.login.as_str()),
    );
    haystack.to_lowercase().contains(&needle)
}

fn review_filter_matches(
    filter: PullRequestReviewFilter,
    decision: Option<PullRequestReviewDecision>,
) -> bool {
    let wanted = match filter {
        PullRequestReviewFilter::Approved => Some(PullRequestReviewDecision::Approved),
        PullRequestReviewFilter::ChangesRequested => {
            Some(PullRequestReviewDecision::ChangesRequested)
        }
        PullRequestReviewFilter::ReviewRequired => Some(PullRequestReviewDecision::ReviewRequired),
        PullRequestReviewFilter::None => None,
    };
    match wanted {
        Some(wanted) => decision == Some(wanted),
        None => decision.is_none(),
    }
}

/// The further narrowings over rows that have already arrived, for the hosts that could not apply
/// them themselves and for the moment before an answer that did lands.
///
/// `checks` is absent because no listed row carries its check state: that one filter is the
/// host's alone, and a row a host did not narrow stays rather than being guessed at.
///
/// `viewer` is the row's own host's signed-in login, so `author:me` resolves against it rather
/// than being compared as the literal name "me". Optional, and left unresolved without one.
pub fn matches_pull_request_filters<E: ListEntry>(
    entry: &E,
    filters: &PullRequestListFilters,
    viewer: Option<&str>,
) -> bool {
    let listing = entry.listing();
    let labels: Vec<String> = listing
        .labels
        .iter()
        .map(|label| js_trim(label.name.as_str()).to_lowercase())
        .collect();
    let holds = |label: &str| labels.contains(&js_trim(label).to_lowercase());
    filters
        .draft
        .is_none_or(|draft| listing.is_draft == (draft == PullRequestDraftFilter::Only))
        && filters
            .review
            .is_none_or(|review| review_filter_matches(review, listing.review_decision))
        && filters.labels.as_ref().is_none_or(|groups| {
            groups
                .iter()
                .all(|group| group.iter().any(|label| holds(label.as_str())))
        })
        && filters
            .excluded_labels
            .as_ref()
            .is_none_or(|excluded| !excluded.iter().any(|label| holds(label.as_str())))
        && filters.author.as_ref().is_none_or(|author| {
            listing.author.as_ref().is_some_and(|row_author| {
                row_author.login.as_str().to_lowercase()
                    == resolve_pull_request_author_filter(author.as_str(), viewer).to_lowercase()
            })
        })
}

/// The server returns the involvement superset for a state, so switching between the Reviewing
/// and Authored tabs never waits on the network.
pub fn filter_pull_requests_by_involvement<E: ListEntry>(
    entries: &[E],
    viewers: &PullRequestViewers,
    involvement: PullRequestInvolvement,
) -> Vec<E> {
    match involvement {
        PullRequestInvolvement::Reviewing => entries
            .iter()
            .filter(|&entry| entry.listing().viewer_review_requested)
            .cloned()
            .collect(),
        PullRequestInvolvement::Authored => entries
            .iter()
            .filter(|&entry| is_authored_by_viewer(entry, viewers))
            .cloned()
            .collect(),
        PullRequestInvolvement::All => entries.to_vec(),
    }
}

/// The rows already read, kept only where the filters now being asked about would keep them.
///
/// Every combination of filters is its own question to the hosts, and asking a new one is no
/// reason to blank the page: what has been read is narrowed here and stays until the answer for
/// the new question replaces it. So this may only ever drop rows (a merged pull request cannot
/// sit under "Open" for the round trip), and what it leaves is a subset of the answer rather than
/// the answer itself.
///
/// Only the filters a row can be judged by from its own fields. Involvement is left out because
/// it needs to know who is signed in on each host, and the search text because searching is the
/// hosts' own answer; both are narrowed where that knowledge already lives.
pub fn narrow_pull_requests_to_filters<E: ListEntry>(
    entries: &[E],
    state: PullRequestListState,
    project_id: Option<&str>,
    host: Option<&str>,
) -> Vec<E> {
    entries
        .iter()
        .filter(|&entry| {
            let listing = entry.listing();
            list_state_matches(state, listing.state)
                && project_id.is_none_or(|id| listing.project_id.as_str() == id)
                && host.is_none_or(|host| listing.host.as_str() == host)
        })
        .cloned()
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PullRequestGroupKey {
    ReviewRequested,
    Authored,
    Others,
}

impl PullRequestGroupKey {
    /// `GROUP_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReviewRequested => "Review requested",
            Self::Authored => "Authored",
            Self::Others => "Others",
        }
    }
}

/// A group of rows under one heading. `label` is what the heading says; the page hides it when
/// it is empty.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestGroup<E> {
    pub key: PullRequestGroupKey,
    pub label: String,
    pub entries: Vec<E>,
}

impl<E> PullRequestGroup<E> {
    pub fn new(key: PullRequestGroupKey, entries: Vec<E>) -> Self {
        Self {
            key,
            label: key.label().to_owned(),
            entries,
        }
    }
}

/// Only relationships the list data actually carries: no "previously reviewed" bucket is
/// inferred, because the listing has no review history.
pub fn group_pull_requests_by_involvement<E: ListEntry>(
    entries: &[E],
    viewers: &PullRequestViewers,
) -> Vec<PullRequestGroup<E>> {
    let mut authored = Vec::new();
    let mut review_requested = Vec::new();
    let mut others = Vec::new();
    for entry in entries {
        if is_authored_by_viewer(entry, viewers) {
            authored.push(entry.clone());
        } else if entry.listing().viewer_review_requested {
            review_requested.push(entry.clone());
        } else {
            others.push(entry.clone());
        }
    }
    [
        (PullRequestGroupKey::Authored, authored),
        (PullRequestGroupKey::ReviewRequested, review_requested),
        (PullRequestGroupKey::Others, others),
    ]
    .into_iter()
    .filter(|(_, entries)| !entries.is_empty())
    .map(|(key, entries)| PullRequestGroup::new(key, entries))
    .collect()
}

/// Repository plus number is unique on one host, so the host makes the key unique overall, and
/// the environment on top of that, because two connected machines can hold the same repository and
/// would otherwise contribute two rows under one key.
pub fn pull_request_entry_key(
    entry: &PullRequestListEntry,
    environment_id: Option<&str>,
) -> String {
    let scope = environment_id.map_or(String::new(), |id| format!("{id}:"));
    format!(
        "{scope}{}:{}#{}",
        entry.host.as_str(),
        entry.repository.as_str(),
        entry.number.get()
    )
}

/// [`pull_request_entry_key`] for any [`ListEntry`].
pub fn pull_request_entry_key_of<E: ListEntry>(entry: &E) -> String {
    pull_request_entry_key(entry.listing(), entry.environment_id())
}

/// Which rows a stats read covers: `Visible` reads the rows near the viewport, `Eager` every row
/// loaded (size sorting needs all of them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestStatsPolicy {
    Visible,
    Eager,
}

/// What a stats read is for: the list it reads and the policy its rows are chosen by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestStatsScope {
    pub key: String,
    pub policy: PullRequestStatsPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestLineCounts {
    pub additions: u64,
    pub deletions: u64,
}

/// The line counts held so far, keyed by [`pull_request_diff_stat_key`].
pub type PullRequestDiffStats = IndexMap<String, PullRequestLineCounts>;

/// One bounded line-count read: the refs it asks for, the keys those refs answer, and the
/// environment they all belong to.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestStatsBatch {
    pub environment_id: EnvironmentId,
    pub input: PullRequestListStatsInput,
    pub keys: IndexSet<String>,
}

/// Each read is capped at the largest listing a page can hold.
const MAX_PULL_REQUEST_STATS_REFS: usize = 500;

/// A project id only names a project within its own environment, so the key carries both.
pub fn pull_request_diff_stat_key(environment_id: &str, project_id: &str, number: u64) -> String {
    format!("{environment_id} {project_id} {number}")
}

fn stat_key_of<E: ListEntry>(entry: &E) -> String {
    let listing = entry.listing();
    pull_request_diff_stat_key(
        entry.environment_id().unwrap_or(""),
        listing.project_id.as_str(),
        listing.number.get(),
    )
}

/// Excludes rows already covered by an active batch or the received-count cache.
pub fn pull_request_stats_keys_to_request(
    entries_by_key: &IndexMap<String, EnvironmentPullRequestEntry>,
    entered_keys: &IndexSet<String>,
    batches: &[PullRequestStatsBatch],
    stats_by_row: &PullRequestDiffStats,
) -> IndexSet<String> {
    let requested: HashSet<&str> = batches
        .iter()
        .flat_map(|batch| batch.keys.iter().map(String::as_str))
        .collect();
    entered_keys
        .iter()
        .filter(|&key| {
            let Some(entry) = entries_by_key.get(key.as_str()) else {
                return false;
            };
            let listing = &entry.entry;
            listing.additions == 0
                && listing.deletions == 0
                && !requested.contains(key.as_str())
                && !stats_by_row.contains_key(&stat_key_of(entry))
        })
        .cloned()
        .collect()
}

/// Groups selected rows into bounded, immutable line-count reads per environment.
pub fn pull_request_stats_batches(
    entries_by_key: &IndexMap<String, EnvironmentPullRequestEntry>,
    keys: &IndexSet<String>,
) -> Vec<PullRequestStatsBatch> {
    let mut by_environment: IndexMap<EnvironmentId, Vec<(String, PullRequestRef)>> =
        IndexMap::new();
    for key in keys {
        let Some(entry) = entries_by_key.get(key.as_str()) else {
            continue;
        };
        let listing = &entry.entry;
        by_environment
            .entry(entry.environment_id.clone())
            .or_default()
            .push((
                key.clone(),
                PullRequestRef {
                    project_id: listing.project_id.clone(),
                    host: None,
                    expected_account_id: None,
                    allow_stale: None,
                    repository: listing.repository.clone(),
                    number: listing.number,
                },
            ));
    }
    by_environment
        .into_iter()
        .flat_map(|(environment_id, rows)| {
            rows.chunks(MAX_PULL_REQUEST_STATS_REFS)
                .map(|chunk| PullRequestStatsBatch {
                    environment_id: environment_id.clone(),
                    input: PullRequestListStatsInput {
                        refs: chunk
                            .iter()
                            .map(|(_, reference)| reference.clone())
                            .collect(),
                    },
                    keys: chunk.iter().map(|(key, _)| key.clone()).collect(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The inputs of [`pull_request_stats_request_batches`].
pub struct PullRequestStatsRequest<'a> {
    pub entries_by_key: &'a IndexMap<String, EnvironmentPullRequestEntry>,
    pub candidate_keys: &'a IndexSet<String>,
    pub policy: PullRequestStatsPolicy,
    pub active_batches: &'a [PullRequestStatsBatch],
    pub stats_by_row: &'a PullRequestDiffStats,
    /// An explicit refresh asks for the selected rows again, even ones already counted.
    pub refresh: bool,
}

/// Selects the next immutable stats batches. Size sorting needs every loaded row, while the other
/// modes only need rows near the viewport. Normal reads skip active and cached rows; an explicit
/// refresh asks for the selected rows again.
pub fn pull_request_stats_request_batches(
    request: PullRequestStatsRequest<'_>,
) -> Vec<PullRequestStatsBatch> {
    let requested_keys: IndexSet<String> = match request.policy {
        PullRequestStatsPolicy::Eager => request.entries_by_key.keys().cloned().collect(),
        PullRequestStatsPolicy::Visible => request.candidate_keys.clone(),
    };
    let keys = if request.refresh {
        requested_keys
    } else {
        pull_request_stats_keys_to_request(
            request.entries_by_key,
            &requested_keys,
            request.active_batches,
            request.stats_by_row,
        )
    };
    pull_request_stats_batches(request.entries_by_key, &keys)
}

/// Ignores a refresh that finished after the list moved to another filter or stats policy.
pub fn pull_request_stats_refresh_batches(
    requested_scope: &PullRequestStatsScope,
    current_scope: &PullRequestStatsScope,
    entries_by_key: &IndexMap<String, EnvironmentPullRequestEntry>,
    candidate_keys: &IndexSet<String>,
    stats_by_row: &PullRequestDiffStats,
) -> Option<Vec<PullRequestStatsBatch>> {
    if requested_scope.key != current_scope.key || requested_scope.policy != current_scope.policy {
        return None;
    }
    Some(pull_request_stats_request_batches(
        PullRequestStatsRequest {
            entries_by_key,
            candidate_keys,
            policy: requested_scope.policy,
            active_batches: &[],
            stats_by_row,
            refresh: true,
        },
    ))
}

/// Drops completed batches once every row in them has left the observer window.
pub fn retain_visible_pull_request_stats_batches(
    batches: &[PullRequestStatsBatch],
    visible_keys: &IndexSet<String>,
) -> Vec<PullRequestStatsBatch> {
    batches
        .iter()
        .filter(|batch| batch.keys.iter().any(|key| visible_keys.contains(key)))
        .cloned()
        .collect()
}

/// The priority groups built from the hosts' own answers rather than re-partitioned from the
/// paginated feed. The feed is sliced by recency, so an older authored or review-requested row
/// can be missing from its first page and arrive with a later one; grouping the loaded pages
/// would then move it above rows already read. Here the partitions come whole from their own
/// server-filtered reads, the feed fills "Others" in its own order, and a continuation can only
/// append: a row it carries that a partition already holds is dropped rather than moved.
pub fn partition_pull_requests_with_priority<E: ListEntry>(
    entries: &[E],
    authored: &[E],
    review_requested: &[E],
) -> Vec<PullRequestGroup<E>> {
    let mut authored_by_key: IndexMap<String, E> = authored
        .iter()
        .map(|entry| (pull_request_entry_key_of(entry), entry.clone()))
        .collect();
    // A row can be both authored and review-requested; authored wins, as the local grouping has it.
    let mut review_by_key: IndexMap<String, E> = review_requested
        .iter()
        .filter_map(|entry| {
            let key = pull_request_entry_key_of(entry);
            (!authored_by_key.contains_key(&key)).then(|| (key, entry.clone()))
        })
        .collect();
    let mut others: Vec<E> = Vec::new();
    for entry in entries {
        let key = pull_request_entry_key_of(entry);
        // The feed's copy of a partitioned row is at least as fresh, so it replaces in place.
        if let Some(held) = authored_by_key.get_mut(&key) {
            *held = entry.clone();
        } else if let Some(held) = review_by_key.get_mut(&key) {
            *held = entry.clone();
        } else {
            others.push(entry.clone());
        }
    }
    let by_recency = |left: &E, right: &E| {
        locale_compare(&right.listing().updated_at, &left.listing().updated_at)
    };
    let mut authored_entries: Vec<E> = authored_by_key.into_values().collect();
    authored_entries.sort_by(by_recency);
    let mut review_entries: Vec<E> = review_by_key.into_values().collect();
    review_entries.sort_by(by_recency);
    [
        (PullRequestGroupKey::Authored, authored_entries),
        (PullRequestGroupKey::ReviewRequested, review_entries),
        (PullRequestGroupKey::Others, others),
    ]
    .into_iter()
    .filter(|(_, entries)| !entries.is_empty())
    .map(|(key, entries)| PullRequestGroup::new(key, entries))
    .collect()
}

/// The line counts held so far, with a new batch merged on. The stats read is keyed by the rows
/// on screen, so adding or removing one row is a fresh query with nothing in it yet; replacing
/// the map would blank every count already showing for the round trip. Merging by row identity
/// keeps them until their replacements arrive.
pub fn merge_pull_request_diff_stats(
    previous: &PullRequestDiffStats,
    stats: &[EnvironmentPullRequestStat],
) -> PullRequestDiffStats {
    let mut next = previous.clone();
    for stat in stats {
        next.insert(
            pull_request_diff_stat_key(
                stat.environment_id.as_str(),
                stat.stat.project_id.as_str(),
                stat.stat.number.get(),
            ),
            PullRequestLineCounts {
                additions: stat.stat.additions,
                deletions: stat.stat.deletions,
            },
        );
    }
    next
}

/// Every connected environment's listing, read as one list.
///
/// `next_cursors` is keyed by environment rather than by repository: a cursor only means anything
/// to the host that issued it, and two machines can hold the same repository. `viewers` is not:
/// a host names one account, and the same host reached from two machines is the same account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPullRequestList {
    /// Keyed `"<environmentId> <host>"`, so one host's two accounts stay two accounts.
    pub viewers: PullRequestViewers,
    pub providers: Vec<PullRequestProviderSummary>,
    pub entries: Vec<EnvironmentPullRequestEntry>,
    pub errors: Vec<EnvironmentPullRequestError>,
    pub truncated: bool,
    pub next_cursors: IndexMap<String, PullRequestListCursors>,
    /// The environments with rows still on their hosts. Those with a cursor are continued from it;
    /// the rest can only be reached by asking them for a longer page, and are named here so that
    /// asking still happens once every other environment has run out of cursors.
    pub truncated_environments: Vec<String>,
}

/// The environments' answers folded into one. A host reached from more than one environment is one
/// row in the switcher, readable if any environment could read it and searched on the host only if
/// every one of them did: a host answering unnarrowed anywhere still needs the local pass.
pub fn merge_pull_request_lists(
    answers: &[(EnvironmentId, PullRequestListResult)],
) -> Option<MergedPullRequestList> {
    if answers.is_empty() {
        return None;
    }
    let mut viewers: PullRequestViewers = IndexMap::new();
    let mut truncated_environments: Vec<String> = Vec::new();
    let mut providers: IndexMap<String, PullRequestProviderSummary> = IndexMap::new();
    let mut entries: Vec<EnvironmentPullRequestEntry> = Vec::new();
    let mut errors: Vec<EnvironmentPullRequestError> = Vec::new();
    let mut next_cursors: IndexMap<String, PullRequestListCursors> = IndexMap::new();
    let mut truncated = false;
    for (environment_id, answer) in answers {
        let environment = environment_id.as_str();
        for (host, login) in &answer.viewers {
            viewers.insert(
                format!("{environment} {}", host.as_str()),
                login.as_str().to_owned(),
            );
        }
        for provider in &answer.providers {
            let host = provider.host.as_str().to_owned();
            let next = match providers.get(&host) {
                None => provider.clone(),
                Some(held) => {
                    // The configured answer's own details win, so a host's CLI problem is not
                    // hidden behind an unconfigured reader's copy.
                    let mut next = if held.configured {
                        held.clone()
                    } else {
                        provider.clone()
                    };
                    next.project_count = PositiveInt::new(
                        held.project_count
                            .get()
                            .saturating_add(provider.project_count.get()),
                    )
                    .unwrap_or(held.project_count);
                    next.searches_on_host = held.searches_on_host && provider.searches_on_host;
                    next.configured = held.configured || provider.configured;
                    next
                }
            };
            providers.insert(host, next);
        }
        entries.extend(
            answer
                .entries
                .iter()
                .map(|entry| EnvironmentPullRequestEntry {
                    entry: entry.clone(),
                    environment_id: environment_id.clone(),
                }),
        );
        errors.extend(
            answer
                .errors
                .iter()
                .map(|error| EnvironmentPullRequestError {
                    error: error.clone(),
                    environment_id: environment_id.clone(),
                }),
        );
        truncated |= answer.truncated;
        if answer.truncated {
            truncated_environments.push(environment.to_owned());
        }
        if !answer.next_cursors.is_empty() {
            next_cursors.insert(environment.to_owned(), answer.next_cursors.clone());
        }
    }
    entries.sort_by(|left, right| locale_compare(&right.entry.updated_at, &left.entry.updated_at));
    Some(MergedPullRequestList {
        viewers,
        providers: providers.into_values().collect(),
        entries,
        errors,
        truncated,
        next_cursors,
        truncated_environments,
    })
}

/// One page is what the list itself starts with, and all a cold start needs to look warm.
const SNAPSHOT_MAX_ENTRIES: usize = 99;

/// Keyed by the whole set of environments the list was read from: connecting or dropping one
/// changes which rows belong on the page, and a snapshot taken from a different set would
/// hydrate rows no longer being read.
pub fn pull_request_environment_set_key<S: AsRef<str>>(environment_ids: &[S]) -> String {
    let mut ids: Vec<&str> = environment_ids.iter().map(|id| id.as_ref()).collect();
    ids.sort_by(|left, right| locale_compare(left, right));
    ids.join(",")
}

fn snapshot_storage_key(environment_set_key: &str) -> String {
    format!("t3.pullRequests.list:{environment_set_key}")
}

/// The priority groups' own server-filtered answers, carried with the feed. An authored pull
/// request older than the feed's first page lives only in these, so a snapshot without them
/// cold-starts into an Authored group missing exactly the rows that made it worth having.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestPartitionsSnapshot {
    pub authored: Vec<EnvironmentPullRequestEntry>,
    pub reviewing: Vec<EnvironmentPullRequestEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListSnapshot {
    pub scope: String,
    pub data: MergedPullRequestList,
    /// Optional so a snapshot written before the partitions existed still hydrates the feed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<PullRequestPartitionsSnapshot>,
}

/// The last list answered for this environment, brought back across a reload. Without this a
/// revisit cold-starts into skeletons even though almost every row is unchanged; hydrated, the
/// stale rows render at once and the live read reconciles them in place by key. Errors are not
/// carried: a failure is never cached, and yesterday's is not this morning's.
///
/// Decoded with serde rather than trusted from a cast: storage is writable by anything in the
/// origin and by any past version of this app, and one malformed row would otherwise crash the
/// list on every reload until the key is cleared. A snapshot from before a schema change is
/// rejected the same way, which is exactly the cold start it would have broken.
pub fn read_pull_request_list_snapshot(
    storage: Option<&dyn SnapshotStorage>,
    environment_set_key: &str,
) -> Option<PullRequestListSnapshot> {
    let raw = storage?.get_item(&snapshot_storage_key(environment_set_key))?;
    serde_json::from_str(&raw).ok()
}

/// Storage can be full or denied; the snapshot is a convenience, not a record, so a failed write
/// is dropped.
pub fn write_pull_request_list_snapshot(
    storage: Option<&mut dyn SnapshotStorage>,
    environment_set_key: &str,
    snapshot: &PullRequestListSnapshot,
) {
    let Some(storage) = storage else {
        return;
    };
    let mut data = snapshot.data.clone();
    data.entries.truncate(SNAPSHOT_MAX_ENTRIES);
    // A failure is never cached and yesterday's is not this morning's; a cursor names a position
    // in a listing the host has long since forgotten.
    data.errors.clear();
    data.next_cursors.clear();
    // Where a listing stopped is as stale as the cursor that named it.
    data.truncated_environments.clear();
    let stored = PullRequestListSnapshot {
        scope: snapshot.scope.clone(),
        data,
        partitions: snapshot
            .partitions
            .as_ref()
            .map(|partitions| PullRequestPartitionsSnapshot {
                authored: partitions
                    .authored
                    .iter()
                    .take(SNAPSHOT_MAX_ENTRIES)
                    .cloned()
                    .collect(),
                reviewing: partitions
                    .reviewing
                    .iter()
                    .take(SNAPSHOT_MAX_ENTRIES)
                    .cloned()
                    .collect(),
            }),
    };
    if let Ok(raw) = serde_json::to_string(&stored) {
        let _ = storage.set_item(&snapshot_storage_key(environment_set_key), &raw);
    }
}

/// The project scope to actually ask for. A `projectId` in the URL outlives the environment it
/// came from, and one from elsewhere narrows the listing to nothing: an empty page with no
/// visible filter explaining it, since the switcher has no such project to show as selected. So
/// an id the environment does not have is dropped.
///
/// Until `projects_known`, the id is kept rather than dropped: an environment that has not
/// reported yet is not the same as one without the project, and dropping first would show every
/// project's pull requests for a moment before narrowing back down.
pub fn resolve_project_scope<Id: Clone + PartialEq>(
    project_id: Option<Id>,
    project_ids: &[Id],
    projects_known: bool,
) -> Option<Id> {
    if !projects_known {
        return project_id;
    }
    project_id.filter(|id| project_ids.contains(id))
}

/// The project an id names, on the server that owns it. A project id is only unique within its own
/// environment, so an id alone can name two projects on two connected machines: with a server in
/// hand the answer is exact, and without one it is only given where a single environment has that
/// id, since narrowing to the wrong machine reads an empty list nobody asked for.
pub fn find_scoped_project<'a, P, F, G>(
    projects: &'a [P],
    environment_id: Option<&str>,
    project_id: Option<&str>,
    id_of: F,
    environment_of: G,
) -> Option<&'a P>
where
    F: Fn(&P) -> &str,
    G: Fn(&P) -> &str,
{
    let project_id = project_id?;
    let matches: Vec<&P> = projects
        .iter()
        .filter(|project| id_of(project) == project_id)
        .collect();
    match environment_id {
        Some(environment_id) => matches
            .into_iter()
            .find(|project| environment_of(project) == environment_id),
        None => (matches.len() == 1).then(|| matches[0]),
    }
}

/// Which environments a listing should ask, once the project scope is known. Scoping to a project
/// scopes to the server that owns it, saving every other server a read that could only answer with
/// nothing. Where an id is ambiguous (two servers holding the same project id, with no server
/// named) only the servers that actually hold that id are asked: a server without it may still
/// hold an unrelated project of its own under the same string, and asking it would return that
/// project's rows rather than an honest empty answer.
pub fn resolve_query_environment_ids<Id: Clone + PartialEq>(
    environment_ids: &[Id],
    projects: &[(String, Id)],
    scoped_project_environment: Option<&Id>,
    scoped_project_id: Option<&str>,
    projects_known: bool,
) -> Vec<Id> {
    if let Some(scoped) = scoped_project_environment {
        return environment_ids
            .iter()
            .filter(|id| *id == scoped)
            .cloned()
            .collect();
    }
    // Before the servers have said what they hold, an id nothing matches is an id nothing has been
    // asked about yet: reading none of them would show an empty page for a project that is there.
    if scoped_project_id.is_none() || !projects_known {
        return environment_ids.to_vec();
    }
    environment_ids
        .iter()
        .filter(|id| {
            projects.iter().any(|(project_id, environment)| {
                Some(project_id.as_str()) == scoped_project_id && environment == *id
            })
        })
        .cloned()
        .collect()
}

/// The server a saved selection names, before its project is looked up against it. Still
/// connecting is not the same as gone: a server the workspace already knows about is kept named
/// here even while it has nothing to answer with yet, so the lookup that follows finds nothing
/// under it rather than falling through to a different server's project of the same id. Only a
/// name outside the workspace's whole catalog falls back to the page's own scope, which is what
/// lets a link to a server since removed still resolve by project id alone.
pub fn resolve_selected_environment_id<Id: Clone + Eq + std::hash::Hash>(
    named: Option<&Id>,
    known: &HashSet<Id>,
    fallback: Option<&Id>,
) -> Option<Id> {
    match named {
        Some(named) if known.contains(named) => Some(named.clone()),
        _ => fallback.cloned(),
    }
}

/// How well a row answers the text that was searched for, as a number to order by.
///
/// Every host searches more than a row shows (GitHub reads bodies and commit messages, GitLab and
/// Bitbucket read descriptions), so a result can be a real match with nothing on the row to show
/// for it. Ordering those by recency alone is what puts an apparently unrelated pull request
/// between two obvious ones. They are still results, so they are still shown; they are shown last,
/// under the rows whose own words matched.
///
/// The scale is deliberately coarse. It sorts rows into "this is the one", "this mentions it" and
/// "the host says so", which is as fine a judgement as the row's own fields support.
pub fn score_pull_request_match<E: ListEntry>(entry: &E, query: &str) -> u8 {
    let needle = js_trim(query).to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    let listing = entry.listing();
    let number = needle.strip_prefix('#').unwrap_or(&needle);
    // Asking for a number is asking for one pull request, and it is the answer or it is not.
    if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
        return if listing.number.get().to_string() == number {
            100
        } else {
            0
        };
    }

    let title = listing.title.as_str().to_lowercase();
    let terms: Vec<&str> = needle
        .split(is_js_space)
        .filter(|term| !term.is_empty())
        .collect();
    if title == needle {
        90
    } else if title.contains(&needle) {
        80
    } else if terms.len() > 1 && terms.iter().all(|term| title.contains(term)) {
        // Every word, in any order: "wizard welcome" is still about the welcome wizard.
        70
    } else if listing
        .head_branch
        .as_str()
        .to_lowercase()
        .contains(&needle)
    {
        60
    } else if listing
        .author
        .as_ref()
        .is_some_and(|author| author.login.as_str().to_lowercase().contains(&needle))
    {
        50
    } else if listing.repository.as_str().to_lowercase().contains(&needle) {
        40
    } else if terms.iter().any(|term| title.contains(term)) {
        30
    } else {
        // The host matched something this row does not show: a description, a comment, a commit.
        10
    }
}

/// Search results in the order they answer the question, most convincing first, and by recency
/// among equals. Without a search, preserve the host order for the caller's browse-time ranking.
pub fn rank_pull_request_matches<E: ListEntry>(entries: &[E], query: &str) -> Vec<E> {
    if js_trim(query).is_empty() {
        return entries.to_vec();
    }
    let mut scored: Vec<(u8, E)> = entries
        .iter()
        .map(|entry| (score_pull_request_match(entry, query), entry.clone()))
        .collect();
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| locale_compare(&right.listing().updated_at, &left.listing().updated_at))
    });
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// Default for the `has_measured_size` parameters: a row whose line counts have arrived.
pub fn has_measured_size<E: ListEntry>(entry: &E) -> bool {
    diff_size(entry.listing()) > 0
}

fn diff_size(listing: &PullRequestListEntry) -> u64 {
    listing.additions + listing.deletions
}

/// The default review queue: work that is green and approved, then green work still waiting on a
/// verdict, then everything else still open. Drafts stay in that third tier because their author
/// has not made them mergeable yet. Finished work follows open work when all states are visible. A
/// known conflict is never ready, whatever its checks, review or state say, so it stays at the
/// bottom. Within each tier, smaller measured diffs come first, then unknown sizes. Recency
/// breaks ties between equally sized diffs.
pub fn rank_pull_requests_by_merge_readiness<E: ListEntry>(
    entries: &[E],
    has_measured_size: impl Fn(&E) -> bool,
) -> Vec<E> {
    let tier = |entry: &E| -> u8 {
        let listing = entry.listing();
        if listing.mergeability == PullRequestMergeability::Conflicting {
            4
        } else if listing.state != PullRequestState::Open {
            3
        } else if listing.is_draft {
            2
        } else if listing.checks_state == Some(PullRequestChecksState::Passing)
            && listing.review_decision == Some(PullRequestReviewDecision::Approved)
        {
            0
        } else if listing.checks_state == Some(PullRequestChecksState::Passing) {
            1
        } else {
            2
        }
    };
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| {
        tier(left)
            .cmp(&tier(right))
            .then_with(|| has_measured_size(right).cmp(&has_measured_size(left)))
            .then_with(|| diff_size(left.listing()).cmp(&diff_size(right.listing())))
            .then_with(|| locale_compare(&right.listing().updated_at, &left.listing().updated_at))
    });
    sorted
}

/// Ranks by tier, then by the parsed `updated_at` newest first. Rows whose timestamp does not
/// parse sort after the ones that do (`toSortableTimestamp` gives `null` for both empty and NaN).
fn rank_by_tier_then_recency<E: ListEntry>(
    entries: &[E],
    tier: impl Fn(&PullRequestListEntry) -> u8,
) -> Vec<E> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| {
        tier(left.listing())
            .cmp(&tier(right.listing()))
            .then_with(|| {
                match (
                    parse_ms(&left.listing().updated_at),
                    parse_ms(&right.listing().updated_at),
                ) {
                    (Some(l), Some(r)) => r.cmp(&l),
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (None, None) => Ordering::Equal,
                }
            })
    });
    sorted
}

pub fn rank_pull_requests_blocked_on_author<E: ListEntry>(entries: &[E]) -> Vec<E> {
    rank_by_tier_then_recency(entries, |listing| {
        if listing.state != PullRequestState::Open {
            6
        } else if listing.mergeability == PullRequestMergeability::Conflicting {
            0
        } else if listing.review_decision == Some(PullRequestReviewDecision::ChangesRequested) {
            1
        } else if listing.checks_state == Some(PullRequestChecksState::Failing) {
            2
        } else if listing.is_draft {
            3
        } else if listing.checks_state == Some(PullRequestChecksState::Passing)
            && listing.review_decision == Some(PullRequestReviewDecision::Approved)
        {
            5
        } else {
            4
        }
    })
}

pub fn rank_pull_requests_blocked_on_reviewer<E: ListEntry>(entries: &[E]) -> Vec<E> {
    rank_by_tier_then_recency(entries, |listing| {
        if listing.state == PullRequestState::Open {
            0
        } else {
            1
        }
    })
}

/// Replaces each group's entries with `rank(entries)`, keeping its key and label.
fn map_groups<E: ListEntry>(
    groups: &[PullRequestGroup<E>],
    rank: impl Fn(&[E]) -> Vec<E>,
) -> Vec<PullRequestGroup<E>> {
    groups
        .iter()
        .map(|group| with_entries(group, rank(&group.entries)))
        .collect()
}

fn with_entries<E>(group: &PullRequestGroup<E>, entries: Vec<E>) -> PullRequestGroup<E> {
    PullRequestGroup {
        key: group.key,
        label: group.label.clone(),
        entries,
    }
}

/// The comparator for the sorts that read the row's own dates and sizes. Measured rows come
/// first, then the sort's own order, then the newest activity.
fn compare_for_sort<E: ListEntry>(
    sort: PullRequestListSort,
    left: &E,
    right: &E,
    has_measured_size: &impl Fn(&E) -> bool,
) -> Ordering {
    let (left_listing, right_listing) = (left.listing(), right.listing());
    // `timestamp` in upstream: updatedAt, else createdAt, else 0.
    let activity = |listing: &PullRequestListEntry| {
        parse_ms(&listing.updated_at)
            .or_else(|| parse_ms(&listing.created_at))
            .unwrap_or(0)
    };
    let by_activity = activity(right_listing).cmp(&activity(left_listing));
    match sort {
        PullRequestListSort::Newest | PullRequestListSort::Oldest => {
            let left_created = parse_ms(&left_listing.created_at);
            let right_created = parse_ms(&right_listing.created_at);
            right_created
                .is_some()
                .cmp(&left_created.is_some())
                .then_with(|| {
                    if sort == PullRequestListSort::Newest {
                        right_created.unwrap_or(0).cmp(&left_created.unwrap_or(0))
                    } else {
                        left_created.unwrap_or(0).cmp(&right_created.unwrap_or(0))
                    }
                })
                .then(by_activity)
        }
        _ => has_measured_size(right)
            .cmp(&has_measured_size(left))
            .then_with(|| {
                if sort == PullRequestListSort::Largest {
                    diff_size(right_listing).cmp(&diff_size(left_listing))
                } else {
                    diff_size(left_listing).cmp(&diff_size(right_listing))
                }
            })
            .then(by_activity),
    }
}

/// Keeps authored work first while applying the selected ordering inside every involvement group.
pub fn sort_pull_request_groups<E: ListEntry>(
    groups: &[PullRequestGroup<E>],
    sort: PullRequestListSort,
    search_text: &str,
    has_measured_size: impl Fn(&E) -> bool,
    involvement: PullRequestInvolvement,
) -> Vec<PullRequestGroup<E>> {
    let searching = !js_trim(search_text).is_empty();
    match sort {
        PullRequestListSort::Ready if !searching => map_groups(groups, |entries| {
            rank_pull_requests_by_merge_readiness(entries, &has_measured_size)
        }),
        PullRequestListSort::Blocked if !searching => groups
            .iter()
            .map(|group| {
                let role = match group.key {
                    PullRequestGroupKey::Others => involvement,
                    PullRequestGroupKey::Authored => PullRequestInvolvement::Authored,
                    PullRequestGroupKey::ReviewRequested => PullRequestInvolvement::Reviewing,
                };
                match role {
                    PullRequestInvolvement::All => group.clone(),
                    PullRequestInvolvement::Authored => {
                        with_entries(group, rank_pull_requests_blocked_on_author(&group.entries))
                    }
                    PullRequestInvolvement::Reviewing => with_entries(
                        group,
                        rank_pull_requests_blocked_on_reviewer(&group.entries),
                    ),
                }
            })
            .collect(),
        PullRequestListSort::Ready
        | PullRequestListSort::Blocked
        | PullRequestListSort::Updated => groups.to_vec(),
        PullRequestListSort::Newest
        | PullRequestListSort::Oldest
        | PullRequestListSort::Largest
        | PullRequestListSort::Smallest => map_groups(groups, |entries| {
            let mut sorted = entries.to_vec();
            sorted.sort_by(|left, right| compare_for_sort(sort, left, right, &has_measured_size));
            sorted
        }),
    }
}

/// A row with the line counts that arrived after it did. Only where the host left them out (a
/// listing that carried them is not second-guessed), and only where they have arrived, since a row
/// draws perfectly well without them in the meantime.
pub fn with_diff_stat<E: ListEntry>(entry: &E, stats_by_row: &PullRequestDiffStats) -> E {
    let listing = entry.listing();
    if listing.additions != 0 || listing.deletions != 0 {
        return entry.clone();
    }
    let Some(stat) = stats_by_row.get(&stat_key_of(entry)) else {
        return entry.clone();
    };
    let mut next = entry.clone();
    let listing = next.listing_mut();
    listing.additions = stat.additions;
    listing.deletions = stat.deletions;
    next
}

/// What a row should say the moment an action is sent, before any host has answered. The host
/// is the record and a later read replaces this, but the reader pressed the button and should
/// see the row answer at once: a closed pull request leaves an "open" list on the click, not
/// after the reads that follow.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestListOverride {
    pub state: PullRequestState,
    pub is_draft: Option<bool>,
    pub updated_at: String,
    /// Which action wrote it, so a failure takes back its own note and not a later one's.
    pub token: u64,
    /// When it was written, in the reader's clock (epoch milliseconds).
    pub at: i64,
}

/// The override an action writes over its row, or `None` for an action that does not change the
/// listed fields. `now_ms` is the reader's clock in epoch milliseconds.
pub fn pull_request_override_after_action(
    entry: &PullRequestListEntry,
    action: PullRequestAction,
    now_ms: i64,
    token: u64,
) -> Option<PullRequestListOverride> {
    let stamped = |state: PullRequestState, is_draft: Option<bool>| PullRequestListOverride {
        state,
        is_draft,
        updated_at: to_iso_string(now_ms),
        token,
        at: now_ms,
    };
    match action {
        PullRequestAction::Close => Some(stamped(PullRequestState::Closed, None)),
        PullRequestAction::Reopen => Some(stamped(PullRequestState::Open, None)),
        PullRequestAction::Merge => Some(stamped(PullRequestState::Merged, None)),
        PullRequestAction::Draft => Some(stamped(entry.state, Some(true))),
        PullRequestAction::Ready => Some(stamped(entry.state, Some(false))),
        _ => None,
    }
}

/// The rows with their pending answers written over them, and the ones the list's state filter no
/// longer holds dropped.
pub fn apply_pull_request_overrides<E: ListEntry>(
    entries: &[E],
    overrides: &IndexMap<String, PullRequestListOverride>,
    key_of: impl Fn(&E) -> String,
    state: PullRequestListState,
) -> Vec<E> {
    if overrides.is_empty() && state == PullRequestListState::All {
        return entries.to_vec();
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let applied = overrides.get(&key_of(entry));
        let effective_state = applied.map_or(entry.listing().state, |applied| applied.state);
        if !list_state_matches(state, effective_state) {
            continue;
        }
        match applied {
            None => out.push(entry.clone()),
            Some(applied) => {
                let mut next = entry.clone();
                let listing = next.listing_mut();
                listing.state = applied.state;
                if let Some(is_draft) = applied.is_draft {
                    listing.is_draft = is_draft;
                }
                listing.updated_at = applied.updated_at.clone();
                out.push(next);
            }
        }
    }
    out
}

/// A fresh answer with the rows it did not change kept as they were, so a memoized row whose data
/// is the same does not render again. Every refresh otherwise rebuilds every entry, and a hundred
/// rows repaint for the one that moved.
///
/// Value-level only: an owned `Vec` cannot hand back the previous allocation, so a caller that
/// needs identity (the GPUI row cache) compares keys and values itself.
pub fn reuse_pull_request_entries<E: ListEntry + PartialEq>(
    previous: &[E],
    next: &[E],
    key_of: impl Fn(&E) -> String,
) -> Vec<E> {
    if previous.is_empty() {
        return next.to_vec();
    }
    let held: IndexMap<String, &E> = previous
        .iter()
        .map(|entry| (key_of(entry), entry))
        .collect();
    let mut reused = 0;
    let out: Vec<E> = next
        .iter()
        .map(|entry| match held.get(&key_of(entry)) {
            Some(before) if *before == entry => {
                reused += 1;
                (*before).clone()
            }
            _ => entry.clone(),
        })
        .collect();
    if reused == next.len()
        && previous.len() == next.len()
        && previous
            .iter()
            .zip(next)
            .all(|(before, entry)| key_of(before) == key_of(entry))
    {
        previous.to_vec()
    } else {
        out
    }
}

/// How long a read that disagrees is taken for a stale one rather than for news.
const PULL_REQUEST_OVERRIDE_TRUST_MS: i64 = 60_000;

/// The overrides an answer has confirmed, dropped; the rest kept. A read that started before
/// the action can land after it and still say the old thing, so an override is not cleared
/// because an answer arrived but because the answer agrees: the row is there in the state the
/// override said. A row that is absent says nothing (the authored and reviewing groups are read
/// apart from the feed, and a page is only a page), so absence never confirms. A row present in
/// another state is taken for a stale read for a minute, and for the host's news after that,
/// which is how a pull request reopened elsewhere comes back.
pub fn settle_pull_request_overrides<E: ListEntry>(
    overrides: &IndexMap<String, PullRequestListOverride>,
    answered: &[E],
    key_of: impl Fn(&E) -> String,
    now_ms: i64,
) -> IndexMap<String, PullRequestListOverride> {
    if overrides.is_empty() {
        return overrides.clone();
    }
    let by_key: IndexMap<String, &E> = answered
        .iter()
        .map(|entry| (key_of(entry), entry))
        .collect();
    let mut kept = IndexMap::new();
    for (key, applied) in overrides {
        let Some(row) = by_key.get(key.as_str()) else {
            kept.insert(key.clone(), applied.clone());
            continue;
        };
        let listing = row.listing();
        let agrees = listing.state == applied.state
            && applied
                .is_draft
                .is_none_or(|is_draft| listing.is_draft == is_draft);
        let outranked = now_ms - applied.at > PULL_REQUEST_OVERRIDE_TRUST_MS;
        if !agrees && !outranked {
            kept.insert(key.clone(), applied.clone());
        }
    }
    kept
}

/// The row actions speed mode offers, in the order they are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestQuickAction {
    Close,
    Merge,
    Ready,
    Reopen,
}

impl PullRequestQuickAction {
    fn action(self) -> PullRequestAction {
        match self {
            Self::Close => PullRequestAction::Close,
            Self::Merge => PullRequestAction::Merge,
            Self::Ready => PullRequestAction::Ready,
            Self::Reopen => PullRequestAction::Reopen,
        }
    }
}

/// Which actions each host can take on a listed row, before any row's detail is read. Hosts are
/// matched by the host summary; a server that does not report actions per host only ever offered
/// these on GitHub, so that is what it keeps getting.
pub fn pull_request_host_actions(
    providers: &[PullRequestProviderSummary],
) -> impl Fn(&PullRequestListEntry) -> HashSet<PullRequestAction> {
    let by_host: IndexMap<String, HashSet<PullRequestAction>> = providers
        .iter()
        .filter_map(|provider| {
            provider.actions.as_ref().map(|actions| {
                (
                    provider.host.as_str().to_lowercase(),
                    actions.iter().copied().collect(),
                )
            })
        })
        .collect();
    let legacy: HashSet<PullRequestAction> = [
        PullRequestAction::Close,
        PullRequestAction::Merge,
        PullRequestAction::Ready,
        PullRequestAction::Reopen,
    ]
    .into_iter()
    .collect();
    move |entry: &PullRequestListEntry| {
        by_host
            .get(&entry.host.as_str().to_lowercase())
            .cloned()
            .unwrap_or_else(|| {
                if entry.provider.as_str() == "github" {
                    legacy.clone()
                } else {
                    HashSet::new()
                }
            })
    }
}

/// The quick actions a row offers: what its state allows, narrowed to what its host can do.
pub fn pull_request_quick_actions(
    entry: &PullRequestListEntry,
    host_actions: &HashSet<PullRequestAction>,
) -> Vec<PullRequestQuickAction> {
    let for_state: &[PullRequestQuickAction] = match entry.state {
        PullRequestState::Merged => &[],
        PullRequestState::Closed => &[PullRequestQuickAction::Reopen],
        PullRequestState::Open if entry.is_draft => {
            &[PullRequestQuickAction::Close, PullRequestQuickAction::Ready]
        }
        PullRequestState::Open => &[PullRequestQuickAction::Close, PullRequestQuickAction::Merge],
    };
    for_state
        .iter()
        .copied()
        .filter(|action| host_actions.contains(&action.action()))
        .collect()
}

#[cfg(test)]
#[path = "pull_request_list_logic_tests/mod.rs"]
mod tests;
