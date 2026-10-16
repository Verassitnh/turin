//! Summaries and watch fingerprints, read in batches
//! (`PullRequestSummaryRead` / `PullRequestWatchFingerprintRead` and their `RequestResolver`s).

use std::future::Future;
use std::pin::Pin;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;
use tokio::sync::oneshot;

use super::batcher::{BatchResult, Batcher, Entry, Queued};
use super::errors::GitHubPullRequestApiError;
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, GraphqlRead, Inner, RepositorySelector,
    STAT_ALIASES_PER_REQUEST, STAT_REQUEST_CONCURRENCY, SUMMARY_BATCH_WINDOW,
    parse_repository_selector, variables,
};
use crate::github::pull_request_json::{
    ChangeRequestSelector, GitHubPullRequestSummary, GitHubPullRequestWatchFingerprint,
    build_pull_request_summaries_graphql_query,
    build_pull_request_watch_fingerprints_graphql_query, decode_pull_request_summaries_json,
    decode_pull_request_watch_fingerprints_json, pull_request_summary_graphql_query,
};
use crate::source_control_core::ambient;
use crate::source_control_core::pull_request_provider::ProviderChangeRequestSummary;

impl From<GitHubPullRequestSummary> for ProviderChangeRequestSummary {
    fn from(summary: GitHubPullRequestSummary) -> Self {
        Self {
            number: summary.number,
            title: summary.title,
            url: summary.url,
            head_branch: summary.head_branch,
            base_branch: summary.base_branch,
            state: summary.state,
            is_draft: Some(summary.is_draft),
            closed_at: Some(summary.closed_at),
            merged_at: Some(summary.merged_at),
            updated_at: summary.updated_at,
            author: Some(summary.author),
            additions: Some(summary.additions),
            deletions: Some(summary.deletions),
            changed_files: Some(summary.changed_files),
            review_decision: Some(summary.review_decision),
            checks_state: Some(summary.checks_state),
            mergeability: Some(summary.mergeability),
            stack: summary.stack,
        }
    }
}

type Resolver<Res> = fn(
    GitHubPullRequestApi,
    Vec<Entry<GitHubPullRequestTarget, Res>>,
) -> Pin<Box<dyn Future<Output = ()> + Send>>;

/// The group a request batches with: host, pinned credential, and rate-limit scope.
fn batch_key(host: &str) -> String {
    serde_json::to_string(&serde_json::json!([
        host.to_lowercase(),
        ambient::pinned_credential().map(|credential| credential.credential_fingerprint),
        ambient::credential_scope(),
    ]))
    .expect("a key of strings serializes")
}

fn selector(target: &GitHubPullRequestTarget) -> ChangeRequestSelector {
    ChangeRequestSelector {
        repository: target.repository.clone(),
        number: target.number,
    }
}

fn fail_all<Res>(
    entries: Vec<Entry<GitHubPullRequestTarget, Res>>,
    error: GitHubPullRequestApiError,
) {
    for entry in entries {
        let _ = entry.reply.send(Err(error.clone()));
    }
}

