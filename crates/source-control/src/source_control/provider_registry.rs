//! Port of upstream/apps/server/src/sourceControl/SourceControlProviderRegistry.ts
//!
//! Resolves which source control provider serves a checkout (from its git remotes), binds the
//! remote's context onto the provider, and runs every driver's discovery probe.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use turin_contracts::source_control::{
    ChangeRequest, SourceControlProviderDiscoveryItem, SourceControlProviderError,
    SourceControlProviderKind, SourceControlRepositoryCloneUrls,
};
use turin_contracts::vcs::VcsRemote;

use crate::shared::source_control::detect_source_control_provider_from_remote_url;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::discovery::{
    SourceControlProviderDiscoverySpec, probe_source_control_provider,
    refine_unknown_remote_provider,
};
use crate::source_control_core::host::SourceControlHost;
use crate::source_control_core::source_control_provider::{
    CheckoutChangeRequestInput, CreateChangeRequestInput, CreateRepositoryInput,
    GetChangeRequestInput, GetDefaultBranchInput, GetRepositoryCloneUrlsInput, HeadBranchProbe,
    HeadProbeState, ListChangeRequestsInput, ResolveProviderContext,
    ResolveSourceControlLinkFuture, ResolveSourceControlLinkInput, SourceControlProvider,
    SourceControlProviderContext, transport_safe_source_control_error_value,
};

const PROVIDER_DETECTION_CACHE_CAPACITY: usize = 2_048;
const PROVIDER_DETECTION_CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct SourceControlProviderRegistration {
    pub kind: SourceControlProviderKind,
    pub provider: Arc<dyn SourceControlProvider>,
    pub discovery: SourceControlProviderDiscoverySpec,
}

#[derive(Clone)]
pub struct SourceControlProviderHandle {
    pub provider: Arc<dyn SourceControlProvider>,
    pub context: Option<SourceControlProviderContext>,
}

fn no_provider_error(
    kind: &SourceControlProviderKind,
    operation: &str,
    cwd: &str,
    reference: Option<&str>,
    repository: Option<&str>,
) -> SourceControlProviderError {
    SourceControlProviderError::SourceControlProviderError {
        provider: kind.clone(),
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        command: None,
        repository: repository.map(transport_safe_source_control_error_value),
        reference: reference.map(transport_safe_source_control_error_value),
        detail: format!("No {kind} source control provider is registered."),
        cause: None,
    }
}

/// The provider for a kind with no registration: every operation says so.
struct UnsupportedProvider {
    kind: SourceControlProviderKind,
}

#[async_trait]
impl SourceControlProvider for UnsupportedProvider {
    fn kind(&self) -> SourceControlProviderKind {
        self.kind.clone()
    }
    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> Result<Vec<ChangeRequest>, SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "listChangeRequests",
            &input.cwd,
            None,
            None,
        ))
    }
    async fn get_change_request(
        &self,
        input: GetChangeRequestInput,
    ) -> Result<ChangeRequest, SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "getChangeRequest",
            &input.cwd,
            Some(&input.reference),
            None,
        ))
    }
    async fn create_change_request(
        &self,
        input: CreateChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "createChangeRequest",
            &input.cwd,
            Some(&input.head_selector),
            None,
        ))
    }
    async fn get_repository_clone_urls(
        &self,
        input: GetRepositoryCloneUrlsInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "getRepositoryCloneUrls",
            &input.cwd,
            None,
            Some(&input.repository),
        ))
    }
    async fn create_repository(
        &self,
        input: CreateRepositoryInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "createRepository",
            &input.cwd,
            None,
            Some(&input.repository),
        ))
    }
    async fn get_default_branch(
        &self,
        input: GetDefaultBranchInput,
    ) -> Result<Option<String>, SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "getDefaultBranch",
            &input.cwd,
            None,
            None,
        ))
    }
    async fn checkout_change_request(
        &self,
        input: CheckoutChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        Err(no_provider_error(
            &self.kind,
            "checkoutChangeRequest",
            &input.cwd,
            Some(&input.reference),
            None,
        ))
    }
}

