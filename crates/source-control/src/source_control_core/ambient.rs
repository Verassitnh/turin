//! Fiber-local context references from upstream, as tokio task-locals.
//!
//! Effect `Context.Reference`s travel with the fiber and are inherited by forked fibers. Tokio
//! task-locals travel with the task but are **not** inherited by `tokio::spawn`; code that spawns
//! must call [`capture`] first and run the child future through [`Captured::run`].
//!
//! - `SourceControlRateLimit.CredentialScope` → [`credential_scope`] / [`with_credential_scope`]
//! - `SourceControlRateLimit.Interactive` → [`interactive`] / [`with_interactive`]
//! - `GitHubApi.PinnedGitHubCredential` → [`pinned_credential`] / [`with_pinned_credential`]
//!   (moved here so the neutral service layer can pin a credential without naming GitHub).

use std::future::Future;

use super::redacted::Redacted;

/// A credential already verified for one host. Every request made under it must target that
/// host, so a page cannot read one account's data with another's token mid-flight. Server-local:
/// never put its value in RPC payloads or cache keys.
#[derive(Debug, Clone)]
pub struct PinnedCredential {
    pub host: String,
    pub token: Redacted<String>,
    pub credential_fingerprint: String,
}

tokio::task_local! {
    static CREDENTIAL_SCOPE: String;
    static INTERACTIVE: bool;
    static PINNED_CREDENTIAL: Option<PinnedCredential>;
}

/// The rate-limit scope; defaults to `""`.
pub fn credential_scope() -> String {
    CREDENTIAL_SCOPE.try_with(Clone::clone).unwrap_or_default()
}

/// Set by interactive callers (a user's read or write, not a background sweep). Defaults to false.
pub fn interactive() -> bool {
    INTERACTIVE.try_with(|v| *v).unwrap_or(false)
}

/// The pinned credential, or `None`.
pub fn pinned_credential() -> Option<PinnedCredential> {
    PINNED_CREDENTIAL.try_with(Clone::clone).unwrap_or(None)
}

pub async fn with_credential_scope<F: Future>(scope: String, fut: F) -> F::Output {
    CREDENTIAL_SCOPE.scope(scope, fut).await
}

pub async fn with_interactive<F: Future>(value: bool, fut: F) -> F::Output {
    INTERACTIVE.scope(value, fut).await
}

pub async fn with_pinned_credential<F: Future>(
    credential: Option<PinnedCredential>,
    fut: F,
) -> F::Output {
    PINNED_CREDENTIAL.scope(credential, fut).await
}

/// A snapshot of every reference, for carrying into a spawned task.
#[derive(Debug, Clone)]
pub struct Captured {
    credential_scope: String,
    interactive: bool,
    pinned: Option<PinnedCredential>,
}

pub fn capture() -> Captured {
    Captured {
        credential_scope: credential_scope(),
        interactive: interactive(),
        pinned: pinned_credential(),
    }
}

impl Captured {
    /// Runs `fut` with the captured references in scope.
    pub async fn run<F: Future>(self, fut: F) -> F::Output {
        CREDENTIAL_SCOPE
            .scope(
                self.credential_scope,
                INTERACTIVE.scope(self.interactive, PINNED_CREDENTIAL.scope(self.pinned, fut)),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn defaults_and_scoping() {
        assert_eq!(credential_scope(), "");
        assert!(!interactive());
        assert!(pinned_credential().is_none());
        with_interactive(true, async {
            assert!(interactive());
            let captured = capture();
            tokio::spawn(captured.run(async { assert!(interactive()) }))
                .await
                .unwrap();
            with_interactive(false, async { assert!(!interactive()) }).await;
        })
        .await;
    }
}
