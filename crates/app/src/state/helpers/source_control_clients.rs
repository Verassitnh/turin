//! Port of upstream/packages/client-runtime/src/sourceControlClients.ts
//!
//! The source control host definitions the client ships, and the one lookup it uses. Turin ships
//! GitHub only (PORTING.md cut list), so the registry holds that one definition.
//!
//! The definition type and registry come from
//! `upstream/packages/source-control-core/src/client/definition.ts`, and GitHub's definition from
//! `upstream/packages/source-control-github/src/client/definition.ts` (its client half; the
//! settings half is `turin_source_control::github::definition`).

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::js_trim;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestReviewVerdict, RepositoryIdentityLike, pull_request_host_of,
};
use turin_contracts::source_control::SourceControlProviderKind;
use url::Url;

use super::shared_support::{
    ChangeRequestLink, JS_NOT_LINE_TERMINATOR_CLASS, JS_WS_CLASS, encode_uri_component,
    url_host_with_port,
};

/// What a host calls a change request, e.g. `MR` and `merge request` on GitLab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestTerminology {
    pub short_label: &'static str,
    pub singular: &'static str,
}

/// Where a project's repository lives, for building its change request URLs.
#[derive(Debug, Clone, Copy)]
pub struct ChangeRequestUrlInput<'a> {
    /// The host below which the repository is addressed, as `pullRequestHostOf` reports it.
    pub host: &'a str,
    /// The repository path below the host.
    pub repository: &'a str,
    pub number: u64,
    /// The checkout's remote URL, whose origin some hosts serve their web pages from.
    pub remote_url: Option<&'a str>,
    /// The repository's browser URL, when the server resolved one from a hosting account.
    pub web_url: Option<&'a str>,
}

/// A project's repository, as a change request link is matched against it. Upstream picks
/// `canonicalKey | locator | webUrl | displayName | owner | name` from `RepositoryIdentity`.
pub type ChangeRequestProjectIdentity = RepositoryIdentity;

/// The change request a checkout command is built for.
#[derive(Debug, Clone, Copy)]
pub struct ChangeRequestCheckoutInput<'a> {
    pub number: u64,
    pub head_branch: &'a str,
    pub head_repository_name_with_owner: Option<&'a str>,
    /// The repository's web URL, for hosts whose checkout fetches from it.
    pub repository_url: Option<&'a str>,
}

/// Which of a repository's clone URLs a new clone uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneTransport {
    Https,
    Ssh,
}

