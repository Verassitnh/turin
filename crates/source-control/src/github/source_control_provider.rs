//! Port of upstream/packages/source-control-github/src/server/GitHubSourceControlProvider.ts
//!
//! GitHub repository operations (change request lookup, create, clone URLs, checkout, link
//! resolution) over the REST/GraphQL transport plus git, and the GitHub discovery probe over
//! `gh auth status --json hosts`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::base_schemas::EffectOption;
use turin_contracts::pull_request::{PullRequestListState, PullRequestState};
use turin_contracts::source_control::{
    ChangeRequest, ChangeRequestState, SourceControlProviderAccount, SourceControlProviderAuth,
    SourceControlProviderAuthStatus, SourceControlProviderDiscoveryItem,
    SourceControlProviderError, SourceControlProviderInfo, SourceControlProviderKind,
    SourceControlRepositoryCloneUrls, SourceControlRepositoryVisibility,
};

use super::api::{
    GitHubApi, GitHubApiError, GitHubGraphQlInput, GitHubRestInput, GitHubRestResponse, RestMethod,
};
use super::auth_status::{
    GitHubAuthStatusAccount, effective_github_account, find_authenticated_github_account,
    parse_github_auth_status,
};
use super::change_request_template;
use super::credentials::GitHubCredentialUnavailableError;
use super::definition::{GitHubSettings, KIND, read_settings};
use super::pull_requests::{NormalizedGitHubPullRequestRecord, decode_github_pull_request_entries};
use super::repository_resolution::{
    GitHubRepositoryLocator, PullRequestReference, ResolveGitHubRepositoryInput,
    github_api_host_for_remote, parse_fetch_remotes, parse_github_repository_selector,
    parse_pull_request_reference, resolve_github_repository,
};
use crate::shared::git::normalize_git_remote_url;
use crate::shared::host_process::Environment;
use crate::shared::js::{encode_uri_component, js_trim};
use crate::shared::source_control::is_ssh_remote_url;
use crate::source_control_core::Cause;
use crate::source_control_core::ambient;
use crate::source_control_core::discovery::{
    ManagedCliProbe, ProviderAuthInput, SourceControlAuthProbeInput, SourceControlCliDiscoverySpec,
    SourceControlProviderDiscoverySpec, combined_auth_output, first_safe_auth_line,
    probe_source_control_provider, provider_auth,
};
use crate::source_control_core::host::{
    SourceControlGitExecuteInput, SourceControlHost, SourceControlProcessInput,
};
use crate::source_control_core::source_control_provider::{
    CheckoutChangeRequestInput, CreateChangeRequestInput, CreateRepositoryInput,
    GetChangeRequestInput, GetDefaultBranchInput, GetRepositoryCloneUrlsInput, HeadBranchProbe,
    HeadProbeState, ListChangeRequestsInput, ResolveSourceControlLinkFuture,
    ResolveSourceControlLinkInput, SourceControlLinkSubject, SourceControlProvider,
    SourceControlProviderContext, transport_safe_source_control_error_value,
};

fn kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new(KIND).expect("github is a valid kind")
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn to_change_request(record: NormalizedGitHubPullRequestRecord) -> Option<ChangeRequest> {
    Some(ChangeRequest {
        provider: kind(),
        number: turin_contracts::PositiveInt::new(record.number)?,
        title: tne(&record.title)?,
        url: record.url,
        base_ref_name: tne(&record.base_ref_name)?,
        head_ref_name: tne(&record.head_ref_name)?,
        head_sha: record.head_sha.as_deref().and_then(tne),
        state: match record.state {
            PullRequestState::Open => ChangeRequestState::Open,
            PullRequestState::Closed => ChangeRequestState::Closed,
            PullRequestState::Merged => ChangeRequestState::Merged,
        },
        is_draft: record.is_draft,
        closed_at: Some(record.closed_at),
        merged_at: Some(record.merged_at),
        updated_at: match record.updated_at {
            Some(at) => EffectOption::some(at),
            None => EffectOption::None,
        },
        is_cross_repository: record.is_cross_repository,
        head_repository_name_with_owner: record
            .head_repository_name_with_owner
            .as_deref()
            .and_then(tne)
            .map(Some),
        head_repository_owner_login: record
            .head_repository_owner_login
            .as_deref()
            .and_then(tne)
            .map(Some),
    })
}

fn auth_accounts(accounts: &[GitHubAuthStatusAccount]) -> Vec<SourceControlProviderAccount> {
    accounts
        .iter()
        .filter_map(|entry| {
            Some(SourceControlProviderAccount {
                host: tne(&entry.host)?,
                account: tne(&entry.account)?,
                active: entry.active,
                authenticated: entry.authenticated,
                error: entry.error.as_deref().and_then(tne),
                environment_variable: entry.environment_variable.as_deref().and_then(tne),
            })
        })
        .collect()
}

fn with_accounts(
    mut auth: SourceControlProviderAuth,
    accounts: Option<Vec<SourceControlProviderAccount>>,
) -> SourceControlProviderAuth {
    if accounts.is_some() {
        auth.accounts = accounts;
    }
    auth
}

/// Reads `gh auth status --json hosts`. The headline account is the one GitHub requests will
/// use: Settings can pin a login per host or turn a host off, and an environment token beats both.
pub fn parse_github_auth(
    input: &SourceControlAuthProbeInput,
    settings: &GitHubSettings,
) -> SourceControlProviderAuth {
    let output = combined_auth_output(input);
    let auth_status = parse_github_auth_status(&input.stdout);
    let mut hosts: Vec<&str> = Vec::new();
    for entry in &auth_status.accounts {
        if !hosts.contains(&entry.host.as_str()) {
            hosts.push(&entry.host);
        }
    }
    let fallback = find_authenticated_github_account(&auth_status.accounts);
    // Lead with the host gh would pick, unless Settings turned it off.
    let ordered_hosts: Vec<&str> = match fallback {
        Some(fallback) => std::iter::once(fallback.host.as_str())
            .chain(hosts.iter().copied().filter(|h| *h != fallback.host))
            .collect(),
        None => hosts.clone(),
    };
    let chosen = ordered_hosts
        .iter()
        .map(|host| effective_github_account(host, &auth_status.accounts, settings))
        .find(Option::is_some)
        .flatten();
    let accounts = auth_status
        .parsed
        .then(|| auth_accounts(&auth_status.accounts));

    if let Some(chosen) = chosen {
        let detail = chosen.environment_variable.as_ref().map(|variable| {
            format!(
                "Using {variable} from the server environment; it overrides the account chosen in Settings."
            )
        });
        return with_accounts(
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Authenticated,
                account: Some(&chosen.account),
                host: Some(&chosen.host),
                detail: detail.as_deref(),
            }),
            accounts,
        );
    }

    if let Some(fallback) = fallback {
        return with_accounts(
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Unauthenticated,
                account: None,
                host: Some(&fallback.host),
                detail: Some(
                    "Every GitHub host gh is signed in to is turned off in Settings → Source Control.",
                ),
            }),
            accounts,
        );
    }

    let failed_account = auth_status
        .accounts
        .iter()
        .find(|entry| entry.active)
        .or_else(|| auth_status.accounts.first());
    if auth_status.parsed {
        return with_accounts(
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Unauthenticated,
                account: None,
                host: failed_account.map(|a| a.host.as_str()),
                detail: Some(failed_account.and_then(|a| a.error.as_deref()).unwrap_or(
                    "Run `gh auth login` to authenticate GitHub CLI with an active account.",
                )),
            }),
            accounts,
        );
    }

    // gh gained `auth status --json` in 2.81.0. Older versions reject the flag and exit
    // non-zero, which reads exactly like a signed-out CLI. Name the real problem instead.
    if input.exit_code != 0 && output.contains("unknown flag: --json") {
        return provider_auth(ProviderAuthInput {
            status: SourceControlProviderAuthStatus::Unknown,
            account: None,
            host: None,
            detail: Some(
                "GitHub CLI is too old to report sign-in status. Update `gh` to 2.81.0 or newer (for example `brew upgrade gh`) and rescan.",
            ),
        });
    }

    if input.exit_code != 0 {
        let line = first_safe_auth_line(&output);
        return provider_auth(ProviderAuthInput {
            status: SourceControlProviderAuthStatus::Unauthenticated,
            account: None,
            host: None,
            detail: Some(
                line.as_deref()
                    .unwrap_or("Run `gh auth login` to authenticate GitHub CLI."),
            ),
        });
    }

    let line = first_safe_auth_line(&output);
    provider_auth(ProviderAuthInput {
        status: SourceControlProviderAuthStatus::Unknown,
        account: None,
        host: None,
        detail: Some(
            line.as_deref()
                .unwrap_or("GitHub CLI auth status could not be parsed."),
        ),
    })
}

pub const DISCOVERY_LABEL: &str = "GitHub";
pub const DISCOVERY_INSTALL_HINT: &str = "Install the GitHub command-line tool (`gh`) via https://cli.github.com/ or your package manager (for example `brew install gh`).";

/// The `gh` CLI probe spec (`discovery`), with auth parsed against `settings`.
pub fn cli_discovery_spec(settings: GitHubSettings) -> SourceControlCliDiscoverySpec {
    SourceControlCliDiscoverySpec {
        kind: kind(),
        label: DISCOVERY_LABEL.into(),
        install_hint: DISCOVERY_INSTALL_HINT.into(),
        executable: "gh".into(),
        version_args: vec!["--version".into()],
        auth_args: vec![
            "auth".into(),
            "status".into(),
            "--json".into(),
            "hosts".into(),
        ],
        remote_refinement_args: None,
        probe_timeout_ms: None,
        parse_auth: Arc::new(move |input| parse_github_auth(input, &settings)),
        refine_unknown_remote: None,
    }
}

#[derive(Deserialize)]
struct ViewerBody {
    login: String,
}

/// The environment variable gh would take a github.com token from, if one is set.
fn environment_token_variable(environment: &dyn Environment) -> Option<&'static str> {
    ["GH_TOKEN", "GITHUB_TOKEN"].into_iter().find(|name| {
        environment
            .get(name)
            .is_some_and(|v| !js_trim(&v).is_empty())
    })
}

async fn read_host_settings(host: &dyn SourceControlHost) -> GitHubSettings {
    match host.settings_get().await {
        Ok(settings) => {
            let saved = settings.source_control_hosts.get(KIND).map(|map| {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            });
            read_settings(saved.as_ref())
        }
        Err(_) => read_settings(None),
    }
}

/// GitHub is usable with a token saved in Settings, one from the environment, or `gh` to hand one
/// over. Reads the GitHub settings on every probe, so a saved account choice shows on rescan. An
/// environment token is checked against the API, since `gh auth status` may not know it.
pub struct GitHubDiscoveryProbe {
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
}

/// `makeDiscovery`.
pub fn make_discovery(
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
) -> SourceControlProviderDiscoverySpec {
    SourceControlProviderDiscoverySpec::ManagedCli(
        crate::source_control_core::discovery::SourceControlManagedCliDiscoverySpec {
            kind: kind(),
            label: DISCOVERY_LABEL.into(),
            install_hint: DISCOVERY_INSTALL_HINT.into(),
            probe: Arc::new(GitHubDiscoveryProbe {
                api,
                host,
                environment,
            }),
        },
    )
}

#[async_trait]
impl ManagedCliProbe for GitHubDiscoveryProbe {
    async fn probe(&self, cwd: &str) -> SourceControlProviderDiscoveryItem {
        let settings = read_host_settings(self.host.as_ref()).await;
        let spec = SourceControlProviderDiscoverySpec::Cli(cli_discovery_spec(settings.clone()));
        let mut cli = probe_source_control_provider(&spec, self.host.as_ref(), cwd).await;
        // A token saved in Settings wins over the environment, which wins over gh.
        let saved_token = settings.token("github.com").is_some();
        let variable = environment_token_variable(self.environment.as_ref());
        let token_source: Option<String> = if saved_token {
            Some("the token saved in Settings".to_owned())
        } else {
            variable.map(str::to_owned)
        };
        // A host turned off in Settings stays off even with a token.
        let Some(token_source) = token_source else {
            return cli;
        };
        if settings
            .host_choice("github.com")
            .is_some_and(|c| !c.enabled)
        {
            return cli;
        }
        let viewer = self
            .api
            .rest(GitHubRestInput {
                host: "github.com".into(),
                operation: "discovery".into(),
                path: "user".into(),
                ..Default::default()
            })
            .await;
        let login = viewer
            .as_ref()
            .ok()
            .and_then(|response| serde_json::from_str::<ViewerBody>(&response.body).ok())
            .map(|b| b.login);
        // The per-host logins stay, so the account picker still lists every host gh knows.
        let accounts = cli.auth.accounts.clone();
        let auth = if let Some(login) = login {
            let detail = if saved_token {
                "Using the token saved in Settings; it overrides GH_TOKEN and the gh login."
                    .to_owned()
            } else {
                format!(
                    "Using {} from the server environment; it overrides the account chosen in Settings.",
                    variable.unwrap_or_default()
                )
            };
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Authenticated,
                account: Some(&login),
                host: Some("github.com"),
                detail: Some(&detail),
            })
        } else if let Err(failure) = &viewer
            && !matches!(failure, GitHubApiError::Authentication(_))
        {
            // Only a refusal says the token is bad; a network error or a pause says nothing.
            let subject = if saved_token {
                token_source.clone()
            } else {
                format!("the token in {token_source}")
            };
            let detail = format!("Could not check {subject}: {failure}");
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Unknown,
                account: None,
                host: Some("github.com"),
                detail: Some(&detail),
            })
        } else {
            let detail = if saved_token {
                "GitHub refused the token saved in Settings. Replace or remove it in Settings → Source Control.".to_owned()
            } else {
                format!(
                    "GitHub refused the token in {token_source}. Replace it, or unset it to use `gh auth login`."
                )
            };
            provider_auth(ProviderAuthInput {
                status: SourceControlProviderAuthStatus::Unauthenticated,
                account: None,
                host: Some("github.com"),
                detail: Some(&detail),
            })
        };
        cli.status = turin_contracts::source_control::SourceControlDiscoveryStatus::Available;
        cli.auth = with_accounts(auth, accounts);
        cli
    }

    /// Claims a custom host GitHub holds a credential for, from whichever source supplies it
    /// (Settings, the environment or gh). A host turned off in Settings is claimed too, so its
    /// error says so instead of "unsupported host".
    async fn refine_unknown_remote(
        &self,
        _cwd: &str,
        context: &SourceControlProviderContext,
    ) -> Option<SourceControlProviderInfo> {
        // Identity resolution also probes providers before a web base URL is known.
        let url = reqwest::Url::parse(&context.provider.base_url).ok()?;
        let host = url_host(&url)?;
        let known = match self.api.credential(&host).await {
            Ok(_) => true,
            Err(GitHubApiError::CredentialUnavailable(
                GitHubCredentialUnavailableError::HostDisabled(_),
            )) => true,
            Err(_) => false,
        };
        known.then(|| SourceControlProviderInfo {
            kind: kind(),
            name: tne("GitHub Self-Hosted").expect("non-empty"),
            base_url: context.provider.base_url.clone(),
        })
    }
}

