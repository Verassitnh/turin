//! Port of upstream/apps/server/src/sourceControl/SourceControlRepositoryService.ts
//!
//! Looks up, prepares, clones, discards and publishes repositories through the source control
//! provider for the repository's host. Upstream reads its services from the Effect context;
//! here they are explicit: the provider registry, the git operations it needs ([`RepositoryGit`]),
//! the server's working directory and the user's home directory.
//!
//! Deviations (see PORTING.md): git is narrowed to the four operations this service calls, so
//! the git crate's driver implements [`RepositoryGit`] rather than this module depending on it.
//! Bitbucket's locator error is not mapped (Bitbucket is cut). Progress is a plain callback, not
//! an Effect. The git error's cause is its message with credentials redacted, not the full
//! `GitCommandError`.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{Value, json};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::source_control::{
    SourceControlCloneProtocol, SourceControlCloneRepositoryInput,
    SourceControlCloneRepositoryResult, SourceControlProviderError, SourceControlProviderKind,
    SourceControlPublishRepositoryInput, SourceControlPublishRepositoryResult,
    SourceControlPublishStatus, SourceControlRepositoryCloneUrls, SourceControlRepositoryError,
    SourceControlRepositoryInfo, SourceControlRepositoryLookupInput,
};

use crate::shared::git_clone_progress::{GitCloneProgressLine, parse_git_clone_progress_line};
use crate::shared::path_expansion::expand_home_path;
use crate::source_control::provider_registry::SourceControlProviderRegistry;
use crate::source_control_core::Cause;
use crate::source_control_core::source_control_provider::{
    CreateRepositoryInput, GetRepositoryCloneUrlsInput,
};

/// The synchronous RPC (older clients, mobile) keeps a deadline: nothing else tells the user a
/// clone stalled. The tracked path disables it and relies on progress and Cancel instead.
const CLONE_TIMEOUT: Duration = Duration::from_millis(120_000);
/// Retry budget for removing a partial clone: `Schedule.spaced("200 millis")`, five times.
const DISCARD_RETRIES: usize = 5;
const DISCARD_RETRY_SPACING: Duration = Duration::from_millis(200);

/// Git is given `--progress`-style env so the counters flow and a missing tty fails instead of
/// prompting. Upstream's `CLONE_ENV`.
const CLONE_ENV: [(&str, &str); 3] = [
    // `--progress` forces the transfer counters through the pipe; the delay env makes the
    // checkout counter start immediately. No tty means a credential prompt would hang forever,
    // so tell git to fail instead.
    ("GIT_PROGRESS_DELAY", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("LC_ALL", "C"),
];

const UNKNOWN_PROVIDER: &str = "unknown";
const GENERIC_DETAIL: &str = "The source control operation could not be completed.";

/// The git operations this service calls, narrowed from upstream's `GitVcsDriver`.
#[async_trait]
pub trait RepositoryGit: Send + Sync {
    /// Runs `git <args>` in `command.cwd`. Errors carry the failure; output is not returned.
    async fn execute(&self, command: GitCommand) -> Result<(), Cause>;
    /// Adds `url` as `preferred_name` (or the first free name) and returns the name used.
    async fn ensure_remote(
        &self,
        cwd: &str,
        preferred_name: &str,
        url: &str,
    ) -> Result<String, Cause>;
    /// The current branch from `git status`, or `None` when git cannot say.
    async fn status_branch(&self, cwd: &str) -> Result<Option<String>, Cause>;
    /// Pushes the current branch to `remote_name`, with upstream tracking.
    async fn push_current_branch(
        &self,
        cwd: &str,
        remote_name: &str,
    ) -> Result<PushedBranch, Cause>;
}

#[derive(Clone)]
pub struct GitCommand {
    pub operation: &'static str,
    pub cwd: String,
    pub args: Vec<String>,
    /// `None` disables the deadline.
    pub timeout: Option<Duration>,
    pub env: Vec<(&'static str, &'static str)>,
    /// Receives each stderr line as git writes it; progress redraws arrive as separate lines.
    pub on_stderr_line: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedBranch {
    pub branch: String,
    pub upstream_branch: Option<String>,
}

/// Everything `clone_repository` checks before running git: the resolved remote, the normalized
/// destination, and that the destination is empty. Lets a caller create the project first.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceControlPreparedClone {
    pub destination_path: String,
    /// Credential-free; safe to show and to store in snapshots.
    pub remote_url: String,
    /// What git is given; may carry embedded credentials.
    pub clone_url: String,
    pub repository: Option<SourceControlRepositoryInfo>,
}

/// How long a clone may run. Upstream's `timeoutMs` is `undefined` (the default), a number, or
/// `null` (disabled).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CloneDeadline {
    #[default]
    Default,
    Disabled,
    After(Duration),
}

