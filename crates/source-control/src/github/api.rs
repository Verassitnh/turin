//! Port of upstream/packages/source-control-github/src/server/GitHubApi.ts
//!
//! The port (trait, inputs, errors) is defined here for every GitHub reader, followed by the HTTP
//! implementation ([`GitHubApiClient`]) over a small [`GitHubHttp`] transport trait so tests can
//! inject a fake. [`collect_uint8_stream_text`] is
//! upstream/packages/provider-core/src/server/collectStreamText.ts (`collectUint8StreamText`).
//!
//! `PinnedGitHubCredential` is [`crate::source_control_core::ambient::PinnedCredential`].

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use tokio::sync::Semaphore;
use tracing::Instrument;
use turin_contracts::source_control::SourceControlProviderKind;

use super::credentials::{
    GitHubCredentialUnavailableError, GitHubCredentials, GitHubCredentialsImpl,
};
use super::quota::{GitHubQuota, js_number};
use crate::shared::host_process::Environment;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::host::SourceControlHost;
use crate::source_control_core::rate_limit::{
    RateLimitKey, RateLimitLease, SourceControlRateLimit, SourceControlRateLimitPausedError,
    retry_at_from_header,
};
use crate::source_control_core::redacted::Redacted;
use crate::source_control_core::{Cause, ambient, cause_from_message};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Polite to GitHub's secondary limits, which punish bursts of concurrent requests.
pub const CONCURRENCY: usize = 8;
pub const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, thiserror::Error)]
#[error("Could not reach GitHub at {host}.")]
pub struct GitHubApiRequestError {
    pub host: String,
    pub operation: String,
    pub cause: Cause,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "GitHub refused the credential for {host}. Run `gh auth login --hostname {host}` and retry."
)]
pub struct GitHubApiAuthenticationError {
    pub host: String,
    pub operation: String,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub API rate limit exceeded.")]
pub struct GitHubApiRateLimitError {
    pub host: String,
    pub operation: String,
    /// Epoch milliseconds.
    pub retry_at: Option<i64>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub could not find the requested resource, or the credential cannot see it.")]
pub struct GitHubApiNotFoundError {
    pub host: String,
    pub operation: String,
}

/// GitHub answered with a failure that is none of the above. `github_errors` carries GitHub's own
/// error messages, from a GraphQL `errors` list or a REST `message`/`errors` body: they name the
/// field and the reason ("A pull request already exists for acme:feature"), never a token.
#[derive(Debug, Clone)]
pub struct GitHubApiResponseError {
    pub host: String,
    pub operation: String,
    pub status: u16,
    pub github_errors: Option<Vec<String>>,
}

impl std::fmt::Display for GitHubApiResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.github_errors {
            Some(errors) if !errors.is_empty() => {
                write!(f, "GitHub returned an error: {}", errors.join("; "))
            }
            _ => write!(f, "GitHub returned HTTP {}.", self.status),
        }
    }
}
impl std::error::Error for GitHubApiResponseError {}

