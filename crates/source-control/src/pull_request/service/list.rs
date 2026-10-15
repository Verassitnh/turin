//! Listings of `PullRequestService.ts`: the cross-repository list with filters, cursors,
//! involvement and batched search with a per-repository fallback, and the deferred line counts.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::FutureExt;
use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::base_schemas::{ProjectId, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestDiffStat, PullRequestDraftFilter, PullRequestInvolvement, PullRequestListCursor,
    PullRequestListEntry, PullRequestListFilters, PullRequestListInput,
    PullRequestListProjectError, PullRequestListResult, PullRequestListStatsInput,
    PullRequestListStatsResult, PullRequestProviderSummary, PullRequestRef,
    PullRequestReviewFilter, resolve_pull_request_author_filter,
};

use super::cursor::{ListCursor, list_cursor_at, next_list_cursor, parse_list_cursor};
use super::epochs::{CredentialRef, REF_EPOCH_CAPACITY, ref_cache_key};
use super::errors::{
    PullRequestError, is_provider_unusable, operation_error, positive, provider_detail, tne,
    to_pull_request_error,
};
use super::projects::{ProjectFilter, SupportedProject};
use super::viewers::ViewerTarget;
use super::{
    DEFAULT_REPOSITORY_LIST_LIMIT, Inner, LIST_STATS_CACHE_TTL_MS, REPOSITORY_CONCURRENCY,
    REPOSITORY_SEARCH_CHUNK, SEARCH_VISIBILITY_TTL_MS,
};
use crate::source_control_core::pull_request_provider::{
    ListChangeRequestStatsInput, ListChangeRequestsAcrossInput, ListChangeRequestsInput,
    ProviderChangeRequest, ProviderRepositoryRef, PullRequestOperation,
};

/// Which repository this slice came from, which is what a cursor for it is filed under.
struct RepositoryBatch {
    key: String,
    entries: Vec<PullRequestListEntry>,
    errors: Vec<PullRequestListProjectError>,
    truncated: bool,
    next_cursor: Option<String>,
}

/// The narrowings a row can be judged by from its own fields, applied here rather than trusted
/// to the host. Only GitHub is asked to narrow a listing for itself; every other provider
/// answers unnarrowed, and without this pass a draft filter or a label filter would be sent,
/// accepted and quietly ignored. Idempotent for the hosts that did narrow.
///
/// `checks` is absent because no listed row carries its check state: that one filter is the
/// host's alone, and a row nobody narrowed stays rather than being guessed at.
fn matches_row_filters(
    item: &ProviderChangeRequest,
    filters: Option<&PullRequestListFilters>,
    viewer: &str,
) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    let labels: HashSet<String> = item
        .labels
        .iter()
        .map(|label| label.name.as_str().trim().to_lowercase())
        .collect();
    let holds = |label: &str| labels.contains(&label.trim().to_lowercase());
    filters
        .draft
        .is_none_or(|draft| item.is_draft == (draft == PullRequestDraftFilter::Only))
        // Judged on the provider row rather than the entry, because the two absences mean
        // different things and the entry keeps only one of them: `null` is a host that summarises
        // its reviews saying there is no decision yet, which is what "none" asks for, while
        // `undefined` is a host that does not summarise at all — an unjudgeable row, left alone
        // the way an unreadable check state is.
        && filters.review.is_none_or(|review| match item.review_decision {
            None => true,
            Some(decision) => match review {
                PullRequestReviewFilter::None => decision.is_none(),
                PullRequestReviewFilter::Approved => {
                    decision == Some(turin_contracts::pull_request::PullRequestReviewDecision::Approved)
                }
                PullRequestReviewFilter::ChangesRequested => {
                    decision
                        == Some(turin_contracts::pull_request::PullRequestReviewDecision::ChangesRequested)
                }
                PullRequestReviewFilter::ReviewRequired => {
                    decision
                        == Some(turin_contracts::pull_request::PullRequestReviewDecision::ReviewRequired)
                }
            },
        })
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
            item.author.as_ref().is_some_and(|actor| {
                actor.login.as_str().to_lowercase()
                    == resolve_pull_request_author_filter(author.as_str(), Some(viewer))
                        .to_lowercase()
            })
        })
}

