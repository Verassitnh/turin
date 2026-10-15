//! Port of upstream/packages/source-control-core/src/client/definition.ts
//!
//! The client half of a source control host package. Every host package exports a
//! [`SourceControlClientDefinition`]; the UI reads labels, change request nouns, picker copy, and
//! checkout commands from these definitions, so a new host needs no per-kind branches in client
//! code. Definitions are plain data and pure functions: no server or platform UI imports.
//!
//! Deviation: `settings` (an Effect schema attached to the definition upstream) lives next to each
//! host as a serde struct plus a decode function (see `github::definition`), because Rust has no
//! schema value to attach; the definition only records which of its fields are secret.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestReviewVerdict, RepositoryIdentityLike, pull_request_host_of,
};
use turin_contracts::source_control::SourceControlProviderKind;

use crate::shared::change_request_url::ChangeRequestLink;

/// What a host calls a change request, e.g. `MR` and `merge request` on GitLab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestTerminology {
    pub short_label: &'static str,
    pub singular: &'static str,
}

/// Where a project's repository lives, for building its change request URLs.
#[derive(Debug, Clone)]
pub struct ChangeRequestUrlInput {
    /// The host below which the repository is addressed, as `pull_request_host_of` reports it.
    pub host: String,
    /// The repository path below the host.
    pub repository: String,
    pub number: u64,
    /// The checkout's remote URL, whose origin some hosts serve their web pages from.
    pub remote_url: Option<String>,
    /// The repository's browser URL, when the server resolved one from a hosting account.
    pub web_url: Option<String>,
}

/// The change request a checkout command is built for.
#[derive(Debug, Clone)]
pub struct ChangeRequestCheckoutInput {
    pub number: u64,
    pub head_branch: String,
    pub head_repository_name_with_owner: Option<String>,
    /// The repository's web URL, for hosts whose checkout fetches from it.
    pub repository_url: Option<String>,
}

/// A project's repository, as a change request link is matched against it
/// (`Pick<RepositoryIdentity, "canonicalKey" | "locator" | "webUrl" | "displayName" | "owner" | "name">`).
pub type ChangeRequestProjectIdentity = RepositoryIdentity;

type Fn1<A, R> = Arc<dyn Fn(&A) -> R + Send + Sync>;