/// `new URL(..).host`: hostname plus a non-default port.
fn url_host(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// Why a GitHub read or write failed, in words a user can act on. The provider methods attach the
/// operation and request context; `cause` keeps the raw failure for logs, never for transport.
#[derive(Debug, Clone)]
pub struct GitHubFailure {
    pub detail: String,
    pub cause: Option<Cause>,
}

impl std::fmt::Display for GitHubFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

fn failure(detail: impl Into<String>, cause: Option<Cause>) -> GitHubFailure {
    GitHubFailure {
        detail: detail.into(),
        cause,
    }
}

fn cause_of(error: impl std::error::Error + Send + Sync + 'static) -> Option<Cause> {
    Some(Arc::new(error))
}

const PULL_REQUEST_NOT_FOUND: &str =
    "Pull request not found. Check the PR number or URL and try again.";
const REPOSITORY_NOT_FOUND: &str = "Repository not found. Check the owner and name and try again.";

/// `not_found` names what was missing, which only the operation knows.
fn from_github_api_error(error: GitHubApiError, not_found: &str) -> GitHubFailure {
    let cause = cause_of(error.clone());
    match &error {
        GitHubApiError::CredentialUnavailable(unavailable) => match unavailable {
            GitHubCredentialUnavailableError::CliMissing(_) => failure(
                "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI and run `gh auth login`.",
                cause,
            ),
            // A missing or turned-off credential already says what to do about it.
            GitHubCredentialUnavailableError::NotSignedIn(_)
            | GitHubCredentialUnavailableError::HostDisabled(_) => {
                failure(error.to_string(), cause)
            }
            GitHubCredentialUnavailableError::CliFailed(_) => failure(non_empty(&error), cause),
        },
        GitHubApiError::Authentication(_) => failure(
            "GitHub is not authenticated. Run `gh auth login` (or set GH_TOKEN) and retry.",
            cause,
        ),
        GitHubApiError::RateLimit(_) | GitHubApiError::Paused(_) => failure(
            "GitHub API rate limit exceeded. Requests resume when the limit resets.",
            cause,
        ),
        GitHubApiError::NotFound(_) => failure(not_found, cause),
        // GitHub's own reason ("A pull request already exists…") when it gave one.
        GitHubApiError::Response(_) | GitHubApiError::Request(_) => {
            failure(non_empty(&error), cause)
        }
    }
}

fn non_empty(error: &GitHubApiError) -> String {
    let message = error.to_string();
    let trimmed = js_trim(&message);
    if trimmed.is_empty() {
        "GitHub request failed.".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn not_found(cause: &str) -> GitHubFailure {
    failure(
        PULL_REQUEST_NOT_FOUND,
        Some(crate::source_control_core::cause_from_message(cause)),
    )
}

#[derive(Deserialize)]
struct RawRepository {
    full_name: TrimmedNonEmptyString,
    html_url: TrimmedNonEmptyString,
    ssh_url: TrimmedNonEmptyString,
    #[serde(default)]
    default_branch: Option<String>,
}

fn repository_clone_urls(raw: &RawRepository) -> SourceControlRepositoryCloneUrls {
    SourceControlRepositoryCloneUrls {
        name_with_owner: raw.full_name.clone(),
        url: raw.html_url.clone(),
        ssh_url: raw.ssh_url.clone(),
    }
}

#[derive(Deserialize)]
struct ViewerLogin {
    login: TrimmedNonEmptyString,
}

/// The pull request fields every read selects, in GraphQL.
const PULL_REQUEST_NODE_SELECTION: &str = "number title url baseRefName headRefName headRefOid state isDraft mergedAt closedAt updatedAt isCrossRepository headRepository { name nameWithOwner } headRepositoryOwner { login }";

fn graphql_states(state: PullRequestListState) -> &'static [&'static str] {
    match state {
        PullRequestListState::Open => &["OPEN"],
        PullRequestListState::Closed => &["CLOSED"],
        PullRequestListState::Merged => &["MERGED"],
        PullRequestListState::All => &["OPEN", "CLOSED", "MERGED"],
    }
}

/// Head lookups per GraphQL document. A document of a hundred costs one point, the same as one
/// single lookup, but half that keeps each answer near half a second.
const HEAD_LOOKUPS_PER_DOCUMENT: usize = 50;
/// How long a head lookup waits for company. Branch discovery reaches GitHub only after each
/// branch's own git reads, so lookups started together arrive tens of milliseconds apart.
/// A background sweep's lookups spread over up to ~300ms, and every document costs a point no
/// matter how few heads it holds, so reads without reserve wait longer.
const HEAD_LOOKUP_BATCH_WINDOW: Duration = Duration::from_millis(50);
const BACKGROUND_HEAD_LOOKUP_BATCH_WINDOW: Duration = Duration::from_millis(500);
/// Background documents fill up under the longer window, and a failed document fails every head
/// in it. Fifty `main`-like heads of a hundred pull requests each took up to ~10s, GitHub's own
/// processing limit; twenty-five took ~7s.
const BACKGROUND_HEAD_LOOKUPS_PER_DOCUMENT: usize = 25;
/// A full document is 5,000 rows of well under 2 KB each.
const HEAD_LOOKUP_MAX_RESPONSE_BYTES: usize = 16_000_000;
/// Rows read for a fork's `owner:branch` head, which GitHub cannot filter by owner. A branch
/// named like a busy default (`main`) is the case this bounds; the owner's own row is near the
/// top because the newest come first.
const OWNER_HEAD_SCAN_LIMIT: u32 = 100;

#[derive(Debug, Clone)]
struct PullRequestsByHeadRead {
    host: String,
    owner: String,
    name: String,
    head_ref_name: String,
    state: PullRequestListState,
    limit: u32,
    allow_reserve: bool,
}

/// One aliased `pullRequests` connection per lookup, each head and state passed as a variable.
fn build_pull_requests_by_head_query(
    lookups: &[PullRequestsByHeadRead],
) -> (String, Map<String, Value>) {
    let mut variables = Map::new();
    let mut declarations: Vec<String> = vec!["$owner: String!".into(), "$name: String!".into()];
    let mut selections: Vec<String> = Vec::new();
    for (index, lookup) in lookups.iter().enumerate() {
        variables.insert(
            format!("h{index}"),
            Value::String(lookup.head_ref_name.clone()),
        );
        variables.insert(format!("s{index}"), json!(graphql_states(lookup.state)));
        declarations.push(format!("$h{index}: String!"));
        declarations.push(format!("$s{index}: [PullRequestState!]"));
        // `gh pr list` orders the same way, so a head with more matches than the limit keeps the
        // same rows.
        selections.push(format!(
            "    h{index}: pullRequests(headRefName: $h{index}, states: $s{index}, first: {}, orderBy: {{ field: CREATED_AT, direction: DESC }}) {{ nodes {{ {PULL_REQUEST_NODE_SELECTION} }} }}",
            lookup.limit
        ));
    }
    (
        format!(
            "query PullRequestsByHead({}) {{\n  repository(owner: $owner, name: $name) {{\n{}\n  }}\n}}",
            declarations.join(", "),
            selections.join("\n")
        ),
        variables,
    )
}

fn pull_request_by_number_query() -> String {
    format!(
        "query PullRequestByNumber($owner: String!, $name: String!, $number: Int!) {{\n  repository(owner: $owner, name: $name) {{\n    pullRequest(number: $number) {{ {PULL_REQUEST_NODE_SELECTION} }}\n  }}\n}}"
    )
}

#[derive(Deserialize)]
struct AliasNodes {
    nodes: Vec<Value>,
}

#[derive(Deserialize)]
struct HeadsData {
    repository: Option<HashMap<String, Option<AliasNodes>>>,
}

#[derive(Deserialize)]
struct HeadsResponse {
    data: HeadsData,
}

#[derive(Deserialize)]
struct ByNumberRepository {
    #[serde(rename = "pullRequest")]
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct ByNumberData {
    repository: Option<ByNumberRepository>,
}

#[derive(Deserialize)]
struct ByNumberResponse {
    data: ByNumberData,
}

/// The local branch a pull request checks out into, the way `gh pr checkout` names it.
pub fn pull_request_checkout_branch_name(
    head_ref_name: &str,
    head_owner: Option<&str>,
    is_cross_repository: bool,
    default_branch: Option<&str>,
) -> String {
    // gh prefixes the owner only where the fork's branch would take over the default branch's
    // name, which is the one collision every fork pull request from `main` would hit.
    match (head_owner, default_branch) {
        (Some(owner), Some(default)) if is_cross_repository && head_ref_name == default => {
            format!("{owner}/{head_ref_name}")
        }
        _ => head_ref_name.to_owned(),
    }
}

/// The checkout's GitHub API host, from the remote the caller resolved the provider from.
fn context_host(context: Option<&SourceControlProviderContext>) -> Option<String> {
    let url = reqwest::Url::parse(&context?.provider.base_url).ok()?;
    url_host(&url)
}

type HeadResult = Result<Vec<NormalizedGitHubPullRequestRecord>, GitHubFailure>;

struct Entry {
    request: PullRequestsByHeadRead,
    tx: oneshot::Sender<HeadResult>,
}

/// Coalesces head lookups on one repository into shared GraphQL documents (upstream
/// `RequestResolver.makeGrouped(...).pipe(batchN, setDelay)`).
///
/// The first lookup of a group is its leader: it waits out the window, takes everyone who joined
/// meanwhile, runs the documents (`batchN` chunks concurrently) and completes each entry. If a
/// leader is dropped mid-wait its followers see a closed channel and elect a new leader, so no
/// lookup is ever lost to a cancelled caller — and nothing is spawned detached.
struct HeadBatcher {
    api: Arc<dyn GitHubApi>,
    groups: Mutex<HashMap<String, Vec<Entry>>>,
}

impl HeadBatcher {
    fn new(api: Arc<dyn GitHubApi>) -> Self {
        Self {
            api,
            groups: Mutex::new(HashMap::new()),
        }
    }

    fn key(request: &PullRequestsByHeadRead) -> String {
        [
            request.host.as_str(),
            request.owner.as_str(),
            request.name.as_str(),
            &request.allow_reserve.to_string(),
            ambient::pinned_credential()
                .map(|c| c.credential_fingerprint)
                .unwrap_or_default()
                .as_str(),
            ambient::credential_scope().as_str(),
        ]
        .join("\0")
    }

    async fn lookup(&self, request: PullRequestsByHeadRead) -> HeadResult {
        let key = Self::key(&request);
        loop {
            let (tx, rx) = oneshot::channel();
            let leader = {
                let mut groups = self.groups.lock();
                let group = groups.entry(key.clone()).or_default();
                group.push(Entry {
                    request: request.clone(),
                    tx,
                });
                group.len() == 1
            };
            if leader {
                let (window, batch_size) = if request.allow_reserve {
                    (HEAD_LOOKUP_BATCH_WINDOW, HEAD_LOOKUPS_PER_DOCUMENT)
                } else {
                    (
                        BACKGROUND_HEAD_LOOKUP_BATCH_WINDOW,
                        BACKGROUND_HEAD_LOOKUPS_PER_DOCUMENT,
                    )
                };
                tokio::time::sleep(window).await;
                let entries = self.groups.lock().remove(&key).unwrap_or_default();
                let mut chunks: Vec<Vec<Entry>> = Vec::new();
                let mut iter = entries.into_iter().peekable();
                while iter.peek().is_some() {
                    chunks.push(iter.by_ref().take(batch_size).collect());
                }
                futures::future::join_all(chunks.into_iter().map(|chunk| self.run_chunk(chunk)))
                    .await;
            }
            match rx.await {
                Ok(result) => return result,
                // The leader went away before answering: join (or lead) a new group.
                Err(_) => continue,
            }
        }
    }

    async fn run_chunk(&self, entries: Vec<Entry>) {
        let Some(first) = entries.first() else { return };
        let (host, owner, name, allow_reserve) = (
            first.request.host.clone(),
            first.request.owner.clone(),
            first.request.name.clone(),
            first.request.allow_reserve,
        );
        let requests: Vec<PullRequestsByHeadRead> =
            entries.iter().map(|e| e.request.clone()).collect();
        let (document, extra) = build_pull_requests_by_head_query(&requests);
        let mut variables = Map::new();
        variables.insert("owner".into(), Value::String(owner));
        variables.insert("name".into(), Value::String(name));
        variables.extend(extra);
        let decoded = graphql_json::<HeadsResponse>(
            self.api.as_ref(),
            GitHubGraphQlInput {
                host,
                operation: "listPullRequestsByHead".into(),
                query: document,
                variables: Some(variables),
                allow_reserve: Some(allow_reserve),
                // Up to 50 heads of 100 rows each. A default branch such as `main` can match a
                // hundred fork pull requests, so the usual cap would cut the answer short.
                max_response_bytes: Some(HEAD_LOOKUP_MAX_RESPONSE_BYTES),
            },
            "GitHub returned an invalid change request list.",
        )
        .await;
        let aliases = match decoded {
            Ok(decoded) => match decoded.data.repository {
                Some(aliases) => Ok(aliases),
                None => Err(not_found("The repository could not be read.")),
            },
            Err(error) => Err(error),
        };
        match aliases {
            Ok(mut aliases) => {
                for (index, entry) in entries.into_iter().enumerate() {
                    let rows = match aliases.remove(&format!("h{index}")).flatten() {
                        Some(alias) => decode_github_pull_request_entries(&alias.nodes),
                        None => Vec::new(),
                    };
                    let _ = entry.tx.send(Ok(rows));
                }
            }
            Err(error) => {
                for entry in entries {
                    let _ = entry.tx.send(Err(error.clone()));
                }
            }
        }
    }
}

async fn graphql_json<A: serde::de::DeserializeOwned>(
    api: &dyn GitHubApi,
    input: GitHubGraphQlInput,
    decode_detail: &str,
) -> Result<A, GitHubFailure> {
    let raw = api
        .graphql(input)
        .await
        .map_err(|error| from_github_api_error(error, PULL_REQUEST_NOT_FOUND))?;
    serde_json::from_str::<A>(&raw).map_err(|error| failure(decode_detail, cause_of(error)))
}

static LINK_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/([\w.-]+)/([\w.-]+)/(?:pull|issues)/([1-9]\d*)(?:/.*)?$").unwrap()
});
static OWNER_SELECTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^:/\s]+):(.+)$").unwrap());
static BARE_REPOSITORY_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^/\s]+$").unwrap());
static DOT_GIT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.git$").unwrap());

