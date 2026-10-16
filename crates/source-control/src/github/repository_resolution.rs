//! Port of upstream/packages/source-control-github/src/server/gitHubRepositoryResolution.ts

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;

use crate::shared::git::normalize_git_remote_url;
use crate::shared::js::js_trim;
use crate::shared::source_control::{
    detect_source_control_provider_from_remote_url, is_ssh_remote_url,
};

/// A repository on a GitHub host: the API it is read through, and its owner and name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubRepositoryLocator {
    pub host: String,
    pub owner: String,
    pub name: String,
}

/// A repository without its host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubRepositoryName {
    pub owner: String,
    pub name: String,
}

/// A remote of `git remote -v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRemote {
    pub name: String,
    pub url: String,
}

static FETCH_REMOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+(\S+)\s+\(fetch\)$").unwrap());
static GH_RESOLVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^remote\.(.+)\.gh-resolved\s+(\S+)$").unwrap());
static DOT_GIT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.git$").unwrap());
static HTTP_URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());
static PR_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#?([1-9][0-9]*)$").unwrap());
static PR_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+)/([^/]+)/pull/([1-9][0-9]*)(?:/.*)?$").unwrap());

/// The fetch remotes in `git remote -v` output, by name.
pub fn parse_fetch_remotes(remotes: &str) -> Vec<FetchRemote> {
    remotes
        .split('\n')
        .filter_map(|line| FETCH_REMOTE.captures(js_trim(line)))
        .map(|captures| FetchRemote {
            name: captures[1].to_owned(),
            url: captures[2].to_owned(),
        })
        .collect()
}

pub struct SelectGitHubBaseRepositoryInput<'a> {
    pub remotes: &'a str,
    pub resolved: &'a str,
    pub host: &'a str,
}

/// The repository `gh pr list` reads in a checkout, picked the way gh picks one without a
/// prompt: the remote `gh repo set-default` marked, else the first of upstream, github, origin
/// (in any case), else the only remote. `remotes` is `git remote -v` output and `resolved` is the
/// output of `git config --get-regexp '^remote\..*\.gh-resolved$'`.
///
/// None whenever gh might weigh the remotes differently: a remote on another host or under an
/// SSH alias, more than one mark, or several remotes with none of those names. Callers then
/// fall back to the provider's remote, then to the best-ranked remote on the host.
pub fn select_github_base_repository(
    input: &SelectGitHubBaseRepositoryInput<'_>,
) -> Option<GitHubRepositoryName> {
    let host = input.host.to_lowercase();
    let mut repositories: IndexMap<String, GitHubRepositoryName> = IndexMap::new();
    for remote in parse_fetch_remotes(input.remotes) {
        let normalized = normalize_git_remote_url(&remote.url);
        let mut parts = normalized.split('/');
        let remote_host = parts.next();
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if remote_host != Some(host.as_str())
            || owner.is_empty()
            || name.is_empty()
            || parts.next().is_some()
        {
            return None;
        }
        repositories.insert(
            remote.name,
            GitHubRepositoryName {
                owner: owner.to_owned(),
                name: name.to_owned(),
            },
        );
    }
    let marks: Vec<(String, String)> = input
        .resolved
        .split('\n')
        .filter_map(|line| GH_RESOLVED.captures(js_trim(line)))
        .filter(|captures| repositories.contains_key(&captures[1]))
        .map(|captures| (captures[1].to_owned(), captures[2].to_owned()))
        .collect();
    if marks.len() > 1 {
        return None;
    }
    if let Some((remote_name, value)) = marks.into_iter().next() {
        if value == "base" {
            return repositories.get(&remote_name).cloned();
        }
        let lowered = value.to_lowercase();
        let mut parts = lowered.split('/');
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        return (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| {
            GitHubRepositoryName {
                owner: owner.to_owned(),
                name: name.to_owned(),
            }
        });
    }
    // gh sorts remotes by these names, case-insensitively, and takes the first. A tie for the
    // top place has no defined winner.
    let score = |remote_name: &str| -> usize {
        ["origin", "github", "upstream"]
            .iter()
            .position(|name| *name == remote_name.to_lowercase())
            .map_or(0, |index| index + 1)
    };
    let mut ranked: Vec<(&String, &GitHubRepositoryName)> = repositories.iter().collect();
    ranked.sort_by_key(|(name, _)| std::cmp::Reverse(score(name)));
    let top = ranked.first()?;
    let beaten = ranked
        .get(1)
        .is_none_or(|next| score(top.0) > score(next.0));
    beaten.then(|| top.1.clone())
}

pub struct ResolveGitHubRepositoryInput<'a> {
    pub remotes: &'a str,
    pub resolved: &'a str,
    /// The checkout's GitHub API host, when the caller already knows it.
    pub host_hint: Option<&'a str>,
    pub default_host: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGitHubRepository {
    pub host: String,
    pub locator: Option<GitHubRepositoryLocator>,
}

