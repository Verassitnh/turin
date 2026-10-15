//! Port of upstream/packages/shared/src/changeRequestUrl.ts
//!
//! GitHub only: GitCafe, Forgejo, GitLab, Bitbucket and Azure DevOps are cut
//! (PORTING cut list), so a URL that upstream would hand to one of those hosts
//! is `None` here, and `changeRequestUrlFor` has no branch for them. The
//! GitHub branch of every other export is ported.
//!
//! Upstream reads URLs with the WHATWG `URL` parser. [`parse_http_url`]
//! implements the slice of it that [`parse_change_request_url`] observes; the
//! exports below that build URLs use the `url` crate, which is the same parser.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;

/// A change request named the way a thread link names one: the host below which the repository
/// is addressed, the repository path as that host writes it, and the number.
///
/// The two strings are what `pullRequestHostOf` and the project's `repositoryIdentity` produce
/// from a git remote — lower case, no port, the full path below the host — because links are
/// matched against those. Anything else matches nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port, separate from the portless repository identity.
    /// Forgejo is cut, so this is always `None`; kept for upstream's shape.
    pub authority: Option<String>,
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex
        || hostname
            .strip_suffix(apex)
            .is_some_and(|rest| rest.ends_with('.'))
    {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

// The `u` flag keeps `\d` ASCII in JS; Rust's `\d` is Unicode, so spell it out.
static GITHUB_PULL_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)").expect("static regex"));

/// The repository and number behind a change request URL on a host this can read, or null for
/// anything else — an issue, a commit, a repository root, a host this cannot tell apart from an
/// ordinary link. A doubtful match is worse than no match, so nothing here guesses.
///
/// Each host is recognised by the path shape it alone uses, guarded by a hostname it could
/// plausibly be served from, since self-hosted installs are named whatever their admin chose:
/// `/pull/` is generic enough that it is only believed from a GitHub-ish host.
///
/// Nothing here tries to tell a lookalike hostname from a real one — `github.com.evil.test` and
/// the rest are an open set, and blocking spellings of it costs real hosts. What a claim is worth
/// is decided where it is used.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = parse_http_url(target_url)?;
    // `javascript:`, `mailto:` and friends have no host to speak of and nothing to open.
    // (`parse_http_url` only accepts `http:` and `https:`.)
    let host = url.hostname.to_lowercase();

    // GitHub, and any Enterprise install: /{owner}/{repo}/pull/{n}
    if is_host_of(&host, "github.com", Some("github")) {
        let captures = GITHUB_PULL_PATH.captures(&url.pathname);
        if captures.is_some() {
            return claim(&host, captures);
        }
    }
    // Upstream continues with Forgejo/Gitea (`/pulls/`), GitLab (`/-/merge_requests/`),
    // Bitbucket and Azure DevOps here. Those hosts are cut.
    None
}

