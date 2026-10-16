//! Port of upstream/packages/source-control-github/src/server/gitHubConditionalChecks.ts
//!
//! Decides, with conditional REST requests (ETags), whether a pull request's checks changed since
//! they were last read, so an unchanged poll costs a few free `304`s instead of the GraphQL read.
//!
//! Effect mapping:
//! - `makeChecksRevalidator` is [`ChecksRevalidator::new`]; the returned function is
//!   [`ChecksRevalidator::revalidate`].
//! - The `KnownWorkflowRuns` context reference is a task-local, read with [`known_workflow_runs`]
//!   and scoped with [`with_known_workflow_runs`].
//! - The 128-entry, 30-minute `Cache` of per-pull-request state is [`EntryCache`], on the injected
//!   [`Clock`] so tests move time by hand; each entry's `Semaphore(1)` is a `tokio::sync::Mutex`
//!   around the state it guards.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Deserialize;
use turin_contracts::pull_request::{PullRequestCheck, PullRequestChecks, PullRequestState};

use super::api::{
    GitHubApi, GitHubApiError, GitHubApiResponseError, GitHubRestInput, GitHubRestResponse,
};
use super::pull_request_json::de::{self, Int};
use crate::source_control_core::ambient::pinned_credential;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::ProviderChangeRequestRef;

const OPERATION: &str = "revalidateChecks";

/// A cached answer is reused for at most this long, even while every endpoint is unchanged.
const READ_REUSE_MILLIS: i64 = 5 * 60_000;

const CACHE_CAPACITY: usize = 128;
const CACHE_TIME_TO_LIVE_MILLIS: i64 = 30 * 60_000;

/// The pull request endpoint's answer: which commit is the head, and whether it comes from a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    sha: String,
    /// `None` once the fork a pull request came from has been deleted.
    head_repo_id: Option<i64>,
    base_repo_id: i64,
}

#[derive(Deserialize)]
struct RepoId {
    id: Int,
}

#[derive(Deserialize)]
struct HeadSide {
    sha: String,
    #[serde(deserialize_with = "de::nullable")]
    repo: Option<RepoId>,
}

#[derive(Deserialize)]
struct BaseSide {
    repo: RepoId,
}

#[derive(Deserialize)]
struct HeadSchema {
    head: HeadSide,
    base: BaseSide,
}

/// `Schema.String.check(Schema.isPattern(/^[a-f0-9]{40,64}$/))`.
fn is_commit_sha(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `PositiveInt`: an integer of at least one.
fn positive_id(id: &RepoId) -> Option<i64> {
    (id.id.0 >= 1).then_some(id.id.0)
}

fn decode_head(body: &str) -> Option<Head> {
    let decoded: HeadSchema = serde_json::from_str(body).ok()?;
    if !is_commit_sha(&decoded.head.sha) {
        return None;
    }
    let head_repo_id = match &decoded.head.repo {
        None => None,
        Some(repo) => Some(positive_id(repo)?),
    };
    Some(Head {
        sha: decoded.head.sha,
        head_repo_id,
        base_repo_id: positive_id(&decoded.base.repo)?,
    })
}

/// A workflow run as the runs endpoint lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KnownWorkflowRun {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
}

#[derive(Deserialize)]
struct WorkflowRunSchema {
    id: Int,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    head_branch: Option<String>,
}

#[derive(Deserialize)]
struct WorkflowRunsSchema {
    workflow_runs: Vec<WorkflowRunSchema>,
}

/// `Schema.decodeUnknownOption(Schema.fromJsonString(...))`.
fn decode_workflow_runs(body: &str) -> Option<Vec<KnownWorkflowRun>> {
    let decoded: WorkflowRunsSchema = serde_json::from_str(body).ok()?;
    Some(
        decoded
            .workflow_runs
            .into_iter()
            .map(|run| KnownWorkflowRun {
                id: run.id.0,
                name: run.name,
                html_url: run.html_url,
                status: run.status,
                conclusion: run.conclusion,
                head_branch: run.head_branch,
            })
            .collect(),
    )
}

/// Every `pull_request` workflow run of one head, as the revalidator just confirmed them against
/// GitHub. A read running under it filters these rather than asking for the runs again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownWorkflowRuns {
    pub head_sha: String,
    pub runs: Vec<KnownWorkflowRun>,
}

tokio::task_local! {
    static KNOWN_WORKFLOW_RUNS: Option<KnownWorkflowRuns>;
}