/// Who a new project's repository is published under: `owner` to name, `None` to let the host's
/// CLI pick the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepositoryOwner {
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceControlClientDefinition {
    pub kind: SourceControlProviderKind,
    /// The host's name, as in "Open on GitHub".
    pub label: &'static str,
    /// How the clone and publish pickers name the host; Forgejo's covers Gitea too.
    pub picker_label: &'static str,
    /// Glyph key each client maps to its own icon; a key a client lacks draws the generic glyph.
    pub icon: &'static str,
    pub change_request: ChangeRequestTerminology,
    /// Placeholder for a repository path on this host, such as `owner/repo`.
    pub repository_path_hint: &'static str,
    /// The hostname a change request reference can be attributed to before any repository
    /// identity is known, or `None` when a hostname alone does not name this host.
    pub public_host: Option<&'static str>,
    /// Subtitle beside the host in the publish picker.
    pub publish_description: &'static str,
    /// Where a publish lands, given the host the server is signed in to, if it reported one.
    pub publish_host: fn(signed_in_host: Option<&str>) -> String,
    /// Who a new project's repository is published under, given the signed-in account, or `None`
    /// when the host needs a choice an account cannot make, such as Azure DevOps' project.
    pub new_repository_owner: fn(account: Option<&str>) -> Option<NewRepositoryOwner>,
    pub default_clone_transport: CloneTransport,
    /// The web URL of a change request, or `None` when this client cannot build one for the host.
    pub change_request_url: fn(&ChangeRequestUrlInput<'_>) -> Option<String>,
    /// The actions this host can take on a change request, the same list its server provider
    /// declares in `capabilities.actions`. Surfaces with no server answer to hand, such as a
    /// thread's linked pull requests, offer actions from this. (A `ReadonlySet` upstream.)
    pub change_request_actions: &'static [PullRequestAction],
    /// A shell command that checks the change request out, or `None` when it cannot be built.
    pub checkout_command: fn(&ChangeRequestCheckoutInput<'_>) -> Option<String>,
    /// A comment author's profile page, or `None` where the host has none clients can link.
    pub author_profile_url: fn(login: &str, repository_url: &str) -> Option<String>,
    /// The repository URL that `#123` and commit SHAs in change request text link under, using
    /// GitHub's `/issues/` and `/commit/` routes, or `None` where the host does not route them so.
    pub reference_autolink_repository_url: fn(repository_url: &str) -> Option<String>,
    /// Whether a review with this verdict must carry a summary, beyond what every host asks.
    pub review_summary_required: fn(PullRequestReviewVerdict) -> bool,
    /// The change request a pasted command names, such as `gh pr checkout 42`: its argument, which
    /// is a number or a URL. `None` for anything that is not this host's checkout command.
    pub checkout_command_argument: fn(input: &str) -> Option<String>,
    /// Whether a pasted URL is one of this host's change request URLs. Stricter than
    /// `is_change_request_url`, which only tells hosts apart, because this decides what a
    /// reference field accepts.
    pub is_change_request_reference: fn(url: &str) -> bool,
    /// Whether a change request link names this project's own repository. The link's host is where
    /// the repository is addressed, `link.authority` the web host and port where they differ.
    pub is_change_request_in_repository:
        fn(&ChangeRequestProjectIdentity, &ChangeRequestLink) -> bool,
    /// Whether this project can read a change request on the link's host on the link's behalf, for
    /// a repository nobody has checked out. False where the host's reads are scoped to the checkout.
    pub can_read_change_request_on_host:
        fn(&ChangeRequestProjectIdentity, &ChangeRequestLink) -> bool,
    /// The host a change request URL addresses its repository below, as a `PullRequestRef.host`
    /// names it: the hostname, or the host and port for servers addressed by both.
    pub change_request_url_host: fn(&Url) -> String,
    /// The host a reference with none names, read from the checkout alone, or `None` where only
    /// the server can tell, such as when an SSH remote resolves to a different web host.
    pub checkout_change_request_host: fn(&ChangeRequestProjectIdentity) -> Option<String>,
    /// Whether a change request URL has this host's path shape, e.g. GitLab's `/-/merge_requests/`.
    pub is_change_request_url: fn(url: &str) -> bool,
}

/// Whether a URL's path is a change request at `route`, such as `/pull/`, followed by its number.
/// Matching the parsed path keeps a query or fragment that mentions another host's route out.
pub fn is_change_request_path(url: &str, route: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let path = parsed.path();
    match path.find(route) {
        // `at > 0`
        Some(at) if at > 0 => {
            // `/^\d+(?:\/|$)/u`
            let rest = &path[at + route.len()..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && matches!(rest.as_bytes().get(digits), None | Some(b'/'))
        }
        _ => false,
    }
}

/// `pullRequestHostOf(identity, kind)`
fn pull_request_host_of_identity(
    identity: &ChangeRequestProjectIdentity,
    kind: &SourceControlProviderKind,
) -> String {
    pull_request_host_of(
        Some(&RepositoryIdentityLike {
            canonical_key: Some(identity.canonical_key.as_str()),
            remote_url: Some(identity.locator.remote_url.as_str()),
        }),
        kind.clone(),
    )
}

/// Whether a project's HTTP remote names the link's web authority. A link carries one only where
/// the host and port differ from where the repository is addressed, as on a Forgejo server with a
/// port; an SSH remote says nothing about ports, so it does not rule the link out.
pub fn is_change_request_authority_of_project(
    identity: &ChangeRequestProjectIdentity,
    link: &ChangeRequestLink,
) -> bool {
    let Some(authority) = &link.authority else {
        return true;
    };
    if let Ok(remote) = Url::parse(identity.locator.remote_url.as_str())
        && (remote.scheme() == "http" || remote.scheme() == "https")
    {
        return url_host_with_port(&remote).to_lowercase() == *authority;
    }
    // SSH remotes do not specify the server's HTTP port.
    true
}

/// Whether a project lives on the link's host, as `pullRequestHostOf` reads it from the checkout.
pub fn is_change_request_on_project_host(
    kind: &SourceControlProviderKind,
    identity: &ChangeRequestProjectIdentity,
    link: &ChangeRequestLink,
) -> bool {
    if !is_change_request_authority_of_project(identity, link) {
        return false;
    }
    let host = pull_request_host_of_identity(identity, kind);
    host == link.host.to_lowercase() || link.authority.as_deref() == Some(host.as_str())
}

/// The repository path a project's identity names below its host.
fn project_repository_path(identity: &ChangeRequestProjectIdentity) -> Option<String> {
    if let Some(display_name) = &identity.display_name {
        return Some(display_name.as_str().to_owned());
    }
    match (&identity.owner, &identity.name) {
        (Some(owner), Some(name)) => Some(format!("{owner}/{name}")),
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

fn unknown_kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new("unknown").expect("valid kind")
}

/// What clients show for a host they ship no definition for, including `unknown`.
pub static UNKNOWN_SOURCE_CONTROL_CLIENT: LazyLock<SourceControlClientDefinition> =
    LazyLock::new(|| SourceControlClientDefinition {
        kind: unknown_kind(),
        label: "source control",
        picker_label: "source control",
        icon: "change-request",
        change_request: ChangeRequestTerminology {
            short_label: "change request",
            singular: "change request",
        },
        repository_path_hint: "URL",
        public_host: None,
        publish_description: "Your signed-in server",
        publish_host: |signed_in_host| signed_in_host.unwrap_or("your server").to_owned(),
        new_repository_owner: |_| None,
        default_clone_transport: CloneTransport::Ssh,
        change_request_url: |_| None,
        change_request_actions: &[],
        checkout_command: |_| None,
        author_profile_url: |_, _| None,
        reference_autolink_repository_url: |_| None,
        review_summary_required: |_| false,
        checkout_command_argument: |_| None,
        is_change_request_reference: |_| false,
        change_request_url_host: |url| url.host_str().unwrap_or("").to_owned(),
        checkout_change_request_host: |_| None,
        is_change_request_in_repository: |identity, link| {
            is_change_request_in_project_repository(&unknown_kind(), identity, link)
        },
        can_read_change_request_on_host: |identity, link| {
            is_change_request_on_project_host(&unknown_kind(), identity, link)
        },
        is_change_request_url: |_| false,
    });

fn github_kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new("github").expect("valid kind")
}

/// `/^gh\s+pr\s+checkout\s+(.+)$/i`
static CHECKOUT_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?i-u:gh)[{JS_WS_CLASS}]+(?i-u:pr)[{JS_WS_CLASS}]+(?i-u:checkout)[{JS_WS_CLASS}]+([{JS_NOT_LINE_TERMINATOR_CLASS}]+)$"
    ))
    .expect("valid regex")
});
/// `/^https:\/\/github\.com\/[^/\s]+\/[^/\s]+\/pull\/(\d+)(?:[/?#].*)?$/i`
static CHANGE_REQUEST_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?i-u:https://github\.com/)[^/{JS_WS_CLASS}]+/[^/{JS_WS_CLASS}]+(?i-u:/pull/)[0-9]+(?:[/?#][{JS_NOT_LINE_TERMINATOR_CLASS}]*)?$"
    ))
    .expect("valid regex")
});