fn claim(host: &str, captures: Option<regex::Captures<'_>>) -> Option<ChangeRequestLink> {
    let captures = captures?;
    let repository = captures.get(1)?.as_str();
    // `Number(digits)` then `Number.isSafeInteger(number) && number > 0`.
    let number = captures.get(2)?.as_str().parse::<u64>().ok()?;
    const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
    if repository.is_empty() || number == 0 || number > MAX_SAFE_INTEGER {
        return None;
    }
    Some(ChangeRequestLink {
        host: host.to_string(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

/// The parts of a WHATWG `URL` that [`parse_change_request_url`] reads.
#[derive(Debug, PartialEq, Eq)]
struct HttpUrl {
    hostname: String,
    pathname: String,
}

/// `new URL(input)` restricted to `http:`/`https:`; `None` where `new URL`
/// throws or yields another protocol.
///
/// Follows the WHATWG URL standard for special schemes: surrounding C0
/// controls and spaces are stripped and tabs/newlines removed; any run of `/`
/// or `\` after the scheme is skipped; userinfo and port are dropped from the
/// host (an invalid port throws); `\` separates path segments; `.`/`..`
/// segments (including `%2e` spellings) are resolved; and path bytes outside
/// the path percent-encode set's complement are percent-encoded. Not
/// implemented: IDNA (non-ASCII hosts are rejected), percent-decoding of
/// hosts (`%` in a host is rejected), and IPv4 number normalisation.
fn parse_http_url(input: &str) -> Option<HttpUrl> {
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();

    let colon = cleaned.find(':')?;
    let scheme = &cleaned[..colon];
    let mut scheme_chars = scheme.chars();
    if !scheme_chars.next()?.is_ascii_alphabetic()
        || !scheme_chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let rest = cleaned[colon + 1..].trim_start_matches(['/', '\\']);
    let authority_end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let after_authority = &rest[authority_end..];

    let host_and_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let (host, port) = split_host_port(host_and_port)?;
    if host.is_empty() || !is_valid_host(host) {
        return None;
    }
    if let Some(port) = port
        && !port.is_empty()
        && (!port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u32>().ok()? > 65_535)
    {
        return None;
    }

    let path_end = after_authority
        .find(['?', '#'])
        .unwrap_or(after_authority.len());
    let pathname = normalize_special_path(&after_authority[..path_end]);
    Some(HttpUrl {
        hostname: host.to_ascii_lowercase(),
        pathname,
    })
}

/// Splits `host[:port]`, where a `:` inside `[...]` (IPv6) is not a port separator.
fn split_host_port(value: &str) -> Option<(&str, Option<&str>)> {
    if value.starts_with('[') {
        let close = value.find(']')?;
        let host = &value[..=close];
        let rest = &value[close + 1..];
        return match rest.strip_prefix(':') {
            Some(port) => Some((host, Some(port))),
            None if rest.is_empty() => Some((host, None)),
            None => None,
        };
    }
    Some(match value.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    })
}

fn is_valid_host(host: &str) -> bool {
    if host.starts_with('[') {
        // IPv6 literal; never a GitHub-ish host, so only shape-check it.
        return host.ends_with(']')
            && host[1..host.len() - 1]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.');
    }
    // Forbidden domain code points, plus non-ASCII (no IDNA) and `%` (no host decoding).
    host.bytes().all(|b| {
        b.is_ascii()
            && b > 0x20
            && b != 0x7f
            && !matches!(
                b,
                b'#' | b'%'
                    | b'/'
                    | b':'
                    | b'<'
                    | b'>'
                    | b'?'
                    | b'@'
                    | b'['
                    | b'\\'
                    | b']'
                    | b'^'
                    | b'|'
            )
    })
}

fn is_single_dot_segment(segment: &str) -> bool {
    segment == "." || segment.eq_ignore_ascii_case("%2e")
}

fn is_double_dot_segment(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        ".." | ".%2e" | "%2e." | "%2e%2e"
    )
}

/// The path state of the WHATWG parser for a special scheme.
fn normalize_special_path(path: &str) -> String {
    // A special URL always has a path; an empty one is "/".
    let path = path.strip_prefix(['/', '\\']).unwrap_or(path);
    let raw_segments: Vec<&str> = path.split(['/', '\\']).collect();
    let mut segments: Vec<String> = Vec::new();
    let last_index = raw_segments.len().saturating_sub(1);
    for (index, segment) in raw_segments.iter().enumerate() {
        let is_last = index == last_index;
        if is_double_dot_segment(segment) {
            segments.pop();
            if is_last {
                segments.push(String::new());
            }
        } else if is_single_dot_segment(segment) {
            if is_last {
                segments.push(String::new());
            }
        } else {
            segments.push(percent_encode_path_segment(segment));
        }
    }
    format!("/{}", segments.join("/"))
}

/// The path percent-encode set: C0 controls, space, `"`, `#`, `<`, `>`, `?`,
/// `` ` ``, `{`, `}`, and everything above `~`. (Node does not encode `^`.)
fn percent_encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        let encode = !(0x20..=0x7e).contains(&byte)
            || matches!(
                byte,
                b' ' | b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}'
            );
        if encode {
            encoded.push_str(&format!("%{byte:02X}"));
        } else {
            encoded.push(char::from(byte));
        }
    }
    encoded
}

/// `Number.isSafeInteger(number) && number >= 1`
fn is_pull_request_number(number: u64) -> bool {
    (1..=(1 << 53) - 1).contains(&number)
}

/// The web URL GitHub writes for a change request; `None` for every other kind (cut hosts).
pub fn change_request_url_for(
    kind: Option<&str>,
    host: &str,
    repository: &str,
    number: u64,
) -> Option<String> {
    match kind? {
        "github" => Some(format!("https://{host}/{repository}/pull/{number}")),
        _ => None,
    }
}

