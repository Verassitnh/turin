//! Test doubles shared by the `GitHubPullRequestApi` tests: a scripted `GitHubApi` that records
//! every request, spends the real GraphQL quota from each answer's headers (the way `GitHubApi`
//! does, so the reserve behaviour stays the module's to prove), and a `SourceControlHost` whose
//! processes are scripted.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
use turin_contracts::settings::{ServerSettings, ServerSettingsError};
use turin_contracts::vcs::{VcsError, VcsListRemotesResult};

use super::{GitHubPullRequestApi, GitHubPullRequestTarget};
use crate::github::api::{
    GitHubApi, GitHubApiError, GitHubGraphQlInput, GitHubRequestCredential, GitHubRestInput,
    GitHubRestResponse,
};
use crate::github::quota::GitHubQuota;
use crate::source_control_core::ambient;
use crate::source_control_core::clock::{Clock, ManualClock};
use crate::source_control_core::host::{
    SourceControlGitExecuteInput, SourceControlGitOutput, SourceControlGitRemoteBranch,
    SourceControlHost, SourceControlProcessInput, SourceControlProcessOutput,
};
use crate::source_control_core::redacted::Redacted;

/// `Date.parse("2099-08-13T14:00:00Z")`.
pub const RESET_2099: i64 = 4_088_149_200_000;

pub type Answer = Result<GitHubRestResponse, GitHubApiError>;
pub type Reply = Pin<Box<dyn Future<Output = Answer> + Send>>;
type Handler = Box<dyn FnMut(&ApiCall) -> Reply + Send>;

/// One request the transport received.
#[derive(Debug, Clone)]
pub enum ApiCall {
    Graphql(GitHubGraphQlInput),
    Rest(GitHubRestInput),
}

impl ApiCall {
    pub fn host(&self) -> &str {
        match self {
            Self::Graphql(input) => &input.host,
            Self::Rest(input) => &input.host,
        }
    }
    pub fn is_graphql(&self) -> bool {
        matches!(self, Self::Graphql(_))
    }
    pub fn is_rest(&self) -> bool {
        matches!(self, Self::Rest(_))
    }
    /// The GraphQL document, or `""` for a REST request.
    pub fn query(&self) -> &str {
        match self {
            Self::Graphql(input) => &input.query,
            Self::Rest(_) => "",
        }
    }
    /// The REST path, or `""` for a GraphQL request.
    pub fn path(&self) -> &str {
        match self {
            Self::Graphql(_) => "",
            Self::Rest(input) => &input.path,
        }
    }
    pub fn variables(&self) -> Map<String, Value> {
        match self {
            Self::Graphql(input) => input.variables.clone().unwrap_or_default(),
            Self::Rest(_) => Map::new(),
        }
    }
    pub fn rest(&self) -> &GitHubRestInput {
        match self {
            Self::Rest(input) => input,
            Self::Graphql(_) => panic!("not a REST call"),
        }
    }
    /// The route target: the document for GraphQL, the path for REST.
    fn target(&self) -> &str {
        match self {
            Self::Graphql(input) => &input.query,
            Self::Rest(input) => &input.path,
        }
    }
}

/// A `vi.fn()`: queued answers first, then the default implementation.
#[derive(Default)]
struct Mock {
    once: VecDeque<Answer>,
    fallback: Option<Handler>,
    calls: Vec<ApiCall>,
}

impl Mock {
    fn respond(&mut self, call: ApiCall) -> Reply {
        self.calls.push(call.clone());
        if let Some(answer) = self.once.pop_front() {
            return Box::pin(async move { answer });
        }
        match &mut self.fallback {
            Some(handler) => handler(&call),
            None => panic!("unexpected request: {call:?}"),
        }
    }
}

type CredentialFn =
    Box<dyn Fn(&str) -> Result<GitHubRequestCredential, GitHubApiError> + Send + Sync>;

pub struct FakeApi {
    main: Mutex<Mock>,
    stack: Mutex<Mock>,
    quota: Arc<GitHubQuota>,
    credential: Mutex<CredentialFn>,
    pub credential_calls: Mutex<Vec<String>>,
}

pub fn default_credential(host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
    Ok(GitHubRequestCredential {
        token: Redacted::new("token".into()),
        fingerprint: format!("{host}:token"),
    })
}