/// `yield* KnownWorkflowRuns`: the runs the surrounding revalidation confirmed, or `None`.
pub fn known_workflow_runs() -> Option<KnownWorkflowRuns> {
    KNOWN_WORKFLOW_RUNS.try_with(Clone::clone).unwrap_or(None)
}

/// `Effect.provideService(KnownWorkflowRuns, known)`.
pub async fn with_known_workflow_runs<F: Future>(
    known: Option<KnownWorkflowRuns>,
    fut: F,
) -> F::Output {
    KNOWN_WORKFLOW_RUNS.scope(known, fut).await
}

/// What a checks read returns: `Pick<GitHubPullRequestDetail, "state" | "checks" | "headSha"> &
/// { workflowApprovalsRequired?: number }`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChecksRead {
    pub state: PullRequestState,
    pub checks: Vec<PullRequestCheck>,
    pub head_sha: Option<String>,
    pub workflow_approvals_required: Option<u64>,
}

#[derive(Debug, Clone)]
struct Validator {
    etag: Option<String>,
    next: bool,
    body: String,
}

struct EntryState {
    validators: HashMap<String, Validator>,
    head: Option<Head>,
    value: Option<PullRequestChecks>,
    read_at: i64,
    supported: bool,
}

impl EntryState {
    fn new() -> Self {
        Self {
            validators: HashMap::new(),
            head: None,
            value: None,
            read_at: 0,
            supported: true,
        }
    }
}

type Entry = Arc<tokio::sync::Mutex<EntryState>>;

/// `Cache.makeWith(..., { capacity: 128, timeToLive: "30 minutes" })`: created on first use,
/// expired by age since creation, least recently used out first.
struct EntryCache {
    entries: Mutex<IndexMap<String, (Entry, i64)>>,
}

impl EntryCache {
    fn new() -> Self {
        Self {
            entries: Mutex::new(IndexMap::new()),
        }
    }

    fn get(&self, key: &str, now: i64) -> Entry {
        let mut entries = self.entries.lock();
        if let Some(index) = entries.get_index_of(key) {
            let created = entries[index].1;
            if now - created < CACHE_TIME_TO_LIVE_MILLIS {
                let (_, (entry, created)) = entries.shift_remove_index(index).expect("present");
                let handle = Arc::clone(&entry);
                entries.insert(key.to_owned(), (entry, created));
                return handle;
            }
            entries.shift_remove_index(index);
        }
        let entry: Entry = Arc::new(tokio::sync::Mutex::new(EntryState::new()));
        entries.insert(key.to_owned(), (Arc::clone(&entry), now));
        while entries.len() > CACHE_CAPACITY {
            entries.shift_remove_index(0);
        }
        entry
    }
}

/// A host that answers these with one of them has no conditional REST for it at all.
const UNSUPPORTED_STATUSES: [u16; 3] = [404, 405, 501];

pub struct ChecksRevalidator {
    api: Arc<dyn GitHubApi>,
    clock: Arc<dyn Clock>,
    entries: EntryCache,
}

impl ChecksRevalidator {
    pub fn new(api: Arc<dyn GitHubApi>, clock: Arc<dyn Clock>) -> Self {
        Self {
            api,
            clock,
            entries: EntryCache::new(),
        }
    }