/// GitHub's client definition. Browser- and React Native-safe upstream; plain data here.
pub static GITHUB_SOURCE_CONTROL_CLIENT: LazyLock<SourceControlClientDefinition> =
    LazyLock::new(|| SourceControlClientDefinition {
        kind: github_kind(),
        label: "GitHub",
        picker_label: "GitHub",
        icon: "github",
        change_request: ChangeRequestTerminology {
            short_label: "PR",
            singular: "pull request",
        },
        repository_path_hint: "owner/repo",
        public_host: Some("github.com"),
        publish_description: "github.com",
        publish_host: |_| "github.com".to_owned(),
        new_repository_owner: |account| {
            Some(NewRepositoryOwner {
                owner: account.map(str::to_owned),
            })
        },
        default_clone_transport: CloneTransport::Https,
        change_request_url: |input| {
            Some(format!(
                "https://{}/{}/pull/{}",
                input.host, input.repository, input.number
            ))
        },
        change_request_actions: &[
            PullRequestAction::Merge,
            PullRequestAction::Ready,
            PullRequestAction::Draft,
            PullRequestAction::Close,
            PullRequestAction::Reopen,
            PullRequestAction::UpdateBranch,
            PullRequestAction::EnableAutoMerge,
            PullRequestAction::DisableAutoMerge,
            PullRequestAction::Revert,
            PullRequestAction::ApproveWorkflows,
        ],
        checkout_command: |checkout| Some(format!("gh pr checkout {}", checkout.number)),
        // Upstream's `new URL(...)` throws on an unparseable repository URL; this has no
        // profile to link instead.
        author_profile_url: |login, repository_url| {
            if login.ends_with("[bot]") {
                return None;
            }
            Url::parse(repository_url)
                .ok()?
                .join(&format!("/{}", encode_uri_component(login)))
                .ok()
                .map(|url| url.to_string())
        },
        reference_autolink_repository_url: |repository_url| Some(repository_url.to_owned()),
        review_summary_required: |_| false,
        checkout_command_argument: |input| {
            CHECKOUT_COMMAND
                .captures(input)
                .and_then(|captures| captures.get(1))
                .map(|argument| js_trim(argument.as_str()).to_owned())
        },
        is_change_request_reference: |url| CHANGE_REQUEST_REFERENCE.is_match(url),
        change_request_url_host: |url| url.host_str().unwrap_or("").to_owned(),
        // A GitHub web host is the checkout's own host, Enterprise installs included.
        checkout_change_request_host: |identity| {
            Some(pull_request_host_of_identity(identity, &github_kind()))
        },
        is_change_request_in_repository: |identity, link| {
            is_change_request_in_project_repository(&github_kind(), identity, link)
        },
        can_read_change_request_on_host: |identity, link| {
            is_change_request_on_project_host(&github_kind(), identity, link)
        },
        is_change_request_url: |url| is_change_request_path(url, "/pull/"),
    });

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SourceControlClientRegistryError {
    #[error("Source control host '{0}' is defined more than once.")]
    DuplicateHost(String),
}

