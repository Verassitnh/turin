//! Port of upstream/packages/source-control-core/src/server/SourceControlProvider.ts

use std::sync::LazyLock;

use async_trait::async_trait;
use futures::future::BoxFuture;
use regex::Regex;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::source_control::{
    ChangeRequest, ChangeRequestState, SourceControlProviderError, SourceControlProviderInfo,
    SourceControlProviderKind, SourceControlRepositoryCloneUrls, SourceControlRepositoryVisibility,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceControlLinkSubject {
    pub title: String,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolveSourceControlLinkInput {
    pub cwd: String,
    pub url: reqwest::Url,
}

/// `Effect | undefined`: `None` synchronously for unsupported URLs, without starting a lookup.
pub type ResolveSourceControlLinkFuture<'a> =
    Option<BoxFuture<'a, Result<SourceControlLinkSubject, SourceControlProviderError>>>;

#[derive(Debug, Clone, PartialEq)]
pub struct SourceControlProviderContext {
    pub provider: SourceControlProviderInfo,
    pub remote_name: String,
    pub remote_url: String,
    /// An explicit web authority can disambiguate Forgejo logins sharing an SSH alias.
    pub requested_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceControlRefSelector {
    pub ref_name: String,
    pub owner: Option<String>,
    pub repository: Option<String>,
}

const MAX_ERROR_TRANSPORT_VALUE_LENGTH: usize = 256;

static WHITESPACE_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static OWNER_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([^:/\s]+):(.+)$").unwrap());
static REMOTE_URL_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:[^@/\s]+@[^:/\s]+:|(?:ssh|https?|git)://[^/]+/)((?:[^/\s]+/)+[^/\s]+?)(?:\.git)?/?$",
    )
    .unwrap()
});

/// JS `String.prototype.trim`.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// Sanitizes user-provided source-control identifiers before attaching them to
/// contract errors. This is intentionally narrower than request validation: it
/// only strips URL secrets and bounds diagnostic values sent over transport.
pub fn transport_safe_source_control_error_value(value: &str) -> String {
    let printable: String = value
        .chars()
        .map(|c| {
            let code = c as u32;
            if code < 32 || code == 127 { ' ' } else { c }
        })
        .collect();
    let normalized = WHITESPACE_RUN
        .replace_all(js_trim(&printable), " ")
        .into_owned();

    let mut safe = normalized.clone();
    if let Ok(mut url) = reqwest::Url::parse(&normalized) {
        // Plain repository and change-request identifiers are not URLs.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        safe = url.to_string();
    }
    // `slice(0, 256)` counts UTF-16 units; counting chars is the nearest lossless cut.
    safe.chars()
        .take(MAX_ERROR_TRANSPORT_VALUE_LENGTH)
        .collect()
}

pub fn parse_source_control_owner_ref(head_selector: &str) -> Option<SourceControlRefSelector> {
    let caps = OWNER_REF.captures(js_trim(head_selector))?;
    let owner = js_trim(caps.get(1)?.as_str());
    let ref_name = js_trim(caps.get(2)?.as_str());
    (!owner.is_empty() && !ref_name.is_empty()).then(|| SourceControlRefSelector {
        owner: Some(owner.to_owned()),
        ref_name: ref_name.to_owned(),
        repository: None,
    })
}

fn normalize_source_branch(head_selector: &str) -> String {
    parse_source_control_owner_ref(head_selector)
        .map(|s| s.ref_name)
        .unwrap_or_else(|| js_trim(head_selector).to_owned())
}

pub fn source_branch(head_selector: &str, source: Option<&SourceControlRefSelector>) -> String {
    source.map_or_else(
        || normalize_source_branch(head_selector),
        |s| s.ref_name.clone(),
    )
}

pub fn source_control_ref_from_input(
    head_selector: &str,
    source: Option<&SourceControlRefSelector>,
) -> Option<SourceControlRefSelector> {
    source
        .cloned()
        .or_else(|| parse_source_control_owner_ref(head_selector))
}

/// The repository path a remote URL names (`owner/name`, or deeper for nested groups).
pub fn repository_path_from_remote_url(url: Option<&str>) -> Option<String> {
    let trimmed = js_trim(url.unwrap_or(""));
    if trimmed.is_empty() {
        return None;
    }
    let path = js_trim(REMOTE_URL_PATH.captures(trimmed)?.get(1)?.as_str());
    (!path.is_empty()).then(|| path.to_owned())
}

/// Which change requests a head lookup asks about (upstream `headBranchProbe`'s result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadBranchProbe {
    pub head_selectors: Vec<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadProbeState {
    Open,
    All,
}

/// Asks the registry which host (and base URL) serves a remote.
#[async_trait]
pub trait ResolveProviderContext: Send + Sync {
    async fn resolve(
        &self,
        cwd: &str,
        context: &SourceControlProviderContext,
    ) -> Result<Option<SourceControlProviderContext>, SourceControlProviderError>;
}

#[derive(Debug, Clone)]
pub struct ListChangeRequestsInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
    pub source: Option<SourceControlRefSelector>,
    pub head_selector: String,
    /// `None` is upstream's `"all"`.
    pub state: Option<ChangeRequestState>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct GetChangeRequestInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
    pub reference: String,
}

#[derive(Debug, Clone)]
pub struct CreateChangeRequestInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
    pub source: Option<SourceControlRefSelector>,
    pub target: Option<SourceControlRefSelector>,
    pub base_ref_name: String,
    pub head_selector: String,
    pub title: String,
    pub body_file: String,
}