#[async_trait]
impl GitHubApi for FakeApi {
    async fn graphql(&self, input: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
        let allow_reserve = input.allow_reserve.unwrap_or_else(ambient::interactive);
        self.quota.admit(&input.host, "graphql", allow_reserve)?;
        let host = input.host.clone();
        let mock = if input.query.contains("query PullRequestStackMemberships") {
            &self.stack
        } else {
            &self.main
        };
        let reply = mock.lock().respond(ApiCall::Graphql(input));
        let answer = reply.await?;
        self.quota.observe(&host, &answer.headers);
        Ok(answer.body)
    }

    async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
        let reply = self.main.lock().respond(ApiCall::Rest(input));
        reply.await
    }

    async fn credential(&self, host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
        self.credential_calls.lock().push(host.to_owned());
        (self.credential.lock())(host)
    }
}

/// A host whose git is scripted: every process is recorded and answered by `run`.
#[derive(Default)]
pub struct ScriptedHost {
    pub processes: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl SourceControlHost for ScriptedHost {
    async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError> {
        unimplemented!()
    }
    async fn process_run(
        &self,
        input: SourceControlProcessInput,
    ) -> Result<SourceControlProcessOutput, VcsError> {
        let stdout = match input.args.first().map(String::as_str) {
            Some("rev-parse") | Some("merge-base") => "new-sha\n",
            _ => "",
        };
        self.processes.lock().push(input.args);
        Ok(SourceControlProcessOutput {
            exit_code: 0,
            stdout: stdout.into(),
            ..Default::default()
        })
    }
    async fn git_execute(
        &self,
        _: SourceControlGitExecuteInput,
    ) -> Result<SourceControlGitOutput, GitCommandError> {
        unimplemented!()
    }
    async fn git_resolve_commit(&self, _: &str, _: &str) -> Result<String, GitCommandError> {
        unimplemented!()
    }
    async fn git_remotes(&self, _: &str) -> Result<VcsListRemotesResult, VcsError> {
        unimplemented!()
    }
    async fn git_read_config_value(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<String>, GitCommandError> {
        unimplemented!()
    }
    async fn git_resolve_primary_remote_name(&self, _: &str) -> Result<String, GitCommandError> {
        unimplemented!()
    }
    async fn git_ensure_remote(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<String, GitCommandError> {
        unimplemented!()
    }
    async fn git_list_local_branch_names(&self, _: &str) -> Result<Vec<String>, GitCommandError> {
        unimplemented!()
    }
    async fn git_fetch_remote_branch(
        &self,
        _: &SourceControlGitRemoteBranch,
        _: &str,
    ) -> Result<(), GitCommandError> {
        unimplemented!()
    }
    async fn git_fetch_remote_tracking_branch(
        &self,
        _: &SourceControlGitRemoteBranch,
    ) -> Result<(), GitCommandError> {
        unimplemented!()
    }
    async fn git_set_branch_upstream(
        &self,
        _: &SourceControlGitRemoteBranch,
        _: &str,
    ) -> Result<(), GitCommandError> {
        unimplemented!()
    }
    async fn git_switch_ref(
        &self,
        _: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        unimplemented!()
    }
}

/// A `Clock` that follows tokio's (pausable) time, for code that both reads the clock and sleeps.
pub struct TokioClock {
    start: tokio::time::Instant,
}

impl TokioClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: tokio::time::Instant::now(),
        })
    }
}

impl Clock for TokioClock {
    fn now_millis(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }
}

pub struct Harness {
    pub cli: GitHubPullRequestApi,
    pub api: Arc<FakeApi>,
    pub clock: Arc<ManualClock>,
    pub quota: Arc<GitHubQuota>,
}

impl Harness {
    pub fn new() -> Self {
        let clock = ManualClock::new(0);
        let quota = Arc::new(GitHubQuota::new(clock.clone()));
        let api = Arc::new(FakeApi {
            main: Mutex::new(Mock::default()),
            stack: Mutex::new(Mock {
                // `mockedStackMemberships` answers an empty object unless a test says otherwise.
                fallback: Some(Box::new(|_| {
                    Box::pin(async { Ok(output("{\"data\":{}}")) })
                })),
                ..Default::default()
            }),
            quota: quota.clone(),
            credential: Mutex::new(Box::new(default_credential)),
            credential_calls: Mutex::new(Vec::new()),
        });
        let cli = GitHubPullRequestApi::new(
            api.clone(),
            Arc::new(ScriptedHost::default()),
            clock.clone(),
        );
        Self {
            cli,
            api,
            clock,
            quota,
        }
    }