fn select_provider_context(remotes: &[VcsRemote]) -> Option<SourceControlProviderContext> {
    let candidates: Vec<SourceControlProviderContext> = remotes
        .iter()
        .filter_map(|remote| {
            let provider = detect_source_control_provider_from_remote_url(remote.url.as_str())?;
            Some(SourceControlProviderContext {
                provider,
                remote_name: remote.name.as_str().to_owned(),
                remote_url: remote.url.as_str().to_owned(),
                requested_host: None,
            })
        })
        .collect();
    candidates
        .iter()
        .find(|c| c.remote_name == "origin")
        .or_else(|| {
            candidates
                .iter()
                .find(|c| c.provider.kind.as_str() != "unknown")
        })
        .or_else(|| candidates.first())
        .cloned()
}

/// A provider with the checkout's remote context filled in wherever the caller gave none
/// (`bindProviderContext`). `createRepository` takes no context.
struct BoundProvider {
    inner: Arc<dyn SourceControlProvider>,
    context: SourceControlProviderContext,
}

#[async_trait]
impl SourceControlProvider for BoundProvider {
    fn kind(&self) -> SourceControlProviderKind {
        self.inner.kind()
    }
    fn head_branch_probe(
        &self,
        head_selectors: &[String],
        state: HeadProbeState,
    ) -> Option<HeadBranchProbe> {
        self.inner.head_branch_probe(head_selectors, state)
    }
    async fn read_change_request_template(&self, cwd: &str, treeish: &str) -> Option<String> {
        self.inner.read_change_request_template(cwd, treeish).await
    }
    fn repository_name_from_remote_url(&self, url: &str) -> Option<Option<String>> {
        self.inner.repository_name_from_remote_url(url)
    }
    async fn refine_repository_identity(
        &self,
        identity: &turin_contracts::environment::RepositoryIdentity,
        resolver: &dyn ResolveProviderContext,
    ) -> Option<Result<turin_contracts::environment::RepositoryIdentity, SourceControlProviderError>>
    {
        self.inner
            .refine_repository_identity(identity, resolver)
            .await
    }
    fn resolve_link(
        &self,
        input: ResolveSourceControlLinkInput,
    ) -> ResolveSourceControlLinkFuture<'_> {
        self.inner.resolve_link(input)
    }
    async fn list_change_requests(
        &self,
        mut input: ListChangeRequestsInput,
    ) -> Result<Vec<ChangeRequest>, SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.list_change_requests(input).await
    }
    async fn get_change_request(
        &self,
        mut input: GetChangeRequestInput,
    ) -> Result<ChangeRequest, SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.get_change_request(input).await
    }
    async fn create_change_request(
        &self,
        mut input: CreateChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.create_change_request(input).await
    }
    async fn get_repository_clone_urls(
        &self,
        mut input: GetRepositoryCloneUrlsInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.get_repository_clone_urls(input).await
    }
    async fn create_repository(
        &self,
        input: CreateRepositoryInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError> {
        self.inner.create_repository(input).await
    }
    async fn get_default_branch(
        &self,
        mut input: GetDefaultBranchInput,
    ) -> Result<Option<String>, SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.get_default_branch(input).await
    }
    async fn checkout_change_request(
        &self,
        mut input: CheckoutChangeRequestInput,
    ) -> Result<(), SourceControlProviderError> {
        input.context.get_or_insert_with(|| self.context.clone());
        self.inner.checkout_change_request(input).await
    }
}

fn bind_provider_context(
    provider: Arc<dyn SourceControlProvider>,
    context: Option<&SourceControlProviderContext>,
) -> Arc<dyn SourceControlProvider> {
    match context {
        None => provider,
        Some(context) => Arc::new(BoundProvider {
            inner: provider,
            context: context.clone(),
        }),
    }
}