#[derive(Clone, Default)]
pub struct SourceControlCloneOptions {
    pub on_progress: Option<Arc<dyn Fn(GitCloneProgressLine) + Send + Sync>>,
    pub deadline: CloneDeadline,
}

pub struct SourceControlRepositoryService {
    /// `ServerConfig.cwd`: the default working directory for lookups.
    config_cwd: String,
    /// `HostProcess.HomeDirectory`: what `~` expands to in a destination path.
    home: String,
    providers: Arc<SourceControlProviderRegistry>,
    git: Arc<dyn RepositoryGit>,
}

type ServiceResult<T> = Result<T, SourceControlRepositoryError>;

fn repository_error(
    operation: &str,
    provider: &str,
    detail: &str,
    cause: Option<Value>,
) -> SourceControlRepositoryError {
    SourceControlRepositoryError::SourceControlRepositoryError {
        provider: provider_kind(provider),
        operation: operation.to_owned(),
        detail: detail.to_owned(),
        cause,
    }
}

fn provider_kind(value: &str) -> SourceControlProviderKind {
    SourceControlProviderKind::new(value).expect("a provider kind is a non-empty slug")
}

fn unknown_provider() -> SourceControlProviderKind {
    provider_kind(UNKNOWN_PROVIDER)
}

/// What a provider failure becomes at the repository boundary: upstream's `mapRepositoryError`,
/// with the Bitbucket locator detail cut along with Bitbucket.
fn provider_failure(
    operation: &str,
    provider: &SourceControlProviderKind,
    error: SourceControlProviderError,
) -> SourceControlRepositoryError {
    SourceControlRepositoryError::SourceControlRepositoryError {
        provider: provider.clone(),
        operation: operation.to_owned(),
        detail: GENERIC_DETAIL.to_owned(),
        cause: serde_json::to_value(&error).ok(),
    }
}

/// A git or filesystem failure that surfaces with a fixed detail and the cause's message.
fn cause_failure(
    operation: &str,
    provider: &SourceControlProviderKind,
    detail: &str,
    cause: &(dyn std::error::Error + 'static),
) -> SourceControlRepositoryError {
    SourceControlRepositoryError::SourceControlRepositoryError {
        provider: provider.clone(),
        operation: operation.to_owned(),
        detail: detail.to_owned(),
        cause: Some(json!({ "message": redact_url_credentials(&cause.to_string()) })),
    }
}

/// The URL clients see. A pasted `https://user:token@host/…` must not travel back over
/// `subscribeProjectClones` to every reader; git still gets the original.
fn redact_remote_url(remote_url: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(remote_url) else {
        return remote_url.to_owned();
    };
    // Clone URLs have no legitimate query; when one is present it is a token.
    let has_credentials = !url.username().is_empty() || url.password().is_some();
    let has_query = url.query().is_some_and(|query| !query.is_empty());
    if !has_credentials && !has_query {
        return remote_url.to_owned();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.to_string()
}

// Userinfo may itself contain `@`; everything up to the last one before the host boundary goes.
static URL_WITH_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/]+@").unwrap());

/// Drops `user:token@` from any URL embedded in free text.
fn redact_url_credentials(text: &str) -> String {
    URL_WITH_USERINFO.replace_all(text, "$1").into_owned()
}

fn select_remote_url<'a>(
    url: &'a str,
    ssh_url: &'a str,
    protocol: Option<SourceControlCloneProtocol>,
) -> &'a str {
    match protocol {
        Some(SourceControlCloneProtocol::Https) => url,
        // `ssh` and `auto` (the default) both take the SSH URL.
        Some(SourceControlCloneProtocol::Ssh) | Some(SourceControlCloneProtocol::Auto) | None => {
            ssh_url
        }
    }
}

fn to_repository_info(
    provider: &SourceControlProviderKind,
    urls: SourceControlRepositoryCloneUrls,
) -> SourceControlRepositoryInfo {
    SourceControlRepositoryInfo {
        provider: provider.clone(),
        name_with_owner: urls.name_with_owner,
        url: urls.url,
        ssh_url: urls.ssh_url,
    }
}

/// `path.resolve` without the filesystem: relative paths join the process cwd, and `.` and `..`
/// are folded away.
fn resolve_lexically(value: &str) -> String {
    let joined = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        std::env::current_dir().unwrap_or_default().join(value)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved.to_string_lossy().into_owned()
}

