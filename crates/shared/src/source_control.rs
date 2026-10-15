//! Port of upstream/packages/shared/src/sourceControl.ts
//!
//! GitHub only (PORTING cut list): GitLab, Forgejo, Azure DevOps, Bitbucket and GitCafe have no
//! presentation or detection here. Their hosts classify as `unknown`, which gets the generic
//! change-request copy, the same as any host upstream does not recognise.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::source_control::{SourceControlProviderInfo, SourceControlProviderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestPresentation {
    pub icon: &'static str,
    pub provider_name: &'static str,
    pub short_name: &'static str,
    pub long_name: &'static str,
    pub plural_long_name: &'static str,
    pub provider_long_name: &'static str,
    pub checkout_command_example: Option<&'static str>,
    pub url_example: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeRequestTerminology {
    pub short_label: &'static str,
    pub singular: &'static str,
}

const GITHUB_CHANGE_REQUEST_PRESENTATION: ChangeRequestPresentation = ChangeRequestPresentation {
    icon: "github",
    provider_name: "GitHub",
    short_name: "PR",
    long_name: "pull request",
    plural_long_name: "pull requests",
    provider_long_name: "GitHub pull request",
    checkout_command_example: Some("gh pr checkout 123"),
    url_example: "https://github.com/owner/repo/pull/42",
};

const GENERIC_CHANGE_REQUEST_PRESENTATION: ChangeRequestPresentation = ChangeRequestPresentation {
    icon: "change-request",
    provider_name: "source control",
    short_name: "change request",
    long_name: "change request",
    plural_long_name: "change requests",
    provider_long_name: "change request",
    checkout_command_example: None,
    url_example: "#42",
};

/// `resolveChangeRequestPresentation`. A missing provider is GitHub, as upstream treats it.
pub fn resolve_change_request_presentation(
    provider: Option<&SourceControlProviderInfo>,
) -> ChangeRequestPresentation {
    match provider.map(|provider| provider.kind.as_ref()) {
        None | Some("github") => GITHUB_CHANGE_REQUEST_PRESENTATION,
        Some(_) => GENERIC_CHANGE_REQUEST_PRESENTATION,
    }
}

pub fn get_change_request_terminology_for_kind(kind: &str) -> ChangeRequestTerminology {
    let presentation = match kind {
        "github" => GITHUB_CHANGE_REQUEST_PRESENTATION,
        _ => GENERIC_CHANGE_REQUEST_PRESENTATION,
    };
    ChangeRequestTerminology {
        short_label: presentation.short_name,
        singular: presentation.long_name,
    }
}

static SCP_SSH_REMOTE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+@([^:/]+):").expect("static regex"));

pub fn is_ssh_remote_url(remote_url: &str) -> bool {
    let trimmed = js_trim(remote_url);
    SCP_SSH_REMOTE_PATTERN.is_match(trimmed) || trimmed.to_lowercase().starts_with("ssh://")
}

/// Host used for provider detection. SCP-style and SSH remotes return the hostname only, while
/// other URL schemes keep an explicit port for non-default web endpoints.
fn parse_remote_host(remote_url: &str) -> Option<String> {
    let trimmed = js_trim(remote_url);
    if trimmed.is_empty() {
        return None;
    }
    if let Some(captures) = SCP_SSH_REMOTE_PATTERN.captures(trimmed) {
        let host = &captures[1];
        if !host.is_empty() {
            return Some(host.to_lowercase());
        }
    }
    let url = url::Url::parse(trimmed).ok()?;
    let host = url.host_str()?;
    let host = match (url.scheme(), url.port()) {
        ("ssh", _) | (_, None) => host.to_owned(),
        (_, Some(port)) => format!("{host}:{port}"),
    };
    Some(host.to_lowercase())
}

fn parse_host_name(host: &str) -> String {
    match url::Url::parse(&format!("https://{host}"))
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
    {
        Some(hostname) => hostname,
        None => {
            static PORT_SUFFIX: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r":[0-9]+$").expect("static regex"));
            PORT_SUFFIX.replace(host, "").to_lowercase()
        }
    }
}

fn to_base_url(host: &str) -> String {
    format!("https://{host}")
}

fn has_dns_label(host: &str, label: &str) -> bool {
    host.split('.').any(|part| part == label)
}

fn is_github_host(host: &str) -> bool {
    host == "github.com" || has_dns_label(host, "github")
}

