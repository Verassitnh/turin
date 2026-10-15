//! Port of upstream/packages/shared/src/git.ts (only the remote-URL helpers source control uses;
//! branch-name helpers belong to the git crate).

use std::sync::LazyLock;

use regex::Regex;

static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:ssh|https?|git)://").unwrap());
static SCP_STYLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[a-zA-Z0-9._-]+@([^:/\s]+):([^/\s]+(?:/[^/\s]+)+)$").unwrap()
});
static TRAILING_SLASHES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/+$").unwrap());
static DOT_GIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.git$").unwrap());
static REMOTE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\[\s*remote(?:\s+"([^"]+)"|\.([^\]\s]+))\s*\](?:\s*[#;].*)?$"#).unwrap()
});
static URL_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^url\s*=\s*(.*)$").unwrap());
static LINE_CONTINUATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\\r?\n[ \t]*").unwrap());
static GITHUB_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:git@github\.com:|ssh://(?:git@)?github\.com/|https://github\.com/|git://github\.com/)([^/\s]+/[^/\s]+?)(?:\.git)?/?$",
    )
    .unwrap()
});

fn js_trim(s: &str) -> &str {
    super::js::js_trim(s)
}

/// The web spelling of an Azure DevOps repository reached over SSH, or `None` for anything else.
///
/// Azure alone addresses one repository under two names that share no part: `ssh.dev.azure.com` and
/// `v3/{org}/{project}/{repo}` over SSH, against `dev.azure.com` and `{org}/{project}/_git/{repo}`
/// everywhere a person sees it. A project cloned over SSH would otherwise be a different repository
/// to every comparison made against a pull request URL, which arrives in the web spelling. So the
/// web spelling is the one both are keyed by.
fn azure_dev_ops_repository_key(host: &str, segments: &[&str]) -> Option<String> {
    if host != "ssh.dev.azure.com" && host != "vs-ssh.visualstudio.com" {
        return None;
    }
    if segments.len() != 4 || segments[0] != "v3" {
        return None;
    }
    let (organization, project, repository) = (segments[1], segments[2], segments[3]);
    if organization.is_empty() || project.is_empty() || repository.is_empty() {
        return None;
    }
    Some(if host == "ssh.dev.azure.com" {
        format!("dev.azure.com/{organization}/{project}/_git/{repository}")
    } else {
        format!("{organization}.visualstudio.com/{project}/_git/{repository}")
    })
}

/// Normalize a git remote URL into a stable comparison key.
pub fn normalize_git_remote_url(value: &str) -> String {
    let trimmed = js_trim(value);
    let no_slashes = TRAILING_SLASHES.replace(trimmed, "");
    let no_suffix = DOT_GIT.replace(&no_slashes, "");
    let normalized = no_suffix.to_lowercase();

    if SCHEME.is_match(&normalized) {
        match reqwest::Url::parse(&normalized) {
            Ok(url) => {
                let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
                if let Some(hostname) = url.host_str().filter(|h| !h.is_empty())
                    && segments.len() > 1
                {
                    return azure_dev_ops_repository_key(hostname, &segments)
                        .unwrap_or_else(|| format!("{hostname}/{}", segments.join("/")));
                }
            }
            Err(_) => return normalized,
        }
    }

    if let Some(caps) = SCP_STYLE.captures(&normalized) {
        let host = caps.get(1).map_or("", |m| m.as_str());
        let path = caps.get(2).map_or("", |m| m.as_str());
        if !host.is_empty() && !path.is_empty() {
            let segments: Vec<&str> = path.split('/').collect();
            return azure_dev_ops_repository_key(host, &segments)
                .unwrap_or_else(|| format!("{host}/{path}"));
        }
    }

    normalized
}

/// Unquote a git config value: strip an inline `#` or `;` comment outside quotes, then drop
/// surrounding quotes and backslash escapes.
fn parse_git_config_value(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
            index += 1;
            continue;
        }
        if !quoted && (c == '#' || c == ';') {
            break;
        }
        out.push(c);
        index += 1;
    }
    js_trim(&out).to_owned()
}