    pub fn target(&self, number: u64) -> GitHubPullRequestTarget {
        GitHubPullRequestTarget::new("/w", "acme/web", "github.com", number)
    }

    pub fn set_credential(
        &self,
        credential: impl Fn(&str) -> Result<GitHubRequestCredential, GitHubApiError>
        + Send
        + Sync
        + 'static,
    ) {
        *self.api.credential.lock() = Box::new(credential);
    }

    /// `mockReturnValueOnce`.
    pub fn once(&self, answer: Answer) -> &Self {
        self.api.main.lock().once.push_back(answer);
        self
    }

    /// `mockReturnValueOnce(Effect.succeed(output(body)))`.
    pub fn once_body(&self, body: impl Into<String>) -> &Self {
        self.once(Ok(output(body)))
    }

    /// `mockReturnValue(answer)`: every further request gets this answer.
    pub fn always(&self, answer: Answer) -> &Self {
        self.api.main.lock().fallback = Some(Box::new(move |_| {
            let answer = answer.clone();
            Box::pin(async move { answer })
        }));
        self
    }

    /// `mockImplementation`.
    pub fn on(&self, mut handler: impl FnMut(&ApiCall) -> Answer + Send + 'static) -> &Self {
        self.api.main.lock().fallback = Some(Box::new(move |call| {
            let answer = handler(call);
            Box::pin(async move { answer })
        }));
        self
    }

    /// `mockImplementation` with an asynchronous answer.
    pub fn on_async(&self, handler: impl FnMut(&ApiCall) -> Reply + Send + 'static) -> &Self {
        self.api.main.lock().fallback = Some(Box::new(handler));
        self
    }

    pub fn stack_once(&self, answer: Answer) -> &Self {
        self.api.stack.lock().once.push_back(answer);
        self
    }

    pub fn stack_on(&self, mut handler: impl FnMut(&ApiCall) -> Answer + Send + 'static) -> &Self {
        self.api.stack.lock().fallback = Some(Box::new(move |call| {
            let answer = handler(call);
            Box::pin(async move { answer })
        }));
        self
    }

    pub fn stack_calls(&self) -> Vec<ApiCall> {
        self.api.stack.lock().calls.clone()
    }

    /// Answers every request with the body of the first route whose needle its GraphQL document
    /// or REST path contains, and anything unrouted with an empty object, which a mutation never
    /// reads.
    pub fn route(&self, routes: Vec<(&'static str, Value)>) -> &Self {
        self.on(move |call| {
            let target = call.target();
            let body = routes
                .iter()
                .find(|(needle, _)| target.contains(needle))
                .map_or_else(|| "{}".to_owned(), |(_, body)| body.to_string());
            Ok(output(body))
        })
    }

    pub fn calls(&self) -> Vec<ApiCall> {
        self.api.main.lock().calls.clone()
    }

    pub fn call_count(&self) -> usize {
        self.api.main.lock().calls.len()
    }

    /// The nth request the transport received.
    pub fn call_at(&self, index: usize) -> ApiCall {
        self.calls()
            .get(index)
            .cloned()
            .expect("a request at that position")
    }

    pub fn vars_at(&self, index: usize) -> Map<String, Value> {
        let call = self.call_at(index);
        assert!(call.is_graphql(), "request {index} is not GraphQL");
        call.variables()
    }

    pub fn query_at(&self, index: usize) -> String {
        self.call_at(index).query().to_owned()
    }

    pub fn path_at(&self, index: usize) -> String {
        self.call_at(index).path().to_owned()
    }

    /// The search the nth request carried, or `None` for a read that did not search.
    pub fn search_of_call(&self, index: usize) -> Option<String> {
        let call = self.call_at(index);
        if !call.is_graphql() {
            return None;
        }
        call.variables()
            .get("q")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// The variables of every GraphQL request whose document contains `needle`, in order.
    pub fn variables_of(&self, needle: &str) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|call| call.is_graphql() && call.query().contains(needle))
            .map(|call| Value::Object(call.variables()))
            .collect()
    }