/// What `prepare_destination` learns about a clone destination.
struct PreparedDestination {
    destination_path: String,
    parent_path: String,
}

impl SourceControlRepositoryService {
    pub fn new(
        config_cwd: impl Into<String>,
        home: impl Into<String>,
        providers: Arc<SourceControlProviderRegistry>,
        git: Arc<dyn RepositoryGit>,
    ) -> Self {
        Self {
            config_cwd: config_cwd.into(),
            home: home.into(),
            providers,
            git,
        }
    }

    /// Resolves `repository` on `input.provider`, in `input.cwd` or the server's cwd.
    pub async fn lookup_repository(
        &self,
        input: SourceControlRepositoryLookupInput,
    ) -> ServiceResult<SourceControlRepositoryInfo> {
        let cwd = input
            .cwd
            .as_ref()
            .map_or_else(|| self.config_cwd.clone(), |cwd| cwd.as_str().to_owned());
        self.lookup_in(&input.provider, input.repository.as_str(), cwd)
            .await
    }

    async fn lookup_in(
        &self,
        provider_kind: &SourceControlProviderKind,
        repository: &str,
        cwd: String,
    ) -> ServiceResult<SourceControlRepositoryInfo> {
        self.ensure_concrete_provider("lookupRepository", provider_kind)?;
        let provider = self.providers.get(provider_kind);
        let urls = provider
            .get_repository_clone_urls(GetRepositoryCloneUrlsInput {
                cwd,
                context: None,
                repository: repository.trim().to_owned(),
            })
            .await
            .map_err(|error| provider_failure("lookupRepository", provider_kind, error))?;
        Ok(to_repository_info(provider_kind, urls))
    }

    fn ensure_concrete_provider(
        &self,
        operation: &str,
        provider: &SourceControlProviderKind,
    ) -> ServiceResult<()> {
        if provider.as_ref() != UNKNOWN_PROVIDER {
            return Ok(());
        }
        Err(repository_error(
            operation,
            UNKNOWN_PROVIDER,
            "Choose a source control provider before continuing.",
            None,
        ))
    }

    async fn normalize_destination(&self, destination_path: &str) -> ServiceResult<String> {
        let trimmed = destination_path.trim();
        if trimmed.is_empty() {
            return Err(repository_error(
                "cloneRepository",
                UNKNOWN_PROVIDER,
                "Choose a destination path before cloning.",
                None,
            ));
        }
        Ok(resolve_lexically(&expand_home_path(trimmed, &self.home)))
    }

