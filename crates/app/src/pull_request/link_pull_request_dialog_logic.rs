//! Port of resolveLinkPullRequestInput from upstream/apps/web/src/components/pullRequest/
//! LinkPullRequestDialog.tsx. The React dialog and host are skipped. URL parsing reuses the
//! existing shared change request parser; checkout-command extraction is the pure portion of
//! upstream/apps/web/src/pullRequestReference.ts.
use crate::state::support::change_request_url::parse_change_request_url;
use regex::Regex;
use std::sync::LazyLock;
use turin_contracts::js_trim;
static BARE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#?([0-9]+)$").unwrap());
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPullRequestResolution {
    Link(ResolvedLink),
    Error(String),
}
pub struct LinkPullRequestProject<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub web_url: &'a dyn Fn(u64) -> Option<String>,
}
/// URL owns its host and repository; a bare number needs this thread's own project.
pub fn resolve_link_pull_request_input(
    reference: &str,
    project: Option<&LinkPullRequestProject<'_>>,
    has_project: &dyn Fn(&ResolvedLink) -> bool,
) -> Option<LinkPullRequestResolution> {
    let trimmed = js_trim(reference);
    let parsed = if parse_change_request_url(trimmed).is_some() {
        trimmed.to_owned()
    } else {
        parse_pull_request_reference(trimmed)?
    };
    if let Some(url) = parse_change_request_url(&parsed) {
        let link = ResolvedLink {
            host: url.host,
            repository: url.repository,
            number: url.number,
            url: parsed,
        };
        if !has_project(&link) {
            return Some(LinkPullRequestResolution::Error(format!(
                "No project in this environment can read {}/{}.",
                link.host, link.repository
            )));
        }
        return Some(LinkPullRequestResolution::Link(link));
    }
    let number: u64 = parsed.parse().ok()?;
    if number == 0 || number > 9_007_199_254_740_991 {
        return None;
    }
    let Some(project) = project else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL to link a pull request from another repository.".to_owned(),
        ));
    };
    let _ = (project.host, project.repository);
    let Some(web_url) = (project.web_url)(number) else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL; this project's host has no known pull request URL.".to_owned(),
        ));
    };
    let Some(web_reference) = parse_change_request_url(&web_url) else {
        return Some(LinkPullRequestResolution::Error(
            "Paste a full URL; this project's host has no known pull request URL.".to_owned(),
        ));
    };
    Some(LinkPullRequestResolution::Link(ResolvedLink {
        host: web_reference.host,
        repository: web_reference.repository,
        number: web_reference.number,
        url: web_url,
    }))
}
fn parse_pull_request_reference(input: &str) -> Option<String> {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return None;
    }
    let normalized = if let Some(argument) = trimmed.strip_prefix("gh pr checkout ") {
        argument.trim()
    } else {
        trimmed
    };
    if parse_change_request_url(normalized).is_some() {
        return Some(normalized.to_owned());
    }
    BARE_NUMBER
        .captures(normalized)
        .map(|captures| captures[1].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn resolve(
        reference: &str,
        project: Option<&LinkPullRequestProject<'_>>,
        has_project: &dyn Fn(&ResolvedLink) -> bool,
    ) -> Option<LinkPullRequestResolution> {
        resolve_link_pull_request_input(reference, project, has_project)
    }
    fn project<'a>(url: &'a dyn Fn(u64) -> Option<String>) -> LinkPullRequestProject<'a> {
        LinkPullRequestProject {
            host: "github.com",
            repository: "acme/web",
            web_url: url,
        }
    }
    #[test]
    fn supported_host_urls_without_thread_project() {
        for (url, host) in [
            (
                "https://bitbucket.org/acme/web/pull-requests/42",
                "bitbucket.org",
            ),
            (
                "https://github.acme.test/acme/web/pull/42",
                "github.acme.test",
            ),
            (
                "https://git.acme.test/acme/web/-/merge_requests/42",
                "git.acme.test",
            ),
        ] {
            assert_eq!(
                resolve(&format!(" {url} "), None, &|candidate| candidate.host
                    == host),
                Some(LinkPullRequestResolution::Link(ResolvedLink {
                    host: host.to_owned(),
                    repository: "acme/web".to_owned(),
                    number: 42,
                    url: url.to_owned()
                }))
            );
        }
    }
    #[test]
    fn validates_full_azure_repository() {
        let allowed = |r: &ResolvedLink| {
            r.host == "dev.azure.com" && r.repository == "org-a/project/_git/web"
        };
        assert!(matches!(
            resolve(
                "https://dev.azure.com/org-a/project/_git/web/pullrequest/42",
                None,
                &allowed
            ),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                number: 42,
                ..
            }))
        ));
        let denied = resolve(
            "https://dev.azure.com/org-b/project/_git/web/pullrequest/42",
            None,
            &allowed,
        );
        assert!(
            matches!(denied,Some(LinkPullRequestResolution::Error(message)) if message.contains("org-b/project/_git/web"))
        );
    }
    #[test]
    fn bare_azure_number_to_canonical_url() {
        let web = |n| {
            Some(format!(
                "https://dev.azure.com/org/project/_git/web/pullrequest/{n}"
            ))
        };
        let project = LinkPullRequestProject {
            host: "ssh.dev.azure.com",
            repository: "v3/org/project/web",
            web_url: &web,
        };
        assert_eq!(
            resolve("#42", Some(&project), &|_| true),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "dev.azure.com".to_owned(),
                repository: "org/project/_git/web".to_owned(),
                number: 42,
                url: "https://dev.azure.com/org/project/_git/web/pullrequest/42".to_owned()
            }))
        );
    }
    #[test]
    fn non_reference_is_none() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(resolve("hello", Some(&project(&web)), &|_| true), None);
    }
    #[test]
    fn bare_number_resolves_against_project() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(
            resolve("#42", Some(&project(&web)), &|_| true),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "github.com".to_owned(),
                repository: "acme/web".to_owned(),
                number: 42,
                url: "https://github.com/acme/web/pull/42".to_owned()
            }))
        );
    }
    #[test]
    fn another_repository_same_host() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert_eq!(
            resolve(
                "https://github.com/acme/api/pull/7",
                Some(&project(&web)),
                &|r| r.host == "github.com"
            ),
            Some(LinkPullRequestResolution::Link(ResolvedLink {
                host: "github.com".to_owned(),
                repository: "acme/api".to_owned(),
                number: 7,
                url: "https://github.com/acme/api/pull/7".to_owned()
            }))
        );
    }
    #[test]
    fn host_without_checkout_refused() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert!(
            matches!(resolve("https://gitlab.com/acme/api/-/merge_requests/7",Some(&project(&web)),&|_|false),Some(LinkPullRequestResolution::Error(message)) if message.contains("gitlab.com"))
        );
    }
    #[test]
    fn bare_number_without_project_asks_for_url() {
        assert!(
            matches!(resolve("12",None,&|_|true),Some(LinkPullRequestResolution::Error(message)) if message.contains("full URL"))
        );
    }
    #[test]
    fn checkout_command_is_reference() {
        let web = |n| Some(format!("https://github.com/acme/web/pull/{n}"));
        assert!(
            matches!(resolve("gh pr checkout https://github.com/acme/web/pull/3",Some(&project(&web)),&|_|true),Some(LinkPullRequestResolution::Link(ResolvedLink{number:3,repository,..})) if repository=="acme/web")
        );
    }
    #[test]
    fn known_web_url_and_unknown_returns_none() {
        assert!(
            parse_change_request_url("https://gitlab.com/g/sub/repo/-/merge_requests/5").is_some()
        );
        assert!(parse_change_request_url("https://x/a/b/pull/1").is_none());
    }
}
