//! Port of upstream/packages/source-control-github/src/server/GitHubApi.ts
//!
//! The port (trait, inputs, errors) is defined here for every GitHub reader; the HTTP
//! implementation (`GitHubApiClient`) is assigned.
//!
//! `PinnedGitHubCredential` is [`crate::source_control_core::ambient::PinnedCredential`].

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::credentials::GitHubCredentialUnavailableError;
use crate::source_control_core::Cause;
use crate::source_control_core::rate_limit::SourceControlRateLimitPausedError;
use crate::source_control_core::redacted::Redacted;

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