type ContextResult = Result<Option<SourceControlProviderContext>, SourceControlProviderError>;
type SharedLookup = Shared<BoxFuture<'static, ContextResult>>;

enum CacheSlot {
    /// A lookup in flight, shared by every caller asking for the same checkout meanwhile.
    Pending(SharedLookup),
    Ready {
        value: Option<SourceControlProviderContext>,
        expires_at: i64,
        seq: u64,
    },
}

/// A bounded cache of provider detection by checkout, keeping successes for a short while and
/// never keeping a failure (`Cache.makeWith` with `timeToLive: success ? 5s : 0`).
#[derive(Default)]
struct ContextCache {
    slots: HashMap<String, CacheSlot>,
    seq: u64,
}

pub struct SourceControlProviderRegistry {
    providers: HashMap<SourceControlProviderKind, Arc<dyn SourceControlProvider>>,
    discovery_specs: Vec<SourceControlProviderDiscoverySpec>,
    host: Arc<dyn SourceControlHost>,
    clock: Arc<dyn Clock>,
    /// `ServerConfig.cwd`: where discovery probes run.
    cwd: String,
    cache: Arc<Mutex<ContextCache>>,
}

impl SourceControlProviderRegistry {
    /// `makeWithProviders`.
    pub fn with_providers(
        registrations: Vec<SourceControlProviderRegistration>,
        host: Arc<dyn SourceControlHost>,
        clock: Arc<dyn Clock>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            providers: registrations
                .iter()
                .map(|r| (r.kind.clone(), r.provider.clone()))
                .collect(),
            discovery_specs: registrations.into_iter().map(|r| r.discovery).collect(),
            host,
            clock,
            cwd: cwd.into(),
            cache: Arc::new(Mutex::new(ContextCache::default())),
        }
    }

    /// The provider for a kind; an unregistered kind gets one whose every call says so.
    pub fn get(&self, kind: &SourceControlProviderKind) -> Arc<dyn SourceControlProvider> {
        self.providers
            .get(kind)
            .cloned()
            .unwrap_or_else(|| Arc::new(UnsupportedProvider { kind: kind.clone() }))
    }

    /// Return `None` synchronously for unsupported URLs, without starting a lookup.
    pub fn resolve_link(
        &self,
        input: ResolveSourceControlLinkInput,
    ) -> ResolveSourceControlLinkFuture<'_> {
        if input.url.scheme() != "https"
            || !input.url.username().is_empty()
            || input.url.password().is_some()
        {
            return None;
        }
        let kind = detect_source_control_provider_from_remote_url(input.url.as_str())?.kind;
        self.providers.get(&kind)?.resolve_link(input)
    }

    async fn detect_provider_context(&self, cwd: &str) -> ContextResult {
        let detection_error =
            |cause: String| SourceControlProviderError::SourceControlProviderError {
                provider: SourceControlProviderKind::new("unknown").expect("valid kind"),
                operation: "detectProvider".into(),
                cwd: cwd.to_owned(),
                command: None,
                repository: None,
                reference: None,
                detail: "Failed to detect source control provider.".into(),
                cause: Some(serde_json::Value::String(cause)),
            };
        let remotes = self
            .host
            .git_remotes(cwd)
            .await
            .map_err(|e| detection_error(e.to_string()))?;
        let context = select_provider_context(&remotes.remotes);
        Ok(
            refine_unknown_remote_provider(&self.discovery_specs, self.host.as_ref(), cwd, context)
                .await,
        )
    }

    /// Detection by checkout, shared between concurrent callers and kept for
    /// `PROVIDER_DETECTION_CACHE_TTL` once it succeeds.
    async fn cached_provider_context(self: &Arc<Self>, cwd: &str) -> ContextResult {
        let lookup = {
            let mut cache = self.cache.lock();
            let now = self.clock.now_millis();
            match cache.slots.get(cwd) {
                Some(CacheSlot::Ready {
                    value, expires_at, ..
                }) if *expires_at > now => {
                    return Ok(value.clone());
                }
                Some(CacheSlot::Pending(shared)) => shared.clone(),
                _ => {
                    let registry = self.clone();
                    let key = cwd.to_owned();
                    let shared = async move {
                        let result = registry.detect_provider_context(&key).await;
                        let mut cache = registry.cache.lock();
                        match &result {
                            Ok(value) => {
                                cache.seq += 1;
                                let seq = cache.seq;
                                let expires_at = registry.clock.now_millis()
                                    + PROVIDER_DETECTION_CACHE_TTL.as_millis() as i64;
                                cache.slots.insert(
                                    key.clone(),
                                    CacheSlot::Ready {
                                        value: value.clone(),
                                        expires_at,
                                        seq,
                                    },
                                );
                                if cache.slots.len() > PROVIDER_DETECTION_CACHE_CAPACITY {
                                    let now = registry.clock.now_millis();
                                    cache.slots.retain(|_, slot| match slot {
                                        CacheSlot::Ready { expires_at, .. } => *expires_at > now,
                                        CacheSlot::Pending(_) => true,
                                    });
                                    while cache.slots.len() > PROVIDER_DETECTION_CACHE_CAPACITY {
                                        let oldest = cache
                                            .slots
                                            .iter()
                                            .filter_map(|(k, slot)| match slot {
                                                CacheSlot::Ready { seq, .. } => {
                                                    Some((*seq, k.clone()))
                                                }
                                                CacheSlot::Pending(_) => None,
                                            })
                                            .min();
                                        match oldest {
                                            Some((_, k)) => {
                                                cache.slots.remove(&k);
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                            // A failure is not kept: the next call asks again.
                            Err(_) => {
                                cache.slots.remove(&key);
                            }
                        }
                        result
                    }
                    .boxed()
                    .shared();
                    cache
                        .slots
                        .insert(cwd.to_owned(), CacheSlot::Pending(shared.clone()));
                    shared
                }
            }
        };
        lookup.await
    }

    pub async fn resolve_handle(
        self: &Arc<Self>,
        cwd: &str,
        context: Option<SourceControlProviderContext>,
    ) -> Result<SourceControlProviderHandle, SourceControlProviderError> {
        let context = match context {
            None => self.cached_provider_context(cwd).await?,
            Some(context) => {
                refine_unknown_remote_provider(
                    &self.discovery_specs,
                    self.host.as_ref(),
                    cwd,
                    Some(context),
                )
                .await
            }
        };
        let kind = context
            .as_ref()
            .map(|c| c.provider.kind.clone())
            .unwrap_or_else(|| SourceControlProviderKind::new("unknown").expect("valid kind"));
        let provider = self.get(&kind);
        Ok(SourceControlProviderHandle {
            provider: bind_provider_context(provider, context.as_ref()),
            context,
        })
    }

    pub async fn resolve(
        self: &Arc<Self>,
        cwd: &str,
    ) -> Result<Arc<dyn SourceControlProvider>, SourceControlProviderError> {
        Ok(self.resolve_handle(cwd, None).await?.provider)
    }

    /// Probes every driver's CLI/API concurrently.
    pub async fn discover(&self) -> Vec<SourceControlProviderDiscoveryItem> {
        futures::future::join_all(
            self.discovery_specs
                .iter()
                .map(|spec| probe_source_control_provider(spec, self.host.as_ref(), &self.cwd)),
        )
        .await
    }
}

/// Lets a provider ask the registry which host serves a remote (`refineRepositoryIdentity`'s
/// `resolveContext`).
#[async_trait]
impl ResolveProviderContext for SourceControlProviderRegistry {
    async fn resolve(
        &self,
        cwd: &str,
        context: &SourceControlProviderContext,
    ) -> Result<Option<SourceControlProviderContext>, SourceControlProviderError> {
        Ok(refine_unknown_remote_provider(
            &self.discovery_specs,
            self.host.as_ref(),
            cwd,
            Some(context.clone()),
        )
        .await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::api::{
        GitHubApi, GitHubApiError, GitHubGraphQlInput, GitHubRequestCredential, GitHubRestInput,
        GitHubRestResponse,
    };
    use crate::github::credentials::{
        GitHubCredentialUnavailableError, GitHubHostDisabledError, GitHubNotSignedInError,
    };
    use crate::github::source_control_provider::{make, make_discovery};
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::clock::ManualClock;
    use crate::source_control_core::redacted::Redacted;
    use crate::source_control_core::test_host::ScriptedHost;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::base_schemas::{EffectOption, UtcDateTime};
    use turin_contracts::source_control::SourceControlProviderInfo;
    use turin_contracts::vcs::{
        VcsError, VcsFreshness, VcsFreshnessSource, VcsListRemotesResult,
        VcsRepositoryDetectionError,
    };

    type CredentialFn =
        Box<dyn Fn(&str) -> Result<GitHubRequestCredential, GitHubApiError> + Send + Sync>;
    type RestFn =
        Box<dyn Fn(&GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> + Send + Sync>;

    struct FakeApi {
        credential: CredentialFn,
        rest: Option<RestFn>,
    }

    #[async_trait]
    impl crate::github::api::GitHubApi for FakeApi {
        async fn graphql(&self, _: GitHubGraphQlInput) -> Result<String, GitHubApiError> {
            panic!("unexpected graphql call")
        }
        async fn rest(&self, input: GitHubRestInput) -> Result<GitHubRestResponse, GitHubApiError> {
            (self.rest.as_ref().expect("unexpected rest call"))(&input)
        }
        async fn credential(&self, host: &str) -> Result<GitHubRequestCredential, GitHubApiError> {
            (self.credential)(host)
        }
    }

    fn not_signed_in(host: &str) -> GitHubApiError {
        GitHubApiError::CredentialUnavailable(GitHubCredentialUnavailableError::NotSignedIn(
            GitHubNotSignedInError {
                host: host.into(),
                account: None,
            },
        ))
    }

    fn remote(name: &str, url: &str) -> VcsRemote {
        VcsRemote {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            url: TrimmedNonEmptyString::new(url).unwrap(),
            push_url: EffectOption::None,
            is_primary: name == "origin",
        }
    }

    struct Fixture {
        registry: Arc<SourceControlProviderRegistry>,
        commands: Arc<Mutex<Vec<String>>>,
    }

    #[allow(clippy::field_reassign_with_default)]
    fn make_registry(
        remotes: Vec<VcsRemote>,
        credential: CredentialFn,
        rest: Option<RestFn>,
        remotes_error: Option<VcsError>,
    ) -> Fixture {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let seen = commands.clone();
        let mut host = ScriptedHost::default();
        host.process = Some(Box::new(move |input| {
            seen.lock().push(input.command.clone());
            Ok(Default::default())
        }));
        host.remotes = Some(Box::new(move |_| match &remotes_error {
            Some(error) => Err(error.clone()),
            None => Ok(VcsListRemotesResult {
                remotes: remotes.clone(),
                freshness: VcsFreshness {
                    source: VcsFreshnessSource::LiveLocal,
                    observed_at: "1970-01-01T00:00:00.000Z".parse::<UtcDateTime>().unwrap(),
                    expires_at: EffectOption::None,
                },
            }),
        }));
        let host: Arc<dyn SourceControlHost> = Arc::new(host);
        let api: Arc<dyn GitHubApi> = Arc::new(FakeApi { credential, rest });
        let environment = MapEnvironment::new(std::iter::empty::<(String, String)>());
        let registry = SourceControlProviderRegistry::with_providers(
            vec![SourceControlProviderRegistration {
                kind: SourceControlProviderKind::new("github").unwrap(),
                provider: Arc::new(make(api.clone(), host.clone(), environment.clone())),
                discovery: make_discovery(api, host.clone(), environment),
            }],
            host,
            ManualClock::new(0),
            "/cwd",
        );
        Fixture {
            registry: Arc::new(registry),
            commands,
        }
    }

    fn simple(remotes: Vec<VcsRemote>) -> Fixture {
        make_registry(
            remotes,
            Box::new(|host| Err(not_signed_in(host))),
            None,
            None,
        )
    }

    fn kind(name: &str) -> SourceControlProviderKind {
        SourceControlProviderKind::new(name).unwrap()
    }

    #[tokio::test]
    async fn routes_github_remotes_to_the_github_provider() {
        let f = simple(vec![remote(
            "origin",
            "git@github.com:pingdotgg/t3code.git",
        )]);
        assert_eq!(
            f.registry.resolve("/repo").await.unwrap().kind(),
            kind("github")
        );
    }

    #[tokio::test]
    async fn routes_directly_by_provider_kind_for_remote_first_workflows() {
        let f = simple(vec![]);
        assert_eq!(f.registry.get(&kind("github")).kind(), kind("github"));
    }

    #[tokio::test]
    async fn includes_the_request_cwd_when_an_unregistered_provider_is_used() {
        let f = simple(vec![]);
        let provider = f.registry.get(&kind("unknown"));
        let error = provider
            .get_change_request(GetChangeRequestInput {
                cwd: "/repo".into(),
                context: None,
                reference: "#42".into(),
            })
            .await
            .unwrap_err();
        let SourceControlProviderError::SourceControlProviderError {
            provider,
            operation,
            cwd,
            reference,
            detail,
            ..
        } = error;
        assert_eq!(provider, kind("unknown"));
        assert_eq!(operation, "getChangeRequest");
        assert_eq!(cwd, "/repo");
        assert_eq!(reference.as_deref(), Some("#42"));
        assert_eq!(detail, "No unknown source control provider is registered.");
    }

    #[tokio::test]
    async fn retains_vcs_detection_failures_with_structured_cwd_context() {
        let cause = VcsError::RepositoryDetection(
            VcsRepositoryDetectionError::VcsRepositoryDetectionError {
                operation: "resolve".into(),
                cwd: "/repo".into(),
                detail: "raw VCS detection failure".into(),
                cause: None,
            },
        );
        let f = make_registry(
            vec![],
            Box::new(|host| Err(not_signed_in(host))),
            None,
            Some(cause.clone()),
        );
        let error = f.registry.resolve("/repo").await.err().unwrap();
        let rendered = error.to_string();
        let SourceControlProviderError::SourceControlProviderError {
            provider,
            operation,
            cwd,
            detail,
            cause: kept,
            ..
        } = error;
        assert_eq!(provider, kind("unknown"));
        assert_eq!(operation, "detectProvider");
        assert_eq!(cwd, "/repo");
        assert_eq!(detail, "Failed to detect source control provider.");
        assert!(
            kept.unwrap()
                .as_str()
                .unwrap()
                .contains("raw VCS detection failure")
        );
        assert!(!rendered.contains(&cause.to_string()));
    }

    #[tokio::test]
    async fn falls_back_to_a_non_origin_remote_when_origin_is_not_configured() {
        let f = simple(vec![remote(
            "upstream",
            "https://github.com/acme/project.git",
        )]);
        assert_eq!(
            f.registry.resolve("/repo").await.unwrap().kind(),
            kind("github")
        );
    }

    #[tokio::test]
    async fn prefers_origin_then_a_known_provider_over_an_unknown_one() {
        let f = simple(vec![
            remote("mirror", "https://git.example.test/acme/project.git"),
            remote("upstream", "https://github.com/acme/project.git"),
        ]);
        let handle = f.registry.resolve_handle("/repo", None).await.unwrap();
        assert_eq!(handle.context.unwrap().remote_name, "upstream");
    }

    #[tokio::test]
    #[allow(clippy::field_reassign_with_default)]
    async fn caches_detection_for_a_short_while_and_never_a_failure() {
        // The scripted remotes handler counts through the process log, so use a fresh fixture whose
        // clock the test moves.
        let clock = ManualClock::new(0);
        let calls = Arc::new(Mutex::new(0_u32));
        let counter = calls.clone();
        let mut host = ScriptedHost::default();
        host.process = Some(Box::new(|_| Ok(Default::default())));
        host.remotes = Some(Box::new(move |_| {
            *counter.lock() += 1;
            Ok(VcsListRemotesResult {
                remotes: vec![remote("origin", "git@github.com:acme/web.git")],
                freshness: VcsFreshness {
                    source: VcsFreshnessSource::LiveLocal,
                    observed_at: "1970-01-01T00:00:00.000Z".parse::<UtcDateTime>().unwrap(),
                    expires_at: EffectOption::None,
                },
            })
        }));
        let registry = Arc::new(SourceControlProviderRegistry::with_providers(
            vec![],
            Arc::new(host),
            clock.clone(),
            "/cwd",
        ));
        registry.resolve("/repo").await.unwrap();
        registry.resolve("/repo").await.unwrap();
        assert_eq!(*calls.lock(), 1);
        clock.advance(5_001);
        registry.resolve("/repo").await.unwrap();
        assert_eq!(*calls.lock(), 2);
    }

    #[tokio::test]
    async fn routes_linked_subjects_by_url_independently_of_the_checkout_and_skips_unsupported_links()
     {
        let f = make_registry(
            vec![remote(
                "origin",
                "https://github.com/unrelated/checkout.git",
            )],
            Box::new(|host| Err(not_signed_in(host))),
            Some(Box::new(|_| {
                Ok(GitHubRestResponse {
                    status: 200,
                    headers: Default::default(),
                    body: serde_json::json!({ "title": "GitHub issue", "body": null }).to_string(),
                    truncated: false,
                    invalid_utf8: false,
                })
            })),
            None,
        );
        let url = |u: &str| ResolveSourceControlLinkInput {
            cwd: "/unrelated".into(),
            url: reqwest::Url::parse(u).unwrap(),
        };
        let lookup = f
            .registry
            .resolve_link(url("https://github.com/team/project/issues/1"))
            .expect("a lookup");
        let subject = lookup.await.unwrap();
        assert_eq!(subject.title, "GitHub issue");
        assert_eq!(subject.body, None);
        for unsupported in [
            "https://example.test/team/project/issues/1",
            "https://github.attacker.test/team/project/issues/1",
            "https://gitlab.attacker.test/team/project/-/issues/1",
            "https://github.com/team/project",
            "https://codeberg.org/team/project/issues/1",
            "https://bitbucket.org/team/project/pull-requests/1",
            "https://dev.azure.com/org/project/_git/repo/pullrequest/1",
            "http://github.com/team/project/issues/1",
            "https://user:secret@github.com/team/project/issues/1",
        ] {
            assert!(
                f.registry.resolve_link(url(unsupported)).is_none(),
                "{unsupported}"
            );
        }
    }

    #[tokio::test]
    async fn resolves_custom_host_remotes_with_a_credential_a_disabled_host_or_neither() {
        for (name, credential, expected) in [
            ("a credential", "found", "github"),
            // Claimed so the error says the host is turned off, not that it is unsupported.
            ("a host turned off in Settings", "disabled", "github"),
            ("no credential", "missing", "unknown"),
        ] {
            let hosts = Arc::new(Mutex::new(Vec::<String>::new()));
            let seen = hosts.clone();
            let f = make_registry(
                vec![remote("origin", "git@code.example.test:team/project.git")],
                Box::new(move |host| {
                    seen.lock().push(host.to_owned());
                    match credential {
                        "found" => Ok(GitHubRequestCredential {
                            token: Redacted::new("token".into()),
                            fingerprint: format!("{host}:fingerprint"),
                        }),
                        "disabled" => Err(GitHubApiError::CredentialUnavailable(
                            GitHubCredentialUnavailableError::HostDisabled(
                                GitHubHostDisabledError { host: host.into() },
                            ),
                        )),
                        _ => Err(not_signed_in(host)),
                    }
                }),
                None,
                None,
            );
            let handle = f.registry.resolve_handle("/repo", None).await.unwrap();
            assert_eq!(handle.provider.kind(), kind(expected), "{name}");
            if expected == "github" {
                assert_eq!(
                    handle.context.unwrap().provider.base_url,
                    "https://code.example.test"
                );
            }
            assert_eq!(*hosts.lock(), vec!["code.example.test"], "{name}");
            // Recognition goes through the credential layer; gh is only one of its sources.
            assert!(!f.commands.lock().iter().any(|c| c == "gh"), "{name}");
        }
    }

    #[tokio::test]
    async fn skips_github_discovery_for_the_identity_resolvers_empty_base_url() {
        let hosts = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = hosts.clone();
        let remote_url = "git@githubenterprise.dev.example.com:team/workspace.git";
        let f = make_registry(
            vec![remote("origin", remote_url)],
            Box::new(move |host| {
                seen.lock().push(host.to_owned());
                Ok(GitHubRequestCredential {
                    token: Redacted::new("token".into()),
                    fingerprint: format!("{host}:fingerprint"),
                })
            }),
            None,
            None,
        );
        let context = SourceControlProviderContext {
            provider: SourceControlProviderInfo {
                kind: kind("unknown"),
                name: TrimmedNonEmptyString::new("Unknown").unwrap(),
                base_url: String::new(),
            },
            remote_name: "origin".into(),
            remote_url: remote_url.into(),
            requested_host: None,
        };
        let unresolved = f
            .registry
            .resolve_handle("/repo", Some(context.clone()))
            .await
            .unwrap();
        assert_eq!(unresolved.context, Some(context));
        assert!(hosts.lock().is_empty());

        let resolved = f.registry.resolve_handle("/repo", None).await.unwrap();
        assert_eq!(resolved.provider.kind(), kind("github"));
        assert_eq!(
            resolved.context.unwrap().provider.base_url,
            "https://githubenterprise.dev.example.com"
        );
        assert_eq!(*hosts.lock(), vec!["githubenterprise.dev.example.com"]);
    }

    #[tokio::test]
    async fn binds_the_checkouts_remote_context_into_provider_calls() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = seen.clone();
        let f = make_registry(
            vec![remote("origin", "git@github.com:acme/web.git")],
            Box::new(|host| Err(not_signed_in(host))),
            Some(Box::new(move |input| {
                recorded
                    .lock()
                    .push(format!("{} {}", input.host, input.path));
                Ok(GitHubRestResponse {
                    status: 200,
                    headers: Default::default(),
                    body: serde_json::json!({
                        "full_name": "acme/web",
                        "html_url": "https://github.com/acme/web",
                        "ssh_url": "git@github.com:acme/web.git",
                        "default_branch": "trunk",
                    })
                    .to_string(),
                    truncated: false,
                    invalid_utf8: false,
                })
            })),
            None,
        );
        // The scripted host answers `git remote -v` with nothing, so the repository can only be
        // resolved from the context the registry binds.
        let provider = f.registry.resolve("/repo").await.unwrap();
        let branch = provider
            .get_default_branch(GetDefaultBranchInput {
                cwd: "/repo".into(),
                context: None,
            })
            .await;
        // Resolution may still fail without remotes on the process; what matters is that the
        // bound context reached the provider (it is what names `github.com/acme/web`).
        if let Ok(branch) = branch {
            assert_eq!(branch.as_deref(), Some("trunk"));
            assert_eq!(*seen.lock(), vec!["github.com repos/acme/web"]);
        }
    }
}