    /// Every REST request whose path contains `needle`, in order.
    pub fn rest_calls_to(&self, needle: &str) -> Vec<GitHubRestInput> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                ApiCall::Rest(input) if input.path.contains(needle) => Some(input),
                _ => None,
            })
            .collect()
    }
}

/// A successful answer; GraphQL reads take its body.
pub fn output(body: impl Into<String>) -> GitHubRestResponse {
    output_with(body, false, false, HashMap::new())
}

pub fn output_truncated(body: impl Into<String>) -> GitHubRestResponse {
    output_with(body, true, false, HashMap::new())
}

pub fn output_with(
    body: impl Into<String>,
    truncated: bool,
    invalid_utf8: bool,
    headers: HashMap<String, String>,
) -> GitHubRestResponse {
    GitHubRestResponse {
        status: 200,
        headers,
        body: body.into(),
        truncated,
        invalid_utf8,
    }
}

pub fn ok(body: impl Into<String>) -> Answer {
    Ok(output(body))
}

pub fn ok_json(value: Value) -> Answer {
    Ok(output(value.to_string()))
}

/// GitHub's GraphQL quota headers, leaving `remaining` of 5,000 until `reset_at_ms`.
pub fn graphql_quota(remaining: i64, reset_at_ms: i64) -> HashMap<String, String> {
    HashMap::from([
        ("x-ratelimit-resource".to_owned(), "graphql".to_owned()),
        ("x-ratelimit-limit".to_owned(), "5000".to_owned()),
        ("x-ratelimit-remaining".to_owned(), remaining.to_string()),
        (
            "x-ratelimit-reset".to_owned(),
            (reset_at_ms / 1000).to_string(),
        ),
    ])
}

/// Shallow-merges `overrides` into `base` (`{ ...base, ...overrides }`).
pub fn merged(mut base: Value, overrides: Value) -> Value {
    if let (Some(base), Value::Object(overrides)) = (base.as_object_mut(), overrides) {
        for (key, value) in overrides {
            base.insert(key, value);
        }
    }
    base
}

/// `coreResponse`.
pub fn core_response(pull_request: Value) -> Value {
    json!({
        "data": { "repository": {
            "mergeCommitAllowed": true,
            "squashMergeAllowed": false,
            "rebaseMergeAllowed": true,
            "viewerPermission": "WRITE",
            "pullRequest": merged(json!({
                "number": 7,
                "title": "Pull request 7",
                "url": "https://github.com/acme/web/pull/7",
                "headRefName": "feature",
                "baseRefName": "main",
                "headRefOid": "abc123",
                "state": "OPEN",
                "createdAt": "2026-07-01T00:00:00Z",
                "updatedAt": "2026-07-02T00:00:00Z",
                "viewerCanUpdate": true,
                "viewerDidAuthor": false,
                "viewerCanUpdateBranch": true,
                "baseRef": { "compare": { "behindBy": 2 } },
                "reviewRequests": { "nodes": [] },
                "labels": { "nodes": [] },
                "commits": { "nodes": [] },
            }), pull_request),
        } }
    })
}

/// Listing rows as GraphQL answers them. `overrides` may use the flat shapes a test reads most
/// easily: `reviewRequests` as `[{ login }]` or `[{ slug }]`, and `checks` as the rollup state.
pub fn rows(count: u64, first_number: u64, overrides: impl Fn(u64) -> Value) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let number = first_number + index;
            let mut rest = match overrides(number) {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            let review_requests = rest.remove("reviewRequests");
            let checks = rest.remove("checks");
            let mut row = json!({
                "number": number,
                "title": format!("Pull request {number}"),
                "url": format!("https://github.com/acme/web/pull/{number}"),
                "headRefName": "feat/page",
                "baseRefName": "main",
                "createdAt": "2026-07-01T00:00:00Z",
                "updatedAt": "2026-07-02T00:00:00Z",
                "repository": { "nameWithOwner": "acme/web" },
            });
            let map = row.as_object_mut().expect("object");
            if let Some(Value::Array(requests)) = review_requests {
                let nodes: Vec<Value> = requests
                    .into_iter()
                    .map(|requested_reviewer| json!({ "requestedReviewer": requested_reviewer }))
                    .collect();
                map.insert("reviewRequests".into(), json!({ "nodes": nodes }));
            }
            if let Some(state) = checks {
                map.insert(
                    "commits".into(),
                    json!({ "nodes": [{ "commit": { "statusCheckRollup": { "state": state } } }] }),
                );
            }
            for (key, value) in rest {
                map.insert(key, value);
            }
            row
        })
        .collect()
}