/// `/pull/\d+(?:/|$)` at the start of `rest`, the GitHub route a sibling is built on.
fn starts_with_pull_route(rest: &str) -> bool {
    static PULL_ROUTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^/pull/[0-9]+(?:/|$)").expect("static regex"));
    PULL_ROUTE.is_match(rest)
}

/// The pull-request URL a GitHub-style `#123` autolink might name. GitHub writes every bare
/// reference through `/issues/`, including pull requests, so this only builds a candidate: the
/// caller must successfully read it as a pull request before treating it as one.
pub fn pull_request_candidate_url_from_reference_autolink(target_url: &str) -> Option<String> {
    static ISSUE_PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^/([^/]+/[^/]+)/issues/([0-9]+)(?:/|$)").expect("static regex")
    });
    let mut url = url::Url::parse(target_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if !(host == "github.com"
        || host.ends_with(".github.com")
        || host.split('.').any(|label| label == "github"))
    {
        return None;
    }
    let captures = ISSUE_PATH.captures(url.path())?;
    let path = format!("/{}/pull/{}", &captures[1], &captures[2]);
    url.set_path(&path);
    Some(url.to_string())
}

/// Match a stored PR without requiring its project to remain available.
pub fn matches_linked_pull_request_url(
    linked_pull_request: &ThreadLinkedPullRequest,
    target_url: &str,
) -> bool {
    match (
        parse_change_request_url(linked_pull_request.url.as_str()),
        parse_change_request_url(target_url),
    ) {
        (Some(linked), Some(target)) => {
            linked.host == target.host
                && linked.repository == target.repository
                && linked.number == target.number
                && linked.authority == target.authority
        }
        _ => false,
    }
}