    async fn prepare_destination(
        &self,
        destination_path: &str,
    ) -> ServiceResult<PreparedDestination> {
        let destination = self.normalize_destination(destination_path).await?;
        let path = Path::new(&destination);
        let exists = tokio::fs::try_exists(path).await.unwrap_or(false);
        if exists {
            let mut entries = tokio::fs::read_dir(path).await.map_err(|cause| {
                cause_failure(
                    "cloneRepository",
                    &unknown_provider(),
                    "Destination path already exists and is not a directory.",
                    &cause,
                )
            })?;
            let has_entries = entries
                .next_entry()
                .await
                .map_err(|cause| {
                    cause_failure(
                        "cloneRepository",
                        &unknown_provider(),
                        "Destination path already exists and is not a directory.",
                        &cause,
                    )
                })?
                .is_some();
            if has_entries {
                return Err(repository_error(
                    "cloneRepository",
                    UNKNOWN_PROVIDER,
                    "Destination path already exists and is not empty.",
                    None,
                ));
            }
        } else if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|cause| {
                cause_failure(
                    "cloneRepository",
                    &unknown_provider(),
                    GENERIC_DETAIL,
                    &cause,
                )
            })?;
        }
        Ok(PreparedDestination {
            parent_path: path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default(),
            destination_path: destination,
        })
    }

    /// Everything `clone_repository` checks before running git.
    pub async fn prepare_clone(
        &self,
        input: SourceControlCloneRepositoryInput,
    ) -> ServiceResult<SourceControlPreparedClone> {
        let prepared = self
            .prepare_destination(input.destination_path.as_str())
            .await?;
        let mut repository = None;
        let mut remote_url = input.remote_url.as_ref().map(|url| url.as_str().to_owned());
        let mut provider = input.provider.clone().unwrap_or_else(unknown_provider);

        if let (Some(kind), Some(repo)) = (&input.provider, &input.repository) {
            let info = self
                .lookup_in(kind, repo.as_str(), prepared.parent_path.clone())
                .await?;
            remote_url = Some(
                select_remote_url(info.url.as_str(), info.ssh_url.as_str(), input.protocol)
                    .to_owned(),
            );
            repository = Some(info);
            provider = kind.clone();
        }

        let Some(remote_url) = remote_url.filter(|url| !url.is_empty()) else {
            return Err(repository_error(
                "cloneRepository",
                provider.as_ref(),
                "Enter a repository path or clone URL before cloning.",
                None,
            ));
        };

        Ok(SourceControlPreparedClone {
            destination_path: prepared.destination_path,
            remote_url: redact_remote_url(&remote_url),
            clone_url: remote_url,
            repository,
        })
    }

    /// Clones with `git clone --progress`. The stderr tail is what explains a failure, so it is
    /// kept for the error detail.
    pub async fn clone_repository(
        &self,
        input: SourceControlCloneRepositoryInput,
        options: SourceControlCloneOptions,
    ) -> ServiceResult<SourceControlCloneRepositoryResult> {
        let prepared = self.prepare_clone(input.clone()).await?;
        let provider = input.provider.clone().unwrap_or_else(unknown_provider);
        // Git interleaves progress redraws with its real messages on stderr. The last
        // non-progress lines are what explain a failure ("Repository not found", "Permission
        // denied"), so keep them for the error detail.
        let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let on_progress = options.on_progress.clone();
        let tail = stderr_tail.clone();
        let on_stderr_line: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
            if let Some(parsed) = parse_git_clone_progress_line(line) {
                if let Some(on_progress) = &on_progress {
                    on_progress(parsed);
                }
                return;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("Cloning into") {
                return;
            }
            // Git echoes the remote in some failures; the tail becomes user-facing text.
            let mut tail = tail.lock();
            tail.push(redact_url_credentials(trimmed));
            if tail.len() > 4 {
                tail.remove(0);
            }
        });
        let destination = Path::new(&prepared.destination_path);
        let command = GitCommand {
            operation: "SourceControlRepositoryService.cloneRepository",
            cwd: destination
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default(),
            args: vec![
                "clone".to_owned(),
                "--progress".to_owned(),
                "--".to_owned(),
                prepared.clone_url.clone(),
                destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ],
            timeout: match options.deadline {
                CloneDeadline::Default => Some(CLONE_TIMEOUT),
                CloneDeadline::Disabled => None,
                CloneDeadline::After(duration) => Some(duration),
            },
            env: CLONE_ENV.to_vec(),
            on_stderr_line: Some(on_stderr_line),
        };
        if let Err(cause) = self.git.execute(command).await {
            let tail = stderr_tail.lock();
            let detail = if tail.is_empty() {
                "The repository could not be cloned.".to_owned()
            } else {
                tail.join(" ")
            };
            return Err(cause_failure(
                "cloneRepository",
                &provider,
                &detail,
                cause.as_ref(),
            ));
        }

        Ok(SourceControlCloneRepositoryResult {
            cwd: TrimmedNonEmptyString::new(&prepared.destination_path)
                .expect("a normalized destination is non-empty"),
            remote_url: TrimmedNonEmptyString::new(&prepared.remote_url)
                .expect("a clone remote is non-empty"),
            repository: prepared.repository,
        })
    }

    /// Removes a partial or failed clone so the destination is empty again.
    pub async fn discard_clone(&self, destination_path: &str) -> ServiceResult<()> {
        let normalized = self.normalize_destination(destination_path).await?;
        let path = Path::new(&normalized);
        // Only what git left behind may go. The destination was empty when the clone started,
        // so anything without a `.git` inside was put there by someone else since; refuse
        // rather than delete their files. A missing destination is already discarded; any other
        // read failure (a file in its place, permissions) is not something to remove through.
        let entries = match tokio::fs::read_dir(path).await {
            Ok(mut dir) => {
                let mut names = Vec::new();
                while let Some(entry) = dir.next_entry().await.map_err(|cause| {
                    cause_failure(
                        "discardClone",
                        &unknown_provider(),
                        "The clone destination could not be inspected.",
                        &cause,
                    )
                })? {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
                names
            }
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(cause) => {
                return Err(cause_failure(
                    "discardClone",
                    &unknown_provider(),
                    "The clone destination could not be inspected.",
                    &cause,
                ));
            }
        };
        if !entries.is_empty() && !entries.iter().any(|name| name == ".git") {
            return Err(repository_error(
                "discardClone",
                UNKNOWN_PROVIDER,
                "Destination path contains files that are not from the clone.",
                None,
            ));
        }
        // The directory itself is the project's workspace root and must stay; only git's partial
        // contents go. An interrupted git may still be closing files, so removal retries briefly.
        let mut attempt = 0;
        loop {
            match remove_and_recreate(path).await {
                Ok(()) => return Ok(()),
                Err(cause) if attempt >= DISCARD_RETRIES => {
                    return Err(cause_failure(
                        "discardClone",
                        &unknown_provider(),
                        "The partial clone could not be removed.",
                        &cause,
                    ));
                }
                Err(_) => {
                    attempt += 1;
                    tokio::time::sleep(DISCARD_RETRY_SPACING).await;
                }
            }
        }
    }

    /// Creates the remote repository, wires it up as `remote_name`, and pushes the current branch.
    /// An empty local repo (no commits) is a partial success: the remote exists, nothing is pushed.
    pub async fn publish_repository(
        &self,
        input: SourceControlPublishRepositoryInput,
    ) -> ServiceResult<SourceControlPublishRepositoryResult> {
        self.ensure_concrete_provider("publishRepository", &input.provider)?;
        let provider = self.providers.get(&input.provider);
        let cwd = input.cwd.as_str().to_owned();
        let urls = provider
            .create_repository(CreateRepositoryInput {
                cwd: cwd.clone(),
                repository: input.repository.as_str().trim().to_owned(),
                visibility: input.visibility,
            })
            .await
            .map_err(|error| provider_failure("publishRepository", &input.provider, error))?;
        let remote_url =
            select_remote_url(urls.url.as_str(), urls.ssh_url.as_str(), input.protocol).to_owned();
        let preferred_name = input
            .remote_name
            .as_ref()
            .map(|name| name.as_str().trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "origin".to_owned());
        let remote_name = self
            .git
            .ensure_remote(&cwd, &preferred_name, &remote_url)
            .await
            .map_err(|cause| {
                cause_failure(
                    "publishRepository",
                    &input.provider,
                    GENERIC_DETAIL,
                    cause.as_ref(),
                )
            })?;

        // An empty local repo (no commits) would make `git push HEAD:...` fail with an opaque
        // "src refspec HEAD does not match any". Treat this as a partial success: the remote was
        // created and wired up, but there is nothing to push yet.
        let has_commits = self
            .git
            .execute(GitCommand {
                operation: "SourceControlRepositoryService.publishRepository.headCheck",
                cwd: cwd.clone(),
                args: vec![
                    "rev-parse".to_owned(),
                    "--verify".to_owned(),
                    "HEAD".to_owned(),
                ],
                timeout: None,
                env: Vec::new(),
                on_stderr_line: None,
            })
            .await
            .is_ok();
        let repository = to_repository_info(&input.provider, urls);
        let remote_name =
            TrimmedNonEmptyString::new(&remote_name).expect("ensure_remote names a remote");
        let remote_url = TrimmedNonEmptyString::new(&remote_url).expect("a clone URL is non-empty");

        if !has_commits {
            let branch = self
                .git
                .status_branch(&cwd)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "main".to_owned());
            return Ok(SourceControlPublishRepositoryResult {
                repository,
                remote_name,
                remote_url,
                branch: TrimmedNonEmptyString::new(&branch).expect("a branch name is non-empty"),
                upstream_branch: None,
                status: SourceControlPublishStatus::RemoteAdded,
            });
        }

        let pushed = self
            .git
            .push_current_branch(&cwd, remote_name.as_str())
            .await
            .map_err(|cause| {
                cause_failure(
                    "publishRepository",
                    &input.provider,
                    GENERIC_DETAIL,
                    cause.as_ref(),
                )
            })?;
        Ok(SourceControlPublishRepositoryResult {
            repository,
            remote_name,
            remote_url,
            branch: TrimmedNonEmptyString::new(&pushed.branch).expect("a branch name is non-empty"),
            upstream_branch: pushed.upstream_branch.map(|branch| {
                TrimmedNonEmptyString::new(branch).expect("a branch name is non-empty")
            }),
            status: SourceControlPublishStatus::Pushed,
        })
    }
}

/// Removes everything at `path` and creates it again empty (`remove(force) then makeDirectory`).
async fn remove_and_recreate(path: &Path) -> Result<(), std::io::Error> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {}
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {}
        Err(cause) => return Err(cause),
    }
    tokio::fs::create_dir_all(path).await
}