    /// Returns the checks of `input`, from the cache while every endpoint is confirmed unchanged,
    /// and from `read` (handed the confirmed workflow runs through [`known_workflow_runs`])
    /// otherwise. Without a pinned credential there is no account to key the cache by, so the
    /// read simply runs.
    pub async fn revalidate<E, Fut, F>(
        &self,
        input: &ProviderChangeRequestRef,
        read: F,
    ) -> Result<PullRequestChecks, E>
    where
        E: From<GitHubApiError>,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ChecksRead, E>>,
    {
        let Some(credential) = pinned_credential() else {
            return read().await.map(checks_of);
        };
        let key = format!(
            "{}\0{}\0{}\0{}",
            credential.credential_fingerprint, input.repo.host, input.repo.repository, input.number
        );
        let entry = self.entries.get(&key, self.clock.now_millis());
        let mut state = entry.lock().await;
        if !state.supported {
            return read().await.map(checks_of);
        }
        let root = format!("repos/{}", input.repo.repository);
        if self
            .get(
                &mut state,
                input,
                &format!("{root}/pulls/{}", input.number),
                true,
            )
            .await?
            .is_none()
        {
            return read().await.map(checks_of);
        }
        let Some(head) = state.head.clone() else {
            return Err(fail(input, 200).into());
        };
        let runs_endpoint = (head.head_repo_id != Some(head.base_repo_id)).then(|| {
            format!(
                "{root}/actions/runs?head_sha={}&event=pull_request",
                head.sha
            )
        });
        let mut endpoints = vec![
            format!("{root}/commits/{}/check-runs?filter=all", head.sha),
            format!("{root}/commits/{}/status", head.sha),
        ];
        endpoints.extend(runs_endpoint.clone());
        let mut run_pages: Vec<String> = Vec::new();
        for endpoint in &endpoints {
            let mut page = 1;
            loop {
                if page > 100 {
                    return Err(fail(input, 200).into());
                }
                let separator = if endpoint.contains('?') { "&" } else { "?" };
                let paged = format!("{endpoint}{separator}per_page=100&page={page}");
                let Some(next) = self.get(&mut state, input, &paged, false).await? else {
                    return read().await.map(checks_of);
                };
                if Some(endpoint) == runs_endpoint.as_ref() {
                    run_pages.push(paged);
                }
                if !next {
                    break;
                }
                page += 1;
            }
        }
        let now = self.clock.now_millis();
        if let Some(value) = &state.value
            && now - state.read_at < READ_REUSE_MILLIS
        {
            return Ok(value.clone());
        }
        // Every page of the head's runs was just confirmed current, so the read filters them
        // rather than listing the runs a second time.
        let pages: Vec<Option<Vec<KnownWorkflowRun>>> = run_pages
            .iter()
            .map(|endpoint| {
                decode_workflow_runs(
                    state
                        .validators
                        .get(endpoint)
                        .map_or("", |validator| validator.body.as_str()),
                )
            })
            .collect();
        let known = (runs_endpoint.is_some() && pages.iter().all(Option::is_some)).then(|| {
            KnownWorkflowRuns {
                head_sha: head.sha.clone(),
                runs: pages.into_iter().flatten().flatten().collect(),
            }
        });
        let fresh = with_known_workflow_runs(known, read()).await?;
        let value = PullRequestChecks {
            state: fresh.state,
            checks: fresh.checks.clone(),
        };
        state.value = (fresh.workflow_approvals_required.is_some()
            && fresh.head_sha.as_deref() == Some(head.sha.as_str()))
        .then(|| value.clone());
        state.read_at = now;
        Ok(value)
    }