#[derive(Deserialize)]
struct LinkSubjectBody {
    title: String,
    #[serde(default)]
    body: Option<String>,
}

pub struct GitHubSourceControlProvider {
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    heads: HeadBatcher,
}

/// `GitHubSourceControlProvider.make`.
pub fn make(
    api: Arc<dyn GitHubApi>,
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
) -> GitHubSourceControlProvider {
    GitHubSourceControlProvider {
        heads: HeadBatcher::new(api.clone()),
        api,
        host,
        environment,
    }
}

struct ListByHead<'a> {
    cwd: &'a str,
    head_selector: &'a str,
    state: PullRequestListState,
    limit: u32,
    context: Option<&'a SourceControlProviderContext>,
    host: Option<String>,
    allow_reserve: bool,
}

struct ReadPullRequest<'a> {
    cwd: &'a str,
    reference: &'a str,
    context: Option<&'a SourceControlProviderContext>,
    host: Option<String>,
}

impl GitHubSourceControlProvider {
    async fn git_read(
        &self,
        cwd: &str,
        args: &[&str],
    ) -> Result<
        crate::source_control_core::host::SourceControlProcessOutput,
        turin_contracts::vcs::VcsError,
    > {
        self.host
            .process_run(SourceControlProcessInput {
                operation: "GitHubSourceControlProvider.resolveRepository".into(),
                command: "git".into(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                cwd: cwd.to_owned(),
                allow_non_zero_exit: true,
                timeout_ms: Some(5_000),
                ..Default::default()
            })
            .await
    }

    /// Uses GH_REPO, then gh's remote selection on the detected host. The provider context is
    /// a fallback when the checkout's remotes cannot resolve that repository.
    async fn resolve_repository(
        &self,
        cwd: &str,
        host: Option<&str>,
        context: Option<&SourceControlProviderContext>,
    ) -> Result<GitHubRepositoryLocator, GitHubFailure> {
        let env_repository = self
            .environment
            .get("GH_REPO")
            .map(|v| js_trim(&v).to_owned());
        let host_hint: Option<String> = host.map(str::to_owned).or_else(|| context_host(context));
        let default_host = host_hint
            .clone()
            .or_else(|| self.environment.get("GH_HOST"))
            .unwrap_or_else(|| "github.com".to_owned())
            .to_lowercase();
        if let Some(env_repository) = env_repository.filter(|r| !r.is_empty())
            && let Some(locator) = parse_github_repository_selector(&env_repository, &default_host)
        {
            return Ok(locator);
        }
        let (remotes, resolved) = tokio::join!(
            self.git_read(cwd, &["remote", "-v"]),
            self.git_read(
                cwd,
                &["config", "--get-regexp", "^remote\\..*\\.gh-resolved$"]
            ),
        );
        let (remotes, resolved) = match (remotes, resolved) {
            (Ok(a), Ok(b)) => (Some(a), Some(b)),
            _ => (None, None),
        };
        let resolution = resolve_github_repository(&ResolveGitHubRepositoryInput {
            remotes: remotes
                .as_ref()
                .filter(|r| r.exit_code == 0)
                .map_or("", |r| r.stdout.as_str()),
            resolved: resolved
                .as_ref()
                .filter(|r| r.exit_code == 0)
                .map_or("", |r| r.stdout.as_str()),
            host_hint: host_hint.as_deref(),
            default_host: &default_host,
        });
        if let Some(locator) = resolution.locator {
            return Ok(locator);
        }
        if let Some(context) = context {
            let remote = normalize_git_remote_url(&context.remote_url);
            let after_host = remote
                .find('/')
                .map_or(remote.as_str(), |i| &remote[i + 1..]);
            // `remote.slice(remote.indexOf("/") + 1)`; indexOf -1 slices from 0.
            if let Some(fallback) = parse_github_repository_selector(
                &format!("{}/{}", resolution.host, after_host),
                &resolution.host,
            ) {
                return Ok(fallback);
            }
        }
        Err(failure(
            format!(
                "No GitHub repository on {} was found among this checkout's git remotes.",
                resolution.host
            ),
            None,
        ))
    }

    async fn rest(
        &self,
        input: GitHubRestInput,
        not_found_detail: &str,
    ) -> Result<GitHubRestResponse, GitHubFailure> {
        self.api
            .rest(input)
            .await
            .map_err(|error| from_github_api_error(error, not_found_detail))
    }

    /// Pull requests whose head is `head_selector` (a branch, or `owner:branch` for a fork), in the
    /// repository `gh pr list` would read in `cwd`. Lookups on one repository that arrive together
    /// share one GraphQL document.
    async fn list_by_head(
        &self,
        input: ListByHead<'_>,
    ) -> Result<Vec<NormalizedGitHubPullRequestRecord>, GitHubFailure> {
        let locator = self
            .resolve_repository(input.cwd, input.host.as_deref(), input.context)
            .await?;
        let limit = input.limit.clamp(1, 100);
        // `owner:branch` names a fork's branch. GitHub filters on the branch name only, so the
        // owner is matched on the rows it returns.
        let selector = js_trim(input.head_selector);
        let owner_match = OWNER_SELECTOR.captures(selector);
        let head_ref_name = owner_match
            .as_ref()
            .map_or(selector, |m| m.get(2).map_or("", |g| g.as_str()))
            .to_owned();
        let rows = self
            .heads
            .lookup(PullRequestsByHeadRead {
                host: locator.host,
                owner: locator.owner,
                name: locator.name,
                head_ref_name,
                state: input.state,
                limit: if owner_match.is_some() {
                    OWNER_HEAD_SCAN_LIMIT
                } else {
                    limit
                },
                allow_reserve: input.allow_reserve,
            })
            .await?;
        let Some(owner_match) = owner_match else {
            return Ok(rows);
        };
        let head_owner = owner_match[1].to_lowercase();
        Ok(rows
            .into_iter()
            .filter(|row| {
                row.head_repository_owner_login
                    .as_deref()
                    .map(str::to_lowercase)
                    .as_deref()
                    == Some(head_owner.as_str())
            })
            .take(limit as usize)
            .collect())
    }

    async fn read_pull_request(
        &self,
        input: ReadPullRequest<'_>,
    ) -> Result<NormalizedGitHubPullRequestRecord, GitHubFailure> {
        let parsed = parse_pull_request_reference(input.reference);
        if let PullRequestReference::Branch { head_selector } = &parsed {
            // `gh pr view <branch>` prefers an open pull request, then the newest of any state.
            let lookup = |state: PullRequestListState| {
                self.list_by_head(ListByHead {
                    cwd: input.cwd,
                    head_selector,
                    state,
                    limit: 1,
                    host: input.host.clone(),
                    context: input.context,
                    allow_reserve: true,
                })
            };
            let open = lookup(PullRequestListState::Open).await?.into_iter().next();
            let found = match open {
                Some(open) => Some(open),
                None => lookup(PullRequestListState::All).await?.into_iter().next(),
            };
            return found.ok_or_else(|| not_found("No pull request has this head branch."));
        }
        let (locator, number) = match parsed {
            PullRequestReference::Url { locator, number } => (locator, number),
            PullRequestReference::Number { number } => (
                self.resolve_repository(input.cwd, input.host.as_deref(), input.context)
                    .await?,
                number,
            ),
            PullRequestReference::Branch { .. } => unreachable!("handled above"),
        };
        let decode_detail = "GitHub returned an invalid pull request.";
        let decoded = graphql_json::<ByNumberResponse>(
            self.api.as_ref(),
            GitHubGraphQlInput {
                host: locator.host.clone(),
                operation: "getPullRequest".into(),
                query: pull_request_by_number_query(),
                variables: Some(
                    json!({ "owner": locator.owner, "name": locator.name, "number": number })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                ),
                allow_reserve: Some(true),
                max_response_bytes: None,
            },
            decode_detail,
        )
        .await?;
        let node = decoded
            .data
            .repository
            .and_then(|r| r.pull_request)
            .filter(|n| !n.is_null());
        let Some(node) = node else {
            return Err(not_found("The pull request does not exist."));
        };
        decode_github_pull_request_entries(&[node])
            .into_iter()
            .next()
            .ok_or_else(|| {
                failure(
                    decode_detail,
                    Some(crate::source_control_core::cause_from_message(
                        "Malformed pull request.",
                    )),
                )
            })
    }

    async fn read_repository(
        &self,
        locator: &GitHubRepositoryLocator,
    ) -> Result<RawRepository, GitHubFailure> {
        let response = self
            .rest(
                GitHubRestInput {
                    host: locator.host.clone(),
                    operation: "getRepository".into(),
                    path: format!(
                        "repos/{}/{}",
                        encode_uri_component(&locator.owner),
                        encode_uri_component(&locator.name)
                    ),
                    allow_reserve: Some(true),
                    ..Default::default()
                },
                REPOSITORY_NOT_FOUND,
            )
            .await?;
        serde_json::from_str::<RawRepository>(&response.body)
            .map_err(|error| failure("GitHub returned an invalid repository.", cause_of(error)))
    }

    async fn read_viewer_login(&self, host: &str) -> Result<String, GitHubFailure> {
        let response = self
            .rest(
                GitHubRestInput {
                    host: host.to_owned(),
                    operation: "getViewer".into(),
                    path: "user".into(),
                    allow_reserve: Some(true),
                    ..Default::default()
                },
                PULL_REQUEST_NOT_FOUND,
            )
            .await?;
        serde_json::from_str::<ViewerLogin>(&response.body)
            .map(|v| v.login.as_str().to_owned())
            .map_err(|error| failure("GitHub request failed.", cause_of(error)))
    }

    // Git and filesystem errors can carry arguments, paths and stderr, so the detail a client
    // sees is fixed and the raw error stays in `cause`.
    fn git_failure(cause: impl std::error::Error + Send + Sync + 'static) -> GitHubFailure {
        failure(
            "The pull request could not be checked out with git.",
            cause_of(cause),
        )
    }

    async fn run_git(
        &self,
        cwd: &str,
        operation: &str,
        args: &[&str],
    ) -> Result<(), turin_contracts::git::GitCommandError> {
        self.host
            .git_execute(SourceControlGitExecuteInput {
                operation: format!("GitHubSourceControlProvider.checkoutPullRequest.{operation}"),
                cwd: cwd.to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                max_output_bytes: None,
                append_truncation_marker: false,
            })
            .await
            .map(|_| ())
    }

    /// `gh pr checkout` in plain git: the head branch is fetched from the remote that holds it (a
    /// fork gets a remote of its own), checked out under the name gh would give it, and set to
    /// track the head. A head branch that is gone is read from the base's `refs/pull/<n>/head`.
    /// An existing branch fast-forwards, or with `force` is reset to the pull request.
    async fn checkout_pull_request(
        &self,
        input: &CheckoutChangeRequestInput,
    ) -> Result<(), GitHubFailure> {
        use crate::source_control_core::host::SourceControlGitRemoteBranch;
        let reference = parse_pull_request_reference(&input.reference);
        let pull_request = self
            .read_pull_request(ReadPullRequest {
                cwd: &input.cwd,
                reference: &input.reference,
                context: input.context.as_ref(),
                host: context_host(input.context.as_ref()),
            })
            .await?;
        let base = match &reference {
            PullRequestReference::Url { locator, .. } => locator.clone(),
            _ => {
                self.resolve_repository(&input.cwd, None, input.context.as_ref())
                    .await?
            }
        };
        let base_name_with_owner = format!("{}/{}", base.owner, base.name).to_lowercase();
        let head_name_with_owner = pull_request.head_repository_name_with_owner.clone();
        let is_cross_repository = pull_request.is_cross_repository.unwrap_or_else(|| {
            head_name_with_owner
                .as_ref()
                .is_some_and(|h| h.to_lowercase() != base_name_with_owner)
        });
        let head_owner = pull_request
            .head_repository_owner_login
            .clone()
            .or_else(|| {
                head_name_with_owner
                    .as_ref()
                    .and_then(|h| h.split('/').next().map(str::to_owned))
            });

        let remotes = match self.git_read(&input.cwd, &["remote", "-v"]).await {
            Ok(result) => {
                if result.exit_code == 0 {
                    result.stdout
                } else {
                    String::new()
                }
            }
            Err(error) => return Err(Self::git_failure(error)),
        };
        let fetch_remotes = parse_fetch_remotes(&remotes);
        let remote_for = |name_with_owner: &str| -> Option<String> {
            fetch_remotes
                .iter()
                .find(|remote| {
                    let normalized = normalize_git_remote_url(&remote.url);
                    let remote_host = github_api_host_for_remote(&remote.url)
                        .unwrap_or_else(|| normalized.split('/').next().unwrap_or("").to_owned());
                    remote_host == base.host
                        && normalized.split('/').skip(1).collect::<Vec<_>>().join("/")
                            == name_with_owner.to_lowercase()
                })
                .map(|remote| remote.name.clone())
        };
        let base_remote = async {
            match remote_for(&base_name_with_owner) {
                Some(known) => Ok(known),
                None => self.host.git_resolve_primary_remote_name(&input.cwd).await,
            }
        };
        // A fork's branch named like the base's default branch is checked out under the owner's
        // prefix. Without the default branch that collision cannot be ruled out, and the checkout
        // would reset the local default branch to the fork's commit, so it fails instead.
        let default_branch = if is_cross_repository {
            self.read_repository(&base).await?.default_branch
        } else {
            None
        };
        let local_branch = pull_request_checkout_branch_name(
            &pull_request.head_ref_name,
            head_owner.as_deref(),
            is_cross_repository,
            default_branch.as_deref(),
        );

        // The remote the head branch lives on; a fork the checkout does not know yet is added.
        let head_remote = async {
            if !is_cross_repository {
                return base_remote.await.map_err(Self::git_failure);
            }
            let Some(head_name_with_owner) = &head_name_with_owner else {
                return Err(failure("The fork is gone.", None));
            };
            if let Some(known) = remote_for(head_name_with_owner) {
                return Ok(known);
            }
            let (owner, name) = head_name_with_owner
                .split_once('/')
                .unwrap_or((head_name_with_owner, ""));
            let fork = self
                .read_repository(&GitHubRepositoryLocator {
                    host: base.host.clone(),
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
                .await?;
            let origin_url = self
                .host
                .git_read_config_value(&input.cwd, "remote.origin.url")
                .await
                .map_err(Self::git_failure)?;
            let url = if origin_url.as_deref().is_some_and(is_ssh_remote_url) {
                fork.ssh_url.as_str().to_owned()
            } else {
                fork.html_url.as_str().to_owned()
            };
            self.host
                .git_ensure_remote(&input.cwd, head_owner.as_deref().unwrap_or("fork"), &url)
                .await
                .map_err(Self::git_failure)
        };

        let exists = self
            .host
            .git_list_local_branch_names(&input.cwd)
            .await
            .map_err(Self::git_failure)?
            .contains(&local_branch);

        // The commit to check out, fetched from the head branch where it still exists.
        struct Target {
            reference: String,
            upstream: Option<(String, String)>,
        }
        let primary = async {
            let remote_name = head_remote.await?;
            self.host
                .git_fetch_remote_tracking_branch(&SourceControlGitRemoteBranch {
                    cwd: input.cwd.clone(),
                    remote_name: remote_name.clone(),
                    remote_branch: pull_request.head_ref_name.clone(),
                })
                .await
                .map_err(Self::git_failure)?;
            Ok::<_, GitHubFailure>(Target {
                reference: format!("refs/remotes/{remote_name}/{}", pull_request.head_ref_name),
                upstream: Some((remote_name, pull_request.head_ref_name.clone())),
            })
        }
        .await;
        let target = match primary {
            Ok(target) => target,
            Err(_) => {
                let fallback = async {
                    let remote =
                        base_remote2(&self.host, &input.cwd, &remote_for, &base_name_with_owner)
                            .await?;
                    self.run_git(
                        &input.cwd,
                        "fetchPullRef",
                        &[
                            "fetch",
                            "--quiet",
                            "--no-tags",
                            &remote,
                            &format!("refs/pull/{}/head", pull_request.number),
                        ],
                    )
                    .await?;
                    let commit_sha = self
                        .host
                        .git_resolve_commit(&input.cwd, "FETCH_HEAD")
                        .await?;
                    Ok::<_, turin_contracts::git::GitCommandError>(Target {
                        reference: commit_sha,
                        upstream: None,
                    })
                }
                .await;
                fallback.map_err(Self::git_failure)?
            }
        };

        let switched = async {
            if !exists {
                self.run_git(
                    &input.cwd,
                    "branch",
                    &["branch", &local_branch, &target.reference],
                )
                .await?;
            }
            self.host
                .git_switch_ref(&turin_contracts::git::VcsSwitchRefInput {
                    cwd: TrimmedNonEmptyString::new(&input.cwd)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new(".").expect("non-empty")),
                    ref_name: TrimmedNonEmptyString::new(&local_branch)
                        .unwrap_or_else(|_| TrimmedNonEmptyString::new("HEAD").expect("non-empty")),
                })
                .await?;
            if exists {
                let args: &[&str] = if input.force {
                    &["reset", "--hard", "--quiet", &target.reference]
                } else {
                    &["merge", "--ff-only", "--quiet", &target.reference]
                };
                // The slice borrows `target`; build owned args for the call.
                let owned: Vec<&str> = args.to_vec();
                self.run_git(&input.cwd, "sync", &owned).await?;
            }
            // Tracking is set once the branch is the pull request's, so a sync that fails leaves
            // an existing branch's upstream as it was.
            if let Some((remote_name, remote_branch)) = &target.upstream {
                self.host
                    .git_set_branch_upstream(
                        &SourceControlGitRemoteBranch {
                            cwd: input.cwd.clone(),
                            remote_name: remote_name.clone(),
                            remote_branch: remote_branch.clone(),
                        },
                        &local_branch,
                    )
                    .await?;
            }
            Ok::<_, turin_contracts::git::GitCommandError>(())
        }
        .await;
        switched.map_err(Self::git_failure)
    }
}

/// `baseRemote` outside the primary path: the base's own remote, else the primary one.
async fn base_remote2(
    host: &Arc<dyn SourceControlHost>,
    cwd: &str,
    remote_for: &(dyn Fn(&str) -> Option<String> + Sync),
    base_name_with_owner: &str,
) -> Result<String, turin_contracts::git::GitCommandError> {
    match remote_for(base_name_with_owner) {
        Some(known) => Ok(known),
        None => host.git_resolve_primary_remote_name(cwd).await,
    }
}

/// Reports a failure against the operation and request that made it, with transport-safe context.
fn provider_error(
    operation: &str,
    cwd: &str,
    reference: Option<&str>,
    repository: Option<&str>,
    error: GitHubFailure,
) -> SourceControlProviderError {
    SourceControlProviderError::SourceControlProviderError {
        provider: kind(),
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        command: None,
        repository: repository.map(transport_safe_source_control_error_value),
        reference: reference.map(transport_safe_source_control_error_value),
        detail: error.detail,
        cause: error.cause.as_ref().map(cause_value),
    }
}

/// The raw cause for logs: GitHub API failures keep their tag and the fields callers act on
/// (`retryAt`), everything else its message. Never put it on a transport.
fn cause_value(cause: &Cause) -> Value {
    match cause.downcast_ref::<GitHubApiError>() {
        Some(GitHubApiError::RateLimit(error)) => json!({
            "_tag": "GitHubApiRateLimitError",
            "host": error.host,
            "operation": error.operation,
            "retryAt": error.retry_at,
        }),
        Some(GitHubApiError::Response(error)) => json!({
            "_tag": "GitHubApiResponseError",
            "host": error.host,
            "operation": error.operation,
            "status": error.status,
        }),
        Some(other) => json!({ "message": other.to_string() }),
        None => Value::String(cause.to_string()),
    }
}

fn link_error(cwd: &str, operation: &str, cause: Value) -> SourceControlProviderError {
    SourceControlProviderError::SourceControlProviderError {
        provider: kind(),
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        command: None,
        repository: None,
        reference: None,
        detail: "The linked subject could not be read.".into(),
        cause: Some(cause),
    }
}

impl GitHubSourceControlProvider {
    async fn read_link_subject(
        &self,
        input: ResolveSourceControlLinkInput,
        endpoint: String,
    ) -> Result<SourceControlLinkSubject, SourceControlProviderError> {
        let host = url_host(&input.url).unwrap_or_default();
        let request = self.api.rest(GitHubRestInput {
            host,
            operation: "resolveLink".into(),
            path: endpoint,
            max_response_bytes: Some(1_000_000),
            ..Default::default()
        });
        let result = match tokio::time::timeout(Duration::from_secs(3), request).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                let cause: Cause = Arc::new(error);
                return Err(link_error(&input.cwd, "resolveLink", cause_value(&cause)));
            }
            Err(elapsed) => {
                return Err(link_error(
                    &input.cwd,
                    "resolveLink",
                    json!({ "_tag": "TimeoutException", "message": elapsed.to_string() }),
                ));
            }
        };
        // serde's messages can quote the offending value, so only the failure kind is kept.
        let subject = serde_json::from_str::<LinkSubjectBody>(&result.body).map_err(|_| {
            link_error(
                &input.cwd,
                "resolveLink.decode",
                json!({ "_tag": "SchemaError", "message": "The linked subject response did not decode." }),
            )
        })?;
        Ok(SourceControlLinkSubject {
            title: subject.title,
            body: subject.body,
        })
    }
}

