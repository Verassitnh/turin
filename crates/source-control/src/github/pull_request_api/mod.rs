//! Port of upstream/packages/source-control-github/src/server/GitHubPullRequestApi.ts
//!
//! The whole `GitHubPullRequestApi` service: every pull request read and write GitHub's REST and
//! GraphQL APIs serve, over the `GitHubApi` transport. The upstream file is 2,857 lines; here it
//! is split by concern:
//!
//! - [`errors`]: the `GitHubPullRequestApiError` family.
//! - [`list`]: listing, searching, stats.
//! - [`summary`]: summaries and watch fingerprints, batched.
//! - [`detail`]: detail (core), preview, workflow runs awaiting approval.
//! - [`stack`]: the host-native stack.
//! - [`activity`]: activity, review threads, thread comments, avatars, viewer access.
//! - [`diff`]: the patch and file contents.
//! - [`files_viewed`]: files viewed.
//! - [`actions`]: every write (actions, comments, reviews, reactions, labels, reviewers, ...).
//!
//! Deviations from upstream, all mechanical:
//! - `Context.Service` + `Layer` becomes a plain struct built with [`GitHubPullRequestApi::new`].
//! - `RequestResolver` batching (10 ms window, 25 per batch, grouped by host + credential) is a
//!   small coalescer ([`batcher`]); each waiting request carries `ambient::capture()`.
//! - `withVerifiedCredential` exists in both forms: the closure form and
//!   [`GitHubPullRequestApi::verify_credential`] (identity plus pin) that the provider port uses.
//! - `FileSystem` is not a dependency: the stack rebase works in a `tempfile` directory.

pub mod actions;
pub mod activity;
pub mod batcher;
pub mod detail;
pub mod diff;
pub mod errors;
pub mod files_viewed;
pub mod list;
pub mod stack;
pub mod summary;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::task::JoinSet;
use turin_contracts::base_schemas::TrimmedNonEmptyString;

pub use self::actions::*;
pub use self::activity::{
    GetReviewThreadCommentsInput, GetViewerAccessInput, ListActorAvatarsInput,
};
use self::batcher::Batcher;
pub use self::detail::{GitHubPullRequestPreview, ListWorkflowRunsRequiringApprovalInput};
pub use self::diff::{
    GetPullRequestDiffFileContentsInput, GetPullRequestDiffInput,
    GitHubPullRequestDiffFileContents, GitHubPullRequestDiffSlice,
};
pub use self::errors::*;
pub use self::files_viewed::{
    GitHubPullRequestFilesViewed, SetPullRequestFilesViewedInput, ViewedChange,
};
pub use self::list::{
    GitHubPullRequestListBatch, GitHubPullRequestSearchBatch, GitHubPullRequestStat,
    ListPullRequestStatsInput, ListPullRequestsInput, SearchPullRequestsInput,
};
pub use self::stack::GetPullRequestStackInput;
use super::api::{GitHubApi, GitHubGraphQlInput, GitHubRestInput};
use super::conditional_checks::ChecksRevalidator;
use super::pull_request_json::de::Int;
use super::pull_request_json::{DecodeFailure, DecodeResult};
use crate::shared::js::js_trim;
use crate::source_control_core::ambient::{self, PinnedCredential};
use crate::source_control_core::clock::Clock;
use crate::source_control_core::host::SourceControlHost;
use crate::source_control_core::pull_request_provider::{
    ProviderChangeRequestRef, ProviderChangeRequestSummary, RoutingIdentity,
    VerifiedCredentialInput, VerifiedIdentity,
};
use crate::source_control_core::{Cause, cause_from_message};

pub use super::pull_request_json::GitHubPullRequestWatchFingerprint;

/// How many pull requests' node ids are remembered at once. A long-lived server sees far more of
/// them than a reader ever has open, and least recently used rather than first in: a listing
/// walking cold pull requests must not evict the review being ticked through.
pub const NODE_ID_CACHE_CAPACITY: usize = 128;