/// Classifies a remote by its host. Only GitHub is recognised; every other host is `unknown`,
/// named by its host, so the page still shows where the remote points.
pub fn detect_source_control_provider_from_remote_url(
    remote_url: &str,
) -> Option<SourceControlProviderInfo> {
    let host = parse_remote_host(remote_url)?;
    let hostname = parse_host_name(&host);
    if is_github_host(&hostname) {
        return Some(SourceControlProviderInfo {
            kind: SourceControlProviderKind::new("github").expect("static slug"),
            name: turin_contracts::base_schemas::TrimmedNonEmptyString::new(
                if hostname == "github.com" {
                    "GitHub"
                } else {
                    "GitHub Self-Hosted"
                },
            )
            .expect("static name"),
            base_url: to_base_url(&host),
        });
    }
    Some(SourceControlProviderInfo {
        kind: SourceControlProviderKind::new("unknown").expect("static slug"),
        name: turin_contracts::base_schemas::TrimmedNonEmptyString::new(&host).ok()?,
        base_url: to_base_url(&host),
    })
}

/// The identity fields `sourceControlRepositorySelector` reads. Build it from a
/// [`RepositoryIdentity`] with `From`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RepositorySelectorIdentity<'a> {
    pub display_name: Option<&'a str>,
    pub owner: Option<&'a str>,
    pub name: Option<&'a str>,
}

impl<'a> From<&'a RepositoryIdentity> for RepositorySelectorIdentity<'a> {
    fn from(identity: &'a RepositoryIdentity) -> Self {
        Self {
            display_name: identity.display_name.as_ref().map(|value| value.as_str()),
            owner: identity.owner.as_ref().map(|value| value.as_str()),
            name: identity.name.as_ref().map(|value| value.as_str()),
        }
    }
}

/// The provider-native repository selector: the full path below the host when the identity has
/// one (nested GitLab groups need it), otherwise owner/name for identities recorded before
/// `displayName` existed.
pub fn source_control_repository_selector(
    identity: Option<RepositorySelectorIdentity<'_>>,
) -> Option<String> {
    let identity = identity?;
    fn non_empty(value: Option<&str>) -> Option<&str> {
        value.filter(|value| !value.is_empty())
    }
    if let Some(display_name) = non_empty(identity.display_name) {
        return Some(display_name.to_owned());
    }
    match (non_empty(identity.owner), non_empty(identity.name)) {
        (Some(owner), Some(name)) => Some(format!("{owner}/{name}")),
        _ => None,
    }
}

