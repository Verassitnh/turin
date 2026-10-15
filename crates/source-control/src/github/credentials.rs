//! Port of upstream/packages/source-control-github/src/server/GitHubCredentials.ts
//!

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use indexmap::IndexMap;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use super::definition::{GitHubHostChoice, GitHubSettings, read_settings};
use crate::shared::host_process::{Environment, working_directory};
use crate::source_control_core::Cause;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::host::{
    SourceControlHost, SourceControlProcessInput, is_command_not_found,
};
use crate::source_control_core::redacted::Redacted;
use turin_contracts::vcs::VcsError;

/// How long a token is reused before `gh` is asked again, so a `gh auth switch` applies soon.
const TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
/// No credential is retried sooner, so a fresh `gh auth login` takes effect on the next read.
const MISSING_TTL: Duration = Duration::from_secs(10);
const CACHE_CAPACITY: usize = 32;

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

fn normalize_host(host: &str) -> String {
    host.trim().to_lowercase()
}

/// Hosts gh treats as GitHub.com-like for `GH_TOKEN`: github.com and GHE.com data residency.
fn is_github_dot_com(host: &str) -> bool {
    host == "github.com" || host.ends_with(".ghe.com")
}

/// The environment token for a host, in gh's precedence order. gh hands `GH_ENTERPRISE_TOKEN` to
/// any non-github.com host; here it only goes to the host `GH_HOST` names, because a remote URL
/// picks the host and a hostile one must not receive an enterprise token.
pub fn environment_token(host: &str, env: &dyn Environment) -> Option<String> {
    let names: &[&str] = if is_github_dot_com(host) {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else if env
        .get("GH_HOST")
        .is_some_and(|value| value.trim().to_lowercase() == host)
    {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    } else {
        &[]
    };
    names.iter().find_map(|name| {
        let value = env.get(name)?;
        let value = crate::shared::js::js_trim(&value);
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// `host:sha256(token)`, safe for cache keys and rate-limit scopes.
pub fn fingerprint_of(host: &str, token: &str) -> String {
    format!("{host}:{:x}", Sha256::digest(token.as_bytes()))
}

type LookupResult = Result<GitHubCredential, GitHubCredentialUnavailableError>;
type SharedLookup = Shared<BoxFuture<'static, LookupResult>>;

enum Slot {
    /// A lookup in flight; concurrent readers of the key share it.
    Pending { id: u64, future: SharedLookup },
    Ready {
        result: LookupResult,
        expires_at: i64,
    },
}

#[derive(Default)]
struct CacheState {
    /// Least recently used first.
    slots: IndexMap<String, Slot>,
    next_id: u64,
}

struct Inner {
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    clock: Arc<dyn Clock>,
    cache: Mutex<CacheState>,
}

/// Upstream `GitHubCredentials.make`: settings token, then environment, then `gh auth token`,
/// with a small TTL cache over the last two (the settings token is read fresh each time).
#[derive(Clone)]
pub struct GitHubCredentialsImpl {
    inner: Arc<Inner>,
}

impl GitHubCredentialsImpl {
    pub fn new(
        host: Arc<dyn SourceControlHost>,
        environment: Arc<dyn Environment>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                host,
                environment,
                clock,
                cache: Mutex::new(CacheState::default()),
            }),
        }
    }
}

/// Cache key: the host plus its pinned account, so a changed pin misses the cache.
fn cache_key(host: &str, account: Option<&str>) -> String {
    match account {
        None => host.to_owned(),
        Some(account) => format!("{host}\u{0}{account}"),
    }
}

impl Inner {
    async fn read_settings(&self) -> Option<GitHubSettings> {
        // Unreadable settings fall back to gh's own choice.
        let settings = self.host.settings_get().await.ok()?;
        let saved = settings.source_control_hosts.get("github").map(|map| {
            serde_json::Value::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        });
        Some(read_settings(saved.as_ref()))
    }

    /// The Settings choice for a host; unreadable settings fall back to gh's own choice.
    async fn host_choice(&self, host: &str) -> Option<GitHubHostChoice> {
        self.read_settings().await?.host_choice(host).cloned()
    }

    /// A token saved in Settings for the host, read fresh so a saved or removed one applies at once.
    async fn saved_token(&self, host: &str) -> Option<String> {
        let settings = self.read_settings().await?;
        settings.token(host).map(str::to_owned)
    }

    async fn read_gh_token(
        &self,
        host: &str,
        account: Option<&str>,
    ) -> Result<String, GitHubCredentialUnavailableError> {
        let mut args: Vec<String> = ["auth", "token", "--hostname", host]
            .into_iter()
            .map(str::to_owned)
            .collect();
        if let Some(account) = account {
            args.push("--user".into());
            args.push(account.to_owned());
        }
        let not_signed_in = || GitHubNotSignedInError {
            host: host.to_owned(),
            account: account.map(str::to_owned),
        };
        let output = self
            .host
            .process_run(SourceControlProcessInput {
                operation: "GitHubCredentials.get".into(),
                command: "gh".into(),
                args,
                cwd: working_directory(),
                // Never let gh print the token into a debug log.
                env: Some(HashMap::from([
                    ("GH_DEBUG".to_owned(), String::new()),
                    ("GH_PROMPT_DISABLED".to_owned(), "1".to_owned()),
                ])),
                timeout_ms: Some(10_000),
                ..Default::default()
            })
            .await
            .map_err(|error| {
                if is_command_not_found(&error) {
                    GitHubCliMissingError {
                        host: host.to_owned(),
                    }
                    .into()
                } else if matches!(error, VcsError::Exit(_)) {
                    // gh exits non-zero with "no oauth token" when it has no login for the host.
                    GitHubCredentialUnavailableError::from(not_signed_in())
                } else {
                    GitHubCliFailedError {
                        host: host.to_owned(),
                        cause: Arc::new(error),
                    }
                    .into()
                }
            })?;
        let token = crate::shared::js::js_trim(&output.stdout);
        if token.is_empty() {
            return Err(not_signed_in().into());
        }
        Ok(token.to_owned())
    }

    async fn lookup(&self, key: String) -> LookupResult {
        let mut parts = key.split('\u{0}');
        let host = parts.next().unwrap_or(&key).to_owned();
        let choice = parts.next().map(str::to_owned);
        // An environment token wins over a pinned account, exactly as it does in gh.
        let from_env = environment_token(&host, self.environment.as_ref());
        // A pinned login gh no longer holds (logged out, expired) falls back to the active one,
        // which is what discovery reports as the account in use.
        let token = match &from_env {
            Some(token) => token.clone(),
            None => match self.read_gh_token(&host, choice.as_deref()).await {
                Err(GitHubCredentialUnavailableError::NotSignedIn(_)) if choice.is_some() => {
                    self.read_gh_token(&host, None).await?
                }
                other => other?,
            },
        };
        Ok(GitHubCredential {
            fingerprint: fingerprint_of(&host, &token),
            host,
            token: Redacted::new(token),
            source: if from_env.is_some() {
                GitHubCredentialSource::Env
            } else {
                GitHubCredentialSource::Gh
            },
        })
    }

    /// TTL of a finished lookup. A transient gh failure (a timeout, a locked keyring) is asked
    /// again on the next read.
    fn time_to_live(result: &LookupResult) -> Duration {
        match result {
            Ok(_) => TOKEN_TTL,
            Err(GitHubCredentialUnavailableError::CliFailed(_)) => Duration::ZERO,
            Err(_) => MISSING_TTL,
        }
    }

    /// Upstream `Cache.get`: capacity-bounded (least recently used goes first), per-outcome TTL,
    /// and concurrent readers of one key share a single lookup.
    async fn cached(self: &Arc<Self>, key: String) -> LookupResult {
        let (id, future) = {
            let mut cache = self.cache.lock();
            let now = self.clock.now_millis();
            match cache.slots.shift_remove(&key) {
                Some(Slot::Ready { result, expires_at }) if expires_at > now => {
                    cache.slots.insert(
                        key,
                        Slot::Ready {
                            result: result.clone(),
                            expires_at,
                        },
                    );
                    return result;
                }
                Some(Slot::Pending { id, future }) => {
                    cache.slots.insert(
                        key.clone(),
                        Slot::Pending {
                            id,
                            future: future.clone(),
                        },
                    );
                    (id, future)
                }
                _ => {
                    let id = cache.next_id;
                    cache.next_id += 1;
                    let this = Arc::clone(self);
                    let lookup_key = key.clone();
                    let future = async move { this.lookup(lookup_key).await }
                        .boxed()
                        .shared();
                    cache.slots.insert(
                        key.clone(),
                        Slot::Pending {
                            id,
                            future: future.clone(),
                        },
                    );
                    while cache.slots.len() > CACHE_CAPACITY {
                        cache.slots.shift_remove_index(0);
                    }
                    (id, future)
                }
            }
        };
        let result = future.await;
        // Settle the slot once; whichever reader finishes first does it. An invalidated or
        // evicted slot (or a newer lookup under the same key) is left alone.
        let mut cache = self.cache.lock();
        if let Some(Slot::Pending { id: current, .. }) = cache.slots.get(&key)
            && *current == id
        {
            let ttl = Self::time_to_live(&result);
            if ttl.is_zero() {
                cache.slots.shift_remove(&key);
            } else {
                let expires_at = self.clock.now_millis() + ttl.as_millis() as i64;
                cache.slots.insert(
                    key,
                    Slot::Ready {
                        result: result.clone(),
                        expires_at,
                    },
                );
            }
        }
        result
    }
}

#[async_trait]
impl GitHubCredentials for GitHubCredentialsImpl {
    async fn get(&self, host: &str) -> Result<GitHubCredential, GitHubCredentialUnavailableError> {
        let host = normalize_host(host);
        let choice = self.inner.host_choice(&host).await;
        if choice.as_ref().is_some_and(|choice| !choice.enabled) {
            return Err(GitHubHostDisabledError { host }.into());
        }
        // A token saved in Settings is the most deliberate choice, so it comes before the
        // environment and gh. It is read from the secret store each time, so it needs no cache.
        if let Some(saved) = self.inner.saved_token(&host).await {
            return Ok(GitHubCredential {
                fingerprint: fingerprint_of(&host, &saved),
                host,
                token: Redacted::new(saved),
                source: GitHubCredentialSource::Settings,
            });
        }
        let account = choice
            .as_ref()
            .and_then(|choice| choice.account.as_ref().map(|a| a.as_str()));
        self.inner.cached(cache_key(&host, account)).await
    }

    async fn invalidate(&self, host: &str) {
        let host = normalize_host(host);
        let choice = self.inner.host_choice(&host).await;
        let key = cache_key(
            &host,
            choice
                .as_ref()
                .and_then(|c| c.account.as_ref().map(|a| a.as_str())),
        );
        self.inner.cache.lock().slots.shift_remove(&key);
    }
}

/// A `SourceControlHost` for tests (upstream `TestSourceControlHost`): settings the test can
/// replace mid-test, process runs and `git execute` answered by what the test supplies, and
/// every other call panics so an unexpected one is visible.
#[cfg(test)]
#[allow(clippy::result_large_err)]
pub(crate) mod test_support {
    use super::*;
    use crate::source_control_core::host::{
        SourceControlGitExecuteInput, SourceControlGitOutput, SourceControlGitRemoteBranch,
        SourceControlProcessOutput,
    };
    use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
    use turin_contracts::settings::{ServerSettings, ServerSettingsError};
    use turin_contracts::vcs::VcsListRemotesResult;

    pub type ProcessHandler = Box<
        dyn Fn(SourceControlProcessInput) -> Result<SourceControlProcessOutput, VcsError>
            + Send
            + Sync,
    >;
    pub type GitHandler = Box<
        dyn Fn(SourceControlGitExecuteInput) -> Result<SourceControlGitOutput, GitCommandError>
            + Send
            + Sync,
    >;

    #[derive(Default)]
    pub struct TestHost {
        pub settings: Mutex<ServerSettings>,
        pub process: Option<ProcessHandler>,
        pub git_execute: Option<GitHandler>,
    }

    impl TestHost {
        pub fn new() -> Self {
            Self::default()
        }

        /// Replaces the saved GitHub host settings (the `sourceControlHosts.github` blob).
        pub fn set_github_settings(&self, blob: serde_json::Value) {
            let map: IndexMap<String, serde_json::Value> = blob
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            self.settings
                .lock()
                .source_control_hosts
                .insert("github".into(), map);
        }
    }

    pub fn process_output(stdout: &str) -> SourceControlProcessOutput {
        SourceControlProcessOutput {
            stdout: stdout.to_owned(),
            ..Default::default()
        }
    }

    #[async_trait]
    impl SourceControlHost for TestHost {
        async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError> {
            Ok(self.settings.lock().clone())
        }
        async fn process_run(
            &self,
            input: SourceControlProcessInput,
        ) -> Result<SourceControlProcessOutput, VcsError> {
            match &self.process {
                Some(run) => run(input),
                None => panic!("Unexpected {} run in {}.", input.command, input.operation),
            }
        }
        async fn git_execute(
            &self,
            input: SourceControlGitExecuteInput,
        ) -> Result<SourceControlGitOutput, GitCommandError> {
            match &self.git_execute {
                Some(run) => run(input),
                None => panic!("Unexpected git execute in a test that supplied none."),
            }
        }
        async fn git_resolve_commit(&self, _: &str, _: &str) -> Result<String, GitCommandError> {
            panic!("Unexpected git resolveCommit in a test that supplied none.")
        }
        async fn git_remotes(&self, _: &str) -> Result<VcsListRemotesResult, VcsError> {
            panic!("Unexpected git remotes in a test that supplied none.")
        }
        async fn git_read_config_value(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<String>, GitCommandError> {
            panic!("Unexpected git readConfigValue in a test that supplied none.")
        }
        async fn git_resolve_primary_remote_name(
            &self,
            _: &str,
        ) -> Result<String, GitCommandError> {
            panic!("Unexpected git resolvePrimaryRemoteName in a test that supplied none.")
        }
        async fn git_ensure_remote(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<String, GitCommandError> {
            panic!("Unexpected git ensureRemote in a test that supplied none.")
        }
        async fn git_list_local_branch_names(
            &self,
            _: &str,
        ) -> Result<Vec<String>, GitCommandError> {
            panic!("Unexpected git listLocalBranchNames in a test that supplied none.")
        }
        async fn git_fetch_remote_branch(
            &self,
            _: &SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            panic!("Unexpected git fetchRemoteBranch in a test that supplied none.")
        }
        async fn git_fetch_remote_tracking_branch(
            &self,
            _: &SourceControlGitRemoteBranch,
        ) -> Result<(), GitCommandError> {
            panic!("Unexpected git fetchRemoteTrackingBranch in a test that supplied none.")
        }
        async fn git_set_branch_upstream(
            &self,
            _: &SourceControlGitRemoteBranch,
            _: &str,
        ) -> Result<(), GitCommandError> {
            panic!("Unexpected git setBranchUpstream in a test that supplied none.")
        }
        async fn git_switch_ref(
            &self,
            _: &VcsSwitchRefInput,
        ) -> Result<VcsSwitchRefResult, GitCommandError> {
            panic!("Unexpected git switchRef in a test that supplied none.")
        }
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::test_support::{TestHost, process_output};
    use super::*;
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::clock::ManualClock;
    use serde_json::json;
    use turin_contracts::vcs::{VcsProcessExitError, VcsProcessSpawnError};

    type Calls = Arc<Mutex<Vec<Vec<String>>>>;

    /// Settings keep `enabled` explicit; a host the test leaves it off is enabled.
    fn host_settings(hosts: serde_json::Value, tokens: serde_json::Value) -> serde_json::Value {
        let hosts: serde_json::Map<String, serde_json::Value> = hosts
            .as_object()
            .unwrap()
            .iter()
            .map(|(host, choice)| {
                let mut merged = json!({ "enabled": true });
                merged
                    .as_object_mut()
                    .unwrap()
                    .extend(choice.as_object().unwrap().clone());
                (host.clone(), merged)
            })
            .collect();
        json!({ "hosts": hosts, "tokens": tokens })
    }

    struct Harness {
        host: Arc<TestHost>,
        credentials: GitHubCredentialsImpl,
        calls: Calls,
        clock: Arc<ManualClock>,
    }

    fn harness_with(
        hosts: serde_json::Value,
        signed_out: &[&str],
        tokens: serde_json::Value,
        env: &[(&str, &str)],
        fail: Option<fn() -> VcsError>,
    ) -> Harness {
        let calls: Calls = Arc::default();
        let recorded = calls.clone();
        let signed_out: Vec<String> = signed_out.iter().map(|s| (*s).to_owned()).collect();
        let mut host = TestHost::new();
        host.process = Some(Box::new(move |input| {
            recorded.lock().push(input.args.clone());
            if let Some(fail) = fail {
                return Err(fail());
            }
            let user = input
                .args
                .iter()
                .position(|arg| arg == "--user")
                .map(|i| input.args[i + 1].clone());
            Ok(match user {
                Some(user) if signed_out.contains(&user) => process_output(""),
                Some(user) => process_output(&format!("token-for-{user}\n")),
                None => process_output("active-token\n"),
            })
        }));
        host.set_github_settings(host_settings(hosts, tokens));
        let host = Arc::new(host);
        let clock = ManualClock::new(1_000);
        let credentials = GitHubCredentialsImpl::new(
            host.clone(),
            MapEnvironment::new(env.iter().copied()),
            clock.clone(),
        );
        Harness {
            host,
            credentials,
            calls,
            clock,
        }
    }

    fn harness(hosts: serde_json::Value) -> Harness {
        harness_with(hosts, &[], json!({}), &[], None)
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn exit_error() -> VcsError {
        VcsError::Exit(VcsProcessExitError::VcsProcessExitError {
            operation: "GitHubCredentials.get".into(),
            command: "gh".into(),
            cwd: ".".into(),
            argument_count: None,
            exit_code: 1.0,
            detail: "no oauth token".into(),
            failure_kind: None,
            retryable: None,
            stderr_length: None,
            stderr_truncated: None,
        })
    }

    fn spawn_error(cause: serde_json::Value) -> VcsError {
        VcsError::Spawn(VcsProcessSpawnError::VcsProcessSpawnError {
            operation: "GitHubCredentials.get".into(),
            command: "gh".into(),
            cwd: ".".into(),
            argument_count: None,
            cause,
        })
    }

    #[tokio::test]
    async fn asks_gh_for_the_active_login_when_settings_pin_nothing() {
        let h = harness(json!({}));
        let credential = h.credentials.get("GitHub.com").await.unwrap();
        assert_eq!(credential.token.value(), "active-token");
        assert_eq!(credential.source, GitHubCredentialSource::Gh);
        assert_eq!(
            *h.calls.lock(),
            vec![args(&["auth", "token", "--hostname", "github.com"])]
        );
    }

    #[tokio::test]
    async fn passes_user_for_an_account_pinned_in_settings() {
        let h = harness(json!({ "github.com": { "account": "work" } }));
        let credential = h.credentials.get("github.com").await.unwrap();
        assert_eq!(credential.token.value(), "token-for-work");
        assert_eq!(
            *h.calls.lock(),
            vec![args(&[
                "auth",
                "token",
                "--hostname",
                "github.com",
                "--user",
                "work"
            ])]
        );
    }

    #[tokio::test]
    async fn fails_a_host_turned_off_in_settings_without_asking_gh() {
        let h = harness(json!({ "github.com": { "enabled": false } }));
        let error = h.credentials.get("github.com").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::HostDisabled(_)
        ));
        assert_eq!(
            error.to_string(),
            "GitHub host github.com is turned off in Settings → Source Control."
        );
        assert!(h.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn lets_an_environment_token_win_over_a_pinned_account_as_gh_does() {
        let h = harness_with(
            json!({ "github.com": { "account": "work" } }),
            &[],
            json!({}),
            &[("GH_TOKEN", "from-env")],
            None,
        );
        let credential = h.credentials.get("github.com").await.unwrap();
        assert_eq!(credential.token.value(), "from-env");
        assert_eq!(credential.source, GitHubCredentialSource::Env);
        assert!(h.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn picks_up_a_changed_account_on_the_next_request_without_a_restart() {
        let h = harness(json!({}));
        let token = |h: &Harness| {
            let credentials = h.credentials.clone();
            async move { credentials.get("github.com").await }
        };
        assert_eq!(token(&h).await.unwrap().token.value(), "active-token");

        h.host.set_github_settings(json!({
            "hosts": { "github.com": { "account": "work", "enabled": true } }
        }));
        assert_eq!(token(&h).await.unwrap().token.value(), "token-for-work");

        h.host.set_github_settings(json!({
            "hosts": { "github.com": { "enabled": false } }
        }));
        assert!(matches!(
            token(&h).await.unwrap_err(),
            GitHubCredentialUnavailableError::HostDisabled(_)
        ));

        h.host.set_github_settings(json!({ "hosts": {} }));
        assert_eq!(token(&h).await.unwrap().token.value(), "active-token");
        // The unpinned token stayed cached; only the newly pinned account cost a gh call.
        assert_eq!(h.calls.lock().len(), 2);
    }

    #[tokio::test]
    async fn falls_back_to_the_active_login_when_the_pinned_one_is_no_longer_signed_in() {
        let h = harness_with(
            json!({ "github.com": { "account": "gone" } }),
            &["gone"],
            json!({}),
            &[],
            None,
        );
        assert_eq!(
            h.credentials.get("github.com").await.unwrap().token.value(),
            "active-token"
        );
        assert_eq!(
            *h.calls.lock(),
            vec![
                args(&[
                    "auth",
                    "token",
                    "--hostname",
                    "github.com",
                    "--user",
                    "gone"
                ]),
                args(&["auth", "token", "--hostname", "github.com"]),
            ]
        );
    }

    #[tokio::test]
    async fn uses_a_token_saved_in_settings_before_gh_token_and_gh() {
        let h = harness_with(
            json!({}),
            &[],
            json!({ "github.com": "saved-token" }),
            &[("GH_TOKEN", "env-token")],
            None,
        );
        let credential = h.credentials.get("github.com").await.unwrap();
        assert_eq!(credential.token.value(), "saved-token");
        assert_eq!(credential.source, GitHubCredentialSource::Settings);
        assert!(h.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn keeps_a_host_turned_off_even_with_a_token_saved_in_settings() {
        let h = harness_with(
            json!({ "github.com": { "enabled": false } }),
            &[],
            json!({ "github.com": "saved-token" }),
            &[],
            None,
        );
        assert!(matches!(
            h.credentials.get("github.com").await.unwrap_err(),
            GitHubCredentialUnavailableError::HostDisabled(_)
        ));
    }

    #[tokio::test]
    async fn fingerprint_is_host_and_sha256_of_the_token() {
        let h = harness(json!({}));
        let credential = h.credentials.get("github.com").await.unwrap();
        // sha256("active-token")
        assert_eq!(
            credential.fingerprint,
            format!("github.com:{:x}", Sha256::digest(b"active-token"))
        );
        assert_eq!(credential.fingerprint.len(), "github.com:".len() + 64);
    }

    #[tokio::test]
    async fn caches_a_token_for_five_minutes_then_asks_gh_again() {
        let h = harness(json!({}));
        h.credentials.get("github.com").await.unwrap();
        h.clock.advance(5 * 60 * 1000 - 1);
        h.credentials.get("github.com").await.unwrap();
        assert_eq!(h.calls.lock().len(), 1);
        h.clock.advance(1);
        h.credentials.get("github.com").await.unwrap();
        assert_eq!(h.calls.lock().len(), 2);
    }

    #[tokio::test]
    async fn invalidate_drops_the_held_token() {
        let h = harness(json!({}));
        h.credentials.get("github.com").await.unwrap();
        h.credentials.invalidate("GitHub.com").await;
        h.credentials.get("github.com").await.unwrap();
        assert_eq!(h.calls.lock().len(), 2);
    }

    #[tokio::test]
    async fn caches_a_missing_login_for_ten_seconds() {
        let h = harness_with(json!({}), &[], json!({}), &[], Some(exit_error));
        let error = h.credentials.get("github.com").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::NotSignedIn(_)
        ));
        assert_eq!(
            error.to_string(),
            "No GitHub credential for github.com: run `gh auth login --hostname github.com`."
        );
        h.clock.advance(9_999);
        h.credentials.get("github.com").await.unwrap_err();
        assert_eq!(h.calls.lock().len(), 1);
        h.clock.advance(1);
        h.credentials.get("github.com").await.unwrap_err();
        assert_eq!(h.calls.lock().len(), 2);
    }

    #[tokio::test]
    async fn an_empty_gh_answer_is_not_signed_in_and_names_the_account() {
        let h = harness_with(
            json!({ "github.com": { "account": "gone" } }),
            &["gone"],
            json!({}),
            &[],
            None,
        );
        // The active login is also empty: make the unpinned call empty too.
        let mut host = TestHost::new();
        host.process = Some(Box::new(|_| Ok(process_output("  \n"))));
        host.set_github_settings(
            json!({ "hosts": { "github.com": { "account": "gone", "enabled": true } } }),
        );
        let credentials = GitHubCredentialsImpl::new(
            Arc::new(host),
            MapEnvironment::new(Vec::<(String, String)>::new()),
            h.clock.clone(),
        );
        let error = credentials.get("github.com").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "No GitHub credential for github.com: run `gh auth login --hostname github.com`."
        );
    }

    #[tokio::test]
    async fn a_missing_gh_executable_is_cli_missing() {
        let h = harness_with(
            json!({}),
            &[],
            json!({}),
            &[],
            Some(|| spawn_error(json!({ "reason": { "_tag": "NotFound" } }))),
        );
        let error = h.credentials.get("github.com").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::CliMissing(_)
        ));
        assert_eq!(
            error.to_string(),
            "No GitHub credential for github.com: set GH_TOKEN, or install the GitHub CLI and run `gh auth login`."
        );
    }

    #[tokio::test]
    async fn a_transient_gh_failure_is_asked_again_on_the_next_read() {
        let h = harness_with(
            json!({}),
            &[],
            json!({}),
            &[],
            Some(|| spawn_error(json!("boom"))),
        );
        let error = h.credentials.get("github.com").await.unwrap_err();
        assert!(matches!(
            error,
            GitHubCredentialUnavailableError::CliFailed(_)
        ));
        h.credentials.get("github.com").await.unwrap_err();
        assert_eq!(h.calls.lock().len(), 2);
    }

    #[tokio::test]
    async fn concurrent_reads_share_one_gh_call() {
        let h = harness(json!({}));
        let (a, b) = futures::join!(
            h.credentials.get("github.com"),
            h.credentials.get("github.com")
        );
        assert_eq!(a.unwrap().token.value(), "active-token");
        assert_eq!(b.unwrap().token.value(), "active-token");
        assert_eq!(h.calls.lock().len(), 1);
    }

    #[test]
    fn environment_token_follows_gh_precedence() {
        let env = |pairs: &[(&str, &str)]| MapEnvironment::new(pairs.iter().copied());
        assert_eq!(
            environment_token("github.com", env(&[("GITHUB_TOKEN", " b ")]).as_ref()),
            Some("b".into())
        );
        assert_eq!(
            environment_token(
                "github.com",
                env(&[("GH_TOKEN", "a"), ("GITHUB_TOKEN", "b")]).as_ref()
            ),
            Some("a".into())
        );
        assert_eq!(
            environment_token("acme.ghe.com", env(&[("GH_TOKEN", "a")]).as_ref()),
            Some("a".into())
        );
        // The enterprise token goes only to the host GH_HOST names.
        assert_eq!(
            environment_token(
                "ghe.example.com",
                env(&[("GH_ENTERPRISE_TOKEN", "e"), ("GH_TOKEN", "a")]).as_ref()
            ),
            None
        );
        assert_eq!(
            environment_token(
                "ghe.example.com",
                env(&[
                    ("GH_ENTERPRISE_TOKEN", "e"),
                    ("GH_HOST", " GHE.example.com ")
                ])
                .as_ref()
            ),
            Some("e".into())
        );
        assert_eq!(
            environment_token("github.com", env(&[("GH_TOKEN", "  ")]).as_ref()),
            None
        );
    }
}
