//! Port of upstream/apps/server/src/project/RepositoryIdentityResolver.ts

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use regex::Regex;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::environment::{
    GitRemoteSource, RepositoryIdentity, RepositoryIdentityLocator, RepositoryOrigin,
};
use turin_contracts::source_control::SourceControlProviderError;
use turin_git::process::{
    ProcessRunError, ProcessRunInput, ProcessRunOutput, ProcessRunner, TimeoutBehavior,
};
use turin_git::shared_git::{
    detect_source_control_provider_from_git_remote_url, normalize_git_remote_url,
};

use crate::ttl_cache::{Clock, TtlCache, system_clock};

const DEFAULT_REPOSITORY_IDENTITY_CACHE_CAPACITY: usize = 512;
// Background sweeps resolve every project each minute. A long TTL keeps them
// from spawning git each time. Clone, publish, and PR discovery (after a turn
// and before it saves links) resolve with `refresh: true`.
const DEFAULT_POSITIVE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
// Short, so a folder that gains a repository or a remote shows up quickly.
const DEFAULT_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(60);

static REMOTE_FETCH_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\S+)\s+(\S+)\s+\((fetch|push)\)$").expect("valid remote line regex")
});

/// The git executable seam. Upstream injects `ProcessRunner` as a service; the
/// port takes a trait object so tests can script `git` output.
#[async_trait]
pub trait GitCommandRunner: Send + Sync {
    async fn run(&self, input: ProcessRunInput) -> Result<ProcessRunOutput, ProcessRunError>;
}

#[async_trait]
impl GitCommandRunner for ProcessRunner {
    async fn run(&self, input: ProcessRunInput) -> Result<ProcessRunOutput, ProcessRunError> {
        ProcessRunner::run(self, input).await
    }
}

/// Upstream `refine`: enriches an identity (for example with a provider web
/// URL). A failure keeps the unrefined identity.
pub type IdentityRefiner = Arc<
    dyn Fn(
            RepositoryIdentity,
        ) -> BoxFuture<'static, Result<RepositoryIdentity, SourceControlProviderError>>
        + Send
        + Sync,
>;

pub struct RepositoryIdentityResolverOptions {
    pub cache_capacity: usize,
    pub positive_cache_ttl: Duration,
    pub negative_cache_ttl: Duration,
    pub refine: Option<IdentityRefiner>,
    pub clock: Clock,
}