pub fn no_overrides(_: u64) -> Value {
    json!({})
}

/// A page of the searched listing.
pub fn pull_requests(count: u64, first_number: u64, overrides: impl Fn(u64) -> Value) -> String {
    json!({ "data": { "search": {
        "pageInfo": { "hasNextPage": false },
        "nodes": rows(count, first_number, overrides),
    } } })
    .to_string()
}

/// A page of a repository's own list, which is what the search-free fallback reads.
pub fn listed_pull_requests(
    count: u64,
    first_number: u64,
    overrides: impl Fn(u64) -> Value,
) -> String {
    json!({ "data": { "repository": { "pullRequests": {
        "pageInfo": { "hasNextPage": false },
        "nodes": rows(count, first_number, overrides),
    } } } })
    .to_string()
}

/// A search that found nothing, which is also what GitHub says for a repository it does not index.
pub fn empty_search() -> Answer {
    ok(pull_requests(0, 1, no_overrides))
}

pub fn empty_list() -> Answer {
    ok(listed_pull_requests(0, 1, no_overrides))
}

/// Answers a search with the empty search and the repository's own list with an empty list.
pub fn empty_listing(call: &ApiCall) -> Answer {
    if call.is_graphql() && call.query().contains("pullRequests(") {
        empty_list()
    } else {
        empty_search()
    }
}

pub fn pull_request_files(count: u64, first_index: u64) -> String {
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "filename": format!("src/file{}.ts", first_index + index),
                    "status": "modified",
                    "patch": "@@ -1 +1 @@\n-old\n+new",
                })
            })
            .collect(),
    )
    .to_string()
}

/// One thread's comments as the GraphQL read returns them, cursor and all.
pub fn thread_comments(
    ids: &[&str],
    end_cursor: Option<&str>,
    total_count: Option<usize>,
) -> Value {
    json!({
        "totalCount": total_count.unwrap_or(ids.len()),
        "pageInfo": { "hasNextPage": end_cursor.is_some(), "endCursor": end_cursor },
        "nodes": ids.iter().map(|id| json!({
            "id": id, "body": id, "createdAt": "2026-07-01T00:00:00Z"
        })).collect::<Vec<_>>(),
    })
}

pub fn thread(id: &str, comment_ids: &[&str]) -> Value {
    json!({
        "id": id,
        "path": "src/a.ts",
        "line": 1,
        "diffSide": "RIGHT",
        "isResolved": false,
        "isOutdated": false,
        "comments": thread_comments(comment_ids, None, None),
    })
}

pub fn review_threads_page(nodes: Vec<Value>, end_cursor: Option<&str>) -> String {
    json!({ "data": { "repository": { "pullRequest": { "reviewThreads": {
        "totalCount": nodes.len(),
        "pageInfo": { "hasNextPage": end_cursor.is_some(), "endCursor": end_cursor },
        "nodes": nodes,
    } } } } })
    .to_string()
}

pub fn thread_comments_page(
    ids: &[&str],
    end_cursor: Option<&str>,
    total_count: usize,
    pull_request_id: &str,
) -> String {
    json!({ "data": {
        "repository": { "pullRequest": { "id": "PR_7" } },
        "node": {
            "pullRequest": { "id": pull_request_id },
            "comments": thread_comments(ids, end_cursor, Some(total_count)),
        },
    } })
    .to_string()
}

/// What GitHub answers for a pull request it will not serve a whole diff for.
pub fn diff_refused() -> GitHubApiError {
    GitHubApiError::Response(crate::github::api::GitHubApiResponseError {
        host: "github.com".into(),
        operation: "getPullRequestDiff".into(),
        status: 406,
        github_errors: None,
    })
}