/// The repository root behind a recognised GitHub pull-request URL: the path before `/pull/N`.
pub fn change_request_repository_url(target_url: &str) -> Option<String> {
    parse_change_request_url(target_url)?;
    let mut url = url::Url::parse(target_url).ok()?;
    let path = url.path().to_owned();
    let repository_path = (0..path.len())
        .filter(|&index| path.is_char_boundary(index))
        .find(|&index| pull_route_matches(&path.as_bytes()[index..]))
        .map(|index| path[..index].to_owned())?;
    if repository_path.is_empty() {
        return None;
    }
    url.set_path(&repository_path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

/// `/pull(s)?/<digits>` at the start of `rest`, ASCII case-insensitive and nothing allocated. The
/// digits must end the path or be followed by `/`.
fn pull_route_matches(rest: &[u8]) -> bool {
    let Some(after) = rest
        .get(..5)
        .filter(|head| head.eq_ignore_ascii_case(b"/pull"))
        .map(|_| &rest[5..])
    else {
        return false;
    };
    let after = match after.first() {
        Some(b's' | b'S') => &after[1..],
        _ => after,
    };
    let Some(digits) = after.strip_prefix(b"/") else {
        return false;
    };
    let digit_count = digits
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    digit_count > 0 && matches!(digits.get(digit_count), None | Some(b'/'))
}

/// `siblingPullRequestUrl`: the same GitHub pull-request URL, pointed at another number.
pub fn sibling_pull_request_url(target_url: &str, number: u64) -> Option<String> {
    let reference = parse_change_request_url(target_url)?;
    if !is_pull_request_number(number) {
        return None;
    }
    let mut sibling = url::Url::parse(target_url).ok()?;
    // Upstream slices the pathname past the repository, then requires the route to follow it.
    let rest = sibling.path().get(reference.repository.len() + 1..)?;
    if !starts_with_pull_route(rest) {
        return None;
    }
    sibling.set_path(&format!("/{}/pull/{number}", reference.repository));
    sibling.set_query(None);
    sibling.set_fragment(None);
    Some(sibling.to_string())
}

/// Builds a GitHub URL that remains available when the pull request API cannot be read.
pub fn git_hub_pull_request_browser_url(
    identity: Option<&RepositoryIdentity>,
    repository: &str,
    number: u64,
) -> Option<String> {
    let identity = identity?;
    if identity.provider.as_ref().map(|p| p.as_str()) != Some("github")
        || !is_pull_request_number(number)
    {
        return None;
    }
    let repository_path: Vec<&str> = repository.split('/').collect();
    if repository_path.len() != 2
        || repository_path
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return None;
    }

    let remote = url::Url::parse(js_trim(identity.locator.remote_url.as_str())).ok();
    let origin = remote
        .as_ref()
        .filter(|remote| matches!(remote.scheme(), "http" | "https"))
        .map(|remote| remote.origin().ascii_serialization());
    // SCP-style remotes are read from their normalized identity.
    let hostname = identity
        .canonical_key
        .as_str()
        .split('/')
        .next()
        .unwrap_or("");
    if origin.is_none() && hostname.is_empty() {
        return None;
    }
    let base = origin.unwrap_or_else(|| format!("https://{hostname}"));
    let mut url = url::Url::parse(&base).ok()?;
    url.set_path(&format!("/{}/pull/{number}", repository_path.join("/")));
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(host: &str, repository: &str, number: u64) -> Option<ChangeRequestLink> {
        Some(ChangeRequestLink {
            host: host.to_string(),
            repository: repository.to_string(),
            number,
            authority: None,
        })
    }

    #[test]
    fn reads_a_github_pull_request_lower_casing_the_repository() {
        assert_eq!(
            parse_change_request_url("https://github.com/T3Tools/T3Code/pull/123"),
            link("github.com", "t3tools/t3code", 123)
        );
    }

    #[test]
    fn reads_a_pull_request_on_a_github_enterprise_host() {
        assert_eq!(
            parse_change_request_url("https://github.acme.test/platform/api/pull/7"),
            link("github.acme.test", "platform/api", 7)
        );
    }

    #[test]
    fn reads_a_supported_github_host_with_a_middle_dns_label() {
        assert_eq!(
            parse_change_request_url("https://code.github.example.com/acme/web/pull/42"),
            link("code.github.example.com", "acme/web", 42)
        );
    }

    #[test]
    fn survives_trailing_segments_a_trailing_slash_and_a_query_string() {
        assert_eq!(
            parse_change_request_url("https://github.com/t3tools/t3code/pull/123/files?w=1"),
            link("github.com", "t3tools/t3code", 123)
        );
        assert_eq!(
            parse_change_request_url("https://github.com/t3tools/t3code/pull/123/"),
            link("github.com", "t3tools/t3code", 123)
        );
    }

    #[test]
    fn claims_nothing_it_cannot_be_sure_of() {
        for target in [
            "https://github.com/t3tools/t3code/issues/123",
            "https://github.com/t3tools/t3code/commit/0a1b2c3",
            "https://github.com/t3tools/t3code",
            "https://github.com/t3tools/t3code/pull/abc",
            "https://gitlab.com/t3tools/t3code/-/issues/12",
            "https://blog.example.test/2026/updates/pull/3",
            "javascript:alert(1)//github.com/t3tools/t3code/pull/1",
            "not a url",
        ] {
            assert_eq!(parse_change_request_url(target), None, "{target}");
        }
    }

    #[test]
    fn reads_a_github_pull_request_on_a_middle_label_host_as_an_autolink_candidate() {
        assert_eq!(
            pull_request_candidate_url_from_reference_autolink(
                "https://code.github.example.com/acme/web/issues/42",
            ),
            Some("https://code.github.example.com/acme/web/pull/42".to_owned())
        );
        assert_eq!(
            pull_request_candidate_url_from_reference_autolink(
                "https://example.com/acme/web/issues/42"
            ),
            None
        );
    }

    #[test]
    fn builds_a_canonical_sibling_of_a_github_pull_request() {
        for (url, expected) in [
            (
                "https://github.com/acme/web/pull/42#discussion_r123",
                "https://github.com/acme/web/pull/43",
            ),
            (
                "https://github.com/acme/web/pull/42/files?w=1",
                "https://github.com/acme/web/pull/43",
            ),
            (
                "https://github.acme.test:8443/acme/web/pull/42/",
                "https://github.acme.test:8443/acme/web/pull/43",
            ),
            (
                "https://github.com/pull/1/pull/42/files",
                "https://github.com/pull/1/pull/43",
            ),
        ] {
            assert_eq!(
                sibling_pull_request_url(url, 43).as_deref(),
                Some(expected),
                "{url}"
            );
        }
        assert_eq!(
            sibling_pull_request_url("https://github.com/acme/web/issues/42", 43),
            None
        );
        assert_eq!(
            sibling_pull_request_url("https://github.com/acme/web/pull/42", 0),
            None
        );
    }

    #[test]
    fn builds_a_github_change_request_url_and_leaves_other_kinds_to_their_hosts() {
        assert_eq!(
            change_request_url_for(Some("github"), "github.com", "t3tools/t3code", 7).as_deref(),
            Some("https://github.com/t3tools/t3code/pull/7")
        );
        assert_eq!(
            change_request_url_for(Some("gitlab"), "gitlab.com", "t3tools/t3code", 7),
            None
        );
        assert_eq!(change_request_url_for(None, "github.com", "a/b", 7), None);
    }

    #[test]
    fn finds_the_repository_root_behind_a_github_pull_request_url() {
        assert_eq!(
            change_request_repository_url("https://github.com/acme/web/pull/42/files?w=1")
                .as_deref(),
            Some("https://github.com/acme/web")
        );
        assert_eq!(
            change_request_repository_url("https://github.com/acme/web/issues/42"),
            None
        );
    }

    /// Not an upstream test: pins the WHATWG parsing this module reimplements,
    /// with expectations taken from Node's `URL`.
    #[test]
    fn parses_urls_like_whatwg_url() {
        for (input, expected) in [
            (
                "HTTPS://GitHub.com/A/B/pull/1",
                Some(("github.com", "/A/B/pull/1")),
            ),
            (
                "https://github.com/a/b c/pull/1",
                Some(("github.com", "/a/b%20c/pull/1")),
            ),
            (
                "https://github.com/a/../a/b/pull/2",
                Some(("github.com", "/a/b/pull/2")),
            ),
            (
                "https://github.com/a/%2e%2e/x/b/pull/3",
                Some(("github.com", "/x/b/pull/3")),
            ),
            (
                "https:github.com/a/b/pull/4",
                Some(("github.com", "/a/b/pull/4")),
            ),
            (
                "https://u:p@github.com:443/a/b/pull/5",
                Some(("github.com", "/a/b/pull/5")),
            ),
            (
                "https://github.com/a/b^/pull/6",
                Some(("github.com", "/a/b^/pull/6")),
            ),
            (
                "https://github.com/a/é/pull/7",
                Some(("github.com", "/a/%C3%A9/pull/7")),
            ),
            (
                "https:\\\\github.com\\a\\b\\pull\\8",
                Some(("github.com", "/a/b/pull/8")),
            ),
            (
                "https://github.com/a/b/pull/9?x#y",
                Some(("github.com", "/a/b/pull/9")),
            ),
            (
                "  https://github.com/a/b/pull/10  ",
                Some(("github.com", "/a/b/pull/10")),
            ),
            (
                "https://github.com./a/b/pull/11",
                Some(("github.com.", "/a/b/pull/11")),
            ),
            (
                "https://github.com/a/b/./pull/12",
                Some(("github.com", "/a/b/pull/12")),
            ),
            (
                "https://github.com/a/b{}`/pull/13",
                Some(("github.com", "/a/b%7B%7D%60/pull/13")),
            ),
            ("https://github.com", Some(("github.com", "/"))),
            ("https://exa mple.com/", None),
            ("https://github.com:99999/a", None),
            ("https://", None),
            ("mailto:someone@github.com", None),
        ] {
            let parsed = parse_http_url(input);
            assert_eq!(
                parsed
                    .as_ref()
                    .map(|url| (url.hostname.as_str(), url.pathname.as_str())),
                expected,
                "{input}"
            );
        }
    }
}

#[cfg(test)]
mod qc_regressions {
    use super::*;

    #[test]
    fn repository_scan_is_linear_for_a_long_owner_segment() {
        // The path scan used to copy the remainder of the path at every index.
        let owner = "a".repeat(60_000);
        let url = format!("https://github.com/{owner}/repo/pull/7");
        assert_eq!(
            change_request_repository_url(&url),
            Some(format!("https://github.com/{owner}/repo"))
        );
    }

    #[test]
    fn pull_route_matches_case_insensitively_on_digits_then_end_or_slash() {
        assert!(pull_route_matches(b"/pull/7"));
        assert!(pull_route_matches(b"/PULLS/12/files"));
        assert!(pull_route_matches(b"/Pulls/3"));
        assert!(!pull_route_matches(b"/pull/x"));
        assert!(!pull_route_matches(b"/pull/3x"));
        assert!(!pull_route_matches(b"/pull"));
        assert!(!pull_route_matches(b"/pulls/"));
    }
}