#[derive(Clone)]
pub struct SourceControlClientDefinition {
    pub kind: SourceControlProviderKind,
    /// The host's name, as in "Open on GitHub".
    pub label: String,
    /// How the clone and publish pickers name the host.
    pub picker_label: String,
    /// Glyph key each client maps to its own icon; a key a client lacks draws the generic glyph.
    pub icon: String,
    pub change_request: ChangeRequestTerminology,
    /// Placeholder for a repository path on this host, such as `owner/repo`.
    pub repository_path_hint: String,
    /// The hostname a change request reference can be attributed to before any repository
    /// identity is known, or `None` when a hostname alone does not name this host.
    pub public_host: Option<String>,
    /// Subtitle beside the host in the publish picker.
    pub publish_description: String,
    /// Where a publish lands, given the host the server is signed in to, if it reported one.
    pub publish_host: Arc<dyn Fn(Option<&str>) -> String + Send + Sync>,
    /// Who a new project's repository is published under, given the signed-in account: `Some(owner)`
    /// to name one (`Some(None)` lets the host's CLI pick the signed-in user), or `None` when the
    /// host needs a choice an account cannot make.
    pub new_repository_owner: Arc<dyn Fn(Option<&str>) -> Option<Option<String>> + Send + Sync>,
    /// Which of a repository's clone URLs a new clone uses.
    pub default_clone_transport: CloneTransport,
    /// The web URL of a change request, or `None` when this client cannot build one for the host.
    pub change_request_url: Fn1<ChangeRequestUrlInput, Option<String>>,
    /// The actions this host can take on a change request, the same list its server provider
    /// declares in `capabilities.actions`.
    pub change_request_actions: HashSet<PullRequestAction>,
    /// A shell command that checks the change request out, or `None` when it cannot be built.
    pub checkout_command: Fn1<ChangeRequestCheckoutInput, Option<String>>,
    /// A comment author's profile page, or `None` where the host has none clients can link.
    pub author_profile_url: Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>,
    /// The repository URL that `#123` and commit SHAs in change request text link under, using
    /// GitHub's `/issues/` and `/commit/` routes, or `None` where the host does not route them so.
    pub reference_autolink_repository_url: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// Whether a review with this verdict must carry a summary, beyond what every host asks.
    pub review_summary_required: Arc<dyn Fn(PullRequestReviewVerdict) -> bool + Send + Sync>,
    /// The change request a pasted command names, such as `gh pr checkout 42`: its argument, which
    /// is a number or a URL. `None` for anything that is not this host's checkout command.
    pub checkout_command_argument: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// Whether a pasted URL is one of this host's change request URLs. Stricter than
    /// `is_change_request_url`, which only tells hosts apart, because this decides what a
    /// reference field accepts.
    pub is_change_request_reference: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Whether a change request link names this project's own repository.
    pub is_change_request_in_repository:
        Arc<dyn Fn(&ChangeRequestProjectIdentity, &ChangeRequestLink) -> bool + Send + Sync>,
    /// Whether this project can read a change request on the link's host on the link's behalf, for
    /// a repository nobody has checked out. False where the host's reads are scoped to the checkout.
    pub can_read_change_request_on_host:
        Arc<dyn Fn(&ChangeRequestProjectIdentity, &ChangeRequestLink) -> bool + Send + Sync>,
    /// The host a change request URL addresses its repository below, as a `PullRequestRef.host`
    /// names it: the hostname, or the host and port for servers addressed by both.
    pub change_request_url_host: Arc<dyn Fn(&reqwest::Url) -> String + Send + Sync>,
    /// The host a reference with none names, read from the checkout alone, or `None` where only
    /// the server can tell, such as when an SSH remote resolves to a different web host.
    pub checkout_change_request_host:
        Arc<dyn Fn(&ChangeRequestProjectIdentity) -> Option<String> + Send + Sync>,
    /// Whether a change request URL has this host's path shape, e.g. GitLab's `/-/merge_requests/`.
    pub is_change_request_url: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Which fields of the host's saved settings hold secrets (kept in the secret store).
    pub secret_settings_fields: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneTransport {
    Https,
    Ssh,
}

/// Whether a URL's path is a change request at `route`, such as `/pull/`, followed by its number.
/// Matching the parsed path keeps a query or fragment that mentions another host's route out.
pub fn is_change_request_path(url: &str, route: &str) -> bool {
    static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(?:/|$)").unwrap());
    let Ok(parsed) = reqwest::Url::parse(url) else { return false };
    let path = parsed.path();
    match path.find(route) {
        Some(at) if at > 0 => DIGITS.is_match(&path[at + route.len()..]),
        _ => false,
    }
}

/// Whether a project's HTTP remote names the link's web authority. A link carries one only where
/// the host and port differ from where the repository is addressed, as on a Forgejo server with a
/// port; an SSH remote says nothing about ports, so it does not rule the link out.
pub fn is_change_request_authority_of_project(
    identity: &ChangeRequestProjectIdentity,
    link: &ChangeRequestLink,
) -> bool {
    let Some(authority) = &link.authority else { return true };
    if let Ok(remote) = reqwest::Url::parse(identity.locator.remote_url.as_str())
        && (remote.scheme() == "http" || remote.scheme() == "https")
    {
        let host = remote.host_str().unwrap_or("");
        let with_port = match remote.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        return with_port.to_lowercase() == *authority;
    }
    true
}

fn host_of(kind: &SourceControlProviderKind, identity: &ChangeRequestProjectIdentity) -> String {
    pull_request_host_of(
        Some(&RepositoryIdentityLike {
            canonical_key: Some(identity.canonical_key.as_str()),
        }),
        kind,
    )
}

/// Whether a project lives on the link's host, as `pull_request_host_of` reads it from the checkout.
pub fn is_change_request_on_project_host(
    kind: &SourceControlProviderKind,
    identity: &ChangeRequestProjectIdentity,
    link: &ChangeRequestLink,
) -> bool {
    if !is_change_request_authority_of_project(identity, link) {
        return false;
    }
    let host = host_of(kind, identity);
    host == link.host.to_lowercase() || Some(&host) == link.authority.as_ref()
}

/// The repository path a project's identity names below its host.
fn project_repository_path(identity: &ChangeRequestProjectIdentity) -> Option<String> {
    if let Some(display) = identity.display_name.as_ref().filter(|d| !d.as_str().is_empty()) {
        return Some(display.as_str().to_owned());
    }
    match (&identity.owner, &identity.name) {
        (Some(owner), Some(name)) if !owner.as_str().is_empty() && !name.as_str().is_empty() => {
            Some(format!("{}/{}", owner.as_str(), name.as_str()))
        }
        _ => None,
    }
}

/// `is_change_request_in_repository` for hosts that address a repository by its full path below
/// the host, which is what nested GitLab groups need.
pub fn is_change_request_in_project_repository(
    kind: &SourceControlProviderKind,
    identity: &ChangeRequestProjectIdentity,
    link: &ChangeRequestLink,
) -> bool {
    project_repository_path(identity)
        .is_some_and(|repository| repository.to_lowercase() == link.repository.to_lowercase())
        && is_change_request_on_project_host(kind, identity, link)
}

/// What clients show for a host they ship no definition for, including `unknown`.
pub fn unknown_source_control_client() -> SourceControlClientDefinition {
    let kind = SourceControlProviderKind::new("unknown").expect("valid kind");
    let in_repo_kind = kind.clone();
    let on_host_kind = kind.clone();
    SourceControlClientDefinition {
        kind,
        label: "source control".into(),
        picker_label: "source control".into(),
        icon: "change-request".into(),
        change_request: ChangeRequestTerminology {
            short_label: "change request",
            singular: "change request",
        },
        repository_path_hint: "URL".into(),
        public_host: None,
        publish_description: "Your signed-in server".into(),
        publish_host: Arc::new(|signed_in_host| {
            signed_in_host.unwrap_or("your server").to_owned()
        }),
        new_repository_owner: Arc::new(|_| None),
        default_clone_transport: CloneTransport::Ssh,
        change_request_url: Arc::new(|_| None),
        change_request_actions: HashSet::new(),
        checkout_command: Arc::new(|_| None),
        author_profile_url: Arc::new(|_, _| None),
        reference_autolink_repository_url: Arc::new(|_| None),
        review_summary_required: Arc::new(|_| false),
        checkout_command_argument: Arc::new(|_| None),
        is_change_request_reference: Arc::new(|_| false),
        change_request_url_host: Arc::new(|url| url.host_str().unwrap_or("").to_owned()),
        checkout_change_request_host: Arc::new(|_| None),
        is_change_request_in_repository: Arc::new(move |identity, link| {
            is_change_request_in_project_repository(&in_repo_kind, identity, link)
        }),
        can_read_change_request_on_host: Arc::new(move |identity, link| {
            is_change_request_on_project_host(&on_host_kind, identity, link)
        }),
        is_change_request_url: Arc::new(|_| false),
        secret_settings_fields: &[],
    }
}

/// The host definitions a client loaded.
pub struct SourceControlClientRegistry {
    definitions: Vec<SourceControlClientDefinition>,
    by_kind: HashMap<String, usize>,
    unknown: SourceControlClientDefinition,
}

impl SourceControlClientRegistry {
    pub fn definitions(&self) -> &[SourceControlClientDefinition] {
        &self.definitions
    }

