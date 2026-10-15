//! Port of upstream/packages/source-control-github/src/server/GitHubCredentials.ts
//!
//! (Error types and the `GitHubCredentials` port are defined; the implementation is assigned.)

use async_trait::async_trait;

use crate::source_control_core::Cause;
use crate::source_control_core::redacted::Redacted;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubCredentialSource {
    Settings,
    Env,
    Gh,
}

#[derive(Debug, Clone)]
pub struct GitHubCredential {
    pub host: String,
    pub token: Redacted<String>,
    pub source: GitHubCredentialSource,
    /// A digest of host and token: safe for cache keys and rate-limit scopes, never the token.
    pub fingerprint: String,
}

/// Nothing in the environment, and no `gh` on PATH to ask.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "No GitHub credential for {host}: set GH_TOKEN, or install the GitHub CLI and run `gh auth login`."
)]
pub struct GitHubCliMissingError {
    pub host: String,
}

/// `gh` is installed but holds no login for the host, or not the account chosen in Settings.
#[derive(Debug, Clone, thiserror::Error)]
pub struct GitHubNotSignedInError {
    pub host: String,
    pub account: Option<String>,
}

impl std::fmt::Display for GitHubNotSignedInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.account {
            None => write!(
                f,
                "No GitHub credential for {host}: run `gh auth login --hostname {host}`.",
                host = self.host
            ),
            Some(account) => write!(
                f,
                "No GitHub credential for {account} on {host}: run `gh auth login --hostname {host}` for that account or pick another in Settings → Source Control.",
                host = self.host
            ),
        }
    }
}

/// The user turned the host off in Settings.
#[derive(Debug, Clone, thiserror::Error)]
#[error("GitHub host {host} is turned off in Settings → Source Control.")]
pub struct GitHubHostDisabledError {
    pub host: String,
}

/// `gh auth token` timed out or failed for a reason other than having no login.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "The GitHub CLI could not hand over a credential for {host}. Check `gh auth status` on the server."
)]
pub struct GitHubCliFailedError {
    pub host: String,
    pub cause: Cause,
}

/// There is no token for the host.
#[derive(Debug, Clone, thiserror::Error)]
pub enum GitHubCredentialUnavailableError {
    #[error(transparent)]
    CliMissing(#[from] GitHubCliMissingError),
    #[error(transparent)]
    NotSignedIn(#[from] GitHubNotSignedInError),
    #[error(transparent)]
    HostDisabled(#[from] GitHubHostDisabledError),
    #[error(transparent)]
    CliFailed(#[from] GitHubCliFailedError),
}

/// Where GitHub tokens come from. Callers ask per host and never see how the token was found.
#[async_trait]
pub trait GitHubCredentials: Send + Sync {
    async fn get(&self, host: &str) -> Result<GitHubCredential, GitHubCredentialUnavailableError>;
    /// Drops the held token after GitHub refused it, so the next read asks its source again.
    async fn invalidate(&self, host: &str);
}