/// Aliased lookups per request, and requests at once. Measured over a hundred rows: one request
/// carrying all hundred takes ~5.2s, four of twenty-five in parallel ~2.1s.
pub(crate) const STAT_ALIASES_PER_REQUEST: usize = 25;
pub(crate) const STAT_REQUEST_CONCURRENCY: usize = 4;
/// How long a summary read waits for company. The background sync asks for every linked pull
/// request at once, and each read reaches the resolver after its own cache check, so a batch
/// needs a moment longer than one scheduler tick to gather them.
pub(crate) const SUMMARY_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(10);

/// How long a verified routing identity is trusted.
const ROUTING_IDENTITY_TTL_MS: i64 = 10 * 60_000;
/// How many routing identities are remembered.
const ROUTING_IDENTITY_CAPACITY: usize = 128;

/// Pages of review threads to follow before the conversation is reported as truncated. GitHub
/// serves a hundred threads a page, so this is a thousand threads — past anything a pull request
/// a person is reading has, and short of walking a repository-sized conversation forever.
pub(crate) const REVIEW_THREAD_PAGES: usize = 10;

/// A pull request addressed the way every read and write names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubPullRequestTarget {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub number: u64,
}

impl GitHubPullRequestTarget {
    pub fn new(
        cwd: impl Into<String>,
        repository: impl Into<String>,
        host: impl Into<String>,
        number: u64,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            repository: repository.into(),
            host: host.into(),
            number,
        }
    }
}

impl From<&ProviderChangeRequestRef> for GitHubPullRequestTarget {
    fn from(request: &ProviderChangeRequestRef) -> Self {
        Self {
            cwd: request.repo.cwd.clone(),
            repository: request.repo.repository.clone(),
            host: request.repo.host.clone(),
            number: request.number,
        }
    }
}

impl From<ProviderChangeRequestRef> for GitHubPullRequestTarget {
    fn from(request: ProviderChangeRequestRef) -> Self {
        Self::from(&request)
    }
}

/// `{ owner, name }` from `owner/repo`.
pub(crate) struct RepositorySelector {
    pub owner: String,
    pub name: String,
}

/// The GraphQL API takes owner and name as separate arguments, so `owner/repo` is split here.
/// The host is not read off the identity: it travels alongside it, because the identity a
/// project records is the path below its host and never names the host itself.
pub(crate) fn parse_repository_selector(value: &str) -> RepositorySelector {
    let parts: Vec<&str> = js_trim(value)
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let name = parts.last().copied().unwrap_or("").to_owned();
    let owner = parts
        .len()
        .checked_sub(2)
        .map_or("", |index| parts[index])
        .to_owned();
    RepositorySelector { owner, name }
}

/// Builds a GraphQL variables map from a JSON object literal.
pub(crate) fn variables(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Decodes a JSON payload into `T`, reporting a mismatch as a [`DecodeFailure`].
pub(crate) fn decode_json<T: serde::de::DeserializeOwned>(raw: &str) -> DecodeResult<T> {
    serde_json::from_str(raw).map_err(|error| DecodeFailure::new(error.to_string()))
}

/// What a GraphQL read needs besides its query: the operation it is reported under, whether it
/// may spend the held-back reserve, and its variables.
pub(crate) struct GraphqlRead<'a> {
    pub cwd: &'a str,
    pub host: &'a str,
    pub operation: &'a str,
    pub allow_reserve: bool,
    pub variables: Option<Map<String, Value>>,
    pub query: String,
}

struct IdentityLock {
    gate: Arc<tokio::sync::Mutex<()>>,
    users: usize,
}

struct RoutingState {
    identities: IndexMap<String, (i64, RoutingIdentity)>,
    locks: HashMap<String, IdentityLock>,
}

pub(crate) struct Inner {
    pub(crate) api: Arc<dyn GitHubApi>,
    pub(crate) host: Arc<dyn SourceControlHost>,
    pub(crate) clock: Arc<dyn Clock>,
    revalidator: ChecksRevalidator,
    routing: Mutex<RoutingState>,
    node_ids: Mutex<IndexMap<String, String>>,
    pub(crate) summaries: Batcher<GitHubPullRequestTarget, ProviderChangeRequestSummary>,
    pub(crate) watch_fingerprints:
        Batcher<GitHubPullRequestTarget, Option<GitHubPullRequestWatchFingerprint>>,
    /// The batch timers and flushes, aborted with the service.
    pub(crate) tasks: Mutex<JoinSet<()>>,
}