    /// Whether the endpoint has a next page, or `None` where the host cannot say.
    async fn get(
        &self,
        state: &mut EntryState,
        input: &ProviderChangeRequestRef,
        endpoint: &str,
        head: bool,
    ) -> Result<Option<bool>, GitHubApiError> {
        let previous = state.validators.get(endpoint).cloned();
        let response = self
            .api
            .rest(GitHubRestInput {
                host: input.repo.host.clone(),
                operation: OPERATION.to_owned(),
                path: endpoint.to_owned(),
                if_none_match: previous
                    .as_ref()
                    .and_then(|previous| previous.etag.clone())
                    .filter(|etag| !etag.is_empty()),
                ..Default::default()
            })
            .await;
        let response: GitHubRestResponse = match response {
            Ok(response) => response,
            Err(GitHubApiError::NotFound(_)) => {
                state.supported = false;
                state.value = None;
                return Ok(None);
            }
            Err(GitHubApiError::Response(error)) => {
                state.supported = !UNSUPPORTED_STATUSES.contains(&error.status);
                state.value = None;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if response.status == 304
            && let Some(previous) = previous
        {
            return Ok(Some(previous.next));
        }
        if response.status != 200 || response.truncated {
            return Err(fail(input, response.status));
        }
        state.value = None;
        if head {
            let decoded =
                decode_head(&response.body).ok_or_else(|| fail(input, response.status))?;
            if state.head.as_ref().map(|head| head.sha.as_str()) != Some(decoded.sha.as_str()) {
                state.validators.clear();
            }
            state.head = Some(decoded);
        }
        let validator = Validator {
            etag: response
                .headers
                .get("etag")
                .map(|etag| crate::shared::js::js_trim(etag).to_owned()),
            next: response
                .headers
                .get("link")
                .is_some_and(|link| link.contains("rel=\"next\"")),
            body: response.body,
        };
        let next = validator.next;
        state.validators.insert(endpoint.to_owned(), validator);
        Ok(Some(next))
    }
}

fn checks_of(read: ChecksRead) -> PullRequestChecks {
    PullRequestChecks {
        state: read.state,
        checks: read.checks,
    }
}

fn fail(input: &ProviderChangeRequestRef, status: u16) -> GitHubApiError {
    GitHubApiError::Response(GitHubApiResponseError {
        host: input.repo.host.clone(),
        operation: OPERATION.to_owned(),
        status,
        github_errors: None,
    })
}

#[cfg(test)]
mod tests {
    //! Port of upstream/packages/source-control-github/src/server/gitHubConditionalChecks.test.ts

    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::pull_request::PullRequestCheckStatus;

    use super::*;
    use crate::github::api::{GitHubApiNotFoundError, GitHubGraphQlInput, GitHubRequestCredential};
    use crate::source_control_core::ambient::{PinnedCredential, with_pinned_credential};
    use crate::source_control_core::clock::ManualClock;
    use crate::source_control_core::pull_request_provider::ProviderRepositoryRef;
    use crate::source_control_core::redacted::Redacted;

    type RestHandler =
        Box<dyn Fn(GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> + Send + Sync>;

    /// `Layer.mock(GitHubApi)({ rest })`: only `rest` is answered.
    struct MockApi {
        rest: RestHandler,
    }

    #[async_trait]
    impl GitHubApi for MockApi {
        async fn graphql(&self, _input: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
            unreachable!("the revalidator never calls GraphQL")
        }
        async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
            (self.rest)(input)
        }
        async fn credential(&self, _host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
            unreachable!("the revalidator never asks for a credential")
        }
    }

    fn reference() -> ProviderChangeRequestRef {
        ProviderChangeRequestRef {
            repo: ProviderRepositoryRef {
                cwd: "/repo".to_owned(),
                repository: "acme/web".to_owned(),
                host: "github.com".to_owned(),
            },
            number: 1,
        }
    }

    fn credential(fingerprint: &str) -> PinnedCredential {
        PinnedCredential {
            host: "github.com".to_owned(),
            token: Redacted::new("token".to_owned()),
            credential_fingerprint: fingerprint.to_owned(),
        }
    }

    fn response(status: u16, headers: &[(&str, String)], body: String) -> GitHubRestResponse {
        GitHubRestResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
            body,
            truncated: false,
            invalid_utf8: false,
        }
    }

    fn response_error(operation: &str, status: u16) -> GitHubApiError {
        GitHubApiError::Response(GitHubApiResponseError {
            host: "github.com".to_owned(),
            operation: operation.to_owned(),
            status,
            github_errors: None,
        })
    }

    fn build_check() -> PullRequestCheck {
        PullRequestCheck {
            name: turin_contracts::base_schemas::TrimmedNonEmptyString::new("build").unwrap(),
            status: PullRequestCheckStatus::Success,
            description: None,
            url: None,
            required: None,
        }
    }

    #[tokio::test]
    async fn reuses_unchanged_checks_and_catches_reruns_later_pages_fork_approvals_pushes_and_account_changes()
     {
        let sha = Arc::new(Mutex::new("a".repeat(40)));
        let changed = Arc::new(Mutex::new(String::new()));
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let known: Arc<Mutex<Vec<Option<KnownWorkflowRuns>>>> = Arc::default();
        let reads = Arc::new(AtomicUsize::new(0));
        let clock = ManualClock::new(1_000_000);
        let api = {
            let (sha, changed, requests) = (
                Arc::clone(&sha),
                Arc::clone(&changed),
                Arc::clone(&requests),
            );
            MockApi {
                rest: Box::new(move |input| {
                    let path = input.path;
                    requests.lock().push(path.clone());
                    let changed = changed.lock().clone();
                    let sha = sha.lock().clone();
                    let head = path.ends_with("/pulls/1");
                    let modified = input.if_none_match.is_none()
                        || (path.contains(&changed) && !changed.is_empty());
                    let next = path.contains("check-runs") && path.ends_with("page=1");
                    let runs = path.contains("/actions/runs");
                    let mut headers = Vec::new();
                    if modified {
                        headers.push(("etag", format!("\"{sha}-{changed}\"")));
                        if next {
                            headers.push((
                                "link",
                                "<https://api.github.com/next>; rel=\"next\"".to_owned(),
                            ));
                        }
                    }
                    let body = if !modified {
                        String::new()
                    } else if head {
                        json!({ "head": { "sha": sha, "repo": { "id": 2 } }, "base": { "repo": { "id": 1 } } })
                            .to_string()
                    } else if runs {
                        json!({ "workflow_runs": [{ "id": 9, "status": "completed", "conclusion": "action_required" }] })
                            .to_string()
                    } else {
                        "{}".to_owned()
                    };
                    Ok(response(if modified { 200 } else { 304 }, &headers, body))
                }),
            }
        };
        let revalidator = ChecksRevalidator::new(Arc::new(api), clock.clone());
        let poll = |fingerprint: &'static str| {
            let (revalidator, sha, known, reads) = (
                &revalidator,
                Arc::clone(&sha),
                Arc::clone(&known),
                Arc::clone(&reads),
            );
            async move {
                with_pinned_credential(Some(credential(fingerprint)), async {
                    revalidator
                        .revalidate::<GitHubApiError, _, _>(&reference(), || async {
                            known.lock().push(known_workflow_runs());
                            reads.fetch_add(1, Ordering::SeqCst);
                            Ok(ChecksRead {
                                state: PullRequestState::Open,
                                checks: vec![build_check()],
                                head_sha: Some(sha.lock().clone()),
                                workflow_approvals_required: Some(0),
                            })
                        })
                        .await
                })
                .await
                .unwrap()
            }
        };

        poll("one").await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        // The fork's runs were just confirmed, so the read is handed them instead of listing them.
        assert_eq!(
            known.lock()[0],
            Some(KnownWorkflowRuns {
                head_sha: "a".repeat(40),
                runs: vec![KnownWorkflowRun {
                    id: 9,
                    name: None,
                    html_url: None,
                    status: Some("completed".to_owned()),
                    conclusion: Some("action_required".to_owned()),
                    head_branch: None,
                }],
            })
        );
        requests.lock().clear();
        poll("one").await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(requests.lock().len(), 5);
        for endpoint in [
            "check-runs?filter=all&per_page=100&page=2",
            "/status",
            "/actions/runs",
            "/pulls/1",
        ] {
            *changed.lock() = endpoint.to_owned();
            poll("one").await;
            *changed.lock() = String::new();
            poll("one").await;
        }
        assert_eq!(reads.load(Ordering::SeqCst), 5);
        *sha.lock() = "b".repeat(40);
        *changed.lock() = "/pulls/1".to_owned();
        requests.lock().clear();
        poll("one").await;
        assert_eq!(reads.load(Ordering::SeqCst), 6);
        let new_sha = sha.lock().clone();
        assert!(
            requests
                .lock()
                .iter()
                .filter(|endpoint| endpoint.contains("/commits/"))
                .all(|endpoint| endpoint.contains(&new_sha))
        );
        *changed.lock() = String::new();
        poll("two").await;
        assert_eq!(reads.load(Ordering::SeqCst), 7);
        clock.advance(5 * 60_000);
        poll("one").await;
        assert_eq!(reads.load(Ordering::SeqCst), 8);
    }