/// The repository `gh` would act on in a checkout, from its remotes: the remote `gh` would pick,
/// else the best-ranked remote on the host. `host` is the API host it settled on, which a caller
/// names when no repository was found there. GH_REPO is the caller's to check first.
pub fn resolve_github_repository(
    input: &ResolveGitHubRepositoryInput<'_>,
) -> ResolvedGitHubRepository {
    struct RemoteWithHost {
        name: String,
        url: String,
        host: Option<String>,
    }
    let fetch_remotes: Vec<RemoteWithHost> = parse_fetch_remotes(input.remotes)
        .into_iter()
        .map(|remote| RemoteWithHost {
            host: github_api_host_for_remote(&remote.url),
            name: remote.name,
            url: remote.url,
        })
        .collect();
    let host = input
        .host_hint
        .map(api_host_for_hint)
        .or_else(|| {
            fetch_remotes
                .iter()
                .find(|remote| remote.name == "origin" && remote.host.is_some())
                .and_then(|remote| remote.host.clone())
        })
        .or_else(|| fetch_remotes.iter().find_map(|remote| remote.host.clone()))
        .unwrap_or_else(|| input.default_host.to_owned());
    let selected = select_github_base_repository(&SelectGitHubBaseRepositoryInput {
        remotes: input.remotes,
        resolved: input.resolved,
        host: &host,
    });
    if let Some(selected) = selected {
        return ResolvedGitHubRepository {
            locator: Some(GitHubRepositoryLocator {
                host: host.clone(),
                owner: selected.owner,
                name: selected.name,
            }),
            host,
        };
    }
    // gh's own order without its prompt: upstream, github, origin, then the first remote, among
    // the ones on this host (an SSH alias counts as its API host).
    let rank = |name: &str| -> usize {
        ["upstream", "github", "origin"]
            .iter()
            .position(|candidate| *candidate == name.to_lowercase())
            .unwrap_or(99)
    };
    let mut candidates: Vec<&RemoteWithHost> = fetch_remotes
        .iter()
        .filter(|remote| {
            remote.host.as_deref() == Some(host.as_str())
                || normalize_git_remote_url(&remote.url).split('/').next() == Some(host.as_str())
        })
        .collect();
    candidates.sort_by_key(|remote| rank(&remote.name));
    for remote in candidates {
        let normalized = normalize_git_remote_url(&remote.url);
        let mut parts = normalized.split('/').skip(1);
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if !owner.is_empty() && !name.is_empty() && parts.next().is_none() {
            return ResolvedGitHubRepository {
                locator: Some(GitHubRepositoryLocator {
                    host: host.clone(),
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                }),
                host,
            };
        }
    }
    ResolvedGitHubRepository {
        host,
        locator: None,
    }
}

/// `url.host`: the hostname, with the port unless it is the scheme's default.
fn url_host(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// `owner/name` or `host/owner/name`, as `gh --repo` and GH_REPO take them.
pub fn parse_github_repository_selector(
    selector: &str,
    default_host: &str,
) -> Option<GitHubRepositoryLocator> {
    let trimmed = DOT_GIT_SUFFIX.replace(js_trim(selector), "").into_owned();
    if HTTP_URL.is_match(&trimmed) {
        let url = reqwest::Url::parse(&trimmed).ok()?;
        let mut segments = url.path().split('/').filter(|part| !part.is_empty());
        let owner = segments.next()?;
        let name = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        return Some(GitHubRepositoryLocator {
            host: url_host(&url)?.to_lowercase(),
            owner: owner.to_owned(),
            name: name.to_owned(),
        });
    }
    let parts: Vec<&str> = trimmed.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [owner, name] => Some(GitHubRepositoryLocator {
            host: default_host.to_owned(),
            owner: (*owner).to_owned(),
            name: (*name).to_owned(),
        }),
        [host, owner, name] => Some(GitHubRepositoryLocator {
            host: host.to_lowercase(),
            owner: (*owner).to_owned(),
            name: (*name).to_owned(),
        }),
        _ => None,
    }
}

/// A pull request reference the way `gh pr view` takes one: a number (`#7` too), a pull request
/// URL, or a branch name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestReference {
    Number {
        number: u64,
    },
    Url {
        locator: GitHubRepositoryLocator,
        number: u64,
    },
    Branch {
        head_selector: String,
    },
}