/// The pull request service over GitHub's APIs. Cheap to clone.
#[derive(Clone)]
pub struct GitHubPullRequestApi {
    pub(crate) inner: Arc<Inner>,
}

impl GitHubPullRequestApi {
    /// `GitHubPullRequestApi.make`: needs the transport, the host (for the stack rebase's git) and
    /// a clock for the routing identity cache and the checks revalidator.
    pub fn new(
        api: Arc<dyn GitHubApi>,
        host: Arc<dyn SourceControlHost>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                revalidator: ChecksRevalidator::new(api.clone(), clock.clone()),
                api,
                host,
                clock,
                routing: Mutex::new(RoutingState {
                    identities: IndexMap::new(),
                    locks: HashMap::new(),
                }),
                node_ids: Mutex::new(IndexMap::new()),
                summaries: Batcher::new(),
                watch_fingerprints: Batcher::new(),
                tasks: Mutex::new(JoinSet::new()),
            }),
        }
    }

    pub(crate) fn api(&self) -> &dyn GitHubApi {
        self.inner.api.as_ref()
    }

    /// `revalidateChecks`.
    pub fn revalidate_checks(&self) -> &ChecksRevalidator {
        &self.inner.revalidator
    }

    /// Runs a batch timer or flush as a task owned by the service.
    pub(crate) fn spawn_task(&self, task: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.inner.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(task);
    }
}

// -------------------------------------------------------------------------------------------
// Credentials and routing identity
// -------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RoutingIdentityResponse {
    id: Int,
    login: TrimmedNonEmptyString,
}

/// Releases the per-credential gate registration, removing the lock once nobody uses it.
struct IdentityLockUse<'a> {
    api: &'a GitHubPullRequestApi,
    key: String,
}

impl Drop for IdentityLockUse<'_> {
    fn drop(&mut self) {
        let mut routing = self.api.inner.routing.lock();
        if let Some(lock) = routing.locks.get_mut(&self.key) {
            lock.users -= 1;
            if lock.users == 0 {
                routing.locks.remove(&self.key);
            }
        }
    }
}