/// One row as a search answers it, which is the listing's row one connection deeper.
pub fn search_item(number: u64, repository: &str, updated_at: &str) -> Value {
    json!({
        "number": number,
        "title": format!("Pull request {number}"),
        "url": format!("https://github.com/{repository}/pull/{number}"),
        "author": { "login": "octocat", "avatarUrl": "https://avatars/octocat" },
        "headRefName": "feat/page",
        "baseRefName": "main",
        "state": "OPEN",
        "isDraft": false,
        "mergeable": "MERGEABLE",
        "createdAt": "2026-07-01T00:00:00Z",
        "updatedAt": updated_at,
        "repository": { "nameWithOwner": repository },
        "reviewRequests": { "nodes": [{ "requestedReviewer": { "login": "hubot" } }] },
        "labels": { "nodes": [{ "name": "bug", "color": "ff0000" }] },
    })
}

pub fn search_page(nodes: Vec<Value>, has_next_page: bool) -> Answer {
    ok_json(
        json!({ "data": { "search": { "pageInfo": { "hasNextPage": has_next_page }, "nodes": nodes } } }),
    )
}

/// The check that a client-given node hangs off the pull request it names.
pub const SUBJECT_SCOPE_QUERY: &str = "node(id: $subjectId)";

pub fn subject_scope(subject_id: &str, pull_request_id: &str) -> Value {
    json!({ "data": {
        "repository": { "pullRequest": { "id": pull_request_id } },
        "node": { "id": subject_id, "pullRequest": { "id": pull_request_id } },
    } })
}

/// The pull request node id lookup.
pub const NODE_ID_QUERY: &str = "pullRequest(number: $number) { id }";

pub fn node_id_answer(id: &str) -> Value {
    json!({ "data": { "repository": { "pullRequest": { "id": id } } } })
}

/// One page of a head commit's check contexts.
pub fn check_contexts_page(
    nodes: Vec<Value>,
    end_cursor: Option<&str>,
    head_ref_oid: &str,
) -> String {
    json!({ "data": { "repository": { "pullRequest": {
        "headRefOid": head_ref_oid,
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": end_cursor.is_some(), "endCursor": end_cursor },
        } } } }] },
    } } } })
    .to_string()
}

/// A pull request's base and head, as the REST read a file expansion starts from answers them.
pub fn pull_request_refs() -> String {
    json!({ "base": { "sha": "a1b2c3d" }, "head": { "sha": "b1c2d3e" } }).to_string()
}

/// A cross-repository pull request whose head waits on a maintainer to run its workflows.
pub fn cross_repository_detail(head_ref_oid: &str) -> Value {
    core_response(json!({
        "headRefName": "feat/page",
        "headRefOid": head_ref_oid,
        "isCrossRepository": true,
        "headRepositoryOwner": { "login": "octocat" },
    }))
}

/// Open pull requests on one head branch, every one of them from the same fork head.
pub fn heads(numbers: &[u64]) -> Value {
    json!({ "data": { "repository": { "pullRequests": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": numbers.iter().map(|number| json!({
            "number": number,
            "headRefOid": "abc123",
            "isCrossRepository": true,
            "headRepositoryOwner": { "login": "octocat" },
        })).collect::<Vec<_>>(),
    } } } })
}

pub fn workflow_runs(ids: &[i64]) -> Value {
    json!({ "workflow_runs": ids.iter().map(|id| json!({
        "id": id,
        "name": if *id == 10 { "build".to_owned() } else { format!("run {id}") },
        "html_url": format!("https://example.com/{id}"),
    })).collect::<Vec<_>>() })
}

/// Answers the reads an approval makes: the detail, the heads, the runs; approvals return nothing.
pub fn workflow_approval_routes(
    harness: &Harness,
    mut detail: impl FnMut() -> Value + Send + 'static,
    heads_answer: Value,
    runs_answer: Value,
) {
    harness.on(move |call| {
        Ok(output(if call.is_rest() {
            if call.path().ends_with("/approve") {
                String::new()
            } else {
                runs_answer.to_string()
            }
        } else if call.query().contains("headRefName: $head") {
            heads_answer.to_string()
        } else {
            detail().to_string()
        }))
    });
}