pub fn parse_pull_request_reference(reference: &str) -> PullRequestReference {
    let trimmed = js_trim(reference);
    if let Some(numbered) = PR_NUMBER.captures(trimmed) {
        return PullRequestReference::Number {
            number: numbered[1].parse().unwrap_or(u64::MAX),
        };
    }
    if HTTP_URL.is_match(trimmed)
        // Not a URL after all; read it as a branch.
        && let Ok(url) = reqwest::Url::parse(trimmed)
        && let Some(matched) = PR_PATH.captures(url.path())
        && let Some(host) = url_host(&url)
    {
        return PullRequestReference::Url {
            locator: GitHubRepositoryLocator {
                host: host.to_lowercase(),
                owner: matched[1].to_owned(),
                name: matched[2].to_owned(),
            },
            number: matched[3].parse().unwrap_or(u64::MAX),
        };
    }
    PullRequestReference::Branch {
        head_selector: trimmed.to_owned(),
    }
}

/// The GitHub host a remote URL is served from, or None for a remote that is not GitHub. An SSH
/// alias (`git@github-work:owner/repo`) names no API host of its own; it is read through
/// `github.com`, which is what such an alias almost always stands for (issue #6198).
pub fn github_api_host_for_remote(remote_url: &str) -> Option<String> {
    let provider = detect_source_control_provider_from_remote_url(remote_url)?;
    let host = url_host(&reqwest::Url::parse(&provider.base_url).ok()?)?.to_lowercase();
    // A dotless SSH host is an alias from ~/.ssh/config, never a real API host.
    if is_ssh_remote_url(remote_url) && !host.contains('.') {
        return host.contains("github").then(|| "github.com".to_owned());
    }
    (provider.kind.as_str() == "github").then_some(host)
}