fn to_entry(
    project: &SupportedProject,
    item: &ProviderChangeRequest,
    viewer: &str,
    observed_at: i64,
) -> PullRequestListEntry {
    let viewer = viewer.to_lowercase();
    PullRequestListEntry {
        stack: item.stack.clone(),
        provider: project.api.kind(),
        host: tne(&project.host),
        project_id: project.project.id.clone(),
        project_title: project.project.title.clone(),
        repository: tne(&project.repository),
        number: positive(item.number),
        title: tne(&item.title),
        url: tne(&item.url),
        author: item.author.clone(),
        head_branch: tne(&item.head_branch),
        base_branch: tne(&item.base_branch),
        state: item.state,
        is_draft: item.is_draft,
        mergeability: item.mergeability,
        additions: item.additions,
        deletions: item.deletions,
        created_at: item.created_at.clone(),
        updated_at: item.updated_at.clone(),
        observed_at: Some(observed_at as f64),
        checks_state: item.checks_state.flatten(),
        viewer_review_requested: item
            .author
            .as_ref()
            .map(|author| author.login.as_str().to_lowercase())
            .as_deref()
            != Some(viewer.as_str())
            && item
                .review_request_logins
                .iter()
                .any(|login| login.to_lowercase() == viewer),
        labels: item.labels.clone(),
        review_decision: item.review_decision.flatten(),
    }
}

/// One repository's slice, kept apart from the request that asked for it.
struct ListRequest {
    state: turin_contracts::pull_request::PullRequestListState,
    involvement: PullRequestInvolvement,
    query: Option<String>,
    filters: Option<PullRequestListFilters>,
    limit: u32,
}