impl GitHubPullRequestApi {
    /// Verifies the credential a request to `input.host` would carry (one verification per
    /// credential at a time, remembered for ten minutes) and returns the identity plus the pin to
    /// run follow-up work under (`captureVerifiedCredential`).
    pub async fn verify_credential(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<(VerifiedIdentity, PinnedCredential), GitHubPullRequestApiError> {
        let unavailable = || GitHubViewerLoginUnavailableError {
            cwd: input.cwd.clone(),
        };
        let host = input.host.to_lowercase();
        // A missing or signed-out credential keeps its own error, so the page can say which.
        let credential = self.api().credential(&host).await?;
        let key = credential.fingerprint.clone();
        let pinned = PinnedCredential {
            host: host.clone(),
            token: credential.token.clone(),
            credential_fingerprint: key.clone(),
        };
        // A cold page may ask several times. Wait per credential and check again after the
        // first verification; cancellation releases the next waiter without losing its request.
        let gate = {
            let mut routing = self.inner.routing.lock();
            let lock = routing
                .locks
                .entry(key.clone())
                .or_insert_with(|| IdentityLock {
                    gate: Arc::new(tokio::sync::Mutex::new(())),
                    users: 0,
                });
            lock.users += 1;
            lock.gate.clone()
        };
        let _use = IdentityLockUse {
            api: self,
            key: key.clone(),
        };
        let _permit = gate.lock().await;
        let now = self.inner.clock.now_millis();
        let cached = self
            .inner
            .routing
            .lock()
            .identities
            .get(&key)
            .filter(|(at, _)| now - at < ROUTING_IDENTITY_TTL_MS)
            .map(|(_, value)| value.clone());
        let identity = match cached {
            Some(identity) => identity,
            None => {
                // Pin this read so an auth switch cannot poison its cache entry.
                let response = ambient::with_pinned_credential(
                    Some(pinned.clone()),
                    ambient::with_credential_scope(
                        key.clone(),
                        self.api().rest(GitHubRestInput {
                            host: host.clone(),
                            operation: "getRoutingIdentity".into(),
                            path: "user".into(),
                            allow_reserve: Some(true),
                            ..Default::default()
                        }),
                    ),
                )
                .await?;
                let decoded: RoutingIdentityResponse =
                    serde_json::from_str(&response.body).map_err(|_| unavailable())?;
                if decoded.id.0 < 1 {
                    return Err(unavailable().into());
                }
                let value = RoutingIdentity {
                    account_id: decoded.id.0.to_string(),
                    viewer: decoded.login.as_str().to_owned(),
                };
                let mut routing = self.inner.routing.lock();
                if routing.identities.len() >= ROUTING_IDENTITY_CAPACITY {
                    routing.identities.shift_remove_index(0);
                }
                routing.identities.insert(key.clone(), (now, value.clone()));
                value
            }
        };
        Ok((
            VerifiedIdentity {
                account_id: identity.account_id,
                viewer: identity.viewer,
                credential_fingerprint: key,
            },
            pinned,
        ))
    }

    /// Verifies the credential for `input.host`, then runs `use_` with the credential pinned and
    /// its rate-limit scope set, so a page cannot read one account's data with another's token
    /// mid-flight (`withVerifiedCredential`).
    pub async fn with_verified_credential<A, E, F, Fut>(
        &self,
        input: &VerifiedCredentialInput,
        use_: F,
    ) -> Result<A, E>
    where
        E: From<GitHubPullRequestApiError>,
        F: FnOnce(VerifiedIdentity) -> Fut,
        Fut: Future<Output = Result<A, E>>,
    {
        let (identity, pinned) = self.verify_credential(input).await?;
        let scope = identity.credential_fingerprint.clone();
        ambient::with_credential_scope(
            scope,
            ambient::with_pinned_credential(Some(pinned), use_(identity)),
        )
        .await
    }

    pub async fn get_routing_identity(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<RoutingIdentity, GitHubPullRequestApiError> {
        let (identity, _) = self.verify_credential(input).await?;
        Ok(RoutingIdentity {
            account_id: identity.account_id,
            viewer: identity.viewer,
        })
    }

    pub async fn get_viewer_login(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<String, GitHubPullRequestApiError> {
        Ok(self.get_routing_identity(input).await?.viewer)
    }
}

// -------------------------------------------------------------------------------------------
// Reads
// -------------------------------------------------------------------------------------------

impl GitHubPullRequestApi {
    pub(crate) fn read_error(
        cwd: &str,
        operation: &str,
        cause: Cause,
    ) -> GitHubPullRequestApiError {
        GitHubPullRequestReadError {
            cwd: cwd.to_owned(),
            operation: operation.to_owned(),
            cause,
        }
        .into()
    }

    pub(crate) fn read_error_message(
        cwd: &str,
        operation: &str,
        message: impl Into<String>,
    ) -> GitHubPullRequestApiError {
        Self::read_error(cwd, operation, cause_from_message(message))
    }

    /// Decodes an answer, reporting a failure against the read that made it.
    pub(crate) fn decode_with<A>(
        cwd: &str,
        operation: &str,
        decode: impl FnOnce(&str) -> DecodeResult<A>,
        raw: &str,
    ) -> Result<A, GitHubPullRequestApiError> {
        decode(js_trim(raw)).map_err(|failure| Self::read_error(cwd, operation, Arc::new(failure)))
    }

    /// A GraphQL read whose answer is decoded, reporting a failure against the read that made it.
    pub(crate) async fn graphql_read<A>(
        &self,
        read: GraphqlRead<'_>,
        decode: impl FnOnce(&str) -> DecodeResult<A>,
    ) -> Result<A, GitHubPullRequestApiError> {
        let raw = self
            .api()
            .graphql(GitHubGraphQlInput {
                host: read.host.to_owned(),
                operation: read.operation.to_owned(),
                query: read.query,
                variables: read.variables,
                allow_reserve: read.allow_reserve.then_some(true),
                max_response_bytes: None,
            })
            .await?;
        Self::decode_with(read.cwd, read.operation, decode, &raw)
    }

    /// A REST read whose JSON answer is decoded the same way.
    pub(crate) async fn rest_read<A>(
        &self,
        cwd: &str,
        host: &str,
        operation: &str,
        path: String,
        decode: impl FnOnce(&str) -> DecodeResult<A>,
    ) -> Result<A, GitHubPullRequestApiError> {
        let response = self
            .api()
            .rest(GitHubRestInput {
                host: host.to_owned(),
                operation: operation.to_owned(),
                path,
                ..Default::default()
            })
            .await?;
        Self::decode_with(cwd, operation, decode, &response.body)
    }

    /// A GraphQL mutation whose answer is not read back; a GraphQL error fails it.
    pub(crate) async fn graphql(
        &self,
        host: &str,
        operation: &str,
        query: &str,
        variables: Map<String, Value>,
    ) -> Result<(), GitHubPullRequestApiError> {
        self.api()
            .graphql(GitHubGraphQlInput {
                host: host.to_owned(),
                operation: operation.to_owned(),
                query: query.to_owned(),
                variables: Some(variables),
                allow_reserve: None,
                max_response_bytes: None,
            })
            .await?;
        Ok(())
    }

    /// The pull request's own node id, which is what a mutation against the pull request itself is
    /// addressed by: a reaction on its description, or a rewrite of its words.
    ///
    /// A pull request keeps its node id for life, so it is remembered rather than re-read: a reader
    /// ticking files viewed would otherwise pay a GraphQL round trip per press.
    pub(crate) async fn pull_request_node_id(
        &self,
        target: &GitHubPullRequestTarget,
        operation: &str,
    ) -> Result<String, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let key = format!("{} {owner}/{name} {}", target.host, target.number);
        {
            let mut node_ids = self.inner.node_ids.lock();
            if let Some(held) = node_ids.shift_remove(&key) {
                // Put back at the end on every hit, so what falls out is the pull request nobody
                // has looked at rather than the one being ticked through: a run of cold reads
                // would otherwise evict the open review and make it pay a round trip per press.
                node_ids.insert(key, held.clone());
                return Ok(held);
            }
        }
        let node_id = self
            .graphql_read(
                GraphqlRead {
                    cwd: &target.cwd,
                    host: &target.host,
                    operation,
                    allow_reserve: true,
                    variables: Some(variables(serde_json::json!({
                        "owner": owner, "name": name, "number": target.number
                    }))),
                    query: super::pull_request_json::PULL_REQUEST_NODE_ID_GRAPHQL_QUERY.into(),
                },
                super::pull_request_json::decode_pull_request_node_id_json,
            )
            .await?;
        let mut node_ids = self.inner.node_ids.lock();
        if node_ids.len() >= NODE_ID_CACHE_CAPACITY {
            node_ids.shift_remove_index(0);
        }
        node_ids.insert(key, node_id.clone());
        Ok(node_id)
    }

    /// Whether a client-given subject actually belongs to the pull request the request names. A
    /// subject id is trusted to be whatever node it names, and that node can hang off any pull
    /// request on the host — so the mutation itself would write wherever the id actually belongs,
    /// not wherever the request says it does, unless this confirms the two agree first.
    pub(crate) async fn subject_belongs_to_pull_request(
        &self,
        target: &GitHubPullRequestTarget,
        subject_id: &str,
        operation: &str,
    ) -> Result<bool, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation,
                allow_reserve: true,
                variables: Some(variables(serde_json::json!({
                    "owner": owner, "name": name, "number": target.number, "subjectId": subject_id
                }))),
                query: super::pull_request_json::REACTION_SUBJECT_PULL_REQUEST_GRAPHQL_QUERY.into(),
            },
            super::pull_request_json::decode_reaction_subject_scope_json,
        )
        .await
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    #[test]
    fn splits_owner_and_name_like_upstream() {
        let selector = parse_repository_selector(" acme/web ");
        assert_eq!(
            (selector.owner.as_str(), selector.name.as_str()),
            ("acme", "web")
        );
        let selector = parse_repository_selector("group/sub/project");
        assert_eq!(
            (selector.owner.as_str(), selector.name.as_str()),
            ("sub", "project")
        );
        let selector = parse_repository_selector("web");
        assert_eq!(
            (selector.owner.as_str(), selector.name.as_str()),
            ("", "web")
        );
        let selector = parse_repository_selector("");
        assert_eq!((selector.owner.as_str(), selector.name.as_str()), ("", ""));
    }
}