/// A caller's host hint, read the way a remote's host is: a dotless alias is not an API host.
fn api_host_for_hint(host: &str) -> String {
    let normalized = host.to_lowercase();
    if !normalized.contains('.') && normalized.contains("github") {
        "github.com".to_owned()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remotes(entries: &[(&str, &str)]) -> String {
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

    fn select(remotes: &str, resolved: &str) -> Option<(String, String)> {
        select_github_base_repository(&SelectGitHubBaseRepositoryInput {
            remotes,
            resolved,
            host: "github.com",
        })
        .map(|repository| (repository.owner, repository.name))
    }

    fn pair(owner: &str, name: &str) -> Option<(String, String)> {
        Some((owner.to_owned(), name.to_owned()))
    }

    #[test]
    fn picks_the_repository_gh_reads_without_a_prompt() {
        assert_eq!(
            select(&remotes(&[("fork", "git@github.com:me/web.git")]), ""),
            pair("me", "web")
        );
        let fork = remotes(&[
            ("origin", "git@github.com:me/web.git"),
            ("upstream", "https://github.com/Acme/Web.git"),
        ]);
        assert_eq!(select(&fork, ""), pair("acme", "web"));
        assert_eq!(
            select(&fork, "remote.origin.gh-resolved base"),
            pair("me", "web")
        );
        assert_eq!(
            select(&fork, "remote.origin.gh-resolved acme/other"),
            pair("acme", "other")
        );
        // gh ranks remote names in any case.
        assert_eq!(
            select(
                &remotes(&[
                    ("origin", "git@github.com:me/web.git"),
                    ("Upstream", "git@github.com:acme/web.git"),
                ]),
                ""
            ),
            pair("acme", "web")
        );
    }

    #[test]
    fn leaves_gh_to_choose_when_it_might_weigh_the_remotes_differently() {
        let cases: Vec<(String, &str)> = vec![
            (String::new(), ""),
            (
                remotes(&[
                    ("a", "git@github.com:me/web.git"),
                    ("b", "git@github.com:acme/web.git"),
                ]),
                "",
            ),
            (
                remotes(&[
                    ("origin", "git@github.com:me/web.git"),
                    ("mirror", "git@gitlab.com:me/web.git"),
                ]),
                "",
            ),
            (remotes(&[("origin", "git@github-work:me/web.git")]), ""),
            (
                remotes(&[
                    ("origin", "git@github.com:me/web.git"),
                    ("Origin", "git@github.com:acme/web.git"),
                ]),
                "",
            ),
            (
                remotes(&[
                    ("origin", "git@github.com:me/web.git"),
                    ("upstream", "git@github.com:acme/web.git"),
                ]),
                "remote.origin.gh-resolved base\nremote.upstream.gh-resolved base",
            ),
        ];
        for (remotes, resolved) in cases {
            assert_eq!(select(&remotes, resolved), None, "{remotes:?}");
        }
    }

    #[test]
    fn reads_an_ssh_alias_through_github_com_and_a_real_host_as_itself() {
        assert_eq!(
            github_api_host_for_remote("git@github:acme/web.git"),
            Some("github.com".into())
        );
        assert_eq!(
            github_api_host_for_remote("git@github.example.com:a/b.git"),
            Some("github.example.com".into())
        );
        assert_eq!(github_api_host_for_remote("git@gitlab.com:a/b.git"), None);
        assert_eq!(github_api_host_for_remote("git@work:a/b.git"), None);
    }

    #[test]
    fn parses_fetch_remotes_only() {
        assert_eq!(
            parse_fetch_remotes(
                "origin\tgit@github.com:a/b.git (fetch)\norigin\tgit@github.com:a/b.git (push)\n  up\thttps://x/y (fetch)  \nbad line"
            ),
            vec![
                FetchRemote {
                    name: "origin".into(),
                    url: "git@github.com:a/b.git".into()
                },
                FetchRemote {
                    name: "up".into(),
                    url: "https://x/y".into()
                },
            ]
        );
    }

    #[test]
    fn resolves_the_repository_gh_would_act_on() {
        let resolve = |remotes: &str, hint: Option<&str>| {
            resolve_github_repository(&ResolveGitHubRepositoryInput {
                remotes,
                resolved: "",
                host_hint: hint,
                default_host: "github.com",
            })
        };
        let locator = |host: &str, owner: &str, name: &str| {
            Some(GitHubRepositoryLocator {
                host: host.into(),
                owner: owner.into(),
                name: name.into(),
            })
        };
        // The marked/ranked remote on the host of origin.
        let found = resolve(
            &remotes(&[("origin", "git@github.example.com:me/web.git")]),
            None,
        );
        assert_eq!(found.host, "github.example.com");
        assert_eq!(found.locator, locator("github.example.com", "me", "web"));
        // An SSH alias counts as github.com.
        let alias = resolve(&remotes(&[("origin", "git@github-work:me/web.git")]), None);
        assert_eq!(alias.host, "github.com");
        assert_eq!(alias.locator, locator("github.com", "me", "web"));
        // The best-ranked remote when gh would have to prompt.
        let two = remotes(&[
            ("a", "git@github.com:me/one.git"),
            ("origin", "git@github.com:me/two.git"),
            ("b", "git@github.com:me/three.git"),
        ]);
        assert_eq!(
            resolve(&two, None).locator,
            locator("github.com", "me", "two")
        );
        // A hint wins; no remotes falls back to the default host.
        assert_eq!(
            resolve(&two, Some("GHE.example.com")).host,
            "ghe.example.com"
        );
        assert_eq!(resolve(&two, Some("ghe.example.com")).locator, None);
        assert_eq!(
            resolve("", None),
            ResolvedGitHubRepository {
                host: "github.com".into(),
                locator: None
            }
        );
    }

    #[test]
    fn parses_repository_selectors() {
        let locator = |host: &str, owner: &str, name: &str| {
            Some(GitHubRepositoryLocator {
                host: host.into(),
                owner: owner.into(),
                name: name.into(),
            })
        };
        assert_eq!(
            parse_github_repository_selector(" acme/web.git ", "github.com"),
            locator("github.com", "acme", "web")
        );
        assert_eq!(
            parse_github_repository_selector("GHE.example.com/acme/web", "github.com"),
            locator("ghe.example.com", "acme", "web")
        );
        assert_eq!(
            parse_github_repository_selector("https://GitHub.com/acme/web.git", "x"),
            locator("github.com", "acme", "web")
        );
        assert_eq!(
            parse_github_repository_selector("https://ghe.example.com:8443/acme/web", "x"),
            locator("ghe.example.com:8443", "acme", "web")
        );
        assert_eq!(
            parse_github_repository_selector("https://github.com/acme", "x"),
            None
        );
        assert_eq!(
            parse_github_repository_selector("https://github.com/a/b/c", "x"),
            None
        );
        assert_eq!(parse_github_repository_selector("web", "x"), None);
        assert_eq!(parse_github_repository_selector("a/b/c/d", "x"), None);
    }

    #[test]
    fn parses_pull_request_references() {
        assert_eq!(
            parse_pull_request_reference("#7"),
            PullRequestReference::Number { number: 7 }
        );
        assert_eq!(
            parse_pull_request_reference(" 42 "),
            PullRequestReference::Number { number: 42 }
        );
        assert_eq!(
            parse_pull_request_reference("https://GitHub.com/acme/web/pull/12/files"),
            PullRequestReference::Url {
                locator: GitHubRepositoryLocator {
                    host: "github.com".into(),
                    owner: "acme".into(),
                    name: "web".into()
                },
                number: 12
            }
        );
        for branch in [
            "0",
            "feature/x",
            "https://github.com/acme/web/issues/1",
            "https://%zz",
        ] {
            assert_eq!(
                parse_pull_request_reference(branch),
                PullRequestReference::Branch {
                    head_selector: branch.into()
                },
                "{branch}"
            );
        }
    }
}