/// Read the primary remote URL from raw `.git/config` text without spawning git. Prefers
/// `remote.origin.url` and falls back to the first remote so clones made with
/// `git clone --origin <name>` still resolve.
pub fn parse_origin_url_from_git_config(config_text: &str) -> Option<String> {
    let mut section: Option<String> = None;
    let mut origin_url: Option<String> = None;
    let mut first_remote_url: Option<String> = None;
    // A trailing backslash continues the value on the next line.
    let joined = LINE_CONTINUATION.replace_all(config_text, "");
    for raw_line in joined.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        let line = js_trim(raw_line);
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Both `[remote "origin"]` and the legacy `[remote.origin]` form, with an optional
        // trailing comment. Git keeps quoted subsections case-sensitive but folds the dotted form
        // to lowercase.
        if let Some(header) = REMOTE_HEADER.captures(line) {
            section = header
                .get(1)
                .map(|m| m.as_str().to_owned())
                .or_else(|| header.get(2).map(|m| m.as_str().to_lowercase()));
            continue;
        }
        if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(section) = section.as_deref() else { continue };
        let Some(m) = URL_LINE.captures(line) else { continue };
        let url = parse_git_config_value(m.get(1).map_or("", |v| v.as_str()));
        if url.is_empty() {
            continue;
        }
        if section == "origin" {
            origin_url.get_or_insert(url);
        } else {
            first_remote_url.get_or_insert(url);
        }
    }
    origin_url.or(first_remote_url)
}