impl Default for RepositoryIdentityResolverOptions {
    fn default() -> Self {
        Self {
            cache_capacity: DEFAULT_REPOSITORY_IDENTITY_CACHE_CAPACITY,
            positive_cache_ttl: DEFAULT_POSITIVE_CACHE_TTL,
            negative_cache_ttl: DEFAULT_NEGATIVE_CACHE_TTL,
            refine: None,
            clock: system_clock(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    pub refresh: bool,
}

/// Upstream `RepositoryIdentityResolver`. Two caches: cwd → git toplevel, and
/// toplevel → identity. A `None` result uses the negative TTL.
pub struct RepositoryIdentityResolver {
    runner: Arc<dyn GitCommandRunner>,
    refine: Option<IdentityRefiner>,
    positive_cache_ttl: Duration,
    negative_cache_ttl: Duration,
    repository_roots: TtlCache<String, Option<String>>,
    identities: TtlCache<String, Option<RepositoryIdentity>>,
}

impl RepositoryIdentityResolver {
    pub fn new(
        runner: Arc<dyn GitCommandRunner>,
        options: RepositoryIdentityResolverOptions,
    ) -> Self {
        Self {
            runner,
            refine: options.refine,
            positive_cache_ttl: options.positive_cache_ttl,
            negative_cache_ttl: options.negative_cache_ttl,
            repository_roots: TtlCache::new(options.cache_capacity, options.clock.clone()),
            identities: TtlCache::new(options.cache_capacity, options.clock),
        }
    }

    fn ttl_for<T>(&self, value: &Option<T>) -> Duration {
        if value.is_some() {
            self.positive_cache_ttl
        } else {
            self.negative_cache_ttl
        }
    }

    pub async fn resolve(&self, cwd: &str, options: ResolveOptions) -> Option<RepositoryIdentity> {
        if options.refresh {
            self.repository_roots.invalidate(&cwd.to_owned());
        }
        let cache_key = match self.repository_roots.get(&cwd.to_owned()) {
            Some(cache_key) => cache_key,
            None => {
                let cache_key = self.lookup_cache_key(cwd).await;
                let ttl = self.ttl_for(&cache_key);
                self.repository_roots
                    .insert(cwd.to_owned(), cache_key.clone(), ttl);
                cache_key
            }
        }?;
        if options.refresh {
            self.identities.invalidate(&cache_key);
        }
        match self.identities.get(&cache_key) {
            Some(identity) => identity,
            None => {
                let identity = self.lookup_identity(&cache_key).await;
                let ttl = self.ttl_for(&identity);
                self.identities.insert(cache_key, identity.clone(), ttl);
                identity
            }
        }
    }

    /// Git errors and timeouts resolve to `None`, like "no repository". Upstream
    /// `Effect.option` swallows them.
    async fn lookup_cache_key(&self, cwd: &str) -> Option<String> {
        let output = self
            .runner
            .run(git_input(&["-C", cwd, "rev-parse", "--show-toplevel"]))
            .await
            .ok()?;
        if output.code != Some(0) {
            return None;
        }
        let candidate = js_trim(&output.stdout);
        (!candidate.is_empty()).then(|| candidate.to_owned())
    }

    async fn lookup_identity(&self, cache_key: &str) -> Option<RepositoryIdentity> {
        let identity = self.lookup_from_cache_key(cache_key).await?;
        match &self.refine {
            None => Some(identity),
            Some(refine) => Some(refine(identity.clone()).await.unwrap_or(identity)),
        }
    }

    async fn lookup_from_cache_key(&self, cache_key: &str) -> Option<RepositoryIdentity> {
        let output = self
            .runner
            .run(git_input(&["-C", cache_key, "remote", "-v"]))
            .await
            .ok()?;
        if output.code != Some(0) {
            return None;
        }
        let remotes = parse_remote_fetch_urls(&output.stdout);
        let (remote_name, remote_url) = pick_primary_remote(&remotes)?;
        build_repository_identity(
            &remote_name,
            &remote_url,
            remotes.get("origin").map(String::as_str),
            cache_key,
        )
    }
}

fn git_input(args: &[&str]) -> ProcessRunInput {
    // git is a real executable on every platform: no shell, so paths with
    // spaces are passed through as single arguments.
    ProcessRunInput {
        timeout_behavior: Some(TimeoutBehavior::TimedOutResult),
        ..ProcessRunInput::new("git", args.iter().map(|arg| (*arg).to_owned()).collect())
    }
}

fn nonempty(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn parse_remote_fetch_urls(stdout: &str) -> IndexMap<String, String> {
    let mut remotes = IndexMap::new();
    for line in stdout.split('\n') {
        let trimmed = js_trim(line);
        if trimmed.is_empty() {
            continue;
        }
        let Some(captures) = REMOTE_FETCH_LINE.captures(trimmed) else {
            continue;
        };
        if &captures[3] != "fetch" {
            continue;
        }
        remotes.insert(captures[1].to_owned(), captures[2].to_owned());
    }
    remotes
}

fn pick_primary_remote(remotes: &IndexMap<String, String>) -> Option<(String, String)> {
    for preferred in ["upstream", "origin"] {
        if let Some(remote_url) = remotes.get(preferred) {
            return Some((preferred.to_owned(), remote_url.clone()));
        }
    }
    // Upstream sorts with localeCompare; byte order agrees for ASCII remote names.
    remotes
        .iter()
        .min_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(name, url)| (name.clone(), url.clone()))
}

fn repository_path_of(canonical_key: &str) -> String {
    canonical_key
        .split('/')
        .skip(1)
        .collect::<Vec<_>>()
        .join("/")
}

fn build_repository_origin(
    origin_url: Option<&str>,
    canonical_key: &str,
) -> Option<RepositoryOrigin> {
    let origin_url = origin_url.filter(|url| !url.is_empty())?;
    let origin_key = normalize_git_remote_url(origin_url);
    if origin_key == canonical_key {
        return None;
    }
    let display_name = repository_path_of(&origin_key);
    Some(RepositoryOrigin {
        canonical_key: nonempty(&origin_key)?,
        display_name: nonempty(&display_name),
    })
}

fn build_repository_identity(
    remote_name: &str,
    remote_url: &str,
    origin_url: Option<&str>,
    root_path: &str,
) -> Option<RepositoryIdentity> {
    let canonical_key = normalize_git_remote_url(remote_url);
    let provider = detect_source_control_provider_from_git_remote_url(remote_url);
    let repository_path = repository_path_of(&canonical_key);
    let segments: Vec<&str> = repository_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let origin = build_repository_origin(origin_url, &canonical_key);

    Some(RepositoryIdentity {
        canonical_key: nonempty(&canonical_key)?,
        locator: RepositoryIdentityLocator {
            source: GitRemoteSource,
            remote_name: nonempty(remote_name)?,
            remote_url: nonempty(remote_url)?,
        },
        web_url: None,
        root_path: nonempty(root_path),
        display_name: nonempty(&repository_path),
        provider: provider.and_then(|info| nonempty(info.kind.as_str())),
        owner: segments.first().and_then(|owner| nonempty(owner)),
        name: segments.last().and_then(|name| nonempty(name)),
        origin,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::future::BoxFuture;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::environment::RepositoryIdentity;
    use turin_contracts::source_control::{SourceControlProviderError, SourceControlProviderKind};
    use turin_git::process::{ProcessRunError, ProcessRunInput, ProcessRunOutput, ProcessRunner};

    use super::{
        GitCommandRunner, IdentityRefiner, RepositoryIdentityResolver,
        RepositoryIdentityResolverOptions, ResolveOptions,
    };
    use crate::ttl_cache::Clock;
    use crate::ttl_cache::test_support::TestClock;

    type Responder = Box<dyn Fn(&ProcessRunInput) -> ProcessRunOutput + Send + Sync>;

    /// Records every git invocation and answers from a script.
    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        respond: Responder,
    }

    impl FakeRunner {
        fn new(
            respond: impl Fn(&ProcessRunInput) -> ProcessRunOutput + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                respond: Box::new(respond),
            })
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().clone()
        }
    }

    #[async_trait]
    impl GitCommandRunner for FakeRunner {
        async fn run(&self, input: ProcessRunInput) -> Result<ProcessRunOutput, ProcessRunError> {
            self.calls.lock().push(input.args.clone());
            Ok((self.respond)(&input))
        }
    }

    fn output(stdout: &str, code: i32) -> ProcessRunOutput {
        ProcessRunOutput {
            stdout: stdout.to_owned(),
            stderr: String::new(),
            code: Some(code),
            timed_out: false,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_invalid_utf8: false,
            stderr_invalid_utf8: false,
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn is_rev_parse(input: &ProcessRunInput) -> bool {
        input.args.iter().any(|arg| arg == "rev-parse")
    }

    fn options(clock: Clock) -> RepositoryIdentityResolverOptions {
        RepositoryIdentityResolverOptions {
            clock,
            ..Default::default()
        }
    }

    fn root_path(identity: &Option<RepositoryIdentity>) -> Option<&str> {
        identity
            .as_ref()?
            .root_path
            .as_ref()
            .map(|path| path.as_str())
    }

    fn canonical_key(identity: &Option<RepositoryIdentity>) -> Option<&str> {
        identity
            .as_ref()
            .map(|identity| identity.canonical_key.as_str())
    }

    fn git(cwd: &Path, values: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(values)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {values:?} failed");
    }

    fn real_resolver(
        clock: Option<Clock>,
        negative_ms: u64,
        positive_ms: u64,
    ) -> RepositoryIdentityResolver {
        RepositoryIdentityResolver::new(
            Arc::new(ProcessRunner),
            RepositoryIdentityResolverOptions {
                cache_capacity: 16,
                negative_cache_ttl: Duration::from_millis(negative_ms),
                positive_cache_ttl: Duration::from_millis(positive_ms),
                clock: clock.unwrap_or_else(crate::ttl_cache::system_clock),
                ..Default::default()
            },
        )
    }

    #[derive(Default)]
    struct RemoteState {
        root: String,
        remote_url: String,
        refinements: usize,
        refinement_fails: bool,
    }

    fn remote_responder(state: Arc<Mutex<RemoteState>>) -> Arc<FakeRunner> {
        FakeRunner::new(move |input| {
            let state = state.lock();
            if is_rev_parse(input) {
                output(&format!("{}\n", state.root), 0)
            } else {
                output(&format!("origin\t{} (fetch)\n", state.remote_url), 0)
            }
        })
    }

    fn refiner(state: Arc<Mutex<RemoteState>>) -> IdentityRefiner {
        Arc::new(
            move |identity: RepositoryIdentity| -> BoxFuture<'static, _> {
                let fails = {
                    let mut state = state.lock();
                    state.refinements += 1;
                    state.refinement_fails
                };
                let root = state.lock().root.clone();
                Box::pin(async move {
                    if fails {
                        return Err(SourceControlProviderError::SourceControlProviderError {
                            provider: SourceControlProviderKind::new("forgejo")
                                .expect("valid kind"),
                            operation: "detectProvider".to_owned(),
                            cwd: root,
                            command: None,
                            repository: None,
                            reference: None,
                            detail: "account unavailable".to_owned(),
                            cause: None,
                        });
                    }
                    let mut identity = identity;
                    if identity
                        .canonical_key
                        .as_str()
                        .starts_with("ssh.forge.test/")
                    {
                        identity.provider = TrimmedNonEmptyString::new("forgejo").ok();
                        identity.web_url =
                            TrimmedNonEmptyString::new("http://forge.test:3000/git/team/repo").ok();
                    }
                    Ok(identity)
                })
            },
        )
    }

    #[tokio::test]
    async fn refreshes_the_git_root_only_when_requested() {
        let state = Arc::new(Mutex::new(RemoteState {
            root: "/repo".to_owned(),
            remote_url: "git@github.com:T3Tools/t3code.git".to_owned(),
            ..Default::default()
        }));
        let runner = remote_responder(Arc::clone(&state));
        let clock = TestClock::new();
        let resolver = RepositoryIdentityResolver::new(
            Arc::clone(&runner) as Arc<dyn GitCommandRunner>,
            RepositoryIdentityResolverOptions {
                refine: Some(refiner(Arc::clone(&state))),
                ..options(clock.clock())
            },
        );
        let web = "/repo/packages/web";

        let first = resolver.resolve(web, ResolveOptions::default()).await;
        state.lock().root = web.to_owned();
        // Longer than the one-minute cadence of the background sweeps.
        clock.adjust(Duration::from_secs(10 * 60));
        let second = resolver.resolve(web, ResolveOptions::default()).await;

        assert_eq!(canonical_key(&first), Some("github.com/t3tools/t3code"));
        assert_eq!(second, first);
        assert_eq!(state.lock().refinements, 1);
        assert_eq!(
            runner.calls(),
            vec![
                args(&["-C", web, "rev-parse", "--show-toplevel"]),
                args(&["-C", "/repo", "remote", "-v"]),
            ]
        );

        let refreshed = resolver
            .resolve(web, ResolveOptions { refresh: true })
            .await;
        assert_eq!(root_path(&refreshed), Some(web));
        assert_eq!(
            resolver.resolve(web, ResolveOptions::default()).await,
            refreshed
        );
        assert_eq!(
            runner.calls()[2..].to_vec(),
            vec![
                args(&["-C", web, "rev-parse", "--show-toplevel"]),
                args(&["-C", web, "remote", "-v"]),
            ]
        );

        state.lock().remote_url = "git@ssh.forge.test:team/repo.git".to_owned();
        let forgejo = resolver
            .resolve(web, ResolveOptions { refresh: true })
            .await;
        let forgejo_identity = forgejo.clone().expect("identity");
        assert_eq!(
            forgejo_identity.web_url.as_ref().map(|url| url.as_str()),
            Some("http://forge.test:3000/git/team/repo")
        );
        assert_eq!(
            forgejo_identity
                .provider
                .as_ref()
                .map(|value| value.as_str()),
            Some("forgejo")
        );
        assert_eq!(canonical_key(&forgejo), Some("ssh.forge.test/team/repo"));
        assert_eq!(
            forgejo_identity.locator.remote_url.as_str(),
            "git@ssh.forge.test:team/repo.git"
        );
        assert_eq!(
            resolver.resolve(web, ResolveOptions::default()).await,
            forgejo
        );
        assert_eq!(state.lock().refinements, 3);

        state.lock().refinement_fails = true;
        let unavailable = resolver
            .resolve(web, ResolveOptions { refresh: true })
            .await;
        assert!(unavailable.as_ref().expect("identity").web_url.is_none());
        assert_eq!(
            canonical_key(&unavailable),
            Some("ssh.forge.test/team/repo")
        );
    }

    #[tokio::test]
    async fn retries_git_root_discovery_after_the_negative_ttl() {
        let root_attempts = Arc::new(Mutex::new(0usize));
        let attempts = Arc::clone(&root_attempts);
        let runner = FakeRunner::new(move |input| {
            if is_rev_parse(input) {
                let mut attempts = attempts.lock();
                let failed = *attempts == 0;
                *attempts += 1;
                return if failed {
                    output("", 1)
                } else {
                    output("/repo\n", 0)
                };
            }
            output("origin\tgit@github.com:T3Tools/t3code.git (fetch)\n", 0)
        });
        let clock = TestClock::new();
        let resolver = RepositoryIdentityResolver::new(
            Arc::clone(&runner) as Arc<dyn GitCommandRunner>,
            options(clock.clock()),
        );
        let web = "/repo/packages/web";

        assert_eq!(resolver.resolve(web, ResolveOptions::default()).await, None);
        assert_eq!(resolver.resolve(web, ResolveOptions::default()).await, None);

        clock.adjust(Duration::from_secs(60));
        let recovered = resolver.resolve(web, ResolveOptions::default()).await;
        assert_eq!(root_path(&recovered), Some("/repo"));
        assert_eq!(
            runner.calls(),
            vec![
                args(&["-C", web, "rev-parse", "--show-toplevel"]),
                args(&["-C", web, "rev-parse", "--show-toplevel"]),
                args(&["-C", "/repo", "remote", "-v"]),
            ]
        );
    }

    #[tokio::test]
    async fn normalizes_equivalent_github_remotes_into_a_stable_repository_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path();
        git(cwd, &["init", "-q"]);
        git(
            cwd,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:T3Tools/t3code.git",
            ],
        );

        let identity = real_resolver(None, 60_000, 900_000)
            .resolve(&cwd.to_string_lossy(), ResolveOptions::default())
            .await
            .expect("identity");

        assert_eq!(
            std::fs::canonicalize(&identity.root_path.as_ref().expect("root").as_str()).ok(),
            std::fs::canonicalize(cwd).ok()
        );
        assert_eq!(identity.canonical_key.as_str(), "github.com/t3tools/t3code");
        assert_eq!(
            identity.display_name.as_ref().map(|v| v.as_str()),
            Some("t3tools/t3code")
        );
        assert_eq!(
            identity.provider.as_ref().map(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(identity.owner.as_ref().map(|v| v.as_str()), Some("t3tools"));
        assert_eq!(identity.name.as_ref().map(|v| v.as_str()), Some("t3code"));
    }

    #[tokio::test]
    async fn returns_the_git_top_level_root_path_when_resolving_from_a_nested_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("packages").join("web");
        std::fs::create_dir_all(&nested).expect("nested dir");
        git(temp.path(), &["init", "-q"]);
        git(
            temp.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:T3Tools/t3code.git",
            ],
        );

        let identity = real_resolver(None, 60_000, 900_000)
            .resolve(&nested.to_string_lossy(), ResolveOptions::default())
            .await
            .expect("identity");

        assert_eq!(identity.canonical_key.as_str(), "github.com/t3tools/t3code");
        assert_eq!(
            std::fs::canonicalize(identity.root_path.as_ref().expect("root").as_str()).ok(),
            std::fs::canonicalize(temp.path()).ok()
        );
    }

    #[tokio::test]
    async fn returns_null_for_non_git_folders_and_repos_without_remotes() {
        let non_git = tempfile::tempdir().expect("tempdir");
        let no_remote = tempfile::tempdir().expect("tempdir");
        git(no_remote.path(), &["init", "-q"]);
        let resolver = real_resolver(None, 60_000, 900_000);

        assert_eq!(
            resolver
                .resolve(&non_git.path().to_string_lossy(), ResolveOptions::default())
                .await,
            None
        );
        assert_eq!(
            resolver
                .resolve(
                    &no_remote.path().to_string_lossy(),
                    ResolveOptions::default()
                )
                .await,
            None
        );
    }

    #[tokio::test]
    async fn refreshes_the_primary_upstream_after_a_change_before_cache_expiry() {
        for change in ["add", "replace"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let cwd = temp.path();
            let cwd_str = cwd.to_string_lossy().into_owned();
            git(cwd, &["init", "-q"]);
            git(
                cwd,
                &[
                    "remote",
                    "add",
                    "origin",
                    "git@github.com:julius/t3code.git",
                ],
            );
            if change == "replace" {
                git(
                    cwd,
                    &[
                        "remote",
                        "add",
                        "upstream",
                        "git@github.com:T3Tools/previous.git",
                    ],
                );
            }

            let resolver = real_resolver(None, 60_000, 900_000);
            let initial = resolver.resolve(&cwd_str, ResolveOptions::default()).await;
            assert_eq!(
                canonical_key(&initial),
                Some(if change == "add" {
                    "github.com/julius/t3code"
                } else {
                    "github.com/t3tools/previous"
                })
            );

            git(
                cwd,
                &[
                    "remote",
                    if change == "add" { "add" } else { "set-url" },
                    "upstream",
                    "git@github.com:T3Tools/t3code.git",
                ],
            );
            assert_eq!(
                resolver.resolve(&cwd_str, ResolveOptions::default()).await,
                initial
            );
            let identity = resolver
                .resolve(&cwd_str, ResolveOptions { refresh: true })
                .await;

            let identity_ref = identity.as_ref().expect("identity");
            assert_eq!(identity_ref.locator.remote_name.as_str(), "upstream");
            assert_eq!(
                identity_ref.canonical_key.as_str(),
                "github.com/t3tools/t3code"
            );
            assert_eq!(
                identity_ref.display_name.as_ref().map(|v| v.as_str()),
                Some("t3tools/t3code")
            );
            assert_eq!(
                resolver.resolve(&cwd_str, ResolveOptions::default()).await,
                identity
            );
        }
    }

    #[tokio::test]
    async fn reports_a_forks_own_remote_as_origin_next_to_the_upstream_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path();
        git(cwd, &["init", "-q"]);
        git(
            cwd,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:julius/t3code-fork.git",
            ],
        );
        git(
            cwd,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:T3Tools/t3code.git",
            ],
        );

        let identity = real_resolver(None, 60_000, 900_000)
            .resolve(&cwd.to_string_lossy(), ResolveOptions::default())
            .await
            .expect("identity");

        assert_eq!(identity.canonical_key.as_str(), "github.com/t3tools/t3code");
        assert_eq!(
            identity.display_name.as_ref().map(|v| v.as_str()),
            Some("t3tools/t3code")
        );
        let origin = identity.origin.expect("origin");
        assert_eq!(
            origin.canonical_key.as_str(),
            "github.com/julius/t3code-fork"
        );
        assert_eq!(
            origin.display_name.as_ref().map(|v| v.as_str()),
            Some("julius/t3code-fork")
        );
    }

    #[tokio::test]
    async fn uses_the_last_remote_path_segment_as_the_repository_name_for_nested_groups() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path();
        git(cwd, &["init", "-q"]);
        git(
            cwd,
            &[
                "remote",
                "add",
                "origin",
                "git@gitlab.com:T3Tools/platform/t3code.git",
            ],
        );

        let identity = real_resolver(None, 60_000, 900_000)
            .resolve(&cwd.to_string_lossy(), ResolveOptions::default())
            .await
            .expect("identity");

        assert_eq!(
            identity.canonical_key.as_str(),
            "gitlab.com/t3tools/platform/t3code"
        );
        assert_eq!(
            identity.display_name.as_ref().map(|v| v.as_str()),
            Some("t3tools/platform/t3code")
        );
        assert_eq!(identity.owner.as_ref().map(|v| v.as_str()), Some("t3tools"));
        assert_eq!(identity.name.as_ref().map(|v| v.as_str()), Some("t3code"));
    }

    #[tokio::test]
    async fn keeps_null_identities_cached_until_the_negative_ttl_expires() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path();
        let cwd_str = cwd.to_string_lossy().into_owned();
        git(cwd, &["init", "-q"]);

        let clock = TestClock::new();
        let resolver = real_resolver(Some(clock.clock()), 50, 1_000);
        assert_eq!(
            resolver.resolve(&cwd_str, ResolveOptions::default()).await,
            None
        );

        git(
            cwd,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:T3Tools/t3code.git",
            ],
        );
        for _ in 0..3 {
            assert_eq!(
                resolver.resolve(&cwd_str, ResolveOptions::default()).await,
                None
            );
        }

        clock.adjust(Duration::from_millis(120));
        let refreshed = resolver.resolve(&cwd_str, ResolveOptions::default()).await;
        assert_eq!(canonical_key(&refreshed), Some("github.com/t3tools/t3code"));
        assert_eq!(
            refreshed
                .and_then(|identity| identity.name)
                .map(|v| v.as_str().to_owned()),
            Some("t3code".to_owned())
        );
    }

    #[tokio::test]
    async fn refreshes_cached_identities_after_the_positive_ttl_when_a_remote_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path();
        let cwd_str = cwd.to_string_lossy().into_owned();
        git(cwd, &["init", "-q"]);
        git(
            cwd,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:T3Tools/t3code.git",
            ],
        );

        let clock = TestClock::new();
        let resolver = real_resolver(Some(clock.clock()), 50, 100);
        let initial = resolver.resolve(&cwd_str, ResolveOptions::default()).await;
        assert_eq!(canonical_key(&initial), Some("github.com/t3tools/t3code"));

        git(
            cwd,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:T3Tools/t3code-next.git",
            ],
        );
        let cached = resolver.resolve(&cwd_str, ResolveOptions::default()).await;
        assert_eq!(canonical_key(&cached), Some("github.com/t3tools/t3code"));

        clock.adjust(Duration::from_millis(180));
        let refreshed = resolver.resolve(&cwd_str, ResolveOptions::default()).await;
        let refreshed = refreshed.expect("identity");
        assert_eq!(
            refreshed.canonical_key.as_str(),
            "github.com/t3tools/t3code-next"
        );
        assert_eq!(
            refreshed.display_name.as_ref().map(|v| v.as_str()),
            Some("t3tools/t3code-next")
        );
        assert_eq!(
            refreshed.name.as_ref().map(|v| v.as_str()),
            Some("t3code-next")
        );
    }
}