#[async_trait]
impl SourceControlProvider for GitHubSourceControlProvider {
    fn kind(&self) -> SourceControlProviderKind {
        kind()
    }

    // `gh pr list --head` filters on the head ref name alone and accepts anything, so an
    // `owner:branch` or `remote:branch` selector silently lists zero pull requests while
    // spending a GraphQL call; the bare branch is always among the selectors. Without the owner,
    // a bare branch also lists same-named branches on other forks (`main`, `patch-1`), so read a
    // full page and let the owner check pick the right head. gh fetches up to 100 in one
    // request, and GitHub prices a first:100 connection like first:1.
    fn head_branch_probe(
        &self,
        head_selectors: &[String],
        _state: HeadProbeState,
    ) -> Option<HeadBranchProbe> {
        Some(HeadBranchProbe {
            head_selectors: head_selectors
                .iter()
                .filter(|s| !s.contains(':'))
                .cloned()
                .collect(),
            limit: 100,
        })
    }

    async fn read_change_request_template(&self, cwd: &str, treeish: &str) -> Option<String> {
        change_request_template::detect(cwd, treeish, self.host.as_ref()).await
    }

    fn resolve_link(
        &self,
        input: ResolveSourceControlLinkInput,
    ) -> ResolveSourceControlLinkFuture<'_> {
        // Automatic enrichment must not send ambient CLI credentials to a host from message text.
        if url_host(&input.url).as_deref() != Some("github.com") {
            return None;
        }
        let path = input.url.path().to_owned();
        let matched = LINK_PATH.captures(&path)?;
        let endpoint = format!(
            "repos/{}/{}/issues/{}",
            &matched[1], &matched[2], &matched[3]
        );
        Some(Box::pin(self.read_link_subject(input, endpoint)))
    }

    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> Result<Vec<ChangeRequest>, SourceControlProviderError> {
        // An open lookup is a user waiting on a status; the rest may be a background sweep.
        let allow_reserve =
            matches!(input.state, Some(ChangeRequestState::Open)) || ambient::interactive();
        let state = match input.state {
            None => PullRequestListState::All,
            Some(ChangeRequestState::Open) => PullRequestListState::Open,
            Some(ChangeRequestState::Closed) => PullRequestListState::Closed,
            Some(ChangeRequestState::Merged) => PullRequestListState::Merged,
        };
        let default_limit = if matches!(input.state, Some(ChangeRequestState::Open)) {
            1
        } else {
            20
        };
        let records = self
            .list_by_head(ListByHead {
                cwd: &input.cwd,
                head_selector: &input.head_selector,
                state,
                limit: input.limit.unwrap_or(default_limit),
                host: context_host(input.context.as_ref()),
                context: input.context.as_ref(),
                allow_reserve,
            })
            .await
            .map_err(|e| {
                provider_error(
                    "listChangeRequests",
                    &input.cwd,
                    Some(&input.head_selector),
                    None,
                    e,
                )
            })?;
        Ok(records.into_iter().filter_map(to_change_request).collect())
    }

    async fn get_change_request(
        &self,
        input: GetChangeRequestInput,
    ) -> Result<ChangeRequest, SourceControlProviderError> {
        let to_error = |e| {
            provider_error(
                "getChangeRequest",
                &input.cwd,
                Some(&input.reference),
                None,
                e,
            )
        };
        let record = self
            .read_pull_request(ReadPullRequest {
                cwd: &input.cwd,
                reference: &input.reference,
                context: input.context.as_ref(),
                host: context_host(input.context.as_ref()),
            })
            .await
            .map_err(to_error)?;
        to_change_request(record).ok_or_else(|| {
            provider_error(
                "getChangeRequest",
                &input.cwd,
                Some(&input.reference),
                None,
                failure("GitHub returned an invalid pull request.", None),
            )
        })
    }

    async fn create_change_request(
        &self,
        input: CreateChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        let run = async {
            let locator = self
                .resolve_repository(&input.cwd, None, input.context.as_ref())
                .await?;
            let body = tokio::fs::read_to_string(&input.body_file)
                .await
                .map_err(|error| {
                    failure(
                        "The pull request description could not be read.",
                        cause_of(error),
                    )
                })?;
            self.rest(
                GitHubRestInput {
                    host: locator.host.clone(),
                    operation: "createPullRequest".into(),
                    method: RestMethod::Post,
                    // A user's own write may spend the reserve the background leaves for it.
                    allow_reserve: Some(true),
                    path: format!(
                        "repos/{}/{}/pulls",
                        encode_uri_component(&locator.owner),
                        encode_uri_component(&locator.name)
                    ),
                    // `owner:branch` is how the REST API takes a fork's head, the same as
                    // `gh --head`. gh allows maintainer edits unless told otherwise; the API's
                    // default is not documented.
                    body: Some(json!({
                        "base": input.base_ref_name,
                        "head": input.head_selector,
                        "title": input.title,
                        "body": body,
                        "maintainer_can_modify": true,
                    })),
                    ..Default::default()
                },
                PULL_REQUEST_NOT_FOUND,
            )
            .await?;
            Ok::<_, GitHubFailure>(())
        };
        run.await.map_err(|e| {
            provider_error(
                "createChangeRequest",
                &input.cwd,
                Some(&input.head_selector),
                None,
                e,
            )
        })
    }

    async fn get_repository_clone_urls(
        &self,
        input: GetRepositoryCloneUrlsInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        let run = async {
            let fallback_host = match self
                .resolve_repository(&input.cwd, None, input.context.as_ref())
                .await
            {
                Ok(locator) => locator.host,
                Err(_) => self
                    .environment
                    .get("GH_HOST")
                    .unwrap_or_else(|| "github.com".to_owned()),
            }
            .to_lowercase();
            // A bare name is the signed-in account's repository, the way `gh repo view` reads it.
            let bare_name = DOT_GIT_SUFFIX
                .replace(js_trim(&input.repository), "")
                .into_owned();
            let locator = match parse_github_repository_selector(&input.repository, &fallback_host)
            {
                Some(locator) => Some(locator),
                None if BARE_REPOSITORY_NAME.is_match(&bare_name) => {
                    Some(GitHubRepositoryLocator {
                        host: fallback_host.clone(),
                        owner: self.read_viewer_login(&fallback_host).await?,
                        name: bare_name,
                    })
                }
                None => None,
            };
            let Some(locator) = locator else {
                return Err(failure("Repositories are named owner/name.", None));
            };
            Ok(repository_clone_urls(
                &self.read_repository(&locator).await?,
            ))
        };
        run.await.map_err(|e| {
            provider_error(
                "getRepositoryCloneUrls",
                &input.cwd,
                None,
                Some(&input.repository),
                e,
            )
        })
    }

    async fn create_repository(
        &self,
        input: CreateRepositoryInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        let run = async {
            let default_host = self
                .environment
                .get("GH_HOST")
                .unwrap_or_else(|| "github.com".to_owned())
                .to_lowercase();
            let locator = parse_github_repository_selector(&input.repository, &default_host);
            let viewer = match &locator {
                Some(locator) => Some(self.read_viewer_login(&locator.host).await?),
                None => None,
            };
            let owner: Option<String> = locator
                .as_ref()
                .map(|l| l.owner.clone())
                .or_else(|| viewer.clone());
            let name = locator
                .as_ref()
                .map_or_else(|| js_trim(&input.repository).to_owned(), |l| l.name.clone());
            let host = locator.as_ref().map_or_else(
                || {
                    let gh_host = self
                        .environment
                        .get("GH_HOST")
                        .map(|h| js_trim(&h).to_lowercase());
                    gh_host
                        .filter(|h| !h.is_empty())
                        .unwrap_or_else(|| "github.com".to_owned())
                },
                |l| l.host.clone(),
            );
            let is_viewer = match (&viewer, &owner) {
                (Some(viewer), Some(owner)) => owner.to_lowercase() == viewer.to_lowercase(),
                _ => false,
            };
            let path = if is_viewer || owner.is_none() {
                "user/repos".to_owned()
            } else {
                format!(
                    "orgs/{}/repos",
                    encode_uri_component(owner.as_deref().unwrap_or(""))
                )
            };
            let response = self
                .rest(
                    GitHubRestInput {
                        host,
                        operation: "createRepository".into(),
                        method: RestMethod::Post,
                        allow_reserve: Some(true),
                        path,
                        body: Some(json!({
                            "name": name,
                            "private": input.visibility == SourceControlRepositoryVisibility::Private,
                        })),
                        ..Default::default()
                    },
                    // GitHub answers 404 for an organization the account cannot create repositories in.
                    &format!(
                        "No organization named {} that this account can create repositories in.",
                        owner.as_deref().unwrap_or("that")
                    ),
                )
                .await?;
            let decoded =
                serde_json::from_str::<RawRepository>(&response.body).map_err(|error| {
                    failure("GitHub returned an invalid repository.", cause_of(error))
                })?;
            Ok(repository_clone_urls(&decoded))
        };
        run.await.map_err(|e| {
            provider_error(
                "createRepository",
                &input.cwd,
                None,
                Some(&input.repository),
                e,
            )
        })
    }

    async fn get_default_branch(
        &self,
        input: GetDefaultBranchInput,
    ) -> Result<Option<String>, SourceControlProviderError> {
        let run = async {
            let locator = self
                .resolve_repository(
                    &input.cwd,
                    context_host(input.context.as_ref()).as_deref(),
                    input.context.as_ref(),
                )
                .await?;
            let repository = self.read_repository(&locator).await?;
            let branch = repository
                .default_branch
                .as_deref()
                .map_or("", js_trim)
                .to_owned();
            Ok::<_, GitHubFailure>((!branch.is_empty()).then_some(branch))
        };
        run.await
            .map_err(|e| provider_error("getDefaultBranch", &input.cwd, None, None, e))
    }

    async fn checkout_change_request(
        &self,
        input: CheckoutChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        self.checkout_pull_request(&input).await.map_err(|e| {
            provider_error(
                "checkoutChangeRequest",
                &input.cwd,
                Some(&input.reference),
                None,
                e,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::api::GitHubApiRateLimitError;
    use crate::github::credentials::GitHubCliMissingError;
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::host::{
        SourceControlGitOutput, SourceControlGitRemoteBranch, SourceControlProcessOutput,
    };
    use crate::source_control_core::test_host::ScriptedHost;
    use parking_lot::Mutex as PlMutex;
    use turin_contracts::git::{GitCommandError, VcsSwitchRefResult};

    type GraphqlFn =
        Box<dyn Fn(&GitHubGraphQlInput) -> Result<String, GitHubApiError> + Send + Sync>;
    type RestFn =
        Box<dyn Fn(&GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> + Send + Sync>;

    #[derive(Default)]
    struct FakeApi {
        graphql: Option<GraphqlFn>,
        rest: Option<RestFn>,
    }

    #[async_trait]
    impl GitHubApi for FakeApi {
        async fn graphql(&self, input: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
            (self.graphql.as_ref().expect("unexpected graphql call"))(&input)
        }
        async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
            (self.rest.as_ref().expect("unexpected rest call"))(&input)
        }
        async fn credential(
            &self,
            _: &str,
        ) -> Result<crate::github::api::GitHubRequestCredential, GitHubApiError> {
            panic!("unexpected credential call")
        }
    }

    fn rest_response(body: Value) -> GitHubRestResponse {
        GitHubRestResponse {
            status: 200,
            headers: HashMap::new(),
            body: body.to_string(),
            truncated: false,
            invalid_utf8: false,
        }
    }

    fn remotes_output(entries: &[(&str, &str)]) -> String {
        entries
            .iter()
            .flat_map(|(name, url)| {
                [
                    format!("{name}\t{url} (fetch)"),
                    format!("{name}\t{url} (push)"),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn node(number: u64, head_ref_name: &str, owner: &str) -> Value {
        json!({
            "number": number,
            "title": format!("PR {number}"),
            "url": format!("https://github.com/acme/web/pull/{number}"),
            "baseRefName": "main",
            "headRefName": head_ref_name,
            "state": "OPEN",
            "isCrossRepository": owner != "acme",
            "updatedAt": "2026-01-02T00:00:00Z",
            "headRepository": { "name": "web", "nameWithOwner": format!("{owner}/web") },
            "headRepositoryOwner": { "login": owner },
        })
    }

    fn repository_body(full_name: &str, default_branch: &str) -> Value {
        json!({
            "full_name": full_name,
            "html_url": format!("https://github.com/{full_name}"),
            "ssh_url": format!("git@github.com:{full_name}.git"),
            "default_branch": default_branch,
        })
    }

    struct Options {
        remotes: String,
        resolved: String,
        local_branches: Vec<String>,
        git_failure: bool,
        env: Vec<(&'static str, &'static str)>,
    }

    impl Options {
        fn new(remotes: String) -> Self {
            Self {
                remotes,
                resolved: String::new(),
                local_branches: vec![],
                git_failure: false,
                env: vec![],
            }
        }
    }

    struct Harness {
        provider: GitHubSourceControlProvider,
        /// Every recorded git driver call, in order.
        log: Arc<PlMutex<Vec<(String, Value)>>>,
    }

    fn git_error() -> GitCommandError {
        GitCommandError::GitCommandError {
            operation: "test".into(),
            command: "git".into(),
            cwd: "/repo".into(),
            argument_count: None,
            exit_code: None,
            stdout_length: None,
            stderr_length: None,
            output_length: None,
            reason: None,
            detail: "fatal: /home/me/secret-path: permission denied".into(),
            cause: None,
        }
    }

    fn process_output(stdout: &str, exit_code: i32) -> SourceControlProcessOutput {
        SourceControlProcessOutput {
            exit_code,
            stdout: stdout.to_owned(),
            ..Default::default()
        }
    }

    /// The provider over a faked GitHubApi, git driver and process. `remotes` is what
    /// `git remote -v` prints; `log` records every driver call.
    #[allow(clippy::field_reassign_with_default)]
    fn harness(options: Options, api: FakeApi) -> Harness {
        let log: Arc<PlMutex<Vec<(String, Value)>>> = Arc::new(PlMutex::new(Vec::new()));
        let record = {
            let log = log.clone();
            move |name: &str, value: Value| log.lock().push((name.to_owned(), value))
        };
        let (remotes, resolved) = (options.remotes.clone(), options.resolved.clone());
        let (local_branches, git_failure) = (options.local_branches.clone(), options.git_failure);
        let mut host = ScriptedHost::default();
        host.process = Some(Box::new(move |input| {
            Ok(
                if input.args.first().map(String::as_str) == Some("remote") {
                    process_output(&remotes, 0)
                } else {
                    process_output(&resolved, if resolved.is_empty() { 1 } else { 0 })
                },
            )
        }));
        let r = record.clone();
        host.git_execute = Some(Box::new(move |input| {
            r("execute", json!(input.args));
            Ok(SourceControlGitOutput::default())
        }));
        host.resolve_primary_remote_name = Some(Box::new(|_| Ok("origin".into())));
        host.read_config_value = Some(Box::new(|_| Ok(Some("git@github.com:acme/web.git".into()))));
        let r = record.clone();
        host.ensure_remote = Some(Box::new(move |(cwd, preferred, url)| {
            r(
                "ensureRemote",
                json!({ "cwd": cwd, "preferredName": preferred, "url": url }),
            );
            Ok(preferred)
        }));
        let r = record.clone();
        host.fetch_remote_tracking_branch = Some(Box::new(
            move |input: SourceControlGitRemoteBranch| {
                r(
                    "fetchRemoteTrackingBranch",
                    json!({ "cwd": input.cwd, "remoteName": input.remote_name, "remoteBranch": input.remote_branch }),
                );
                Ok(())
            },
        ));
        let r = record.clone();
        host.set_branch_upstream = Some(Box::new(
            move |(input, branch): (SourceControlGitRemoteBranch, String)| {
                r(
                    "setBranchUpstream",
                    json!({ "cwd": input.cwd, "branch": branch, "remoteName": input.remote_name, "remoteBranch": input.remote_branch }),
                );
                Ok(())
            },
        ));
        let r = record;
        host.switch_ref = Some(Box::new(move |input| {
            r(
                "switchRef",
                json!({ "cwd": input.cwd.as_str(), "refName": input.ref_name.as_str() }),
            );
            Ok(VcsSwitchRefResult {
                ref_name: Some(input.ref_name),
            })
        }));
        host.list_local_branch_names = Some(Box::new(move |_| {
            if git_failure {
                Err(git_error())
            } else {
                Ok(local_branches.clone())
            }
        }));
        host.resolve_commit = Some(Box::new(|_| Ok("abc123".into())));
        let environment = MapEnvironment::new(options.env.iter().map(|(k, v)| (*k, *v)));
        Harness {
            provider: make(Arc::new(api), Arc::new(host), environment),
            log,
        }
    }

    fn github_context(host: &str) -> SourceControlProviderContext {
        SourceControlProviderContext {
            provider: SourceControlProviderInfo {
                kind: kind(),
                name: tne("GitHub").unwrap(),
                base_url: format!("https://{host}"),
            },
            remote_name: "origin".into(),
            remote_url: format!("git@{host}:acme/web.git"),
            requested_host: None,
        }
    }

    fn list_input(
        head_selector: &str,
        state: Option<ChangeRequestState>,
        limit: Option<u32>,
    ) -> ListChangeRequestsInput {
        ListChangeRequestsInput {
            cwd: "/repo".into(),
            context: None,
            source: None,
            head_selector: head_selector.into(),
            state,
            limit,
        }
    }

    fn detail_of(error: &SourceControlProviderError) -> &str {
        let SourceControlProviderError::SourceControlProviderError { detail, .. } = error;
        detail
    }

    fn default_branch_input(
        context: Option<SourceControlProviderContext>,
    ) -> GetDefaultBranchInput {
        GetDefaultBranchInput {
            cwd: "/repo".into(),
            context,
        }
    }

    // --- repository resolution -------------------------------------------------------------

    #[tokio::test]
    async fn reads_the_repository_gh_would_pick_from_the_remotes() {
        let paths = Arc::new(PlMutex::new(Vec::<String>::new()));
        let p = paths.clone();
        let h = harness(
            Options::new(remotes_output(&[
                ("origin", "git@github.com:me/web.git"),
                ("upstream", "https://github.com/acme/web.git"),
            ])),
            FakeApi {
                rest: Some(Box::new(move |input| {
                    p.lock().push(format!("{} {}", input.host, input.path));
                    Ok(rest_response(repository_body("acme/web", "trunk")))
                })),
                ..Default::default()
            },
        );
        assert_eq!(
            h.provider
                .get_default_branch(default_branch_input(None))
                .await
                .unwrap()
                .as_deref(),
            Some("trunk")
        );
        assert_eq!(*paths.lock(), vec!["github.com repos/acme/web"]);
    }

    #[tokio::test]
    async fn reads_an_ssh_alias_remote_through_github_com() {
        let hosts = Arc::new(PlMutex::new(Vec::<String>::new()));
        let p = hosts.clone();
        let h = harness(
            Options::new(remotes_output(&[("origin", "git@github:acme/web.git")])),
            FakeApi {
                rest: Some(Box::new(move |input| {
                    p.lock().push(format!("{} {}", input.host, input.path));
                    Ok(rest_response(repository_body("acme/web", "main")))
                })),
                ..Default::default()
            },
        );
        h.provider
            .get_default_branch(default_branch_input(None))
            .await
            .unwrap();
        // A provider's host hint for the same alias (`github` here) resolves the same way.
        h.provider
            .get_default_branch(default_branch_input(Some(github_context("github"))))
            .await
            .unwrap();
        assert_eq!(
            *hosts.lock(),
            vec!["github.com repos/acme/web", "github.com repos/acme/web"]
        );
    }

    #[tokio::test]
    async fn names_a_missing_repository_rather_than_a_pull_request() {
        let h = harness(
            Options::new(remotes_output(&[(
                "origin",
                "git@github.com:acme/gone.git",
            )])),
            FakeApi {
                rest: Some(Box::new(|input| {
                    Err(GitHubApiError::NotFound(
                        crate::github::api::GitHubApiNotFoundError {
                            host: input.host.clone(),
                            operation: input.operation.clone(),
                        },
                    ))
                })),
                ..Default::default()
            },
        );
        let error = h
            .provider
            .get_default_branch(default_branch_input(None))
            .await
            .unwrap_err();
        assert!(detail_of(&error).contains("Repository not found"));
    }

    #[tokio::test]
    async fn fails_clearly_when_no_remote_is_on_github() {
        let h = harness(
            Options::new(remotes_output(&[("origin", "git@gitlab.com:a/b.git")])),
            FakeApi::default(),
        );
        let error = h
            .provider
            .get_default_branch(default_branch_input(None))
            .await
            .unwrap_err();
        assert!(
            detail_of(&error).contains("No GitHub repository"),
            "{}",
            detail_of(&error)
        );
    }

    // --- change request lookups by head ----------------------------------------------------

    fn web_remotes() -> String {
        remotes_output(&[("origin", "git@github.com:acme/web.git")])
    }

    #[tokio::test(start_paused = true)]
    async fn reads_a_background_sweeps_staggered_heads_in_one_graphql_document() {
        let documents = Arc::new(PlMutex::new(Vec::<GitHubGraphQlInput>::new()));
        let d = documents.clone();
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(move |input| {
                    d.lock().push(input.clone());
                    let mut first = node(7, "feature/a", "acme");
                    first["headRefOid"] = json!("a".repeat(40));
                    Ok(json!({ "data": { "repository": { "h0": { "nodes": [first] }, "h1": { "nodes": [] } } } }).to_string())
                })),
                ..Default::default()
            },
        );
        let lookup = |selector: &'static str| {
            let provider = &h.provider;
            async move {
                let mut input = list_input(selector, None, Some(100));
                input.context = Some(github_context("github.com"));
                provider.list_change_requests(input).await
            }
        };
        // Each branch's own git reads come first, so a sweep's lookups arrive spread out.
        let first = lookup("feature/a");
        let second = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            lookup("feature/b").await
        };
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        assert_eq!(
            first.iter().map(|pr| pr.number.get()).collect::<Vec<_>>(),
            vec![7]
        );
        assert_eq!(
            first[0].head_sha.as_ref().map(|s| s.as_str()),
            Some("a".repeat(40).as_str())
        );
        assert!(second.unwrap().is_empty());
        let documents = documents.lock();
        assert_eq!(documents.len(), 1);
        assert!(documents[0].query.contains("headRefOid"));
        assert_eq!(
            Value::Object(documents[0].variables.clone().unwrap()),
            json!({
                "owner": "acme", "name": "web",
                "h0": "feature/a", "s0": ["OPEN", "CLOSED", "MERGED"],
                "h1": "feature/b", "s1": ["OPEN", "CLOSED", "MERGED"],
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn caps_a_background_document_at_twenty_five_heads() {
        let head_counts = Arc::new(PlMutex::new(Vec::<usize>::new()));
        let c = head_counts.clone();
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(move |input| {
                    let heads: Vec<&String> = input
                        .variables
                        .as_ref()
                        .unwrap()
                        .keys()
                        .filter(|k| {
                            k.starts_with('h') && k[1..].chars().all(|ch| ch.is_ascii_digit())
                        })
                        .collect();
                    c.lock().push(heads.len());
                    let repository: Map<String, Value> = heads
                        .into_iter()
                        .map(|k| (k.clone(), json!({ "nodes": [] })))
                        .collect();
                    Ok(json!({ "data": { "repository": repository } }).to_string())
                })),
                ..Default::default()
            },
        );
        let lookups = (0..26).map(|index| {
            let provider = &h.provider;
            async move {
                let mut input = list_input(&format!("feature/{index}"), None, Some(100));
                input.context = Some(github_context("github.com"));
                provider.list_change_requests(input).await
            }
        });
        for result in futures::future::join_all(lookups).await {
            result.unwrap();
        }
        let mut counts = head_counts.lock().clone();
        counts.sort();
        assert_eq!(counts, vec![1, 25]);
    }

    #[tokio::test(start_paused = true)]
    async fn matches_an_owner_branch_selector_on_the_head_owner() {
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(|input| {
                    let nodes =
                        if input.variables.as_ref().unwrap().get("h0") == Some(&json!("main")) {
                            vec![
                                node(9, "main", "someone"),
                                node(8, "main", "me"),
                                node(7, "main", "me"),
                            ]
                        } else {
                            vec![]
                        };
                    Ok(
                        json!({ "data": { "repository": { "h0": { "nodes": nodes } } } })
                            .to_string(),
                    )
                })),
                ..Default::default()
            },
        );
        let open = h
            .provider
            .list_change_requests(list_input(
                "me:main",
                Some(ChangeRequestState::Open),
                Some(1),
            ))
            .await
            .unwrap();
        assert_eq!(
            open.iter().map(|pr| pr.number.get()).collect::<Vec<_>>(),
            vec![8]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn maps_api_failures_onto_the_errors_callers_handle() {
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(|input| {
                    Err(
                        if input.variables.as_ref().unwrap().get("h0") == Some(&json!("missing")) {
                            GitHubApiError::CredentialUnavailable(
                                GitHubCredentialUnavailableError::CliMissing(
                                    GitHubCliMissingError {
                                        host: "github.com".into(),
                                    },
                                ),
                            )
                        } else {
                            GitHubApiError::RateLimit(GitHubApiRateLimitError {
                                host: "github.com".into(),
                                operation: "x".into(),
                                retry_at: Some(123),
                            })
                        },
                    )
                })),
                ..Default::default()
            },
        );
        let missing = h
            .provider
            .list_change_requests(list_input(
                "missing",
                Some(ChangeRequestState::Open),
                Some(1),
            ))
            .await
            .unwrap_err();
        assert!(detail_of(&missing).contains("No GitHub credential on the server"));
        let limited = h
            .provider
            .list_change_requests(list_input(
                "limited",
                Some(ChangeRequestState::Open),
                Some(1),
            ))
            .await
            .unwrap_err();
        assert!(detail_of(&limited).contains("rate limit exceeded"));
        let SourceControlProviderError::SourceControlProviderError { cause, .. } = &limited;
        assert_eq!(cause.as_ref().unwrap()["retryAt"], json!(123));
    }

    // --- getChangeRequest --------------------------------------------------------------------

    #[tokio::test]
    async fn reads_a_pull_request_by_number_and_by_url_on_its_own_repository() {
        let variables = Arc::new(PlMutex::new(Vec::<Value>::new()));
        let v = variables.clone();
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(move |input| {
                    v.lock()
                        .push(Value::Object(input.variables.clone().unwrap()));
                    Ok(json!({ "data": { "repository": { "pullRequest": node(42, "feature", "acme") } } }).to_string())
                })),
                ..Default::default()
            },
        );
        let get = |reference: &str| GetChangeRequestInput {
            cwd: "/repo".into(),
            context: None,
            reference: reference.into(),
        };
        assert_eq!(
            h.provider
                .get_change_request(get("#42"))
                .await
                .unwrap()
                .number
                .get(),
            42
        );
        h.provider
            .get_change_request(get("https://github.com/other/thing/pull/42"))
            .await
            .unwrap();
        assert_eq!(
            *variables.lock(),
            vec![
                json!({ "owner": "acme", "name": "web", "number": 42 }),
                json!({ "owner": "other", "name": "thing", "number": 42 }),
            ]
        );
    }

    #[tokio::test]
    async fn fails_a_missing_pull_request_as_not_found() {
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(Box::new(|_| {
                    Ok(json!({ "data": { "repository": { "pullRequest": null } } }).to_string())
                })),
                ..Default::default()
            },
        );
        let error = h
            .provider
            .get_change_request(GetChangeRequestInput {
                cwd: "/repo".into(),
                context: None,
                reference: "7".into(),
            })
            .await
            .unwrap_err();
        assert!(detail_of(&error).contains("Pull request not found"));
    }

    // --- writes -----------------------------------------------------------------------------

    #[tokio::test]
    async fn creates_a_cross_repository_pull_request_with_an_owner_branch_head() {
        let requests = Arc::new(PlMutex::new(Vec::<GitHubRestInput>::new()));
        let r = requests.clone();
        let h = harness(
            Options::new(remotes_output(&[
                ("origin", "git@github.com:me/web.git"),
                ("upstream", "git@github.com:acme/web.git"),
            ])),
            FakeApi {
                rest: Some(Box::new(move |input| {
                    r.lock().push(input.clone());
                    Ok(rest_response(json!({ "number": 1 })))
                })),
                ..Default::default()
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let body_file = dir.path().join("body.md");
        std::fs::write(&body_file, "Body").unwrap();
        h.provider
            .create_change_request(CreateChangeRequestInput {
                cwd: "/repo".into(),
                context: None,
                source: None,
                target: None,
                base_ref_name: "main".into(),
                head_selector: "me:feature".into(),
                title: "Title".into(),
                body_file: body_file.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let requests = requests.lock();
        assert_eq!(requests[0].method, RestMethod::Post);
        assert_eq!(requests[0].path, "repos/acme/web/pulls");
        // A user's own write, so the background's reserve is not held against it.
        assert_eq!(requests[0].allow_reserve, Some(true));
        assert_eq!(
            requests[0].body,
            Some(json!({
                "base": "main", "head": "me:feature", "title": "Title", "body": "Body",
                "maintainer_can_modify": true,
            }))
        );
    }

    #[tokio::test]
    async fn creates_a_repository_under_an_organization_the_viewer_is_not() {
        let requests = Arc::new(PlMutex::new(Vec::<String>::new()));
        let r = requests.clone();
        let h = harness(
            Options::new(String::new()),
            FakeApi {
                rest: Some(Box::new(move |input| {
                    let method = if input.method == RestMethod::Post {
                        "POST"
                    } else {
                        "GET"
                    };
                    r.lock().push(format!("{method} {}", input.path));
                    Ok(rest_response(if input.path == "user" {
                        json!({ "login": "me" })
                    } else {
                        json!({
                            "full_name": "acme/new",
                            "html_url": "https://github.com/acme/new",
                            "ssh_url": "git@github.com:acme/new.git",
                        })
                    }))
                })),
                ..Default::default()
            },
        );
        let urls = h
            .provider
            .create_repository(CreateRepositoryInput {
                cwd: "/repo".into(),
                repository: "acme/new".into(),
                visibility: SourceControlRepositoryVisibility::Private,
            })
            .await
            .unwrap();
        assert_eq!(urls.name_with_owner.as_str(), "acme/new");
        assert_eq!(urls.url.as_str(), "https://github.com/acme/new");
        assert_eq!(urls.ssh_url.as_str(), "git@github.com:acme/new.git");
        assert_eq!(*requests.lock(), vec!["GET user", "POST orgs/acme/repos"]);
    }

    #[tokio::test]
    async fn looks_up_a_bare_repository_name_under_the_signed_in_account_on_that_host() {
        let requests = Arc::new(PlMutex::new(Vec::<String>::new()));
        let r = requests.clone();
        let h = harness(
            Options::new(String::new()),
            FakeApi {
                rest: Some(Box::new(move |input| {
                    r.lock().push(format!("{} GET {}", input.host, input.path));
                    Ok(rest_response(if input.path == "user" {
                        json!({ "login": "me" })
                    } else {
                        json!({
                            "full_name": "me/notes",
                            "html_url": format!("https://{}/me/notes", input.host),
                            "ssh_url": format!("git@{}:me/notes.git", input.host),
                        })
                    }))
                })),
                ..Default::default()
            },
        );
        let lookup = |repository: &str| GetRepositoryCloneUrlsInput {
            cwd: "/repo".into(),
            context: Some(github_context("code.example.test")),
            repository: repository.into(),
        };
        let urls = h
            .provider
            .get_repository_clone_urls(lookup(" notes.git "))
            .await
            .unwrap();
        assert_eq!(urls.name_with_owner.as_str(), "me/notes");
        assert_eq!(urls.url.as_str(), "https://code.example.test/me/notes");
        assert_eq!(urls.ssh_url.as_str(), "git@code.example.test:me/notes.git");
        assert_eq!(
            *requests.lock(),
            vec![
                "code.example.test GET user",
                "code.example.test GET repos/me/notes"
            ]
        );
        // Malformed names are still refused before any request.
        for repository in ["me/", "/notes", "a/b/c/d", "", "two words"] {
            let error = h
                .provider
                .get_repository_clone_urls(lookup(repository))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Repositories are named owner/name."),
                "{repository}: {error}"
            );
        }
        assert_eq!(requests.lock().len(), 2);
    }

    // --- checkout ---------------------------------------------------------------------------

    fn checkout(reference: &str, force: bool) -> CheckoutChangeRequestInput {
        CheckoutChangeRequestInput {
            cwd: "/repo".into(),
            context: None,
            reference: reference.into(),
            force,
        }
    }

    fn pull_request_graphql(pr: Value) -> GraphqlFn {
        Box::new(move |_| {
            Ok(json!({ "data": { "repository": { "pullRequest": pr.clone() } } }).to_string())
        })
    }

    #[tokio::test]
    async fn checks_a_same_repository_pull_request_out_from_its_head_branch() {
        let h = harness(
            Options::new(web_remotes()),
            FakeApi {
                graphql: Some(pull_request_graphql(node(5, "feature/x", "acme"))),
                ..Default::default()
            },
        );
        h.provider
            .checkout_change_request(checkout("5", false))
            .await
            .unwrap();
        assert_eq!(
            *h.log.lock(),
            vec![
                (
                    "fetchRemoteTrackingBranch".to_owned(),
                    json!({ "cwd": "/repo", "remoteName": "origin", "remoteBranch": "feature/x" })
                ),
                (
                    "execute".to_owned(),
                    json!(["branch", "feature/x", "refs/remotes/origin/feature/x"])
                ),
                (
                    "switchRef".to_owned(),
                    json!({ "cwd": "/repo", "refName": "feature/x" })
                ),
                (
                    "setBranchUpstream".to_owned(),
                    json!({ "cwd": "/repo", "branch": "feature/x", "remoteName": "origin", "remoteBranch": "feature/x" })
                ),
            ]
        );
    }

    #[tokio::test]
    async fn refuses_a_fork_checkout_when_the_base_default_branch_cannot_be_read() {
        let mut options = Options::new(web_remotes());
        options.local_branches = vec!["main".into()];
        let h = harness(
            options,
            FakeApi {
                graphql: Some(pull_request_graphql(node(6, "main", "someone"))),
                rest: Some(Box::new(|input| {
                    Err(GitHubApiError::Request(
                        crate::github::api::GitHubApiRequestError {
                            host: "github.com".into(),
                            operation: input.operation.clone(),
                            cause: crate::source_control_core::cause_from_message("offline"),
                        },
                    ))
                })),
            },
        );
        h.provider
            .checkout_change_request(checkout("6", true))
            .await
            .unwrap_err();
        // Nothing touched the local branches: `main` must not be reset to the fork's commit.
        assert!(h.log.lock().is_empty());
    }

    #[tokio::test]
    async fn adds_a_remote_for_a_fork_and_names_a_default_branch_head_after_its_owner() {
        let mut options = Options::new(web_remotes());
        options.local_branches = vec!["someone/main".into()];
        let h = harness(
            options,
            FakeApi {
                graphql: Some(pull_request_graphql(node(6, "main", "someone"))),
                rest: Some(Box::new(|input| {
                    Ok(rest_response(repository_body(
                        if input.path == "repos/acme/web" {
                            "acme/web"
                        } else {
                            "someone/web"
                        },
                        "main",
                    )))
                })),
            },
        );
        h.provider
            .checkout_change_request(checkout("6", true))
            .await
            .unwrap();
        assert_eq!(
            *h.log.lock(),
            vec![
                (
                    "ensureRemote".to_owned(),
                    json!({ "cwd": "/repo", "preferredName": "someone", "url": "git@github.com:someone/web.git" })
                ),
                (
                    "fetchRemoteTrackingBranch".to_owned(),
                    json!({ "cwd": "/repo", "remoteName": "someone", "remoteBranch": "main" })
                ),
                (
                    "switchRef".to_owned(),
                    json!({ "cwd": "/repo", "refName": "someone/main" })
                ),
                (
                    "execute".to_owned(),
                    json!(["reset", "--hard", "--quiet", "refs/remotes/someone/main"])
                ),
                (
                    "setBranchUpstream".to_owned(),
                    json!({ "cwd": "/repo", "branch": "someone/main", "remoteName": "someone", "remoteBranch": "main" })
                ),
            ]
        );
    }

    #[tokio::test]
    async fn keeps_gits_own_output_out_of_what_a_failed_checkout_reports() {
        let mut options = Options::new(web_remotes());
        options.git_failure = true;
        let h = harness(
            options,
            FakeApi {
                graphql: Some(pull_request_graphql(node(5, "feature/x", "acme"))),
                ..Default::default()
            },
        );
        let error = h
            .provider
            .checkout_change_request(checkout("5", false))
            .await
            .unwrap_err();
        assert_eq!(
            detail_of(&error),
            "The pull request could not be checked out with git."
        );
        assert!(!error.to_string().contains("secret-path"));
    }

    #[test]
    fn names_the_local_branch_the_way_gh_pr_checkout_does() {
        let name = |head: &str, cross: bool| {
            pull_request_checkout_branch_name(head, Some("someone"), cross, Some("main"))
        };
        assert_eq!(name("main", true), "someone/main");
        assert_eq!(name("feature", true), "feature");
        assert_eq!(name("main", false), "main");
    }

    // --- auth parsing -----------------------------------------------------------------------

    fn probe_input(stdout: &str, stderr: &str, exit_code: i32) -> SourceControlAuthProbeInput {
        SourceControlAuthProbeInput {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code,
        }
    }

    fn text(option: &EffectOption<TrimmedNonEmptyString>) -> Option<&str> {
        option.as_ref_option().map(|v| v.as_str())
    }

    fn single_host_status(extra: Value) -> String {
        json!({ "hosts": { "github.com": [
            { "state": "success", "active": true, "host": "github.com", "login": "active-user", "tokenSource": "keyring", "gitProtocol": "ssh" },
            extra,
        ] } })
        .to_string()
    }

    #[test]
    fn accepts_active_authenticated_github_accounts_when_another_account_fails() {
        let stdout = single_host_status(json!({
            "state": "error", "active": false, "host": "github.com", "login": "stale-user",
            "tokenSource": "keyring", "gitProtocol": "ssh", "error": "The token in keyring is invalid.",
        }));
        let auth = parse_github_auth(&probe_input(&stdout, "", 0), &GitHubSettings::default());
        assert_eq!(auth.status, SourceControlProviderAuthStatus::Authenticated);
        assert_eq!(text(&auth.account), Some("active-user"));
        assert_eq!(text(&auth.host), Some("github.com"));
    }

    #[test]
    fn parses_github_auth_json_from_stdout_when_stderr_has_warnings() {
        let stdout = json!({ "hosts": { "github.com": [
            { "state": "success", "active": true, "host": "github.com", "login": "active-user", "tokenSource": "keyring", "gitProtocol": "ssh" }
        ] } })
        .to_string();
        let auth = parse_github_auth(
            &probe_input(
                &stdout,
                "warning: ignored diagnostic from gh
",
                0,
            ),
            &GitHubSettings::default(),
        );
        assert_eq!(auth.status, SourceControlProviderAuthStatus::Authenticated);
        assert_eq!(text(&auth.account), Some("active-user"));
        assert_eq!(text(&auth.host), Some("github.com"));
    }

    #[test]
    fn reports_unauthenticated_when_github_json_has_accounts_but_none_are_valid() {
        let stdout = json!({ "hosts": { "github.com": [
            { "state": "error", "active": true, "host": "github.com", "login": "stale-user",
              "tokenSource": "keyring", "gitProtocol": "ssh", "error": "The token in keyring is invalid." }
        ] } })
        .to_string();
        let auth = parse_github_auth(&probe_input(&stdout, "", 0), &GitHubSettings::default());
        assert_eq!(
            auth.status,
            SourceControlProviderAuthStatus::Unauthenticated
        );
        assert_eq!(text(&auth.host), Some("github.com"));
        assert_eq!(text(&auth.detail), Some("The token in keyring is invalid."));
    }

    #[test]
    fn reports_an_update_hint_instead_of_unauthenticated_when_gh_predates_json() {
        let auth = parse_github_auth(
            &probe_input(
                "",
                "unknown flag: --json

Usage:  gh auth status [flags]
",
                1,
            ),
            &GitHubSettings::default(),
        );
        assert_eq!(auth.status, SourceControlProviderAuthStatus::Unknown);
        assert!(text(&auth.detail).unwrap().contains("2.81.0"));
    }

    fn multi_account_status(extra: Vec<Value>) -> SourceControlAuthProbeInput {
        let mut github = vec![
            json!({ "state": "success", "active": true, "host": "github.com", "login": "personal" }),
            json!({ "state": "success", "active": false, "host": "github.com", "login": "work" }),
        ];
        github.extend(extra);
        probe_input(
            &json!({ "hosts": {
                "github.com": github,
                "ghe.acme.test": [
                    { "state": "error", "active": true, "host": "ghe.acme.test", "login": "jm", "error": "expired" }
                ],
            } })
            .to_string(),
            "",
            0,
        )
    }

    fn settings(value: Value) -> GitHubSettings {
        read_settings(Some(&value))
    }

    #[test]
    fn reports_every_gh_login_and_leads_with_the_account_settings_pin() {
        let auth = parse_github_auth(
            &multi_account_status(vec![]),
            &settings(
                json!({ "hosts": { "github.com": { "account": "work", "enabled": true } }, "tokens": {} }),
            ),
        );
        assert_eq!(text(&auth.account), Some("work"));
        let accounts = auth.accounts.unwrap();
        let view: Vec<(String, String, bool, bool, Option<String>)> = accounts
            .iter()
            .map(|a| {
                (
                    a.host.as_str().into(),
                    a.account.as_str().into(),
                    a.active,
                    a.authenticated,
                    a.error.as_ref().map(|e| e.as_str().into()),
                )
            })
            .collect();
        assert_eq!(
            view,
            vec![
                ("github.com".into(), "personal".into(), true, true, None),
                ("github.com".into(), "work".into(), false, true, None),
                (
                    "ghe.acme.test".into(),
                    "jm".into(),
                    true,
                    false,
                    Some("expired".into())
                ),
            ]
        );
    }

    #[test]
    fn falls_back_to_ghs_active_login_when_the_pinned_account_is_gone() {
        let auth = parse_github_auth(
            &multi_account_status(vec![]),
            &settings(
                json!({ "hosts": { "github.com": { "account": "former-job", "enabled": true } }, "tokens": {} }),
            ),
        );
        assert_eq!(text(&auth.account), Some("personal"));
    }

    #[test]
    fn reports_unauthenticated_when_settings_turn_off_every_signed_in_host() {
        let auth = parse_github_auth(
            &multi_account_status(vec![]),
            &settings(json!({ "hosts": { "github.com": { "enabled": false } }, "tokens": {} })),
        );
        assert_eq!(
            auth.status,
            SourceControlProviderAuthStatus::Unauthenticated
        );
        assert_eq!(
            text(&auth.detail),
            Some(
                "Every GitHub host gh is signed in to is turned off in Settings → Source Control."
            )
        );
    }

    #[test]
    fn names_the_environment_token_that_overrides_the_settings_choice() {
        let auth = parse_github_auth(
            &multi_account_status(vec![json!({
                "state": "success", "active": false, "host": "github.com", "login": "bot", "tokenSource": "GH_TOKEN",
            })]),
            &settings(
                json!({ "hosts": { "github.com": { "account": "work", "enabled": true } }, "tokens": {} }),
            ),
        );
        assert_eq!(text(&auth.account), Some("bot"));
        assert_eq!(
            text(&auth.detail),
            Some(
                "Using GH_TOKEN from the server environment; it overrides the account chosen in Settings."
            )
        );
        assert_eq!(
            auth.accounts.unwrap()[2]
                .environment_variable
                .as_ref()
                .map(|v| v.as_str()),
            Some("GH_TOKEN")
        );
    }

    // --- link resolution --------------------------------------------------------------------

    #[tokio::test]
    async fn resolves_pull_and_issue_subjects_on_the_linked_host_without_using_the_checkout() {
        for route in ["pull", "issues"] {
            let h = harness(
                Options::new(String::new()),
                FakeApi {
                    rest: Some(Box::new(|input| {
                        assert_eq!(input.host, "github.com");
                        assert_eq!(input.path, "repos/owner/repo/issues/42");
                        Ok(rest_response(
                            json!({ "title": "Pairing expiry", "body": "Preserve remote access", "id": 1 }),
                        ))
                    })),
                    ..Default::default()
                },
            );
            let lookup = h
                .provider
                .resolve_link(ResolveSourceControlLinkInput {
                    cwd: "/unrelated".into(),
                    url: reqwest::Url::parse(&format!("https://github.com/owner/repo/{route}/42"))
                        .unwrap(),
                })
                .expect("a lookup");
            assert_eq!(
                lookup.await.unwrap(),
                SourceControlLinkSubject {
                    title: "Pairing expiry".into(),
                    body: Some("Preserve remote access".into())
                }
            );
            assert!(
                h.provider
                    .resolve_link(ResolveSourceControlLinkInput {
                        cwd: "/unrelated".into(),
                        url: reqwest::Url::parse("https://github.com/owner/repo").unwrap(),
                    })
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn retains_the_read_and_decode_failures_without_exposing_their_raw_contents() {
        for stage in ["read", "decode"] {
            let h = harness(
                Options::new(String::new()),
                FakeApi {
                    rest: Some(Box::new(move |_| {
                        if stage == "read" {
                            Err(GitHubApiError::Response(
                                crate::github::api::GitHubApiResponseError {
                                    host: "github.com".into(),
                                    operation: "resolveLink".into(),
                                    status: 500,
                                    github_errors: None,
                                },
                            ))
                        } else {
                            Ok(GitHubRestResponse {
                                status: 200,
                                headers: HashMap::new(),
                                body: "private response text".into(),
                                truncated: false,
                                invalid_utf8: false,
                            })
                        }
                    })),
                    ..Default::default()
                },
            );
            let lookup = h
                .provider
                .resolve_link(ResolveSourceControlLinkInput {
                    cwd: "/repo".into(),
                    url: reqwest::Url::parse("https://github.com/owner/repo/issues/42").unwrap(),
                })
                .expect("a lookup");
            let error = lookup.await.unwrap_err();
            let SourceControlProviderError::SourceControlProviderError {
                operation,
                detail,
                cause,
                ..
            } = &error;
            assert_eq!(
                operation,
                if stage == "read" {
                    "resolveLink"
                } else {
                    "resolveLink.decode"
                }
            );
            assert_eq!(detail, "The linked subject could not be read.");
            assert!(!error.to_string().contains("private response text"));
            let cause = cause.as_ref().unwrap();
            if stage == "read" {
                assert_eq!(cause["_tag"], json!("GitHubApiResponseError"));
            } else {
                assert_eq!(cause["_tag"], json!("SchemaError"));
                assert!(!cause.to_string().contains("private response text"));
            }
        }
    }

    // --- enterprise and repository precedence -----------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn keeps_enterprise_operations_on_the_selected_remote() {
        for remote_url in [
            "git@code.example.test:team/project.git",
            "ssh://git@code.example.test/team/project.git",
            "https://code.example.test/team/project.git",
        ] {
            let graphql = Arc::new(PlMutex::new(Vec::<GitHubGraphQlInput>::new()));
            let rest = Arc::new(PlMutex::new(Vec::<GitHubRestInput>::new()));
            let (g, r) = (graphql.clone(), rest.clone());
            let h = harness(
                Options::new(remotes_output(&[
                    ("origin", "git@github.com:team/project.git"),
                    ("enterprise", remote_url),
                ])),
                FakeApi {
                    graphql: Some(Box::new(move |input| {
                        g.lock().push(input.clone());
                        let mut pr = node(42, "feature", "team");
                        pr["url"] = json!("https://code.example.test/team/project/pull/42");
                        pr["isCrossRepository"] = json!(false);
                        pr["headRepository"] =
                            json!({ "name": "project", "nameWithOwner": "team/project" });
                        let repository = if input.operation == "getPullRequest" {
                            json!({ "pullRequest": pr })
                        } else {
                            json!({ "h0": { "nodes": [pr] } })
                        };
                        Ok(json!({ "data": { "repository": repository } }).to_string())
                    })),
                    rest: Some(Box::new(move |input| {
                        r.lock().push(input.clone());
                        Ok(rest_response(json!({
                            "full_name": "team/project",
                            "html_url": format!("https://{}/team/project", input.host),
                            "ssh_url": format!("git@{}:team/project.git", input.host),
                            "default_branch": "main",
                        })))
                    })),
                },
            );
            let dir = tempfile::tempdir().unwrap();
            let body_file = dir.path().join("body.md");
            std::fs::write(&body_file, "Enterprise PR").unwrap();
            let context = SourceControlProviderContext {
                provider: SourceControlProviderInfo {
                    kind: kind(),
                    name: tne("GitHub Self-Hosted").unwrap(),
                    base_url: "https://code.example.test".into(),
                },
                remote_name: "enterprise".into(),
                remote_url: remote_url.into(),
                requested_host: None,
            };
            for state in [ChangeRequestState::Open, ChangeRequestState::Closed] {
                let mut input = list_input("feature", Some(state), None);
                input.context = Some(context.clone());
                let prs = h.provider.list_change_requests(input).await.unwrap();
                assert_eq!(prs[0].url, "https://code.example.test/team/project/pull/42");
            }
            let get = |reference: &str| GetChangeRequestInput {
                cwd: "/repo".into(),
                context: Some(context.clone()),
                reference: reference.into(),
            };
            h.provider.get_change_request(get("42")).await.unwrap();
            h.provider.get_change_request(get("feature")).await.unwrap();
            h.provider
                .create_change_request(CreateChangeRequestInput {
                    cwd: "/repo".into(),
                    context: Some(context.clone()),
                    source: None,
                    target: None,
                    base_ref_name: "main".into(),
                    head_selector: "feature".into(),
                    title: "PR".into(),
                    body_file: body_file.to_string_lossy().into_owned(),
                })
                .await
                .unwrap();
            assert_eq!(
                h.provider
                    .get_default_branch(default_branch_input(Some(context.clone())))
                    .await
                    .unwrap()
                    .as_deref(),
                Some("main")
            );
            h.provider
                .checkout_change_request(CheckoutChangeRequestInput {
                    cwd: "/repo".into(),
                    context: Some(context.clone()),
                    reference: "42".into(),
                    force: false,
                })
                .await
                .unwrap();
            for repository in ["team/other", "github.com/team/explicit"] {
                h.provider
                    .get_repository_clone_urls(GetRepositoryCloneUrlsInput {
                        cwd: "/repo".into(),
                        context: Some(context.clone()),
                        repository: repository.into(),
                    })
                    .await
                    .unwrap();
            }
            let graphql = graphql.lock();
            assert_eq!(graphql.len(), 5, "{remote_url}");
            for request in graphql.iter() {
                assert_eq!(request.host, "code.example.test");
                let variables = request.variables.as_ref().unwrap();
                assert_eq!(variables["owner"], json!("team"));
                assert_eq!(variables["name"], json!("project"));
            }
            let rest: Vec<(String, String)> = rest
                .lock()
                .iter()
                .map(|i| (i.host.clone(), i.path.clone()))
                .collect();
            assert_eq!(
                rest,
                vec![
                    (
                        "code.example.test".to_owned(),
                        "repos/team/project/pulls".to_owned()
                    ),
                    (
                        "code.example.test".to_owned(),
                        "repos/team/project".to_owned()
                    ),
                    (
                        "code.example.test".to_owned(),
                        "repos/team/other".to_owned()
                    ),
                    ("github.com".to_owned(), "repos/team/explicit".to_owned()),
                ]
            );
            let log = h.log.lock();
            let fetch = log
                .iter()
                .find(|(name, _)| name == "fetchRemoteTrackingBranch")
                .unwrap();
            assert_eq!(
                fetch.1,
                json!({ "cwd": "/repo", "remoteName": "enterprise", "remoteBranch": "feature" })
            );
        }
    }

    #[tokio::test]
    async fn preserves_repository_precedence() {
        for (host, resolved, env_repository, expected) in [
            ("github.com", "", "", "acme/web"),
            ("code.example.test", "", "", "acme/web"),
            ("github.com", "remote.origin.gh-resolved base", "", "me/web"),
            (
                "code.example.test",
                "remote.origin.gh-resolved base",
                "",
                "me/web",
            ),
            (
                "github.com",
                "remote.upstream.gh-resolved acme/other",
                "",
                "acme/other",
            ),
            (
                "code.example.test",
                "remote.upstream.gh-resolved acme/other",
                "",
                "acme/other",
            ),
            ("github.com", "", "chosen/repo", "chosen/repo"),
            ("code.example.test", "", "chosen/repo", "chosen/repo"),
        ] {
            let targets = Arc::new(PlMutex::new(Vec::<String>::new()));
            let t = targets.clone();
            let mut options = Options::new(remotes_output(&[
                ("origin", &format!("git@{host}:me/web.git")),
                ("upstream", &format!("git@{host}:acme/web.git")),
            ]));
            options.resolved = resolved.into();
            options.env = vec![("GH_REPO", env_repository)];
            let h = harness(
                options,
                FakeApi {
                    rest: Some(Box::new(move |input| {
                        t.lock().push(format!("{}/{}", input.host, input.path));
                        Ok(rest_response(json!({
                            "full_name": expected,
                            "html_url": format!("https://{host}/{expected}"),
                            "ssh_url": format!("git@{host}:{expected}.git"),
                            "default_branch": "main",
                        })))
                    })),
                    ..Default::default()
                },
            );
            let dir = tempfile::tempdir().unwrap();
            let body_file = dir.path().join("body");
            std::fs::write(&body_file, "Body").unwrap();
            let context = SourceControlProviderContext {
                provider: SourceControlProviderInfo {
                    kind: kind(),
                    name: tne("GitHub").unwrap(),
                    base_url: format!("https://{host}"),
                },
                remote_name: "origin".into(),
                remote_url: format!("git@{host}:me/web.git"),
                requested_host: None,
            };
            h.provider
                .get_default_branch(default_branch_input(Some(context.clone())))
                .await
                .unwrap();
            h.provider
                .create_change_request(CreateChangeRequestInput {
                    cwd: "/repo".into(),
                    context: Some(context),
                    source: None,
                    target: None,
                    base_ref_name: "main".into(),
                    head_selector: "me:feature".into(),
                    title: "PR".into(),
                    body_file: body_file.to_string_lossy().into_owned(),
                })
                .await
                .unwrap();
            assert_eq!(
                *targets.lock(),
                vec![
                    format!("{host}/repos/{expected}"),
                    format!("{host}/repos/{expected}/pulls")
                ],
                "{host} {resolved:?} {env_repository:?}"
            );
        }
    }

    #[test]
    fn head_branch_probe_drops_owner_qualified_selectors() {
        let h = harness(Options::new(String::new()), FakeApi::default());
        let probe = h
            .provider
            .head_branch_probe(
                &["feature".into(), "me:feature".into()],
                HeadProbeState::All,
            )
            .unwrap();
        assert_eq!(
            probe,
            HeadBranchProbe {
                head_selectors: vec!["feature".into()],
                limit: 100
            }
        );
    }
}