/// The host definitions a client loaded.
#[derive(Debug)]
pub struct SourceControlClientRegistry {
    definitions: Vec<&'static SourceControlClientDefinition>,
}

pub fn make_source_control_client_registry(
    definitions: Vec<&'static SourceControlClientDefinition>,
) -> Result<SourceControlClientRegistry, SourceControlClientRegistryError> {
    let mut seen: Vec<&str> = Vec::new();
    for definition in &definitions {
        let kind = definition.kind.as_str();
        if seen.contains(&kind) {
            return Err(SourceControlClientRegistryError::DuplicateHost(
                kind.to_owned(),
            ));
        }
        seen.push(kind);
    }
    Ok(SourceControlClientRegistry { definitions })
}

impl SourceControlClientRegistry {
    /// The definitions in registration order, for pickers that offer every host.
    pub fn definitions(&self) -> &[&'static SourceControlClientDefinition] {
        &self.definitions
    }

    /// The definition for a kind. No kind at all, before a repository reports its host, reads as
    /// the first definition; a kind this client lacks, `unknown` included, reads as
    /// `UNKNOWN_SOURCE_CONTROL_CLIENT`.
    pub fn get(&self, kind: Option<&str>) -> &'static SourceControlClientDefinition {
        match kind {
            None => self.definitions.first().copied(),
            Some(kind) => self.find(kind),
        }
        .unwrap_or(&UNKNOWN_SOURCE_CONTROL_CLIENT)
    }

    /// The definition for a kind, or `None` for one this client lacks.
    pub fn find(&self, kind: &str) -> Option<&'static SourceControlClientDefinition> {
        self.definitions
            .iter()
            .copied()
            .find(|definition| definition.kind.as_str() == kind)
    }

    /// The definition whose public instance is `hostname`, if any.
    pub fn find_by_public_host(
        &self,
        hostname: &str,
    ) -> Option<&'static SourceControlClientDefinition> {
        let host = hostname.to_lowercase();
        self.definitions
            .iter()
            .copied()
            .find(|definition| definition.public_host == Some(host.as_str()))
    }

    /// The definition whose change request path shape `url` has, if any.
    pub fn find_by_change_request_url(
        &self,
        url: &str,
    ) -> Option<&'static SourceControlClientDefinition> {
        self.definitions
            .iter()
            .copied()
            .find(|definition| (definition.is_change_request_url)(url))
    }

    /// The name of the host a change request URL is on, for copy about that change request.
    pub fn host_label_for_change_request_url(&self, url: &str) -> &'static str {
        self.find_by_change_request_url(url)
            .map_or("the host", |definition| definition.label)
    }
}