impl GitHubPullRequestApi {
    /// Queues a request on `select`'s batcher and waits for its answer.
    async fn submit<Res: Send + 'static>(
        &self,
        select: fn(&Inner) -> &Batcher<GitHubPullRequestTarget, Res>,
        resolve: Resolver<Res>,
        target: &GitHubPullRequestTarget,
    ) -> BatchResult<Res> {
        let key = batch_key(&target.host);
        let (reply, answer) = oneshot::channel();
        let entry = Entry {
            request: target.clone(),
            ambient: ambient::capture(),
            reply,
        };
        match select(&self.inner).push(&key, entry, STAT_ALIASES_PER_REQUEST) {
            Queued::Joined => {}
            Queued::Opened(id) => {
                let api = self.clone();
                self.spawn_task(async move {
                    tokio::time::sleep(SUMMARY_BATCH_WINDOW).await;
                    let entries = select(&api.inner).take(&key, id);
                    if !entries.is_empty() {
                        resolve(api, entries).await;
                    }
                });
            }
            Queued::Full(entries) => {
                let api = self.clone();
                self.spawn_task(async move { resolve(api, entries).await });
            }
        }
        answer.await.unwrap_or_else(|_| {
            Err(Self::read_error_message(
                &target.cwd,
                "getPullRequestSummary",
                "The batched read was dropped before it answered.",
            ))
        })
    }

    /// One pull request's summary, through the same aliased query the batch uses.
    async fn view_pull_request_summary(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestSummary, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let summaries = self
            .graphql_read(
                GraphqlRead {
                    cwd: &target.cwd,
                    host: &target.host,
                    operation: "getPullRequestSummary",
                    allow_reserve: false,
                    variables: Some(variables(serde_json::json!({
                        "owner": owner, "name": name, "number": target.number
                    }))),
                    query: pull_request_summary_graphql_query(target.host == "github.com"),
                },
                decode_pull_request_summaries_json,
            )
            .await?;
        summaries.get(&0).cloned().ok_or_else(|| {
            Self::read_error_message(
                &target.cwd,
                "getPullRequestSummary",
                format!(
                    "GitHub answered nothing for {}#{}.",
                    target.repository, target.number
                ),
            )
        })
    }

    /// Summaries asked for together, on one host under one credential, share aliased GraphQL reads
    /// of twenty-five: the background sync reads every linked pull request each minute, and one
    /// `gh pr view` apiece is most of what it spends. Whatever the batch cannot answer — a selector
    /// GraphQL cannot address, a pull request GitHub returned nothing for — is read on its own.
    async fn resolve_summaries(
        &self,
        entries: Vec<Entry<GitHubPullRequestTarget, ProviderChangeRequestSummary>>,
    ) {
        let first = entries[0].request.clone();
        let batchable: Vec<usize> = (0..entries.len())
            .filter(|&index| {
                build_pull_request_summaries_graphql_query(
                    &[selector(&entries[index].request)],
                    false,
                )
                .is_some()
            })
            .collect();
        // Stack membership rides along where GitHub serves stacks, so the background sync can skip
        // the REST stack read for pull requests that are in none.
        let selectors: Vec<_> = batchable
            .iter()
            .map(|&index| selector(&entries[index].request))
            .collect();
        let document =
            build_pull_request_summaries_graphql_query(&selectors, first.host == "github.com");
        let batched = match document {
            None => Ok(IndexMap::new()),
            Some(document) => {
                self.graphql_read(
                    GraphqlRead {
                        cwd: &first.cwd,
                        host: &first.host,
                        operation: "getPullRequestSummary",
                        allow_reserve: false,
                        variables: Some(document.variables),
                        query: document.query,
                    },
                    decode_pull_request_summaries_json,
                )
                .await
            }
        };
        // A GraphQL error anywhere fails the whole document — one repository gone or out of
        // reach — so a batch that could not be read leaves every entry to its own read. A paused
        // budget is the exception: reading one at a time would only spend what is being saved.
        let summaries = match batched {
            Ok(summaries) => summaries,
            Err(error) if error.is_rate_limit_paused() => {
                fail_all(entries, error);
                return;
            }
            Err(error) => {
                tracing::debug!(%error, "batched pull request summary read failed");
                IndexMap::new()
            }
        };
        let mut unanswered = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let summary = batchable
                .iter()
                .position(|&batched| batched == index)
                .and_then(|position| summaries.get(&(position as u64)));
            match summary {
                Some(summary) => {
                    let _ = entry.reply.send(Ok(summary.clone().into()));
                }
                None => unanswered.push(entry),
            }
        }
        stream::iter(unanswered)
            .map(|entry| async move {
                let result = self
                    .view_pull_request_summary(&entry.request)
                    .await
                    .map(ProviderChangeRequestSummary::from);
                let _ = entry.reply.send(result);
            })
            .buffer_unordered(STAT_REQUEST_CONCURRENCY)
            .collect::<()>()
            .await;
    }

    // Every watched pull request on a host in one read per pass. A pull request the batch has no
    // answer for gets null, and its watch reads it in full; a failed batch fails every entry.
    async fn resolve_watch_fingerprints(
        &self,
        entries: Vec<Entry<GitHubPullRequestTarget, Option<GitHubPullRequestWatchFingerprint>>>,
    ) {
        let first = entries[0].request.clone();
        let batchable: Vec<usize> = (0..entries.len())
            .filter(|&index| {
                build_pull_request_watch_fingerprints_graphql_query(&[selector(
                    &entries[index].request,
                )])
                .is_some()
            })
            .collect();
        let selectors: Vec<_> = batchable
            .iter()
            .map(|&index| selector(&entries[index].request))
            .collect();
        let read = match build_pull_request_watch_fingerprints_graphql_query(&selectors) {
            None => Ok(IndexMap::new()),
            Some(document) => {
                self.graphql_read(
                    GraphqlRead {
                        cwd: &first.cwd,
                        host: &first.host,
                        operation: "getPullRequestWatchFingerprint",
                        allow_reserve: false,
                        variables: Some(document.variables),
                        query: document.query,
                    },
                    decode_pull_request_watch_fingerprints_json,
                )
                .await
            }
        };
        match read {
            Err(error) => fail_all(entries, error),
            Ok(fingerprints) => {
                for (index, entry) in entries.into_iter().enumerate() {
                    let fingerprint = batchable
                        .iter()
                        .position(|&batched| batched == index)
                        .and_then(|position| fingerprints.get(&(position as u64)))
                        .cloned();
                    let _ = entry.reply.send(Ok(fingerprint));
                }
            }
        }
    }

    pub async fn get_pull_request_summary(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<ProviderChangeRequestSummary, GitHubPullRequestApiError> {
        self.submit(
            |inner| &inner.summaries,
            |api, entries| {
                let ambient = entries[0].ambient.clone();
                Box::pin(async move { ambient.run(api.resolve_summaries(entries)).await })
            },
            target,
        )
        .await
    }

    /// What a watch compares between passes, batched like summaries. `None` when GitHub gave no
    /// answer for this pull request, so the watch reads it in full instead.
    pub async fn get_pull_request_watch_fingerprint(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<Option<GitHubPullRequestWatchFingerprint>, GitHubPullRequestApiError> {
        self.submit(
            |inner| &inner.watch_fingerprints,
            |api, entries| {
                let ambient = entries[0].ambient.clone();
                Box::pin(async move { ambient.run(api.resolve_watch_fingerprints(entries)).await })
            },
            target,
        )
        .await
    }
}