    #[tokio::test]
    async fn does_not_retain_failed_or_incomplete_reads_and_supports_hosts_without_etags() {
        let sha = "a".repeat(40);
        let etags = Arc::new(Mutex::new(true));
        let unavailable = Arc::new(Mutex::new(false));
        let fail = Arc::new(Mutex::new(true));
        let complete = Arc::new(Mutex::new(true));
        let reads = Arc::new(AtomicUsize::new(0));
        let clock = ManualClock::new(1_000_000);
        let api = {
            let (etags, unavailable, sha) =
                (Arc::clone(&etags), Arc::clone(&unavailable), sha.clone());
            MockApi {
                rest: Box::new(move |input| {
                    if *unavailable.lock() {
                        return Err(response_error(OPERATION, 502));
                    }
                    let etags = *etags.lock();
                    if input.if_none_match.is_some() && etags {
                        return Ok(response(304, &[], String::new()));
                    }
                    let body = if input.path.ends_with("/pulls/1") {
                        json!({ "head": { "sha": sha, "repo": { "id": 1 } }, "base": { "repo": { "id": 1 } } })
                            .to_string()
                    } else {
                        "{}".to_owned()
                    };
                    let headers = if etags {
                        vec![("etag", "\"one\"".to_owned())]
                    } else {
                        vec![]
                    };
                    Ok(response(200, &headers, body))
                }),
            }
        };
        let revalidator = ChecksRevalidator::new(Arc::new(api), clock.clone());
        let poll = || {
            let (revalidator, fail, complete, reads, sha) = (
                &revalidator,
                Arc::clone(&fail),
                Arc::clone(&complete),
                Arc::clone(&reads),
                sha.clone(),
            );
            async move {
                with_pinned_credential(Some(credential("one")), async {
                    revalidator
                        .revalidate::<GitHubApiError, _, _>(&reference(), || async {
                            reads.fetch_add(1, Ordering::SeqCst);
                            if *fail.lock() {
                                return Err(response_error("read", 500));
                            }
                            Ok(ChecksRead {
                                state: PullRequestState::Open,
                                checks: vec![],
                                head_sha: Some(sha),
                                workflow_approvals_required: (*complete.lock()).then_some(0),
                            })
                        })
                        .await
                })
                .await
            }
        };

        assert!(poll().await.is_err());
        *fail.lock() = false;
        *complete.lock() = false;
        poll().await.unwrap();
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        *complete.lock() = true;
        poll().await.unwrap();
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 4);
        clock.advance(5 * 60_000);
        *complete.lock() = false;
        poll().await.unwrap();
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 6);
        *complete.lock() = true;
        *unavailable.lock() = true;
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 7);
        *unavailable.lock() = false;
        poll().await.unwrap();
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 8);
        *etags.lock() = false;
        poll().await.unwrap();
        poll().await.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 10);
    }

    #[tokio::test]
    async fn falls_back_when_rest_checks_are_unavailable_without_retrying_unsupported_probes() {
        let probes = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let api = {
            let probes = Arc::clone(&probes);
            MockApi {
                rest: Box::new(move |_| {
                    probes.fetch_add(1, Ordering::SeqCst);
                    Err(GitHubApiError::NotFound(GitHubApiNotFoundError {
                        host: "github.com".to_owned(),
                        operation: OPERATION.to_owned(),
                    }))
                }),
            }
        };
        let revalidator = ChecksRevalidator::new(Arc::new(api), ManualClock::new(0));
        for _ in 0..2 {
            let reads = Arc::clone(&reads);
            with_pinned_credential(Some(credential("one")), async {
                revalidator
                    .revalidate::<GitHubApiError, _, _>(&reference(), || async {
                        reads.fetch_add(1, Ordering::SeqCst);
                        Ok(ChecksRead {
                            state: PullRequestState::Open,
                            checks: vec![],
                            head_sha: None,
                            workflow_approvals_required: None,
                        })
                    })
                    .await
            })
            .await
            .unwrap();
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn without_a_pinned_credential_the_read_just_runs() {
        let api = MockApi {
            rest: Box::new(|_| panic!("no probe without a credential")),
        };
        let revalidator = ChecksRevalidator::new(Arc::new(api), ManualClock::new(0));
        let checks = revalidator
            .revalidate::<GitHubApiError, _, _>(&reference(), || async {
                Ok(ChecksRead {
                    state: PullRequestState::Closed,
                    checks: vec![build_check()],
                    head_sha: None,
                    workflow_approvals_required: None,
                })
            })
            .await
            .unwrap();
        assert_eq!(checks.state, PullRequestState::Closed);
        assert_eq!(checks.checks.len(), 1);
    }

    #[test]
    fn the_entry_cache_expires_by_age_and_evicts_the_least_recently_used() {
        let cache = EntryCache::new();
        let first = cache.get("a", 0);
        assert!(Arc::ptr_eq(&first, &cache.get("a", 1)));
        assert!(!Arc::ptr_eq(
            &first,
            &cache.get("a", CACHE_TIME_TO_LIVE_MILLIS)
        ));
        for index in 0..CACHE_CAPACITY {
            cache.get(&format!("k{index}"), 10);
        }
        // "a" was the oldest by use, so the 129th key pushed it out.
        assert_eq!(cache.entries.lock().len(), CACHE_CAPACITY);
        assert!(cache.entries.lock().get("a").is_none());
    }

    #[test]
    fn commit_shas_are_forty_to_sixty_four_lowercase_hex_digits() {
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(is_commit_sha(&"0".repeat(64)));
        assert!(!is_commit_sha(&"a".repeat(39)));
        assert!(!is_commit_sha(&"a".repeat(65)));
        assert!(!is_commit_sha(&"A".repeat(40)));
        assert!(
            decode_head(r#"{"head":{"sha":"abc","repo":null},"base":{"repo":{"id":1}}}"#).is_none()
        );
    }
}