#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubApiError {
    #[error(transparent)]
    CredentialUnavailable(#[from] GitHubCredentialUnavailableError),
    #[error(transparent)]
    Request(#[from] GitHubApiRequestError),
    #[error(transparent)]
    Authentication(#[from] GitHubApiAuthenticationError),
    #[error(transparent)]
    RateLimit(#[from] GitHubApiRateLimitError),
    #[error(transparent)]
    NotFound(#[from] GitHubApiNotFoundError),
    #[error(transparent)]
    Response(#[from] GitHubApiResponseError),
    #[error(transparent)]
    Paused(#[from] SourceControlRateLimitPausedError),
}

#[derive(Debug, Clone)]
pub struct GitHubRestResponse {
    pub status: u16,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: String,
    pub truncated: bool,
    /// The body was not valid UTF-8, which a raw file read takes to mean binary.
    pub invalid_utf8: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubRestInput {
    pub host: String,
    pub operation: String,
    pub method: RestMethod,
    /// Relative to the API root, e.g. `repos/acme/web/pulls/7`; query string included.
    pub path: String,
    pub body: Option<Value>,
    /// Defaults to `application/vnd.github+json`.
    pub accept: Option<String>,
    /// Revalidates a cached answer. A 304 is returned rather than failed, and is free.
    pub if_none_match: Option<String>,
    pub max_response_bytes: Option<usize>,
    /// Defaults to 30 seconds; a whole pull request's patch may need longer.
    pub timeout: Option<Duration>,
    /// Overrides the ambient `interactive` flag for this one request.
    pub allow_reserve: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubGraphQlInput {
    pub host: String,
    pub operation: String,
    pub query: String,
    pub variables: Option<serde_json::Map<String, Value>>,
    pub allow_reserve: Option<bool>,
    pub max_response_bytes: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct GitHubRequestCredential {
    pub token: Redacted<String>,
    pub fingerprint: String,
}

#[async_trait]
pub trait GitHubApi: Send + Sync {
    /// The raw JSON body of a successful GraphQL answer, with `rateLimit` recorded.
    async fn graphql(&self, input: GitHubGraphQlInput) -> Result<String, GitHubApiError>;
    async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError>;
    /// The credential a request to `host` would carry right now.
    async fn credential(&self, host: &str) -> Result<GitHubRequestCredential, GitHubApiError>;
}

// ---------------------------------------------------------------------------------------------
// collectStreamText.ts
// ---------------------------------------------------------------------------------------------

/// `CollectedUint8StreamText`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedUint8StreamText {
    pub text: String,
    pub truncated: bool,
    pub bytes: usize,
    pub invalid_utf8: bool,
}

/// `decodeUtf8`: lossy text plus whether the bytes were not valid UTF-8.
pub fn decode_utf8(bytes: &[u8]) -> (String, bool) {
    (
        String::from_utf8_lossy(bytes).into_owned(),
        std::str::from_utf8(bytes).is_err(),
    )
}

/// `collectUint8StreamText`: folds a byte stream into text, keeping at most `max_bytes` (`None`
/// is unbounded). `truncated` is set when more bytes arrive than fit; the stream is still drained
/// to its end so the peer can finish normally. An empty stream is an empty answer.
pub async fn collect_uint8_stream_text<S, B, E>(
    stream: S,
    max_bytes: Option<usize>,
) -> Result<CollectedUint8StreamText, E>
where
    S: futures::Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let max_bytes = max_bytes.unwrap_or(usize::MAX);
    let mut chunks: Vec<u8> = Vec::new();
    let mut truncated = false;
    futures::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let chunk = chunk.as_ref();
        if truncated {
            continue;
        }
        let remaining = max_bytes - chunks.len();
        if remaining == 0 {
            truncated = true;
            continue;
        }
        if chunk.len() > remaining {
            chunks.extend_from_slice(&chunk[..remaining]);
            truncated = true;
        } else {
            chunks.extend_from_slice(chunk);
        }
    }
    let (text, invalid_utf8) = decode_utf8(&chunks);
    Ok(CollectedUint8StreamText {
        text,
        bytes: chunks.len(),
        truncated,
        invalid_utf8,
    })
}

// ---------------------------------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------------------------------

/// One HTTP request, fully built (the client adds the credential headers).
#[derive(Debug, Clone)]
pub struct GitHubHttpRequest {
    pub method: RestMethod,
    pub url: String,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
    /// A JSON body, already serialized.
    pub body: Option<String>,
    /// Cap for the collected body; see [`collect_uint8_stream_text`].
    pub max_response_bytes: usize,
}

impl GitHubHttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn set_header(&mut self, name: &str, value: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.headers
            .push((name.to_ascii_lowercase(), value.to_owned()));
    }
}

/// A finished answer: status, lower-cased headers and the body collected under the request's cap.
#[derive(Debug, Clone, Default)]
pub struct GitHubHttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub truncated: bool,
    pub invalid_utf8: bool,
}

/// What [`GitHubApiClient`] needs of an HTTP client. The deadline is applied by the caller around
/// the whole call (headers and body), so an implementation need not time out itself.
#[async_trait]
pub trait GitHubHttp: Send + Sync {
    async fn execute(&self, request: GitHubHttpRequest) -> Result<GitHubHttpResponse, Cause>;
}

/// [`GitHubHttp`] over `reqwest`.
#[derive(Debug, Clone, Default)]
pub struct ReqwestGitHubHttp {
    client: reqwest::Client,
}

impl ReqwestGitHubHttp {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GitHubHttp for ReqwestGitHubHttp {
    async fn execute(&self, request: GitHubHttpRequest) -> Result<GitHubHttpResponse, Cause> {
        let method = match request.method {
            RestMethod::Get => reqwest::Method::GET,
            RestMethod::Post => reqwest::Method::POST,
            RestMethod::Put => reqwest::Method::PUT,
            RestMethod::Patch => reqwest::Method::PATCH,
            RestMethod::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder.send().await.map_err(into_cause)?;
        let status = response.status().as_u16();
        let mut headers: HashMap<String, String> = HashMap::new();
        for (name, value) in response.headers() {
            // Repeated headers combine with ", " as `fetch` does.
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            headers
                .entry(name.as_str().to_ascii_lowercase())
                .and_modify(|existing| {
                    existing.push_str(", ");
                    existing.push_str(&value);
                })
                .or_insert(value);
        }
        // 204, 304 and many refusals carry no body at all, which is an empty answer.
        let collected =
            collect_uint8_stream_text(response.bytes_stream(), Some(request.max_response_bytes))
                .await
                .map_err(into_cause)?;
        Ok(GitHubHttpResponse {
            status,
            headers,
            body: collected.text,
            truncated: collected.truncated,
            invalid_utf8: collected.invalid_utf8,
        })
    }
}

fn into_cause(error: reqwest::Error) -> Cause {
    Arc::new(error)
}

// ---------------------------------------------------------------------------------------------
// GitHubApi.make
// ---------------------------------------------------------------------------------------------

fn normalize_host(host: &str) -> String {
    host.trim().to_lowercase()
}

/// The REST root and GraphQL endpoint of one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubApiUrls {
    pub rest: String,
    pub graphql: String,
}

/// Where the API for a host lives. github.com and GHE.com data residency serve it from an `api.`
/// subdomain; GitHub Enterprise Server serves it under `/api` on the instance itself.
pub fn github_api_urls(host: &str) -> GitHubApiUrls {
    let normalized = normalize_host(host);
    if normalized == "github.com" {
        return GitHubApiUrls {
            rest: "https://api.github.com".into(),
            graphql: "https://api.github.com/graphql".into(),
        };
    }
    if normalized.ends_with(".ghe.com") {
        return GitHubApiUrls {
            rest: format!("https://api.{normalized}"),
            graphql: format!("https://api.{normalized}/graphql"),
        };
    }
    GitHubApiUrls {
        rest: format!("https://{normalized}/api/v3"),
        graphql: format!("https://{normalized}/api/graphql"),
    }
}

/// GraphQL documents longer than this are cut in traces; the hash still identifies them.
const TRACED_QUERY_MAX_CHARS: usize = 4_000;

/// A stable short id for a GraphQL document, so traces group by query without its text.
pub fn query_hash(query: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in query.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// The pause GitHub asked for, from `retry-after` or the primary limit's reset.
fn retry_at_from(headers: &HashMap<String, String>, now: i64) -> Option<i64> {
    if let Some(from_retry_after) =
        retry_at_from_header(headers.get("retry-after").map(String::as_str), now)
    {
        return Some(from_retry_after);
    }
    let reset = js_number(headers.get("x-ratelimit-reset")) * 1_000.0;
    (reset.is_finite() && reset > now as f64).then_some(reset as i64)
}

static RATE_LIMIT_BODY: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)rate limit").unwrap());
static RATE_LIMIT_EXCEEDED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)rate limit (already )?exceeded").unwrap());

struct GraphQlError {
    r#type: Option<String>,
    message: Option<String>,
}

/// `decodeGraphQlErrors`: the non-empty `errors` list of a GraphQL body, or None when the body is
/// not JSON, has no such list, or has entries of another shape.
fn decode_graphql_errors(body: &str) -> Option<Vec<GraphQlError>> {
    fn optional_string(value: Option<&Value>) -> Option<Option<String>> {
        match value {
            None => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => None,
        }
    }
    let value: Value = serde_json::from_str(body).ok()?;
    let errors = value.as_object()?.get("errors")?.as_array()?;
    if errors.is_empty() {
        return None;
    }
    errors
        .iter()
        .map(|error| {
            let error = error.as_object()?;
            Some(GraphQlError {
                r#type: optional_string(error.get("type"))?,
                message: optional_string(error.get("message"))?,
            })
        })
        .collect()
}

/// A REST failure body: `{ message, errors: [{ message } | { resource, field, code }] }`.
fn rest_error_messages(body: &str) -> Option<Vec<String>> {
    fn optional_string(value: Option<&Value>) -> Option<Option<String>> {
        match value {
            None => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => None,
        }
    }
    let value: Value = serde_json::from_str(body).ok()?;
    let object = value.as_object()?;
    let message = optional_string(object.get("message"))?;
    let mut details: Vec<String> = Vec::new();
    match object.get("errors") {
        None => {}
        Some(Value::Array(errors)) => {
            for error in errors {
                match error {
                    Value::String(text) => details.push(text.clone()),
                    Value::Object(error) => {
                        let message = optional_string(error.get("message"))?;
                        let field = optional_string(error.get("field"))?;
                        let code = optional_string(error.get("code"))?;
                        if let Some(message) = message {
                            details.push(message);
                        } else if let (Some(field), Some(code)) = (field, code) {
                            details.push(format!("{field} {code}"));
                        }
                    }
                    _ => return None,
                }
            }
        }
        Some(_) => return None,
    }
    let messages: Vec<String> = message.into_iter().chain(details).collect();
    (!messages.is_empty()).then_some(messages)
}

/// What one GitHub answer means, decided once from its status, headers and body.
#[derive(Debug, PartialEq)]
enum Answer {
    Ok,
    RateLimited,
    Unauthorized,
    NotFound,
    Failed { messages: Option<Vec<String>> },
}

/// GitHub reports a failed GraphQL document with HTTP 200 and an `errors` list, so a GraphQL body
/// is read for its error types as well as its status. `gh api graphql` failed those too, and
/// callers rely on that to fall back to narrower reads.
fn classify(
    status: u16,
    headers: &HashMap<String, String>,
    body: &str,
    graphql: bool,
    accept_not_modified: bool,
) -> Answer {
    let errors = if graphql {
        decode_graphql_errors(body)
    } else {
        None
    };
    let types: Vec<&str> = errors
        .iter()
        .flatten()
        .filter_map(|error| error.r#type.as_deref())
        .collect();
    let messages: Option<Vec<String>> = errors.as_ref().map(|errors| {
        errors
            .iter()
            .filter_map(|error| error.message.clone())
            .collect()
    });
    let remaining_is_zero = headers.get("x-ratelimit-remaining").map(String::as_str) == Some("0");
    if status == 429
        || types.contains(&"RATE_LIMITED")
        // An exhausted GraphQL quota can answer HTTP 200 with an untyped "API rate limit already
        // exceeded" error; the headers say the same thing.
        || (graphql && errors.is_some() && remaining_is_zero)
        || (graphql
            && messages
                .as_ref()
                .is_some_and(|messages| messages.iter().any(|m| RATE_LIMIT_EXCEEDED.is_match(m))))
        || (status == 403
            && (remaining_is_zero
                || headers.contains_key("retry-after")
                || RATE_LIMIT_BODY.is_match(body)))
    {
        return Answer::RateLimited;
    }
    if status == 401 {
        return Answer::Unauthorized;
    }
    if status == 404
        || errors.as_ref().is_some_and(|errors| {
            errors
                .iter()
                .all(|e| e.r#type.as_deref() == Some("NOT_FOUND"))
        })
    {
        return Answer::NotFound;
    }
    if errors.is_some() {
        return Answer::Failed { messages };
    }
    if (200..300).contains(&status) || (status == 304 && accept_not_modified) {
        return Answer::Ok;
    }
    Answer::Failed {
        messages: if graphql {
            None
        } else {
            rest_error_messages(body)
        },
    }
}

struct SendInput {
    host: String,
    operation: String,
    request: GitHubHttpRequest,
    timeout: Option<Duration>,
    allow_reserve: bool,
    accept_not_modified: bool,
    /// Reads the body for GraphQL `errors`, which GitHub sends with HTTP 200.
    graphql: bool,
}

/// `GitHubApi.make`: the HTTP implementation of [`GitHubApi`].
pub struct GitHubApiClient {
    http: Arc<dyn GitHubHttp>,
    credentials: Arc<dyn GitHubCredentials>,
    quota: Arc<GitHubQuota>,
    limits: Arc<SourceControlRateLimit>,
    clock: Arc<dyn Clock>,
    gate: Arc<Semaphore>,
}

/// Everything [`GitHubApiClient::with_dependencies`] built, so the application can share the
/// credentials, quota and rate-limit pauses with other readers.
pub struct GitHubApiDependencies {
    pub api: Arc<GitHubApiClient>,
    pub credentials: Arc<GitHubCredentialsImpl>,
    pub quota: Arc<GitHubQuota>,
    pub limits: Arc<SourceControlRateLimit>,
}

impl GitHubApiClient {
    pub fn new(
        http: Arc<dyn GitHubHttp>,
        credentials: Arc<dyn GitHubCredentials>,
        quota: Arc<GitHubQuota>,
        limits: Arc<SourceControlRateLimit>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            http,
            credentials,
            quota,
            limits,
            clock,
            gate: Arc::new(Semaphore::new(CONCURRENCY)),
        }
    }

    /// `layerWithDependencies`: the transport with the credential, quota reserve and rate-limit
    /// pause it reads through. An application builds this once so every GitHub reader shares one
    /// view of each quota per host.
    pub fn with_dependencies(
        host: Arc<dyn SourceControlHost>,
        environment: Arc<dyn Environment>,
        clock: Arc<dyn Clock>,
        http: Arc<dyn GitHubHttp>,
    ) -> GitHubApiDependencies {
        let credentials = Arc::new(GitHubCredentialsImpl::new(host, environment, clock.clone()));
        let quota = Arc::new(GitHubQuota::new(clock.clone()));
        let limits = SourceControlRateLimit::new(clock.clone());
        let api = Arc::new(Self::new(
            http,
            credentials.clone(),
            quota.clone(),
            limits.clone(),
            clock,
        ));
        GitHubApiDependencies {
            api,
            credentials,
            quota,
            limits,
        }
    }

    async fn credential_for(&self, host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
        let normalized = normalize_host(host);
        if let Some(pinned) = ambient::pinned_credential() {
            // A pinned page only ever talks to the host it verified.
            if pinned.host != normalized {
                return Err(GitHubApiAuthenticationError {
                    host: normalized,
                    operation: "credential".into(),
                }
                .into());
            }
            return Ok(GitHubRequestCredential {
                token: pinned.token,
                fingerprint: pinned.credential_fingerprint,
            });
        }
        let held = self.credentials.get(&normalized).await?;
        Ok(GitHubRequestCredential {
            token: held.token,
            fingerprint: held.fingerprint,
        })
    }

    /// Sends one request under the host's rate-limit pause and fails a refusal with the error that
    /// says why.
    async fn send(&self, input: SendInput) -> Result<GitHubRestResponse, GitHubApiError> {
        let host = normalize_host(&input.host);
        let span = tracing::debug_span!(
            "GitHubApi.send",
            github.host = %host,
            github.operation = %input.operation,
            github.kind = if input.graphql { "graphql" } else { "rest" },
            http.request.method = ?input.request.method,
            // Only the path: a query string can carry a SHA or a branch, and never needs to be in a trace.
            url.path = %url_path(&input.request.url),
        );
        self.send_inner(host, input).instrument(span).await
    }

    async fn send_inner(
        &self,
        host: String,
        input: SendInput,
    ) -> Result<GitHubRestResponse, GitHubApiError> {
        let GitHubRequestCredential { token, fingerprint } = self.credential_for(&host).await?;
        let scope = ambient::credential_scope();
        // REST spends `core` and GraphQL its own quota, each with its own reserve. Every REST path
        // this reads today is `core`; a `search/` read would need its own resource here. A refusal
        // still pauses the whole host, the key PullRequestService records its own backoff under.
        let resource = if input.graphql { "graphql" } else { "core" };
        let key = RateLimitKey {
            provider: SourceControlProviderKind::new("github").expect("github is a valid kind"),
            host: host.clone(),
        };
        // The rate-limit scope follows the credential, so a pause on one account never blocks another.
        let scope = if scope.is_empty() { fingerprint } else { scope };
        ambient::with_credential_scope(scope, async {
            let _permit = self
                .gate
                .acquire()
                .await
                .expect("the gate semaphore is never closed");
            self.run(&host, &input, token, resource, &key).await
        })
        .await
    }

    async fn run(
        &self,
        host: &str,
        input: &SendInput,
        token: Redacted<String>,
        resource: &str,
        key: &RateLimitKey,
    ) -> Result<GitHubRestResponse, GitHubApiError> {
        let paused = |error: SourceControlRateLimitPausedError| {
            tracing::debug!(github.paused = true, github.retry_at = error.retry_at);
            error
        };
        self.quota
            .admit(host, resource, input.allow_reserve)
            .map_err(paused)?;
        let lease = self
            .limits
            .check(key, input.allow_reserve)
            .map_err(paused)?;

        let mut request = input.request.clone();
        request.set_header("authorization", &format!("Bearer {}", token.value()));
        request.set_header("x-github-api-version", API_VERSION);
        request.set_header("user-agent", "t3code");

        // One deadline covers the headers and the body: a host that answers headers and then stalls
        // must not hold a slot of the shared gate.
        let deadline = input.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let request_error = |cause: Cause| GitHubApiRequestError {
            host: host.to_owned(),
            operation: input.operation.clone(),
            cause,
        };
        let response = match tokio::time::timeout(deadline, self.http.execute(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(cause)) => return Err(request_error(cause).into()),
            Err(_) => {
                return Err(request_error(cause_from_message(format!(
                    "Timed out after {}ms",
                    deadline.as_millis()
                )))
                .into());
            }
        };

        let GitHubHttpResponse {
            status,
            headers,
            body,
            truncated,
            invalid_utf8,
        } = response;
        self.quota.observe(host, &headers);
        tracing::debug!(
            http.response.status_code = status,
            rate_limit = %headers_for_trace(&headers)
        );
        let lease_handle = RateLimitLease::new(key, lease);
        match classify(
            status,
            &headers,
            &body,
            input.graphql,
            input.accept_not_modified,
        ) {
            Answer::Ok => {
                self.limits.record_success(&lease_handle);
                Ok(GitHubRestResponse {
                    status,
                    headers,
                    body,
                    truncated,
                    invalid_utf8,
                })
            }
            Answer::RateLimited => {
                let retry_at = retry_at_from(&headers, self.clock.now_millis());
                self.limits.record_rate_limit(&lease_handle, retry_at);
                tracing::debug!(github.rate_limited = true, github.retry_at = ?retry_at);
                Err(GitHubApiRateLimitError {
                    host: host.to_owned(),
                    operation: input.operation.clone(),
                    retry_at,
                }
                .into())
            }
            // The source may hold a newer token than the one that was refused.
            Answer::Unauthorized => {
                self.credentials.invalidate(host).await;
                Err(GitHubApiAuthenticationError {
                    host: host.to_owned(),
                    operation: input.operation.clone(),
                }
                .into())
            }
            Answer::NotFound => Err(GitHubApiNotFoundError {
                host: host.to_owned(),
                operation: input.operation.clone(),
            }
            .into()),
            Answer::Failed { messages } => Err(GitHubApiResponseError {
                host: host.to_owned(),
                operation: input.operation.clone(),
                status,
                github_errors: messages,
            }
            .into()),
        }
    }
}

fn url_path(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|url| url.path().to_owned())
        .unwrap_or_default()
}

/// The numeric rate-limit headers GitHub sends, as trace fields.
fn headers_for_trace(headers: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (header, name) in [
        ("x-ratelimit-limit", "github.ratelimit.limit"),
        ("x-ratelimit-remaining", "github.ratelimit.remaining"),
        ("x-ratelimit-used", "github.ratelimit.used"),
        ("x-ratelimit-reset", "github.ratelimit.reset"),
    ] {
        if let Some(value) = headers.get(header)
            && js_number(Some(value)).is_finite()
        {
            parts.push(format!("{name}={value}"));
        }
    }
    if let Some(resource) = headers.get("x-ratelimit-resource") {
        parts.push(format!("github.ratelimit.resource={resource}"));
    }
    parts.join(" ")
}

#[async_trait]
impl GitHubApi for GitHubApiClient {
    async fn graphql(&self, input: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
        let host = normalize_host(&input.host);
        let allow_reserve = input.allow_reserve.unwrap_or_else(ambient::interactive);
        // The document, never its variables: user text (bodies, search terms) travels as variables.
        let traced_query: String = if input.query.chars().count() > TRACED_QUERY_MAX_CHARS {
            let cut: String = input.query.chars().take(TRACED_QUERY_MAX_CHARS).collect();
            format!("{cut}…")
        } else {
            input.query.clone()
        };
        let span = tracing::debug_span!(
            "GitHubApi.graphql",
            github.operation = %input.operation,
            github.graphql.query_hash = %query_hash(&input.query),
            github.graphql.query = %traced_query,
        );
        async {
            let body = serde_json::to_string(&serde_json::json!({
                "query": input.query,
                "variables": input.variables.clone().unwrap_or_default(),
            }))
            .expect("a JSON value serializes");
            let response = self
                .send(SendInput {
                    host: host.clone(),
                    operation: input.operation.clone(),
                    request: GitHubHttpRequest {
                        method: RestMethod::Post,
                        url: github_api_urls(&host).graphql,
                        headers: vec![
                            ("accept".into(), "application/json".into()),
                            ("content-type".into(), "application/json".into()),
                        ],
                        body: Some(body),
                        max_response_bytes: input
                            .max_response_bytes
                            .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES),
                    },
                    timeout: None,
                    allow_reserve,
                    accept_not_modified: false,
                    graphql: true,
                })
                .await?;
            // A body cut at the byte cap hides any `errors` past the cut and cannot be decoded.
            if response.truncated {
                return Err(GitHubApiResponseError {
                    host,
                    operation: input.operation,
                    status: response.status,
                    github_errors: None,
                }
                .into());
            }
            Ok(response.body)
        }
        .instrument(span)
        .await
    }

    async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
        let url = format!(
            "{}/{}",
            github_api_urls(&input.host).rest,
            input.path.trim_start_matches('/')
        );
        let mut headers = vec![(
            "accept".to_owned(),
            input
                .accept
                .clone()
                .unwrap_or_else(|| "application/vnd.github+json".into()),
        )];
        if let Some(if_none_match) = &input.if_none_match {
            headers.push(("if-none-match".into(), if_none_match.clone()));
        }
        let body = input.body.as_ref().map(|body| {
            headers.push(("content-type".into(), "application/json".into()));
            serde_json::to_string(body).expect("a JSON value serializes")
        });
        self.send(SendInput {
            host: input.host.clone(),
            operation: input.operation.clone(),
            request: GitHubHttpRequest {
                method: input.method,
                url,
                headers,
                body,
                max_response_bytes: input
                    .max_response_bytes
                    .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES),
            },
            timeout: input.timeout,
            allow_reserve: input.allow_reserve.unwrap_or_else(ambient::interactive),
            accept_not_modified: input.if_none_match.is_some(),
            graphql: false,
        })
        .await
    }

    async fn credential(&self, host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
        self.credential_for(host).await
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;
    use serde_json::json;
    use turin_contracts::vcs::{VcsError, VcsProcessSpawnError, VcsProcessTimeoutError};

    use super::*;
    use crate::github::credentials::GitHubCredential;
    use crate::github::credentials::GitHubCredentialSource;
    use crate::github::credentials::test_support::{TestHost, process_output};
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::clock::ManualClock;

    fn now() -> i64 {
        chrono::DateTime::parse_from_rfc3339("2026-10-05T12:00:00.000Z")
            .unwrap()
            .timestamp_millis()
    }

    struct FakeCredentials {
        tokens: Mutex<Vec<String>>,
        invalidations: AtomicUsize,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl GitHubCredentials for FakeCredentials {
        async fn get(
            &self,
            host: &str,
        ) -> Result<GitHubCredential, GitHubCredentialUnavailableError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let token = self.tokens.lock()[0].clone();
            Ok(GitHubCredential {
                host: host.to_owned(),
                fingerprint: format!("{host}:{token}"),
                token: Redacted::new(token),
                source: GitHubCredentialSource::Gh,
            })
        }
        async fn invalidate(&self, _host: &str) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
            self.tokens.lock().remove(0);
        }
    }

    type Respond = Box<dyn Fn(&GitHubHttpRequest, usize) -> GitHubHttpResponse + Send + Sync>;

    struct FakeHttp {
        requests: Mutex<Vec<GitHubHttpRequest>>,
        respond: Respond,
        /// Never answers, for the deadline.
        hang: bool,
    }

    #[async_trait]
    impl GitHubHttp for FakeHttp {
        async fn execute(&self, request: GitHubHttpRequest) -> Result<GitHubHttpResponse, Cause> {
            if self.hang {
                std::future::pending::<()>().await;
            }
            let mut requests = self.requests.lock();
            requests.push(request.clone());
            Ok((self.respond)(&request, requests.len()))
        }
    }

    struct Harness {
        api: GitHubApiClient,
        http: Arc<FakeHttp>,
        credentials: Arc<FakeCredentials>,
        clock: Arc<ManualClock>,
    }

    impl Harness {
        fn requests(&self) -> Vec<GitHubHttpRequest> {
            self.requests_snapshot()
        }
        fn requests_snapshot(&self) -> Vec<GitHubHttpRequest> {
            self.http.requests.lock().clone()
        }
        fn authorizations(&self) -> Vec<String> {
            self.requests()
                .iter()
                .map(|request| request.header("authorization").unwrap().to_owned())
                .collect()
        }
    }

    fn harness_with(
        hang: bool,
        respond: impl Fn(&GitHubHttpRequest, usize) -> GitHubHttpResponse + Send + Sync + 'static,
    ) -> Harness {
        let clock = ManualClock::new(now());
        let http = Arc::new(FakeHttp {
            requests: Mutex::new(Vec::new()),
            respond: Box::new(respond),
            hang,
        });
        let credentials = Arc::new(FakeCredentials {
            tokens: Mutex::new(vec!["first".into(), "second".into()]),
            invalidations: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        });
        let api = GitHubApiClient::new(
            http.clone(),
            credentials.clone(),
            Arc::new(GitHubQuota::new(clock.clone())),
            SourceControlRateLimit::new(clock.clone()),
            clock.clone(),
        );
        Harness {
            api,
            http,
            credentials,
            clock,
        }
    }

    fn harness(
        respond: impl Fn(&GitHubHttpRequest, usize) -> GitHubHttpResponse + Send + Sync + 'static,
    ) -> Harness {
        harness_with(false, respond)
    }

    fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> GitHubHttpResponse {
        GitHubHttpResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.to_owned(),
            truncated: false,
            invalid_utf8: false,
        }
    }

    fn json_ok(body: serde_json::Value) -> GitHubHttpResponse {
        response(
            200,
            &body.to_string(),
            &[("content-type", "application/json")],
        )
    }

    fn json_with(
        body: serde_json::Value,
        status: u16,
        headers: &[(&str, &str)],
    ) -> GitHubHttpResponse {
        response(status, &body.to_string(), headers)
    }

    fn graphql_input(operation: &str, query: &str) -> GitHubGraphQlInput {
        GitHubGraphQlInput {
            host: "github.com".into(),
            operation: operation.into(),
            query: query.into(),
            ..Default::default()
        }
    }

    fn rest_input(operation: &str, path: &str) -> GitHubRestInput {
        GitHubRestInput {
            host: "github.com".into(),
            operation: operation.into(),
            path: path.into(),
            ..Default::default()
        }
    }

    #[test]
    fn github_api_urls_map_github_com_ghe_com_and_github_enterprise_server() {
        assert_eq!(
            github_api_urls("GitHub.com"),
            GitHubApiUrls {
                rest: "https://api.github.com".into(),
                graphql: "https://api.github.com/graphql".into(),
            }
        );
        assert_eq!(
            github_api_urls("acme.ghe.com").graphql,
            "https://api.acme.ghe.com/graphql"
        );
        assert_eq!(
            github_api_urls("git.acme.internal"),
            GitHubApiUrls {
                rest: "https://git.acme.internal/api/v3".into(),
                graphql: "https://git.acme.internal/api/graphql".into(),
            }
        );
    }

    #[test]
    fn environment_token_follows_gh_precedence_per_kind_of_host() {
        use crate::github::credentials::environment_token;
        let env = |pairs: &[(&str, &str)]| MapEnvironment::new(pairs.iter().copied());
        let all = [
            ("GH_TOKEN", "gh"),
            ("GITHUB_TOKEN", "github"),
            ("GH_ENTERPRISE_TOKEN", "ghe"),
        ];
        assert_eq!(
            environment_token("github.com", env(&all).as_ref()),
            Some("gh".into())
        );
        assert_eq!(
            environment_token("acme.ghe.com", env(&[("GITHUB_TOKEN", "github")]).as_ref()),
            Some("github".into())
        );
        // An enterprise token only goes to the host GH_HOST names, never to whatever a remote says.
        assert_eq!(
            environment_token("git.acme.internal", env(&all).as_ref()),
            None
        );
        let mut with_host = all.to_vec();
        with_host.push(("GH_HOST", "git.acme.internal"));
        assert_eq!(
            environment_token("git.acme.internal", env(&with_host).as_ref()),
            Some("ghe".into())
        );
        assert_eq!(
            environment_token("git.acme.internal", env(&[("GH_TOKEN", "gh")]).as_ref()),
            None
        );
    }

    #[test]
    fn query_hash_is_fnv1a_over_utf16_units() {
        assert_eq!(query_hash(""), "811c9dc5");
        assert_eq!(query_hash("a"), "e40c292c");
        assert_eq!(query_hash("query { viewer { login } }").len(), 8);
    }

    #[tokio::test]
    async fn sends_graphql_with_the_token_and_the_document_as_written() {
        let h = harness(|_, _| json_ok(json!({ "data": { "viewer": { "login": "julius" } } })));
        let body = h
            .api
            .graphql(graphql_input("viewer", "query { viewer { login } }"))
            .await
            .unwrap();
        assert!(body.contains("\"login\":\"julius\""));
        let requests = h.requests();
        assert_eq!(requests[0].url, "https://api.github.com/graphql");
        assert_eq!(requests[0].method, RestMethod::Post);
        assert_eq!(requests[0].header("authorization"), Some("Bearer first"));
        assert_eq!(
            requests[0].header("x-github-api-version"),
            Some("2022-11-28")
        );
        assert_eq!(requests[0].header("user-agent"), Some("t3code"));
        assert_eq!(requests[0].header("accept"), Some("application/json"));
        // No `rateLimit` selection is added: the quota is read off the response headers.
        assert!(
            requests[0]
                .body
                .as_deref()
                .unwrap()
                .contains("\"query\":\"query { viewer { login } }\"")
        );
        assert!(
            requests[0]
                .body
                .as_deref()
                .unwrap()
                .contains("\"variables\":{}")
        );
    }

    #[tokio::test]
    async fn resolves_graphql_credentials_once_per_request_and_refreshes_after_refusal() {
        let h = harness(|request, _| {
            if request.header("authorization") == Some("Bearer first") {
                response(401, "", &[])
            } else {
                json_ok(json!({ "data": { "viewer": { "login": "julius" } } }))
            }
        });
        let input = graphql_input("viewer", "query { viewer { login } }");
        let refused = h.api.graphql(input.clone()).await.unwrap_err();
        assert!(matches!(refused, GitHubApiError::Authentication(_)));
        assert_eq!(h.credentials.reads.load(Ordering::SeqCst), 1);
        assert_eq!(h.credentials.invalidations.load(Ordering::SeqCst), 1);

        let body = h.api.graphql(input).await.unwrap();
        assert!(body.contains("\"login\":\"julius\""));
        assert_eq!(h.credentials.reads.load(Ordering::SeqCst), 2);
        assert_eq!(h.authorizations(), vec!["Bearer first", "Bearer second"]);
    }

    #[tokio::test]
    async fn fails_a_graphql_answer_that_carries_errors_naming_githubs_reason() {
        let h = harness(|_, _| {
            json_ok(json!({
                "data": null,
                "errors": [{ "type": "FORBIDDEN", "message": "Resource not accessible" }]
            }))
        });
        let error = h
            .api
            .graphql(graphql_input("detail", "query { viewer { id } }"))
            .await
            .unwrap_err();
        assert!(matches!(error, GitHubApiError::Response(_)));
        assert!(error.to_string().contains("Resource not accessible"));
    }

    #[tokio::test]
    async fn reads_a_not_found_answer_with_another_error_as_a_failure_not_a_missing_resource() {
        let h = harness(|_, _| {
            json_ok(json!({
                "data": null,
                "errors": [
                    { "type": "NOT_FOUND", "message": "Could not resolve" },
                    { "message": "Something went wrong" }
                ]
            }))
        });
        let error = h
            .api
            .graphql(graphql_input("detail", "query { viewer { id } }"))
            .await
            .unwrap_err();
        assert!(matches!(error, GitHubApiError::Response(_)));

        let h = harness(|_, _| {
            json_ok(json!({
                "data": null,
                "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve" }]
            }))
        });
        let error = h
            .api
            .graphql(graphql_input("detail", "query { viewer { id } }"))
            .await
            .unwrap_err();
        assert!(matches!(error, GitHubApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn pauses_the_host_after_a_graphql_rate_limited_answer_until_the_reset() {
        let reset = now() / 1000 + 600;
        let reset_header = reset.to_string();
        let h = harness(move |_, _| {
            json_with(
                json!({ "errors": [{ "type": "RATE_LIMITED", "message": "API rate limit exceeded" }] }),
                200,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", &reset_header),
                ],
            )
        });
        let first = h
            .api
            .graphql(graphql_input("summary", "query { viewer { id } }"))
            .await
            .unwrap_err();
        match first {
            GitHubApiError::RateLimit(error) => assert_eq!(error.retry_at, Some(reset * 1000)),
            other => panic!("expected a rate limit, got {other:?}"),
        }
        let second = h
            .api
            .rest(rest_input("stack", "repos/acme/web/stacks"))
            .await
            .unwrap_err();
        match second {
            GitHubApiError::Paused(error) => assert_eq!(error.retry_at, reset * 1000),
            other => panic!("expected a pause, got {other:?}"),
        }
        assert_eq!(h.requests().len(), 1);
    }

    #[tokio::test]
    async fn keeps_the_last_tenth_of_a_quota_for_interactive_requests_until_it_resets() {
        let reset = now() / 1000 + 600;
        let remaining = Arc::new(AtomicUsize::new(499));
        let counter = remaining.clone();
        let h = harness(move |request, _| {
            if request.url.ends_with("/graphql") {
                json_ok(json!({ "data": { "viewer": { "id": "1" } } }))
            } else {
                let left = counter.fetch_sub(1, Ordering::SeqCst).to_string();
                json_with(
                    json!({ "ok": true }),
                    200,
                    &[
                        ("x-ratelimit-resource", "core"),
                        ("x-ratelimit-limit", "5000"),
                        ("x-ratelimit-remaining", &left),
                        ("x-ratelimit-reset", &reset.to_string()),
                    ],
                )
            }
        });
        let read = || rest_input("sweep", "repos/acme/web/pulls");
        // The first answer leaves 499 of 5000, under the last tenth; the next background read waits.
        h.api.rest(read()).await.unwrap();
        match h.api.rest(read()).await.unwrap_err() {
            GitHubApiError::Paused(error) => assert_eq!(error.retry_at, reset * 1000),
            other => panic!("expected a pause, got {other:?}"),
        }
        // A user's own request may spend the reserve, and GraphQL has a quota of its own.
        ambient::with_interactive(true, async { h.api.rest(read()).await.unwrap() }).await;
        h.api
            .graphql(graphql_input("x", "query { viewer { id } }"))
            .await
            .unwrap();
        assert_eq!(h.requests().len(), 3);
        // The reset gives the background its quota back.
        h.clock.set(reset * 1000);
        h.api.rest(read()).await.unwrap();
        assert_eq!(h.requests().len(), 4);
    }

    #[tokio::test]
    async fn carries_githubs_own_reason_for_a_refused_rest_request() {
        let h = harness(|_, _| {
            json_with(
                json!({
                    "message": "Validation Failed",
                    "errors": [{
                        "resource": "PullRequest",
                        "code": "custom",
                        "message": "A pull request already exists for acme:feature."
                    }]
                }),
                422,
                &[],
            )
        });
        let error = h
            .api
            .rest(GitHubRestInput {
                method: RestMethod::Post,
                ..rest_input("createPullRequest", "repos/acme/web/pulls")
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub returned an error: Validation Failed; A pull request already exists for acme:feature."
        );
    }

    #[tokio::test]
    async fn lets_an_interactive_request_through_a_pause_a_background_read_recorded() {
        let reset = (now() / 1000 + 600).to_string();
        let h = harness(move |_, call| {
            if call == 1 {
                json_with(
                    json!({ "message": "API rate limit exceeded" }),
                    403,
                    &[
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset", &reset),
                    ],
                )
            } else {
                json_ok(json!({ "ok": true }))
            }
        });
        let read = || rest_input("sweep", "repos/acme/web/pulls");
        h.api.rest(read()).await.unwrap_err();
        assert!(matches!(
            h.api.rest(read()).await.unwrap_err(),
            GitHubApiError::Paused(_)
        ));
        let merged = ambient::with_interactive(true, async {
            h.api
                .rest(GitHubRestInput {
                    method: RestMethod::Put,
                    ..rest_input("merge", "repos/acme/web/pulls/7/merge")
                })
                .await
                .unwrap()
        })
        .await;
        assert_eq!(merged.status, 200);
        assert_eq!(h.requests().len(), 2);
    }

    #[tokio::test]
    async fn reads_an_untyped_graphql_quota_error_as_a_rate_limit() {
        let reset = now() / 1000 + 900;
        let reset_header = reset.to_string();
        let h = harness(move |_, _| {
            json_with(
                json!({ "errors": [{ "message": "API rate limit already exceeded for user ID 1." }] }),
                200,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", &reset_header),
                ],
            )
        });
        match h
            .api
            .graphql(graphql_input("lookup", "query { viewer { id } }"))
            .await
            .unwrap_err()
        {
            GitHubApiError::RateLimit(error) => assert_eq!(error.retry_at, Some(reset * 1000)),
            other => panic!("expected a rate limit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn maps_rest_403_with_an_exhausted_quota_to_a_rate_limit_and_304_to_an_answer() {
        let h = harness(|_, call| {
            if call == 1 {
                response(304, "", &[])
            } else {
                json_with(
                    json!({ "message": "API rate limit exceeded" }),
                    403,
                    &[("retry-after", "60")],
                )
            }
        });
        let not_modified = h
            .api
            .rest(GitHubRestInput {
                if_none_match: Some("\"abc\"".into()),
                ..rest_input("checks", "repos/acme/web/pulls/7")
            })
            .await
            .unwrap();
        assert_eq!(not_modified.status, 304);
        assert_eq!(h.requests()[0].header("if-none-match"), Some("\"abc\""));
        match h
            .api
            .rest(rest_input("checks", "repos/acme/web/pulls/7"))
            .await
            .unwrap_err()
        {
            GitHubApiError::RateLimit(error) => assert_eq!(error.retry_at, Some(now() + 60_000)),
            other => panic!("expected a rate limit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_304_without_if_none_match_is_a_failure() {
        let h = harness(|_, _| response(304, "", &[]));
        let error = h.api.rest(rest_input("checks", "x")).await.unwrap_err();
        match error {
            GitHubApiError::Response(error) => {
                assert_eq!(error.status, 304);
                assert_eq!(error.to_string(), "GitHub returned HTTP 304.");
            }
            other => panic!("expected a response error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drops_a_refused_token_so_the_next_request_asks_the_source_again() {
        let h = harness(|request, _| {
            if request.header("authorization") == Some("Bearer first") {
                response(401, "", &[])
            } else {
                json_ok(json!({ "ok": true }))
            }
        });
        let refused = h.api.rest(rest_input("user", "user")).await.unwrap_err();
        assert!(matches!(refused, GitHubApiError::Authentication(_)));
        assert_eq!(h.credentials.invalidations.load(Ordering::SeqCst), 1);
        let answered = h.api.rest(rest_input("user", "user")).await.unwrap();
        assert_eq!(answered.status, 200);
        assert_eq!(h.authorizations(), vec!["Bearer first", "Bearer second"]);
    }

    #[tokio::test]
    async fn refuses_to_send_a_pinned_credential_to_another_host() {
        let h = harness(|_, _| json_ok(json!({})));
        let pinned = ambient::PinnedCredential {
            host: "github.com".into(),
            token: Redacted::new("pinned".into()),
            credential_fingerprint: "github.com:pinned".into(),
        };
        let error = ambient::with_pinned_credential(Some(pinned.clone()), async {
            h.api
                .rest(GitHubRestInput {
                    host: "git.acme.internal".into(),
                    ..rest_input("user", "user")
                })
                .await
                .unwrap_err()
        })
        .await;
        match error {
            GitHubApiError::Authentication(error) => assert_eq!(error.operation, "credential"),
            other => panic!("expected an authentication error, got {other:?}"),
        }
        assert!(h.requests().is_empty());

        // On its own host the pinned token is the one sent, and the credential source is not asked.
        ambient::with_pinned_credential(Some(pinned), async {
            h.api.rest(rest_input("user", "user")).await.unwrap();
        })
        .await;
        assert_eq!(h.authorizations(), vec!["Bearer pinned"]);
        assert_eq!(h.credentials.reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rest_builds_the_url_headers_and_json_body() {
        let h = harness(|_, _| json_ok(json!({})));
        h.api
            .rest(GitHubRestInput {
                method: RestMethod::Patch,
                path: "//repos/acme/web/pulls/7?x=1".into(),
                body: Some(json!({ "title": "t" })),
                accept: Some("application/vnd.github.v3.diff".into()),
                ..rest_input("update", "")
            })
            .await
            .unwrap();
        let request = &h.requests()[0];
        assert_eq!(
            request.url,
            "https://api.github.com/repos/acme/web/pulls/7?x=1"
        );
        assert_eq!(request.method, RestMethod::Patch);
        assert_eq!(
            request.header("accept"),
            Some("application/vnd.github.v3.diff")
        );
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(request.body.as_deref(), Some("{\"title\":\"t\"}"));
    }

    #[tokio::test]
    async fn a_truncated_graphql_body_is_a_response_error() {
        let h = harness(|_, _| GitHubHttpResponse {
            truncated: true,
            ..json_ok(json!({ "data": {} }))
        });
        match h
            .api
            .graphql(graphql_input("big", "query { viewer { id } }"))
            .await
            .unwrap_err()
        {
            GitHubApiError::Response(error) => {
                assert_eq!(error.status, 200);
                assert_eq!(error.to_string(), "GitHub returned HTTP 200.");
            }
            other => panic!("expected a response error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_request_that_outlives_its_deadline_fails_as_unreachable() {
        let h = harness_with(true, |_, _| json_ok(json!({})));
        let error = h
            .api
            .rest(GitHubRestInput {
                timeout: Some(Duration::from_millis(30)),
                ..rest_input("slow", "user")
            })
            .await
            .unwrap_err();
        match error {
            GitHubApiError::Request(error) => {
                assert_eq!(error.to_string(), "Could not reach GitHub at github.com.");
                assert_eq!(error.operation, "slow");
            }
            other => panic!("expected a request error, got {other:?}"),
        }
    }

    #[test]
    fn classify_covers_the_answer_table() {
        let none = HashMap::new();
        assert_eq!(classify(200, &none, "{}", false, false), Answer::Ok);
        assert_eq!(classify(429, &none, "", false, false), Answer::RateLimited);
        assert_eq!(classify(401, &none, "", false, false), Answer::Unauthorized);
        assert_eq!(classify(404, &none, "", false, false), Answer::NotFound);
        assert_eq!(
            classify(403, &none, "secondary Rate Limit hit", false, false),
            Answer::RateLimited
        );
        assert_eq!(
            classify(403, &none, "{\"message\":\"Forbidden\"}", false, false),
            Answer::Failed {
                messages: Some(vec!["Forbidden".into()])
            }
        );
        assert_eq!(
            classify(500, &none, "<html>", false, false),
            Answer::Failed { messages: None }
        );
        // Empty `errors` is not an error list.
        assert_eq!(
            classify(200, &none, "{\"errors\":[]}", true, false),
            Answer::Ok
        );
        // An error without a message is a failure with no messages.
        assert_eq!(
            classify(200, &none, "{\"errors\":[{\"type\":\"X\"}]}", true, false),
            Answer::Failed {
                messages: Some(vec![])
            }
        );
        // REST errors in several shapes.
        assert_eq!(
            rest_error_messages(
                "{\"message\":\"Validation Failed\",\"errors\":[\"plain\",{\"field\":\"head\",\"code\":\"invalid\"},{\"resource\":\"R\"}]}"
            ),
            Some(vec![
                "Validation Failed".into(),
                "plain".into(),
                "head invalid".into()
            ])
        );
        assert_eq!(rest_error_messages("not json"), None);
        assert_eq!(rest_error_messages("{}"), None);
    }

    #[tokio::test]
    async fn collects_stream_text_with_a_byte_cap() {
        use futures::stream;
        let chunks = |parts: &[&str]| {
            stream::iter(
                parts
                    .iter()
                    .map(|p| Ok::<_, ()>(p.as_bytes().to_vec()))
                    .collect::<Vec<_>>(),
            )
        };
        let collected = collect_uint8_stream_text(chunks(&["abc", "def"]), None)
            .await
            .unwrap();
        assert_eq!(
            collected,
            CollectedUint8StreamText {
                text: "abcdef".into(),
                truncated: false,
                bytes: 6,
                invalid_utf8: false
            }
        );
        // A chunk that crosses the cap is cut and marks truncation; the rest is drained.
        let collected = collect_uint8_stream_text(chunks(&["abc", "def", "ghi"]), Some(4))
            .await
            .unwrap();
        assert_eq!(
            (collected.text.as_str(), collected.truncated),
            ("abcd", true)
        );
        // Exactly at the cap is not truncated until more arrives.
        let collected = collect_uint8_stream_text(chunks(&["abc", "d"]), Some(4))
            .await
            .unwrap();
        assert_eq!(
            (collected.text.as_str(), collected.truncated),
            ("abcd", false)
        );
        let collected = collect_uint8_stream_text(chunks(&["abcd", "e"]), Some(4))
            .await
            .unwrap();
        assert_eq!(
            (collected.text.as_str(), collected.truncated),
            ("abcd", true)
        );
        // An empty body is an empty answer.
        let collected = collect_uint8_stream_text(chunks(&[]), Some(4))
            .await
            .unwrap();
        assert_eq!(collected.text, "");
        assert!(!collected.truncated);
        // Bytes that are not UTF-8 are flagged.
        let binary = stream::iter(vec![Ok::<_, ()>(vec![0xff_u8, 0x61])]);
        let collected = collect_uint8_stream_text(binary, None).await.unwrap();
        assert!(collected.invalid_utf8);
        assert_eq!(collected.text, "\u{fffd}a");
    }

    // GitHubCredentials cases that upstream keeps in GitHubApi.test.ts.

    fn credentials_with(
        run: impl Fn() -> Result<crate::source_control_core::host::SourceControlProcessOutput, VcsError>
        + Send
        + Sync
        + 'static,
    ) -> GitHubCredentialsImpl {
        let mut host = TestHost::new();
        host.process = Some(Box::new(move |_| run()));
        GitHubCredentialsImpl::new(
            Arc::new(host),
            MapEnvironment::new(Vec::<(String, String)>::new()),
            ManualClock::new(now()),
        )
    }

    #[tokio::test]
    async fn fails_with_cli_missing_when_gh_is_not_on_path() {
        let credentials = credentials_with(|| {
            Err(VcsError::Spawn(
                VcsProcessSpawnError::VcsProcessSpawnError {
                    operation: "GitHubCredentials.get".into(),
                    command: "gh".into(),
                    cwd: "/".into(),
                    argument_count: None,
                    cause: json!({ "reason": { "_tag": "NotFound" } }),
                },
            ))
        });
        let error = credentials.get("git.acme.internal").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::CliMissing(_)
        ));
    }

    #[tokio::test]
    async fn does_not_cache_a_gh_failure_that_is_not_about_signing_in() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let credentials = credentials_with(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(VcsError::Timeout(
                    VcsProcessTimeoutError::VcsProcessTimeoutError {
                        operation: "GitHubCredentials.get".into(),
                        command: "gh".into(),
                        cwd: "/".into(),
                        argument_count: None,
                        timeout_ms: 10_000.0,
                    },
                ))
            } else {
                Ok(process_output("token\n"))
            }
        });
        let first = credentials.get("github.com").await.unwrap_err();
        assert!(matches!(
            first,
            GitHubCredentialUnavailableError::CliFailed(_)
        ));
        assert_eq!(
            credentials.get("github.com").await.unwrap().source,
            GitHubCredentialSource::Gh
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fails_with_not_signed_in_when_gh_prints_no_token() {
        let credentials = credentials_with(|| Ok(process_output("\n")));
        let error = credentials.get("git.acme.internal").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::NotSignedIn(_)
        ));
    }

    /// Live smoke test: a read-only GET of a public repository with the token `gh auth token`
    /// prints. Run with `TURIN_LIVE_GITHUB=1 cargo test -p turin-source-control -- --ignored live`.
    #[tokio::test]
    #[ignore = "hits the real GitHub API; set TURIN_LIVE_GITHUB=1"]
    async fn live_smoke_reads_a_public_repository() {
        if std::env::var("TURIN_LIVE_GITHUB").as_deref() != Ok("1") {
            return;
        }
        let output = std::process::Command::new("gh")
            .args(["auth", "token", "--hostname", "github.com"])
            .output()
            .expect("gh is installed");
        let token = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        assert!(!token.is_empty(), "gh holds no login for github.com");
        let pinned = ambient::PinnedCredential {
            host: "github.com".into(),
            credential_fingerprint: fingerprint_for_test(&token),
            token: Redacted::new(token),
        };
        let h = harness(|_, _| json_ok(json!({})));
        let api = GitHubApiClient::new(
            Arc::new(ReqwestGitHubHttp::new()),
            h.credentials.clone(),
            Arc::new(GitHubQuota::new(h.clock.clone())),
            SourceControlRateLimit::new(h.clock.clone()),
            h.clock.clone(),
        );
        let answer = ambient::with_pinned_credential(Some(pinned), async {
            api.rest(rest_input("liveSmoke", "repos/octocat/Hello-World"))
                .await
                .unwrap()
        })
        .await;
        assert_eq!(answer.status, 200);
        let repo: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(repo["full_name"], "octocat/Hello-World");
    }

    fn fingerprint_for_test(token: &str) -> String {
        crate::github::credentials::fingerprint_of("github.com", token)
    }
}