#[derive(Debug, Clone)]
pub struct GetRepositoryCloneUrlsInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
    pub repository: String,
}

#[derive(Debug, Clone)]
pub struct CreateRepositoryInput {
    pub cwd: String,
    pub repository: String,
    pub visibility: SourceControlRepositoryVisibility,
}

#[derive(Debug, Clone)]
pub struct GetDefaultBranchInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
}

#[derive(Debug, Clone)]
pub struct CheckoutChangeRequestInput {
    pub cwd: String,
    pub context: Option<SourceControlProviderContext>,
    pub reference: String,
    pub force: bool,
}

/// One host's repository-level operations (upstream `SourceControlProvider` service).
#[async_trait]
pub trait SourceControlProvider: Send + Sync {
    fn kind(&self) -> SourceControlProviderKind;

    /// How to look up a branch's change requests: which head selectors to ask about, and how many
    /// results to read per selector. The caller checks the owner against what comes back, so a
    /// host that cannot search `owner:branch` can drop those selectors without losing anything.
    /// `None` (absent) asks about every selector, reading 1 for the open lookup and 20 for any
    /// state.
    fn head_branch_probe(
        &self,
        _head_selectors: &[String],
        _state: HeadProbeState,
    ) -> Option<HeadBranchProbe> {
        None
    }

    /// The repository's change request template at `treeish`, for change request content
    /// generation to follow. `None` means the host has no template convention or none exists.
    async fn read_change_request_template(&self, _cwd: &str, _treeish: &str) -> Option<String> {
        None
    }

    /// The repository's `owner/name` from a remote URL, for a host whose remote paths can carry
    /// more than that (Forgejo's HTTP installation mount). Outer `None`: capability absent, use the
    /// whole path.
    fn repository_name_from_remote_url(&self, _url: &str) -> Option<Option<String>> {
        None
    }

    /// Fills in what a repository identity read from git cannot know, such as the browser URL of
    /// a host the remote URL does not name. Only consulted for identities this host may own.
    /// Outer `None`: capability absent.
    async fn refine_repository_identity(
        &self,
        _identity: &RepositoryIdentity,
        _resolver: &dyn ResolveProviderContext,
    ) -> Option<Result<RepositoryIdentity, SourceControlProviderError>> {
        None
    }

    /// Optional capability for issue and change-request subjects.
    fn resolve_link(
        &self,
        _input: ResolveSourceControlLinkInput,
    ) -> ResolveSourceControlLinkFuture<'_> {
        None
    }

    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> Result<Vec<ChangeRequest>, SourceControlProviderError>;

    async fn get_change_request(
        &self,
        input: GetChangeRequestInput,
    ) -> Result<ChangeRequest, SourceControlProviderError>;

    async fn create_change_request(
        &self,
        input: CreateChangeRequestInput,
    ) -> Result<(), SourceControlProviderError>;

    async fn get_repository_clone_urls(
        &self,
        input: GetRepositoryCloneUrlsInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError>;

    async fn create_repository(
        &self,
        input: CreateRepositoryInput,
    ) -> Result<SourceControlRepositoryCloneUrls, SourceControlProviderError>;

    async fn get_default_branch(
        &self,
        input: GetDefaultBranchInput,
    ) -> Result<Option<String>, SourceControlProviderError>;

    async fn checkout_change_request(
        &self,
        input: CheckoutChangeRequestInput,
    ) -> Result<(), SourceControlProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_ref_parses_owner_and_branch() {
        let parsed = parse_source_control_owner_ref(" octo:feature/x ").unwrap();
        assert_eq!(parsed.owner.as_deref(), Some("octo"));
        assert_eq!(parsed.ref_name, "feature/x");
        assert!(parse_source_control_owner_ref("feature/x").is_none());
        assert!(parse_source_control_owner_ref("a/b:c").is_none());
    }

    #[test]
    fn repository_path_from_remote_url_handles_common_shapes() {
        for (url, expected) in [
            ("git@github.com:octo/repo.git", Some("octo/repo")),
            ("https://github.com/octo/repo", Some("octo/repo")),
            (
                "ssh://git@host/group/sub/project.git/",
                Some("group/sub/project"),
            ),
            ("not a url", None),
            ("", None),
        ] {
            assert_eq!(
                repository_path_from_remote_url(Some(url)).as_deref(),
                expected,
                "{url}"
            );
        }
        assert_eq!(repository_path_from_remote_url(None), None);
    }

    #[test]
    fn transport_safe_value_strips_url_secrets_and_bounds_length() {
        assert_eq!(
            transport_safe_source_control_error_value("https://user:pw@github.com/o/r?token=1#x"),
            "https://github.com/o/r"
        );
        assert_eq!(
            transport_safe_source_control_error_value(" a\tb\n c "),
            "a b c"
        );
        assert_eq!(
            transport_safe_source_control_error_value(&"x".repeat(400)).len(),
            256
        );
    }

    #[test]
    fn source_branch_prefers_explicit_source() {
        let source = SourceControlRefSelector {
            ref_name: "explicit".into(),
            owner: None,
            repository: None,
        };
        assert_eq!(source_branch("o:b", Some(&source)), "explicit");
        assert_eq!(source_branch("o:b", None), "b");
        assert_eq!(source_branch(" b ", None), "b");
    }
}