    /// The definition for a kind. No kind at all, before a repository reports its host, reads as
    /// the first definition; a kind this client lacks, `unknown` included, reads as the unknown
    /// definition.
    pub fn get(&self, kind: Option<&str>) -> &SourceControlClientDefinition {
        let found = match kind {
            None => self.definitions.first(),
            Some(kind) => self.find(kind),
        };
        found.unwrap_or(&self.unknown)
    }

    /// The definition for a kind, or `None` for one this client lacks.
    pub fn find(&self, kind: &str) -> Option<&SourceControlClientDefinition> {
        self.by_kind.get(kind).map(|&index| &self.definitions[index])
    }

    /// The definition whose public instance is `hostname`, if any.
    pub fn find_by_public_host(&self, hostname: &str) -> Option<&SourceControlClientDefinition> {
        let host = hostname.to_lowercase();
        self.definitions.iter().find(|d| d.public_host.as_deref() == Some(host.as_str()))
    }

    /// The definition whose change request path shape `url` has, if any.
    pub fn find_by_change_request_url(&self, url: &str) -> Option<&SourceControlClientDefinition> {
        self.definitions.iter().find(|d| (d.is_change_request_url)(url))
    }

    /// The name of the host a change request URL is on, for copy about that change request.
    pub fn host_label_for_change_request_url(&self, url: &str) -> String {
        self.find_by_change_request_url(url).map_or("the host", |d| d.label.as_str()).to_owned()
    }
}

/// Errors when a kind is defined twice (upstream throws).
pub fn make_source_control_client_registry(
    definitions: Vec<SourceControlClientDefinition>,
) -> Result<SourceControlClientRegistry, String> {
    let mut by_kind = HashMap::new();
    for (index, definition) in definitions.iter().enumerate() {
        if by_kind.insert(definition.kind.as_str().to_owned(), index).is_some() {
            return Err(format!(
                "Source control host '{}' is defined more than once.",
                definition.kind
            ));
        }
    }
    Ok(SourceControlClientRegistry { definitions, by_kind, unknown: unknown_source_control_client() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(remote_url: &str, canonical_key: &str, display_name: Option<&str>) -> RepositoryIdentity {
        serde_json::from_value(serde_json::json!({
            "canonicalKey": canonical_key,
            "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": remote_url },
            "displayName": display_name,
        }))
        .unwrap()
    }

    #[test]
    fn change_request_path_matches_route_then_digits() {
        assert!(is_change_request_path("https://github.com/a/b/pull/12", "/pull/"));
        assert!(is_change_request_path("https://github.com/a/b/pull/12/files", "/pull/"));
        assert!(!is_change_request_path("https://github.com/a/b/pull/x", "/pull/"));
        // A query or fragment that mentions the route is not the path.
        assert!(!is_change_request_path("https://github.com/a/b?x=/pull/12", "/pull/"));
        assert!(!is_change_request_path("/pull/12", "/pull/"));
        assert!(!is_change_request_path("https://github.com/pull/12", "/nope/"));
    }

    #[test]
    fn unknown_definition_matches_nothing() {
        let registry = make_source_control_client_registry(vec![]).unwrap();
        let unknown = registry.get(None);
        assert_eq!(unknown.label, "source control");
        assert!(!(unknown.is_change_request_url)("https://github.com/a/b/pull/1"));
        assert_eq!((unknown.publish_host)(None), "your server");
        assert_eq!((unknown.publish_host)(Some("h")), "h");
        assert!(registry.find("github").is_none());
    }

    #[test]
    fn project_repository_and_host_matching() {
        let kind = SourceControlProviderKind::new("github").unwrap();
        let project = identity("git@github.com:acme/web.git", "github.com/acme/web", Some("acme/web"));
        let link = ChangeRequestLink {
            host: "github.com".into(),
            repository: "acme/web".into(),
            number: 3,
            authority: None,
        };
        assert!(is_change_request_in_project_repository(&kind, &project, &link));
        assert!(is_change_request_on_project_host(&kind, &project, &link));
        let other = ChangeRequestLink { repository: "acme/api".into(), ..link.clone() };
        assert!(!is_change_request_in_project_repository(&kind, &project, &other));
        let elsewhere = ChangeRequestLink { host: "ghe.example".into(), ..link };
        assert!(!is_change_request_on_project_host(&kind, &project, &elsewhere));
    }

    #[test]
    fn authority_is_checked_against_http_remotes_only() {
        let link = ChangeRequestLink {
            host: "forge.example".into(),
            repository: "a/b".into(),
            number: 1,
            authority: Some("forge.example:3000".into()),
        };
        let http = identity("http://forge.example:3000/a/b.git", "forge.example/a/b", None);
        let other_port = identity("http://forge.example:4000/a/b.git", "forge.example/a/b", None);
        let ssh = identity("git@forge.example:a/b.git", "forge.example/a/b", None);
        assert!(is_change_request_authority_of_project(&http, &link));
        assert!(!is_change_request_authority_of_project(&other_port, &link));
        assert!(is_change_request_authority_of_project(&ssh, &link));
    }

    #[test]
    fn registry_rejects_duplicates() {
        let a = unknown_source_control_client();
        let b = unknown_source_control_client();
        assert!(make_source_control_client_registry(vec![a, b]).is_err());
    }
}