/// Slug-level normalisation of repository keys: Azure DevOps SSH and legacy `visualstudio.com`
/// forms become the browser form. Other keys pass through.
pub fn canonical_repository_key(key: &str) -> String {
    static AZURE_SSH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$",
        )
        .expect("static regex")
    });
    static AZURE_LEGACY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
            .expect("static regex")
    });
    let key = AZURE_SSH.replace(key, "dev.azure.com/$1/$2/_git/$3");
    AZURE_LEGACY
        .replace(&key, "dev.azure.com/$1/$2/_git/$3")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(remote: &str) -> Option<(String, String, String)> {
        detect_source_control_provider_from_remote_url(remote).map(|info| {
            (
                info.kind.as_ref().to_owned(),
                info.name.as_str().to_owned(),
                info.base_url,
            )
        })
    }

    fn kind_of(remote: &str) -> Option<String> {
        detected(remote).map(|(kind, _, _)| kind)
    }

    #[test]
    fn uses_pull_request_terminology_for_github() {
        assert_eq!(
            get_change_request_terminology_for_kind("github"),
            ChangeRequestTerminology {
                short_label: "PR",
                singular: "pull request",
            }
        );
    }

    #[test]
    fn falls_back_to_generic_change_request_copy_for_unknown_providers() {
        let unknown = SourceControlProviderInfo {
            kind: SourceControlProviderKind::new("unknown").unwrap(),
            name: turin_contracts::base_schemas::TrimmedNonEmptyString::new("forge").unwrap(),
            base_url: String::new(),
        };
        let presentation = resolve_change_request_presentation(Some(&unknown));
        assert_eq!(presentation.short_name, "change request");
        assert_eq!(presentation.long_name, "change request");
    }

    #[test]
    fn detects_github_remotes() {
        assert_eq!(
            kind_of("git@github.com:owner/repo.git").as_deref(),
            Some("github")
        );
        assert_eq!(
            kind_of("deploy@github.com:owner/repo.git").as_deref(),
            Some("github")
        );
        assert_eq!(
            kind_of("https://github.example.com/owner/repo.git").as_deref(),
            Some("github")
        );
    }

    #[test]
    fn names_github_com_and_self_hosted_installs() {
        assert_eq!(
            detected("https://github.com:8443/owner/repo.git"),
            Some((
                "github".to_owned(),
                "GitHub".to_owned(),
                "https://github.com:8443".to_owned()
            ))
        );
        assert_eq!(
            detected("https://github.example.com/owner/repo.git"),
            Some((
                "github".to_owned(),
                "GitHub Self-Hosted".to_owned(),
                "https://github.example.com".to_owned()
            ))
        );
    }

    #[test]
    fn classifies_other_hosts_as_unknown_by_their_host() {
        assert_eq!(
            detected("https://self-hosted.example.test:8443/group/repo.git"),
            Some((
                "unknown".to_owned(),
                "self-hosted.example.test:8443".to_owned(),
                "https://self-hosted.example.test:8443".to_owned()
            ))
        );
        assert_eq!(
            detected("ssh://git@code.example.test:24/team/project.git"),
            Some((
                "unknown".to_owned(),
                "code.example.test".to_owned(),
                "https://code.example.test".to_owned()
            ))
        );
        assert_eq!(
            kind_of("https://gitlab.com/group/repo.git").as_deref(),
            Some("unknown")
        );
        assert_eq!(
            kind_of("https://dev.azure.com/org/project/_git/repo").as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn matches_github_by_complete_dns_labels_only() {
        assert_eq!(
            kind_of("https://notgithub.example.com/owner/repo.git").as_deref(),
            Some("unknown")
        );
        assert_eq!(
            kind_of("https://github.example.com/owner/repo.git").as_deref(),
            Some("github")
        );
    }

    #[test]
    fn detects_no_provider_for_an_empty_remote() {
        assert_eq!(kind_of("   "), None);
    }

    #[test]
    fn recognises_scp_like_ssh_urls_with_any_ssh_user_prefix() {
        assert!(is_ssh_remote_url("git@github.com:owner/repo.git"));
        assert!(is_ssh_remote_url(
            "gitlab@gitlab.example.com:group/project.git"
        ));
        assert!(is_ssh_remote_url("deploy@bitbucket.org:workspace/repo.git"));
    }

    #[test]
    fn recognises_ssh_urls_with_any_case() {
        assert!(is_ssh_remote_url(
            "ssh://git@gitlab.example.com/group/project.git"
        ));
        assert!(is_ssh_remote_url(
            "ssh://git@gitlab.example.com:22/group/project.git"
        ));
        assert!(is_ssh_remote_url(
            "SSH://git@gitlab.example.com/group/project.git"
        ));
        assert!(is_ssh_remote_url(
            "SsH://git@gitlab.example.com/group/project.git"
        ));
    }

    #[test]
    fn returns_false_for_https_local_paths_and_scp_like_paths_without_a_colon() {
        assert!(!is_ssh_remote_url(
            "https://gitlab.example.com/group/project.git"
        ));
        assert!(!is_ssh_remote_url("/home/user/repos/project"));
        assert!(!is_ssh_remote_url(""));
        assert!(!is_ssh_remote_url("deploy@github.com/project/repo"));
    }

    #[test]
    fn keeps_a_github_identity_whole_path_and_falls_back_to_owner_name() {
        assert_eq!(
            source_control_repository_selector(Some(RepositorySelectorIdentity {
                display_name: Some("group/subgroup/service"),
                owner: Some("group"),
                name: Some("service"),
            })),
            Some("group/subgroup/service".to_owned())
        );
        assert_eq!(
            source_control_repository_selector(Some(RepositorySelectorIdentity {
                display_name: None,
                owner: Some("t3tools"),
                name: Some("t3code"),
            })),
            Some("t3tools/t3code".to_owned())
        );
    }

    #[test]
    fn names_nothing_for_a_project_with_no_remote_to_name_it_by() {
        assert_eq!(source_control_repository_selector(None), None);
        assert_eq!(
            source_control_repository_selector(Some(RepositorySelectorIdentity::default())),
            None
        );
    }

    #[test]
    fn canonicalises_azure_forms_and_passes_other_keys_through() {
        assert_eq!(
            canonical_repository_key("ssh.dev.azure.com/v3/org/project/repo"),
            "dev.azure.com/org/project/_git/repo"
        );
        assert_eq!(
            canonical_repository_key("org.visualstudio.com/defaultcollection/project/_git/web"),
            "dev.azure.com/org/project/_git/web"
        );
        assert_eq!(
            canonical_repository_key("github.com/owner/repo"),
            "github.com/owner/repo"
        );
    }
}
