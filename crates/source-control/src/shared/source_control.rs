//! Port of upstream/packages/shared/src/sourceControl.ts (GitHub only).
//!
//! Deviation: upstream also classifies GitLab, Forgejo/Gitea, Azure DevOps, Bitbucket and GitCafe
//! remotes; those hosts are cut, so their remotes classify as `unknown`, exactly like any host
//! upstream cannot attribute.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::source_control::{SourceControlProviderInfo, SourceControlProviderKind};

use super::js::js_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// `None` (undefined provider) and `github` read as GitHub; anything else is generic.
pub fn resolve_change_request_presentation(
    provider: Option<&SourceControlProviderInfo>,
) -> ChangeRequestPresentation {
    match provider.map(|p| p.kind.as_str()) {
        None | Some("github") => GITHUB_CHANGE_REQUEST_PRESENTATION,
        Some(_) => GENERIC_CHANGE_REQUEST_PRESENTATION,
    }
}

pub fn get_change_request_terminology_for_kind(
    kind: &SourceControlProviderKind,
) -> ChangeRequestTerminology {
    let presentation = match kind.as_str() {
        "github" => GITHUB_CHANGE_REQUEST_PRESENTATION,
        _ => GENERIC_CHANGE_REQUEST_PRESENTATION,
    };
    ChangeRequestTerminology { short_label: presentation.short_name, singular: presentation.long_name }
}

static SCP_SSH_REMOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+@([^:/]+):").unwrap());

pub fn is_ssh_remote_url(remote_url: &str) -> bool {
    let trimmed = js_trim(remote_url);
    SCP_SSH_REMOTE.is_match(trimmed) || trimmed.to_lowercase().starts_with("ssh://")
}

/// Extracts the normalized host used for provider detection. SCP-style and SSH remotes return the
/// hostname only, while other URL schemes retain explicit ports for non-default web endpoints.
fn parse_remote_host(remote_url: &str) -> Option<String> {
    let trimmed = js_trim(remote_url);
    if trimmed.is_empty() {
        return None;
    }
    if let Some(host) = SCP_SSH_REMOTE.captures(trimmed).and_then(|c| c.get(1)).map(|m| m.as_str())
        && !host.is_empty()
    {
        return Some(host.to_lowercase());
    }
    let url = reqwest::Url::parse(trimmed).ok()?;
    let host = url.host_str()?;
    if url.scheme() == "ssh" {
        return Some(host.to_lowercase());
    }
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
    .to_lowercase())
}