impl Inner {
    /// The cached listing: concurrent identical reads share one request, and an answer is shared
    /// for a short while. Reads that must not share go through `invalidate`.
    pub(crate) async fn list(
        self: &Arc<Self>,
        input: &PullRequestListInput,
    ) -> Result<PullRequestListResult, PullRequestError> {
        let listings_epoch = self.state.lock().epochs.listings_epoch;
        let key = json!([
            listings_epoch,
            serde_json::to_value(input.state).unwrap_or(Value::Null),
            input
                .involvement
                .map(|i| serde_json::to_value(i).unwrap_or(Value::Null)),
            // Positional so two identical filter sets key alike however their record was assembled.
            input.filters.as_ref().map(|filters| json!([
                filters
                    .draft
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
                filters
                    .review
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
                filters
                    .checks
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null)),
                filters.author.as_ref().map(|v| v.as_str()),
                filters.labels.as_ref().map(|groups| groups
                    .iter()
                    .map(|group| group.iter().map(|l| l.as_str()).collect::<Vec<_>>())
                    .collect::<Vec<_>>()),
                filters
                    .excluded_labels
                    .as_ref()
                    .map(|labels| labels.iter().map(|l| l.as_str()).collect::<Vec<_>>()),
            ])),
            input.project_id.as_ref().map(|id| id.as_str()),
            // Sorted so the same narrowing keys alike however the caller ordered it.
            input.project_ids.as_ref().map(|ids| {
                let mut ids: Vec<&str> = ids.iter().map(|id| id.as_str()).collect();
                ids.sort();
                ids
            }),
            input.host.as_ref().map(|host| host.as_str()),
            input.limit,
            input.query.as_ref().map(|query| query.as_str()),
            input.cursors.as_ref().map(|cursors| {
                let mut entries: Vec<(&str, &str)> = cursors
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                entries
            }),
        ])
        .to_string();
        let inner = self.clone();
        let input = input.clone();
        self.list_cache
            .get(&key, move || {
                async move { inner.list_uncached(&input).await }.boxed()
            })
            .await
    }

    async fn list_uncached(
        self: &Arc<Self>,
        input: &PullRequestListInput,
    ) -> Result<PullRequestListResult, PullRequestError> {
        let involvement = input.involvement.unwrap_or(PullRequestInvolvement::All);
        // Refused whole rather than per repository: a cursor is only ever a value this service
        // issued, so one that does not read as one means the page is sending something it made
        // up, and reading part of the listing under that assumption would quietly lose rows.
        let continuation: Option<HashMap<String, ListCursor>> = match &input.cursors {
            None => None,
            Some(cursors) => {
                let mut decoded = HashMap::new();
                for (key, raw) in cursors {
                    let Some(cursor) = parse_list_cursor(raw.as_str()) else {
                        return Err(operation_error(
                            "list",
                            "The list could not be carried on from where it left off.",
                        ));
                    };
                    decoded.insert(key.as_str().to_owned(), cursor);
                }
                Some(decoded)
            }
        };
        let workspace = self
            .list_workspace_projects(&ProjectFilter {
                project_id: input.project_id.clone(),
                project_ids: input.project_ids.clone(),
                host: input.host.as_ref().map(|host| host.as_str().to_owned()),
            })
            .await?;
        let projects = &workspace.supported;
        let mut project_counts: HashMap<&str, u64> = HashMap::new();
        for project in projects {
            *project_counts.entry(project.host.as_str()).or_default() += 1;
        }

        let targets: Vec<ViewerTarget> = projects.iter().map(ViewerTarget::from).collect();
        let viewer_results = self
            .resolve_viewers(&targets, &workspace.viewer_roots, false)
            .await;
        let mut viewers: IndexMap<String, String> = IndexMap::new();
        for result in &viewer_results {
            if let Some(viewer) = &result.viewer {
                viewers.insert(result.host.clone(), viewer.clone());
            }
        }

        // One summary per host, which is what the viewer lookup already answers for: two GitHub
        // hosts sign in separately, so collapsing them by kind would report one as the other.
        let mut providers: Vec<PullRequestProviderSummary> = viewer_results
            .iter()
            .map(|result| {
                let capabilities = projects
                    .iter()
                    .find(|project| project.host == result.host)
                    .map(|project| project.api.capabilities());
                PullRequestProviderSummary {
                    host: tne(&result.host),
                    kind: result.kind.clone(),
                    searches_on_host: capabilities.is_some_and(|c| c.search),
                    actions: Some(capabilities.map(|c| c.actions.clone()).unwrap_or_default()),
                    project_count: positive(
                        project_counts
                            .get(result.host.as_str())
                            .copied()
                            .unwrap_or(1),
                    ),
                    configured: result.viewer.is_some(),
                    detail: result
                        .error
                        .as_ref()
                        .map(|error| tne(provider_detail(error))),
                }
            })
            .collect();
        providers.extend(workspace.unimplemented.iter().map(|(host, (kind, count))| {
            PullRequestProviderSummary {
                host: tne(host),
                kind: kind.clone(),
                searches_on_host: false,
                actions: Some(Vec::new()),
                project_count: positive(*count),
                configured: false,
                detail: Some(tne("This host cannot be browsed here yet.")),
            }
        }));

        let result_viewers = |viewers: &IndexMap<String, String>| {
            viewers
                .iter()
                .map(|(host, viewer)| (tne(host), tne(viewer)))
                .collect::<IndexMap<TrimmedNonEmptyString, TrimmedNonEmptyString>>()
        };

        // A continued listing reads only the repositories it was asked to carry on with: every
        // other one is already on the page, and reading it again is the whole cost this is here to
        // avoid. The host summaries above stay over the whole workspace, because the switcher they
        // fill is about the workspace rather than about this slice.
        let selected: Vec<&SupportedProject> = projects
            .iter()
            .filter(|project| {
                continuation
                    .as_ref()
                    .is_none_or(|cursors| cursors.contains_key(&project.cursor_key))
            })
            .collect();
        let readable: Vec<&SupportedProject> = selected
            .iter()
            .copied()
            .filter(|project| viewers.contains_key(&project.host))
            .collect();
        // A host that could not be read still has projects, and they are absent from the list.
        // Reporting them keeps "N repositories were unavailable" honest instead of dropping them.
        let unreadable: Vec<PullRequestListProjectError> = selected
            .iter()
            .filter(|project| !viewers.contains_key(&project.host))
            .map(|project| PullRequestListProjectError {
                project_id: project.project.id.clone(),
                project_title: project.project.title.clone(),
                message: tne(format!("{} could not be read.", project.repository)),
            })
            .collect();
        if readable.is_empty() {
            // No host this request covers can be read, so it is not a per-project problem. An
            // unusable host is preferred as the reported cause because it names the fix; a host
            // that merely failed reports as a failed operation rather than as a signed-out CLI,
            // which would send the reader to `auth login` over a transient error.
            //
            // Only the hosts this request was actually going to read: a continuation that named
            // nothing has asked for nothing, and a host it never mentioned being signed out is no
            // reason to refuse it.
            let errors: Vec<_> = viewer_results
                .iter()
                .filter_map(|result| {
                    let error = result.error.as_ref()?;
                    selected
                        .iter()
                        .any(|project| project.host == result.host)
                        .then_some(error)
                })
                .collect();
            let blocking = errors
                .iter()
                .find(|error| is_provider_unusable(error))
                .or_else(|| errors.first());
            if let Some(blocking) = blocking {
                return Err(to_pull_request_error("list", (*blocking).clone()));
            }
            return Ok(PullRequestListResult {
                viewers: result_viewers(&viewers),
                providers,
                entries: Vec::new(),
                errors: Vec::new(),
                truncated: false,
                next_cursors: IndexMap::new(),
            });
        }

        let request = Arc::new(ListRequest {
            state: input.state,
            involvement,
            query: input.query.as_ref().map(|query| query.as_str().to_owned()),
            filters: input.filters.clone(),
            limit: input
                .limit
                .unwrap_or(DEFAULT_REPOSITORY_LIST_LIMIT)
                .clamp(0, i64::from(u32::MAX)) as u32,
        });
        let continuation = Arc::new(continuation);
        let viewers = Arc::new(viewers);

        // A host with a search across repositories is asked once for all of them; everyone else is
        // asked once each. Repositories standing at different points of the same listing are
        // different questions, so they are grouped by the boundary they carry on from.
        let mut together: IndexMap<String, Vec<SupportedProject>> = IndexMap::new();
        let mut separate: Vec<SupportedProject> = Vec::new();
        for project in readable {
            if !project
                .api
                .supports(PullRequestOperation::ListChangeRequestsAcross)
            {
                separate.push(project.clone());
                continue;
            }
            let cursor_key = continuation
                .as_ref()
                .as_ref()
                .and_then(|cursors| cursors.get(&project.cursor_key))
                .map(|cursor| cursor.updated_before.as_str())
                .unwrap_or("");
            together
                .entry(format!("{}\n{cursor_key}", project.host))
                .or_default()
                .push(project.clone());
        }
        enum Read {
            One(Box<SupportedProject>),
            Chunk(Vec<SupportedProject>),
        }
        let mut reads: Vec<Read> = separate
            .into_iter()
            .map(|project| Read::One(Box::new(project)))
            .collect();
        for group in together.into_values() {
            for chunk in group.chunks(REPOSITORY_SEARCH_CHUNK) {
                reads.push(Read::Chunk(chunk.to_vec()));
            }
        }
        let batches: Vec<RepositoryBatch> = stream::iter(reads)
            .map(|read| {
                let inner = self.clone();
                let request = request.clone();
                let continuation = continuation.clone();
                let viewers = viewers.clone();
                async move {
                    match read {
                        Read::One(project) => {
                            vec![
                                inner
                                    .read_repository(&project, &request, &continuation, &viewers)
                                    .await,
                            ]
                        }
                        Read::Chunk(chunk) => {
                            inner
                                .read_together(&chunk, &request, &continuation, &viewers)
                                .await
                        }
                    }
                }
            })
            .buffered(REPOSITORY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect();

        let mut next_cursors = IndexMap::new();
        for batch in &batches {
            if let Some(cursor) = &batch.next_cursor
                && let Ok(cursor) = PullRequestListCursor::new(cursor)
            {
                next_cursors.insert(tne(&batch.key), cursor);
            }
        }
        let mut entries: Vec<PullRequestListEntry> = batches
            .iter()
            .flat_map(|batch| batch.entries.iter().cloned())
            .collect();
        entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(PullRequestListResult {
            viewers: result_viewers(&viewers),
            providers,
            entries,
            errors: unreadable
                .into_iter()
                .chain(
                    batches
                        .iter()
                        .flat_map(|batch| batch.errors.iter().cloned()),
                )
                .collect(),
            truncated: batches.iter().any(|batch| batch.truncated),
            next_cursors,
        })
    }

    /// One repository asked on its own. What every host without a search across repositories
    /// does, and what a batched read falls back to for a repository it could not answer for.
    async fn read_repository(
        self: &Arc<Self>,
        project: &SupportedProject,
        request: &ListRequest,
        continuation: &Option<HashMap<String, ListCursor>>,
        viewers: &IndexMap<String, String>,
    ) -> RepositoryBatch {
        let viewer = viewers.get(&project.host).cloned().unwrap_or_default();
        let key = project.cursor_key.clone();
        let cursor = continuation
            .as_ref()
            .and_then(|cursors| cursors.get(&project.cursor_key));
        // Captured before the provider read so a slow response keeps its original freshness
        // through caches.
        let observed_at = self.clock.now_millis();
        let page = project
            .api
            .list_change_requests(ListChangeRequestsInput {
                repo: ProviderRepositoryRef {
                    cwd: project.project.workspace_root.as_str().to_owned(),
                    repository: project.repository.clone(),
                    host: project.host.clone(),
                },
                state: request.state,
                involvement: request.involvement,
                viewer: viewer.clone(),
                limit: request.limit,
                // Each host matches this its own way, and one that cannot match text at all
                // answers unnarrowed rather than failing.
                query: request.query.clone(),
                filters: request.filters.clone(),
                // Only the two fields a host can act on: which rows have already been sent at the
                // boundary instant is this service's business, not a provider's.
                cursor: cursor.map(ListCursor::for_provider),
            })
            .await;
        match page {
            Ok(page) => {
                // The boundary instant was asked for inclusively, so the rows already sent at it
                // come back with the slice. Dropping them here rather than asking for strictly
                // older is what keeps their neighbours at the same instant from being skipped.
                let items: Vec<&ProviderChangeRequest> = page
                    .items
                    .iter()
                    .filter(|item| cursor.is_none_or(|cursor| !cursor.has_sent(item)))
                    .collect();
                RepositoryBatch {
                    key,
                    entries: items
                        .iter()
                        .filter(|item| matches_row_filters(item, request.filters.as_ref(), &viewer))
                        .map(|item| to_entry(project, item, &viewer, observed_at))
                        .collect(),
                    errors: Vec::new(),
                    truncated: page.truncated,
                    next_cursor: if page.continues && page.truncated {
                        next_list_cursor(cursor, &page.items, items.len(), page.cursor_advance)
                    } else {
                        None
                    },
                }
            }
            // One unreachable repository must not blank the page. A host-level failure is
            // already reported through `providers`, so it degrades the same way here.
            Err(_) => RepositoryBatch {
                key,
                entries: Vec::new(),
                errors: vec![PullRequestListProjectError {
                    project_id: project.project.id.clone(),
                    project_title: project.project.title.clone(),
                    message: tne(format!("{} could not be read.", project.repository)),
                }],
                truncated: false,
                next_cursor: None,
            },
        }
    }

    async fn read_separately(
        self: &Arc<Self>,
        chunk: &[SupportedProject],
        request: &ListRequest,
        continuation: &Option<HashMap<String, ListCursor>>,
        viewers: &IndexMap<String, String>,
    ) -> Vec<RepositoryBatch> {
        let reads: Vec<_> = chunk
            .iter()
            .map(|project| self.read_repository(project, request, continuation, viewers))
            .collect();
        stream::iter(reads)
            .buffered(REPOSITORY_CONCURRENCY)
            .collect()
            .await
    }

    /// One host's repositories in one read. The slice is the newest `limit` rows across all of
    /// them, so it is split back up by repository here: the page still reports per project, and
    /// each repository still carries on from a cursor of its own.
    ///
    /// A read that fails is read the long way instead. The batch is an optimisation, and a host
    /// that could not answer one question about twelve repositories should not report twelve
    /// repositories as unreadable before anyone has asked it about them one at a time.
    async fn read_together(
        self: &Arc<Self>,
        chunk: &[SupportedProject],
        request: &ListRequest,
        continuation: &Option<HashMap<String, ListCursor>>,
        viewers: &IndexMap<String, String>,
    ) -> Vec<RepositoryBatch> {
        let first = &chunk[0];
        let viewer = viewers.get(&first.host).cloned().unwrap_or_default();
        let cursor_of = |project: &SupportedProject| -> Option<&ListCursor> {
            continuation
                .as_ref()
                .and_then(|cursors| cursors.get(&project.cursor_key))
        };
        let cursor = cursor_of(first);
        let observed_at = self.clock.now_millis();
        let page = first
            .api
            .list_change_requests_across(ListChangeRequestsAcrossInput {
                cwd: first.project.workspace_root.as_str().to_owned(),
                host: first.host.clone(),
                repositories: chunk
                    .iter()
                    .map(|project| project.repository.clone())
                    .collect(),
                state: request.state,
                involvement: request.involvement,
                viewer: viewer.clone(),
                limit: request.limit,
                query: request.query.clone(),
                filters: request.filters.clone(),
                cursor: cursor.map(ListCursor::for_provider),
            })
            .await;
        let page = match page {
            Ok(page) => page,
            Err(_) => {
                return self
                    .read_separately(chunk, request, continuation, viewers)
                    .await;
            }
        };
        let now = self.clock.now_millis();
        let mut rows: HashMap<String, Vec<ProviderChangeRequest>> = HashMap::new();
        {
            let mut state = self.state.lock();
            state
                .search_visible_at
                .retain(|_, visible_at| now - *visible_at <= SEARCH_VISIBILITY_TTL_MS);
            for item in &page.items {
                rows.entry(item.repository.trim().to_lowercase())
                    .or_default()
                    .push(item.base.clone());
                state
                    .search_visible_at
                    .insert(search_visibility_key(&first.host, &item.repository), now);
            }
        }
        // The oldest row of the whole slice, which is how far every repository in it has now
        // been read — including the ones that contributed nothing to it.
        let boundary: Option<&str> = page.items.iter().fold(None, |oldest, item| match oldest {
            Some(oldest) if item.updated_at.as_str() >= oldest => Some(oldest),
            _ => Some(item.updated_at.as_str()),
        });
        let mut results = Vec::with_capacity(chunk.len());
        let mut fallbacks: Vec<usize> = Vec::new();
        for (index, project) in chunk.iter().enumerate() {
            let fetched = rows
                .remove(&project.repository.trim().to_lowercase())
                .unwrap_or_default();
            // GitHub does not index every repository for search — a renamed one answers for its
            // old name with silence rather than with an error — so a repository the search said
            // nothing at all about is read on its own, once, before it is believed. Only on its
            // first slice: after that it has a boundary to carry on from, and silence past one
            // means the rows are older rather than absent. That keeps a search-invisible
            // repository from disappearing on a busy host, at the price of one request per
            // repository with nothing in the first slice — which run together, and only there.
            let last_visible = self
                .state
                .lock()
                .search_visible_at
                .get(&search_visibility_key(&project.host, &project.repository))
                .copied();
            let search_is_known_visible = !page.truncated
                && last_visible.is_some_and(|visible| now - visible <= SEARCH_VISIBILITY_TTL_MS);
            let cursor_here = cursor_of(project);
            if fetched.is_empty() && cursor_here.is_none() && !search_is_known_visible {
                fallbacks.push(index);
                results.push(None);
                continue;
            }
            let items: Vec<&ProviderChangeRequest> = fetched
                .iter()
                .filter(|item| cursor_here.is_none_or(|cursor| !cursor.has_sent(item)))
                .collect();
            results.push(Some(RepositoryBatch {
                key: project.cursor_key.clone(),
                entries: items
                    .iter()
                    .filter(|item| matches_row_filters(item, request.filters.as_ref(), &viewer))
                    .map(|item| to_entry(project, item, &viewer, observed_at))
                    .collect(),
                errors: Vec::new(),
                truncated: page.truncated,
                next_cursor: match boundary {
                    Some(boundary) if page.truncated => Some(list_cursor_at(
                        cursor_here,
                        boundary,
                        &fetched,
                        items.len() as u64,
                    )),
                    _ => None,
                },
            }));
        }
        let fallback_reads: Vec<_> = fallbacks
            .iter()
            .map(|&index| self.read_repository(&chunk[index], request, continuation, viewers))
            .collect();
        let fallback_batches: Vec<RepositoryBatch> = stream::iter(fallback_reads)
            .buffered(REPOSITORY_CONCURRENCY)
            .collect()
            .await;
        let mut fallback_batches = fallback_batches.into_iter();
        results
            .into_iter()
            .map(|batch| {
                batch.unwrap_or_else(|| fallback_batches.next().expect("one batch per fallback"))
            })
            .collect()
    }

    // -----------------------------------------------------------------------------------------
    // Line counts
    // -----------------------------------------------------------------------------------------

    /// The line counts for rows already on the page, which the listing left out because on GitHub
    /// they cost more than everything else on the row put together.
    ///
    /// One read per host rather than per row, and only for a host whose listing defers them; a
    /// row whose host answered with the counts in the first place is not here to be asked about.
    /// A ref that names no project this workspace has, or a repository that is not the one the
    /// project's remote points at, is dropped rather than refused: it is one row's two numbers,
    /// and the page that asked has already moved on.
    async fn list_stats_uncached(
        self: &Arc<Self>,
        refs: &[(ProjectId, String, u64)],
    ) -> Result<PullRequestListStatsResult, PullRequestError> {
        if refs.is_empty() {
            return Ok(PullRequestListStatsResult { stats: Vec::new() });
        }
        let workspace = self
            .list_workspace_projects(&ProjectFilter::default())
            .await?;
        let by_project: HashMap<&ProjectId, &SupportedProject> = workspace
            .supported
            .iter()
            .map(|project| (&project.project.id, project))
            .collect();
        let mut wanted: IndexMap<String, (&SupportedProject, u64)> = IndexMap::new();
        for (project_id, repository, number) in refs {
            let Some(project) = by_project.get(project_id) else {
                continue;
            };
            // The repository travels through the client, so it is checked against the project's
            // own remote rather than being handed to a provider verbatim.
            if !project
                .api
                .supports(PullRequestOperation::ListChangeRequestStats)
                || project.repository.to_lowercase() != repository.trim().to_lowercase()
            {
                continue;
            }
            wanted.insert(
                format!("{} {number}", project.project.id),
                (project, *number),
            );
        }
        let mut by_host: IndexMap<&str, Vec<(&SupportedProject, u64)>> = IndexMap::new();
        for (project, number) in wanted.into_values() {
            by_host
                .entry(project.host.as_str())
                .or_default()
                .push((project, number));
        }
        let reads: Vec<_> = by_host
            .into_values()
            .map(|entries| Self::read_stats_of_host(entries))
            .collect();
        let stats: Vec<Vec<PullRequestDiffStat>> = stream::iter(reads)
            .buffered(REPOSITORY_CONCURRENCY)
            .collect()
            .await;
        Ok(PullRequestListStatsResult {
            stats: stats.into_iter().flatten().collect(),
        })
    }

    /// One host's line counts, for the rows of it that were asked about.
    async fn read_stats_of_host(
        entries: Vec<(&SupportedProject, u64)>,
    ) -> Vec<PullRequestDiffStat> {
        let first = entries[0].0;
        let projects_by_repository: HashMap<String, &SupportedProject> = entries
            .iter()
            .map(|(project, number)| {
                (
                    format!("{} {number}", project.repository.to_lowercase()),
                    *project,
                )
            })
            .collect();
        let read = first
            .api
            .list_change_request_stats(ListChangeRequestStatsInput {
                cwd: first.project.workspace_root.as_str().to_owned(),
                host: first.host.clone(),
                change_requests: entries
                    .iter()
                    .map(|(project, number)| (project.repository.clone(), *number))
                    .collect(),
            })
            .await;
        // A row without its counts is a row the page already draws without them, so a host that
        // could not answer costs the numbers rather than the answer.
        let Ok(read) = read else {
            return Vec::new();
        };
        read.into_iter()
            .filter_map(|stat| {
                let project = projects_by_repository.get(&format!(
                    "{} {}",
                    stat.repository.to_lowercase(),
                    stat.number
                ))?;
                Some(PullRequestDiffStat {
                    project_id: project.project.id.clone(),
                    repository: tne(&project.repository),
                    number: positive(stat.number),
                    additions: stat.additions,
                    deletions: stat.deletions,
                })
            })
            .collect()
    }

    /// Counts belong to a PR, not a filtered page. Background reads and filter changes reuse
    /// them; explicit refreshes, mutations, and turns strand old and in-flight results.
    pub(crate) fn stats_cache_key(&self, key: &str) -> String {
        json!([self.state.lock().epochs.listings_epoch, key]).to_string()
    }

    pub(crate) fn record_stats(&self, key: String, value: PullRequestDiffStat, at: i64) {
        let mut state = self.state.lock();
        state.recent_stats.shift_remove(&key);
        state.recent_stats.insert(key, (at, value));
        if state.recent_stats.len() > REF_EPOCH_CAPACITY {
            state.recent_stats.shift_remove_index(0);
        }
    }

    /// Exact batches share in-flight reads; overlapping pages reuse each row already fetched.
    pub(crate) async fn list_stats(
        self: &Arc<Self>,
        input: &PullRequestListStatsInput,
    ) -> Result<PullRequestListStatsResult, PullRequestError> {
        let mut refs: Vec<PullRequestRef> = Vec::new();
        for reference in &input.refs {
            if let Ok(canonical) = self.canonical_ref(reference).await {
                refs.push(canonical);
            }
        }
        if refs.is_empty() {
            return Ok(PullRequestListStatsResult { stats: Vec::new() });
        }
        let now = self.clock.now_millis();
        let mut held: Vec<PullRequestDiffStat> = Vec::new();
        let mut missing: IndexMap<String, PullRequestRef> = IndexMap::new();
        for reference in &refs {
            let key = {
                let state = self.state.lock();
                json!([
                    state.epochs.listings_epoch,
                    ref_cache_key(&state.epochs, &CredentialRef::new(reference.clone()))
                ])
                .to_string()
            };
            let cached = self.state.lock().recent_stats.get(&key).cloned();
            match cached {
                Some((at, value)) if now - at < LIST_STATS_CACHE_TTL_MS => held.push(value),
                _ => {
                    missing.insert(key, reference.clone());
                }
            }
        }
        if missing.is_empty() {
            return Ok(PullRequestListStatsResult { stats: held });
        }
        let batch_key = {
            let state = self.state.lock();
            let mut rows: Vec<(String, String, u64, u64)> = missing
                .values()
                .map(|reference| {
                    (
                        reference.project_id.as_str().to_owned(),
                        reference.repository.as_str().to_owned(),
                        reference.number.get(),
                        state.epochs.ref_epoch(reference),
                    )
                })
                .collect();
            rows.sort_by_key(|row| format!("{} {} {}", row.0, row.1, row.2));
            json!([state.epochs.listings_epoch, rows]).to_string()
        };
        let lookup_refs: Vec<(ProjectId, String, u64)> = missing
            .values()
            .map(|reference| {
                (
                    reference.project_id.clone(),
                    reference.repository.as_str().to_owned(),
                    reference.number.get(),
                )
            })
            .collect();
        let inner = self.clone();
        let (result, at) = self
            .list_stats_cache
            .get(&batch_key, move || {
                async move {
                    let result = inner.list_stats_uncached(&lookup_refs).await?;
                    let at = inner.clock.now_millis();
                    Ok((result, at))
                }
                .boxed()
            })
            .await?;
        for (key, reference) in missing {
            let stat = result.stats.iter().find(|stat| {
                stat.project_id == reference.project_id
                    && stat.repository.as_str().to_lowercase()
                        == reference.repository.as_str().to_lowercase()
                    && stat.number == reference.number
            });
            if let Some(stat) = stat {
                self.record_stats(key, stat.clone(), at);
            }
        }
        held.extend(result.stats);
        Ok(PullRequestListStatsResult { stats: held })
    }
}

fn search_visibility_key(host: &str, repository: &str) -> String {
    format!("{host}\n{}", repository.trim().to_lowercase())
}