/// The built-in hosts. Upstream lists GitHub, GitLab, GitCafe, Forgejo, Bitbucket and Azure
/// DevOps, GitHub first because a repository that has not reported its host yet reads as GitHub,
/// which is what clients have always shown there. Turin ships GitHub only.
pub static SOURCE_CONTROL_CLIENTS: LazyLock<SourceControlClientRegistry> = LazyLock::new(|| {
    make_source_control_client_registry(vec![&GITHUB_SOURCE_CONTROL_CLIENT])
        .expect("built-in hosts are unique")
});

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_missing_kind_as_the_first_host_and_an_unshipped_kind_as_the_generic_one() {
        assert_eq!(SOURCE_CONTROL_CLIENTS.get(None).kind.as_str(), "github");
        assert_eq!(
            SOURCE_CONTROL_CLIENTS.get(Some("forkhost")).kind.as_str(),
            "unknown"
        );
    }

    // Upstream also asserts GitCafe and Forgejo claim `/pulls/` URLs in that order; both hosts
    // are cut, so their URLs belong to no shipped host here.
    #[test]
    fn gives_a_change_request_url_to_the_first_host_whose_shape_it_has() {
        let kind_of = |url: &str| {
            SOURCE_CONTROL_CLIENTS
                .find_by_change_request_url(url)
                .map(|definition| definition.kind.as_str())
        };
        assert_eq!(
            kind_of("https://github.com/acme/web/pull/7"),
            Some("github")
        );
        assert_eq!(kind_of("https://git.cafe/acme/web/pulls/7"), None);
        assert_eq!(kind_of("https://codeberg.org/acme/web/pulls/7"), None);
        assert_eq!(
            kind_of("https://gitlab.com/acme/web/-/merge_requests/7"),
            None
        );
    }

    #[test]
    fn labels_the_host_of_a_change_request_url() {
        assert_eq!(
            SOURCE_CONTROL_CLIENTS
                .host_label_for_change_request_url("https://github.com/a/b/pull/1"),
            "GitHub"
        );
        assert_eq!(
            SOURCE_CONTROL_CLIENTS.host_label_for_change_request_url("https://example.com/a/b"),
            "the host"
        );
        assert_eq!(
            SOURCE_CONTROL_CLIENTS
                .find_by_public_host("GitHub.com")
                .map(|d| d.label),
            Some("GitHub")
        );
    }

    #[test]
    fn rejects_a_host_defined_twice() {
        assert_eq!(
            make_source_control_client_registry(vec![
                &GITHUB_SOURCE_CONTROL_CLIENT,
                &GITHUB_SOURCE_CONTROL_CLIENT
            ])
            .unwrap_err(),
            SourceControlClientRegistryError::DuplicateHost("github".to_owned())
        );
    }

    #[test]
    fn github_reads_pasted_checkout_commands_and_references() {
        let github = SOURCE_CONTROL_CLIENTS.get(Some("github"));
        assert_eq!(
            (github.checkout_command_argument)("GH  pr   checkout 42 ").as_deref(),
            Some("42")
        );
        assert_eq!((github.checkout_command_argument)("git checkout 42"), None);
        assert!((github.is_change_request_reference)(
            "https://github.com/acme/web/pull/7#issue"
        ));
        assert!(!(github.is_change_request_reference)(
            "https://github.com/acme/web/issues/7"
        ));
        assert!(!(github.is_change_request_reference)(
            "http://github.com/acme/web/pull/7"
        ));
        assert_eq!(
            (github.checkout_command)(&ChangeRequestCheckoutInput {
                number: 9,
                head_branch: "feat",
                head_repository_name_with_owner: None,
                repository_url: None,
            })
            .as_deref(),
            Some("gh pr checkout 9")
        );
        assert_eq!(
            (github.author_profile_url)("octo cat", "https://github.com/acme/web").as_deref(),
            Some("https://github.com/octo%20cat")
        );
        assert_eq!(
            (github.author_profile_url)("dependabot[bot]", "https://github.com/acme/web"),
            None
        );
    }

    #[test]
    fn matches_a_link_against_the_project_identity() {
        let identity: RepositoryIdentity = serde_json::from_value(json!({
            "canonicalKey": "github.com/acme/web",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "https://github.com/acme/web.git",
            },
            "displayName": "acme/web",
        }))
        .unwrap();
        let link = |host: &str, repository: &str| ChangeRequestLink {
            host: host.to_owned(),
            repository: repository.to_owned(),
            number: 7,
            authority: None,
        };
        let github = SOURCE_CONTROL_CLIENTS.get(Some("github"));
        assert!((github.is_change_request_in_repository)(
            &identity,
            &link("github.com", "acme/web")
        ));
        assert!(!(github.is_change_request_in_repository)(
            &identity,
            &link("github.com", "acme/api")
        ));
        assert!(!(github.is_change_request_in_repository)(
            &identity,
            &link("ghe.example", "acme/web")
        ));
        assert!((github.can_read_change_request_on_host)(
            &identity,
            &link("github.com", "other/repo")
        ));
        assert_eq!(
            (github.checkout_change_request_host)(&identity).as_deref(),
            Some("github.com")
        );
    }

    #[test]
    fn change_request_paths_need_the_route_and_a_number() {
        assert!(is_change_request_path(
            "https://github.com/a/b/pull/12",
            "/pull/"
        ));
        assert!(is_change_request_path(
            "https://github.com/a/b/pull/12/files",
            "/pull/"
        ));
        assert!(!is_change_request_path(
            "https://github.com/a/b/pull/x",
            "/pull/"
        ));
        assert!(!is_change_request_path(
            "https://github.com/a/b?next=/pull/12",
            "/pull/"
        ));
        assert!(!is_change_request_path("/pull/12", "/pull/"));
    }
}