fn parse_host_name(host: &str) -> String {
    match reqwest::Url::parse(&format!("https://{host}")).ok().and_then(|u| u.host_str().map(str::to_owned)) {
        Some(name) => name.to_lowercase(),
        None => {
            static PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":\d+$").unwrap());
            PORT.replace(host, "").to_lowercase()
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

pub fn detect_source_control_provider_from_remote_url(
    remote_url: &str,
) -> Option<SourceControlProviderInfo> {
    let host = parse_remote_host(remote_url)?;
    let hostname = parse_host_name(&host);

    let (kind, name) = if is_github_host(&hostname) {
        (
            "github",
            if hostname == "github.com" { "GitHub".to_owned() } else { "GitHub Self-Hosted".to_owned() },
        )
    } else {
        ("unknown", host.clone())
    };
    Some(SourceControlProviderInfo {
        kind: SourceControlProviderKind::new(kind).expect("valid kind"),
        name: TrimmedNonEmptyString::new(name).ok()?,
        base_url: to_base_url(&host),
    })
}

/// The provider-native repository selector. `display_name` is the full path below the host;
/// owner/name is the two-segment fallback for identities recorded before that field existed.
///
/// One function because everything downstream is keyed by what it answers: the rows' own
/// `repository`, the per-repository cursors, and the detail and diff reads a row leads to.
/// (Upstream's Azure DevOps special case is cut with the host.)
pub fn source_control_repository_selector(identity: Option<&RepositoryIdentity>) -> Option<String> {
    let identity = identity?;
    if let Some(display_name) = identity.display_name.as_ref().map(|d| d.as_str()).filter(|d| !d.is_empty()) {
        return Some(display_name.to_owned());
    }
    match (&identity.owner, &identity.name) {
        (Some(owner), Some(name)) if !owner.as_str().is_empty() && !name.as_str().is_empty() => {
            Some(format!("{}/{}", owner.as_str(), name.as_str()))
        }
        _ => None,
    }
}

pub fn canonical_repository_key(key: &str) -> String {
    static SSH_AZURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$").unwrap()
    });
    static LEGACY_AZURE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$").unwrap()
    });
    let step = SSH_AZURE.replace(key, "dev.azure.com/$1/$2/_git/$3");
    LEGACY_AZURE.replace(&step, "dev.azure.com/$1/$2/_git/$3").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(url: &str) -> Option<String> {
        detect_source_control_provider_from_remote_url(url).map(|p| p.kind.as_str().to_owned())
    }

    #[test]
    fn github_presentation_and_generic_fallback() {
        let github = resolve_change_request_presentation(None);
        assert_eq!(github.short_name, "PR");
        assert_eq!(github.checkout_command_example, Some("gh pr checkout 123"));
        let generic = get_change_request_terminology_for_kind(&SourceControlProviderKind::new("whatever").unwrap());
        assert_eq!(generic.short_label, "change request");
        assert_eq!(
            get_change_request_terminology_for_kind(&SourceControlProviderKind::new("github").unwrap()),
            ChangeRequestTerminology { short_label: "PR", singular: "pull request" }
        );
    }

    #[test]
    fn detects_github_hosts() {
        assert_eq!(kind_of("git@github.com:owner/repo.git").as_deref(), Some("github"));
        assert_eq!(kind_of("deploy@github.com:owner/repo.git").as_deref(), Some("github"));
        assert_eq!(kind_of("https://github.example.com/owner/repo.git").as_deref(), Some("github"));
        let enterprise = detect_source_control_provider_from_remote_url("https://github.example.com/o/r.git").unwrap();
        assert_eq!(enterprise.name.as_str(), "GitHub Self-Hosted");
        assert_eq!(enterprise.base_url, "https://github.example.com");
        assert_eq!(
            detect_source_control_provider_from_remote_url("git@github.com:o/r.git").unwrap().name.as_str(),
            "GitHub"
        );
    }

    #[test]
    fn cut_hosts_classify_as_unknown() {
        for url in [
            "https://gitlab.com/group/repo.git",
            "https://dev.azure.com/org/project/_git/repo",
            "git@bitbucket.org:workspace/repo.git",
            "http://codeberg.org:3000/team/repo.git",
        ] {
            assert_eq!(kind_of(url).as_deref(), Some("unknown"), "{url}");
        }
    }

    #[test]
    fn preserves_ports_while_classifying_by_hostname() {
        let p = detect_source_control_provider_from_remote_url("https://self-hosted.example.test:8443/group/repo.git").unwrap();
        assert_eq!(p.kind.as_str(), "unknown");
        assert_eq!(p.name.as_str(), "self-hosted.example.test:8443");
        assert_eq!(p.base_url, "https://self-hosted.example.test:8443");
        let ssh = detect_source_control_provider_from_remote_url("ssh://git@code.example.test:24/team/project.git").unwrap();
        assert_eq!(ssh.name.as_str(), "code.example.test");
        assert_eq!(ssh.base_url, "https://code.example.test");
    }

    #[test]
    fn does_not_match_provider_names_embedded_in_unrelated_dns_labels() {
        assert_eq!(kind_of("https://notgithub.example.com/owner/repo.git").as_deref(), Some("unknown"));
    }

    #[test]
    fn is_ssh_remote_url_cases() {
        for yes in [
            "git@github.com:owner/repo.git",
            "gitlab@gitlab.example.com:group/project.git",
            "ssh://git@gitlab.example.com:22/group/project.git",
            "SSH://git@gitlab.example.com/group/project.git",
            "SsH://git@gitlab.example.com/group/project.git",
        ] {
            assert!(is_ssh_remote_url(yes), "{yes}");
        }
        for no in ["https://gitlab.example.com/group/project.git", "/home/user/repos/project", "", "deploy@github.com/project/repo"] {
            assert!(!is_ssh_remote_url(no), "{no}");
        }
    }

    #[test]
    fn canonical_repository_key_rewrites_azure_spellings() {
        assert_eq!(
            canonical_repository_key("ssh.dev.azure.com/v3/o/p/r"),
            "dev.azure.com/o/p/_git/r"
        );
        assert_eq!(
            canonical_repository_key("o.visualstudio.com/defaultcollection/p/_git/r"),
            "dev.azure.com/o/p/_git/r"
        );
        assert_eq!(canonical_repository_key("github.com/o/r"), "github.com/o/r");
    }
}