/// Best-effort parse of a GitHub `owner/repo` identifier from common remote URL shapes.
pub fn parse_github_repository_name_with_owner_from_remote_url(url: Option<&str>) -> Option<String> {
    let trimmed = js_trim(url.unwrap_or(""));
    if trimmed.is_empty() {
        return None;
    }
    let name = js_trim(GITHUB_REMOTE.captures(trimmed)?.get(1)?.as_str());
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_equivalent_github_remotes_across_protocol_variants() {
        for url in [
            "git@github.com:T3Tools/T3Code.git",
            "https://github.com/T3Tools/T3Code.git",
            "ssh://git@github.com/T3Tools/T3Code",
        ] {
            assert_eq!(normalize_git_remote_url(url), "github.com/t3tools/t3code", "{url}");
        }
    }

    #[test]
    fn preserves_nested_group_paths() {
        assert_eq!(
            normalize_git_remote_url("git@gitlab.com:T3Tools/platform/T3Code.git"),
            "gitlab.com/t3tools/platform/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("https://gitlab.com/T3Tools/platform/T3Code.git"),
            "gitlab.com/t3tools/platform/t3code"
        );
    }

    #[test]
    fn drops_explicit_ports_from_url_shaped_remotes() {
        assert_eq!(
            normalize_git_remote_url("https://gitlab.company.com:8443/team/project.git"),
            "gitlab.company.com/team/project"
        );
        assert_eq!(
            normalize_git_remote_url("ssh://git@gitlab.company.com:2222/team/project.git"),
            "gitlab.company.com/team/project"
        );
    }

    #[test]
    fn normalizes_scp_like_remotes_with_non_git_ssh_users() {
        assert_eq!(
            normalize_git_remote_url("gitlab@gitlab.example.com:group/project.git"),
            "gitlab.example.com/group/project"
        );
        assert_eq!(
            normalize_git_remote_url("deploy@bitbucket.org:workspace/repo.git"),
            "bitbucket.org/workspace/repo"
        );
    }

    #[test]
    fn gives_an_azure_devops_repository_the_same_key_over_ssh_as_over_https() {
        let expected = "dev.azure.com/t3tools/platform/_git/t3code";
        assert_eq!(
            normalize_git_remote_url("git@ssh.dev.azure.com:v3/T3Tools/Platform/T3Code"),
            expected
        );
        assert_eq!(
            normalize_git_remote_url("ssh://git@ssh.dev.azure.com:22/v3/T3Tools/Platform/T3Code"),
            expected
        );
        assert_eq!(
            normalize_git_remote_url("https://T3Tools@dev.azure.com/T3Tools/Platform/_git/T3Code"),
            expected
        );
    }

    #[test]
    fn puts_the_organization_back_in_the_host_on_the_name_dev_azure_com_replaced() {
        assert_eq!(
            normalize_git_remote_url("T3Tools@vs-ssh.visualstudio.com:v3/T3Tools/Platform/T3Code"),
            "t3tools.visualstudio.com/platform/_git/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("https://T3Tools.visualstudio.com/Platform/_git/T3Code"),
            "t3tools.visualstudio.com/platform/_git/t3code"
        );
    }

    #[test]
    fn leaves_an_azure_ssh_host_it_cannot_read_as_the_path_it_was_given() {
        assert_eq!(
            normalize_git_remote_url("git@ssh.dev.azure.com:v4/T3Tools/Platform/T3Code"),
            "ssh.dev.azure.com/v4/t3tools/platform/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("git@ssh.dev.azure.com:v3/T3Tools/T3Code"),
            "ssh.dev.azure.com/v3/t3tools/t3code"
        );
    }

    #[test]
    fn reads_the_origin_url_and_ignores_other_remotes() {
        let config = [
            "[core]",
            "\trepositoryformatversion = 0",
            "[remote \"upstream\"]",
            "\turl = https://github.com/other/repo.git",
            "[remote \"origin\"]",
            "\turl = git@github.com:pingdotgg/t3code.git",
            "\tfetch = +refs/heads/*:refs/remotes/origin/*",
            "[branch \"main\"]",
            "\tremote = origin",
        ]
        .join("\n");
        assert_eq!(
            parse_origin_url_from_git_config(&config).as_deref(),
            Some("git@github.com:pingdotgg/t3code.git")
        );
    }

    #[test]
    fn strips_inline_comments_and_quotes_from_the_url_value() {
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"]\n\turl = https://github.com/acme/repo.git # mirror\n"
            )
            .as_deref(),
            Some("https://github.com/acme/repo.git")
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"]\n\turl = \"git@github.com:acme/repo.git\"\n"
            )
            .as_deref(),
            Some("git@github.com:acme/repo.git")
        );
    }

    #[test]
    fn accepts_legacy_dotted_headers_header_comments_and_line_continuations() {
        let ab = Some("git@github.com:a/b.git");
        assert_eq!(
            parse_origin_url_from_git_config("[remote.origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            ab
        );
        assert_eq!(
            parse_origin_url_from_git_config("[remote.Origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            ab
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"Origin\"]\n\turl = git@github.com:x/y.git\n[remote \"origin\"]\n\turl = git@github.com:a/b.git\n"
            )
            .as_deref(),
            ab
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"] # primary\n\turl = git@github.com:a/b.git\n"
            )
            .as_deref(),
            ab
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"]\n\turl = https://github.com/acme/\\\n\t\trepo.git\n"
            )
            .as_deref(),
            Some("https://github.com/acme/repo.git")
        );
    }

    #[test]
    fn falls_back_to_the_first_remote_when_there_is_no_origin() {
        let config = [
            "[remote \"upstream\"]",
            "\turl = https://github.com/acme/repo.git",
            "[remote \"fork\"]",
            "\turl = https://github.com/me/repo.git",
        ]
        .join("\n");
        assert_eq!(
            parse_origin_url_from_git_config(&config).as_deref(),
            Some("https://github.com/acme/repo.git")
        );
    }

    #[test]
    fn returns_none_when_there_is_no_remote_section() {
        assert_eq!(parse_origin_url_from_git_config("[core]\n\tbare = false\n"), None);
        assert_eq!(parse_origin_url_from_git_config(""), None);
    }

    #[test]
    fn extracts_the_owner_and_repository_from_common_github_remote_shapes() {
        for url in [
            "git@github.com:T3Tools/T3Code.git",
            "https://github.com/T3Tools/T3Code.git",
            "ssh://github.com/T3Tools/T3Code.git",
        ] {
            assert_eq!(
                parse_github_repository_name_with_owner_from_remote_url(Some(url)).as_deref(),
                Some("T3Tools/T3Code"),
                "{url}"
            );
        }
    }
}
