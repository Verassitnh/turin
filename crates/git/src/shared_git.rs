//! Port of upstream/packages/shared/src/git.ts
//!
//! `detect_source_control_provider_from_git_remote_url` recognizes GitHub only
//! (`github.com` or a DNS label `github`). Upstream also returns GitLab, Azure
//! DevOps, Bitbucket, Forgejo, GitCafe, and `unknown`. Those hosts are cut, so
//! this returns `None` instead of a provider record.

use std::collections::HashSet;

use regex::Regex;
use std::sync::LazyLock;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::git::{
    GitFilePath, VcsRef, VcsStatusLocalResult, VcsStatusRemoteResult, VcsStatusResult,
    VcsStatusStreamEvent, VcsStatusWorkingTree,
};
use turin_contracts::settings::{BranchNamingMode, BranchNamingOptions};
use turin_contracts::source_control::{SourceControlProviderInfo, SourceControlProviderKind};

pub const WORKTREE_BRANCH_PREFIX: &str = "t3";

static TEMP_WORKTREE_BRANCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:t3[-/][0-9a-f]{8}|t3code(?:[-/][0-9a-f]{8}|/[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}))$",
    )
    .unwrap()
});
static QUOTES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"`]"#).unwrap());
static EDGE_SEPARATORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[./\s_-]+|[./\s_-]+$").unwrap());
static FRAGMENT_UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9/_-]+").unwrap());
static MULTI_SLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/+").unwrap());
static MULTI_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-+").unwrap());
static EDGE_DOT_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[./_-]+|[./_-]+$").unwrap());
static TRAILING_DOT_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[./_-]+$").unwrap());
static PREFIX_UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9_-]+").unwrap());
static PREFIX_EDGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-+|-+$").unwrap());
static SCP_REMOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+@([^:/\s]+):([^/\s]+(?:/[^/\s]+)+)$").unwrap());
static GITHUB_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:git@github\.com:|ssh://(?:git@)?github\.com/|https://github\.com/|git://github\.com/)([^/\s]+/[^/\s]+?)(?:\.git)?/?$",
    )
    .unwrap()
});
static CONFIG_CONTINUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\r?\n[ \t]*").unwrap());
static REMOTE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^\[\s*remote(?:\s+"([^"]+)"|\.([^\]\s]+))\s*\](?:\s*[#;].*)?$"#).unwrap()
});
static URL_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^url\s*=\s*(.*)$").unwrap());
static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:ssh|https?|git)://").unwrap());
static SCP_HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9._-]+@([^:/]+)").unwrap());
static WHITESPACE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t\n\r\x0c\x0b]+").unwrap());

/// Sanitize an arbitrary string into a valid, lowercase git refName fragment.
pub fn sanitize_branch_fragment(raw: &str) -> String {
    let normalized = EDGE_SEPARATORS
        .replace_all(&QUOTES.replace_all(&js_trim(raw).to_lowercase(), ""), "")
        .into_owned();
    let collapsed = TRAILING_DOT_DASH
        .replace(
            &js_slice_chars(
                &EDGE_DOT_DASH.replace(
                    &MULTI_DASH.replace_all(
                        &MULTI_SLASH
                            .replace_all(&FRAGMENT_UNSAFE.replace_all(&normalized, "-"), "/"),
                        "-",
                    ),
                    "",
                ),
                64,
            ),
            "",
        )
        .into_owned();
    if collapsed.is_empty() {
        "update".to_owned()
    } else {
        collapsed
    }
}

/// Custom naming preserves the model's complete ref; Git validates it on rename.
pub fn format_generated_branch_name(raw: &str, naming: Option<&BranchNamingOptions>) -> String {
    if naming.is_some_and(|naming| naming.mode == BranchNamingMode::Custom) {
        return js_trim(raw).to_owned();
    }
    let branch = sanitize_branch_fragment(raw);
    if naming.is_none_or(|naming| naming.mode != BranchNamingMode::Static) {
        return branch;
    }
    let prefix = naming
        .map(|naming| {
            naming
                .prefix
                .split('/')
                .map(|part| {
                    PREFIX_EDGE
                        .replace_all(
                            &MULTI_DASH.replace_all(&PREFIX_UNSAFE.replace_all(part, "-"), "-"),
                            "",
                        )
                        .into_owned()
                })
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    if prefix.is_empty() {
        branch
    } else {
        format!("{prefix}/{branch}")
    }
}

/// Sanitize a generated branch name, preserving any slash-separated namespace.
pub fn sanitize_feature_branch_name(raw: &str) -> String {
    let sanitized = sanitize_branch_fragment(raw);
    if sanitized.contains('/') {
        sanitized
    } else {
        format!("feature/{sanitized}")
    }
}

const AUTO_FEATURE_BRANCH_FALLBACK: &str = "feature/update";

/// Resolve a unique generated refName, suffixing only the path component
/// blocked by an existing ref or, for the final component, its descendants.
pub fn resolve_auto_feature_branch_name(
    existing_branch_names: &[String],
    preferred_branch: Option<&str>,
) -> String {
    let preferred = preferred_branch
        .map(js_trim)
        .filter(|value| !value.is_empty());
    let resolved_base =
        sanitize_feature_branch_name(preferred.unwrap_or(AUTO_FEATURE_BRANCH_FALLBACK));
    let existing_names: Vec<String> = existing_branch_names
        .iter()
        .map(|name| name.to_lowercase())
        .collect();
    let existing_refs: HashSet<&str> = existing_names.iter().map(String::as_str).collect();
    let parts: Vec<&str> = resolved_base.split('/').collect();
    let mut resolved = String::new();
    for (index, part) in parts.iter().enumerate() {
        let base = if resolved.is_empty() {
            (*part).to_owned()
        } else {
            format!("{resolved}/{part}")
        };
        let mut candidate = base.clone();
        let mut suffix = 2_u32;
        while existing_refs.contains(candidate.as_str())
            || (index + 1 == parts.len()
                && existing_names
                    .iter()
                    .any(|name| name.starts_with(&format!("{candidate}/"))))
        {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        resolved = candidate;
    }
    resolved
}

/// Strip the remote prefix from a remote ref such as `origin/feature/demo`.
pub fn derive_local_branch_name_from_remote_ref(branch_name: &str) -> String {
    match branch_name.find('/') {
        Some(index) if index > 0 && index + 1 < branch_name.len() => {
            branch_name[index + 1..].to_owned()
        }
        _ => branch_name.to_owned(),
    }
}

pub fn build_temporary_worktree_branch_name(random_hex: impl Fn(usize) -> String) -> String {
    let token: String = random_hex(4)
        .to_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .take(8)
        .collect();
    format!("{WORKTREE_BRANCH_PREFIX}/{token}")
}

/// Git stores refs as paths, so a plain `t3` branch makes every `t3/<hex>`
/// ref impossible. This moves a temporary name to the flat `t3-<hex>` sibling.
pub fn flatten_temporary_worktree_branch_name(ref_name: &str) -> String {
    let normalized = js_trim(ref_name).to_lowercase();
    let token_start = normalized
        .find(['-', '/'])
        .map(|index| index + 1)
        .unwrap_or(0);
    let token: String = normalized.chars().skip(token_start).take(8).collect();
    format!("{WORKTREE_BRANCH_PREFIX}-{token}")
}

pub fn is_temporary_worktree_branch(ref_name: &str) -> bool {
    TEMP_WORKTREE_BRANCH.is_match(&js_trim(ref_name).to_lowercase())
}

fn azure_dev_ops_repository_key(host: &str, segments: &[&str]) -> Option<String> {
    if host != "ssh.dev.azure.com" && host != "vs-ssh.visualstudio.com" {
        return None;
    }
    if segments.len() != 4 || segments[0] != "v3" {
        return None;
    }
    let organization = segments[1];
    let project = segments[2];
    let repository = segments[3];
    if organization.is_empty() || project.is_empty() || repository.is_empty() {
        return None;
    }
    if host == "ssh.dev.azure.com" {
        Some(format!(
            "dev.azure.com/{organization}/{project}/_git/{repository}"
        ))
    } else {
        Some(format!(
            "{organization}.visualstudio.com/{project}/_git/{repository}"
        ))
    }
}

/// Normalize a git remote URL into a stable comparison key.
pub fn normalize_git_remote_url(value: &str) -> String {
    let mut normalized = js_trim(value).trim_end_matches('/').to_owned();
    if normalized.len() >= 4 && normalized[normalized.len() - 4..].eq_ignore_ascii_case(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    let normalized = normalized.to_lowercase();
    if URL_SCHEME.is_match(&normalized) {
        if let Some(url) = parse_simple_url(&normalized) {
            let segments: Vec<&str> = url
                .pathname
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect();
            if !url.hostname.is_empty() && segments.len() > 1 {
                return azure_dev_ops_repository_key(&url.hostname, &segments)
                    .unwrap_or_else(|| format!("{}/{}", url.hostname, segments.join("/")));
            }
        } else {
            return normalized;
        }
    }
    if let Some(caps) = SCP_REMOTE.captures(&normalized) {
        let host = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let path = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let segments: Vec<&str> = path.split('/').collect();
        return azure_dev_ops_repository_key(host, &segments)
            .unwrap_or_else(|| format!("{host}/{path}"));
    }
    normalized
}

struct SimpleUrl {
    hostname: String,
    pathname: String,
}

fn parse_simple_url(value: &str) -> Option<SimpleUrl> {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = value.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = value.strip_prefix("ssh://") {
        ("ssh", rest)
    } else {
        let rest = value.strip_prefix("git://")?;
        ("git", rest)
    };
    if rest.is_empty() {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    if hostport.is_empty() {
        return None;
    }
    let hostname = split_host(hostport)?.0;
    let _ = scheme;
    Some(SimpleUrl {
        hostname: hostname.to_lowercase(),
        pathname: if path.is_empty() {
            "/".to_owned()
        } else {
            path.to_owned()
        },
    })
}

fn split_host(hostport: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = hostport.strip_prefix('[') {
        let end = rest.find(']')?;
        let hostname = &rest[..end];
        let after = &rest[end + 1..];
        let port = after.strip_prefix(':').filter(|port| !port.is_empty());
        return Some((hostname, port));
    }
    match hostport.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some((host, Some(port)))
        }
        _ => Some((hostport, None)),
    }
}

fn parse_git_config_value(raw: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let chars: Vec<char> = raw.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
            index += 1;
            continue;
        }
        if !quoted && (ch == '#' || ch == ';') {
            break;
        }
        out.push(ch);
        index += 1;
    }
    js_trim(&out).to_owned()
}

/// Read the primary remote URL from raw `.git/config` text without spawning git.
pub fn parse_origin_url_from_git_config(config_text: &str) -> Option<String> {
    let joined = CONFIG_CONTINUATION.replace_all(config_text, "");
    let mut section: Option<String> = None;
    let mut origin_url: Option<String> = None;
    let mut first_remote_url: Option<String> = None;
    for raw_line in joined.split('\n') {
        let line = js_trim(raw_line.trim_end_matches('\r'));
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = REMOTE_HEADER.captures(line) {
            section = header
                .get(1)
                .map(|value| value.as_str().to_owned())
                .or_else(|| header.get(2).map(|value| value.as_str().to_lowercase()));
            continue;
        }
        if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(section_name) = section.as_deref() else {
            continue;
        };
        let Some(matched) = URL_KEY.captures(line) else {
            continue;
        };
        let url = parse_git_config_value(matched.get(1).map(|value| value.as_str()).unwrap_or(""));
        if url.is_empty() {
            continue;
        }
        if section_name == "origin" {
            if origin_url.is_none() {
                origin_url = Some(url);
            }
        } else if first_remote_url.is_none() {
            first_remote_url = Some(url);
        }
    }
    origin_url.or(first_remote_url)
}

/// Best-effort parse of a GitHub `owner/repo` identifier from common remote URL shapes.
pub fn parse_github_repository_name_with_owner_from_remote_url(
    url: Option<&str>,
) -> Option<String> {
    let trimmed = js_trim(url.unwrap_or(""));
    if trimmed.is_empty() {
        return None;
    }
    let matched = GITHUB_REMOTE.captures(trimmed)?;
    let name = js_trim(matched.get(1).map(|value| value.as_str()).unwrap_or(""));
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

fn derive_local_branch_name_candidates(
    branch_name: &str,
    remote_name: Option<&str>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    let first = derive_local_branch_name_from_remote_ref(branch_name);
    if !first.is_empty() && !candidates.contains(&first) {
        candidates.push(first);
    }
    if let Some(remote_name) = remote_name {
        let prefix = format!("{remote_name}/");
        if let Some(rest) = branch_name.strip_prefix(&prefix)
            && !rest.is_empty()
            && !candidates.contains(&rest.to_owned())
        {
            candidates.push(rest.to_owned());
        }
    }
    candidates
}

/// Git rejects ASCII space and the ASCII control characters in ref names.
pub fn sanitize_new_ref_name(raw_name: &str) -> String {
    WHITESPACE_RUN
        .replace_all(js_trim(raw_name), "-")
        .into_owned()
}

/// Hide `origin/*` remote refs when a matching local refName already exists.
pub fn dedupe_remote_branches_with_local_matches(refs: &[VcsRef]) -> Vec<VcsRef> {
    let local_names: HashSet<&str> = refs
        .iter()
        .filter(|reference| reference.is_remote != Some(true))
        .map(|reference| reference.name.as_str())
        .collect();
    refs.iter()
        .filter(|reference| {
            if reference.is_remote != Some(true) {
                return true;
            }
            if reference.remote_name.as_ref().map(|name| name.as_str()) != Some("origin") {
                return true;
            }
            let candidates = derive_local_branch_name_candidates(
                reference.name.as_str(),
                reference.remote_name.as_ref().map(|name| name.as_str()),
            );
            !candidates
                .iter()
                .any(|candidate| local_names.contains(candidate.as_str()))
        })
        .cloned()
        .collect()
}

pub fn detect_source_control_provider_from_git_remote_url(
    remote_url: &str,
) -> Option<SourceControlProviderInfo> {
    let host = parse_remote_host(remote_url)?;
    let hostname = parse_host_name(&host);
    if !(hostname == "github.com" || hostname.split('.').any(|label| label == "github")) {
        return None;
    }
    let name = if hostname == "github.com" {
        "GitHub"
    } else {
        "GitHub Self-Hosted"
    };
    Some(SourceControlProviderInfo {
        kind: SourceControlProviderKind::new("github").ok()?,
        name: TrimmedNonEmptyString::new(name).ok()?,
        base_url: format!("https://{host}"),
    })
}

fn parse_remote_host(remote_url: &str) -> Option<String> {
    let trimmed = js_trim(remote_url);
    if trimmed.is_empty() {
        return None;
    }
    if let Some(caps) = SCP_HOST.captures(trimmed) {
        return caps.get(1).map(|value| value.as_str().to_lowercase());
    }
    let url = parse_simple_url(&trimmed.to_lowercase()).or_else(|| parse_simple_url(trimmed))?;
    if trimmed.to_lowercase().starts_with("ssh://") {
        Some(url.hostname)
    } else {
        let hostport = trimmed
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        let authority = hostport.split('/').next().unwrap_or("");
        let hostport = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        Some(hostport.to_lowercase())
    }
}

fn parse_host_name(host: &str) -> String {
    let prefixed = format!("https://{host}");
    parse_simple_url(&prefixed)
        .map(|url| url.hostname)
        .unwrap_or_else(|| {
            Regex::new(r":\d+$")
                .unwrap()
                .replace(host, "")
                .to_lowercase()
        })
}

fn empty_remote() -> VcsStatusRemoteResult {
    VcsStatusRemoteResult {
        has_upstream: false,
        ahead_count: 0,
        behind_count: 0,
        ahead_of_default_count: Some(0),
        pr: None,
    }
}

pub fn merge_git_status_parts(
    local: VcsStatusLocalResult,
    remote: Option<VcsStatusRemoteResult>,
) -> VcsStatusResult {
    let remote = remote.unwrap_or_else(empty_remote);
    VcsStatusResult {
        is_repo: local.is_repo,
        source_control_provider: local.source_control_provider,
        has_primary_remote: local.has_primary_remote,
        is_default_ref: local.is_default_ref,
        ref_name: local.ref_name,
        has_working_tree_changes: local.has_working_tree_changes,
        working_tree: local.working_tree,
        branch_changes: local.branch_changes,
        has_upstream: remote.has_upstream,
        ahead_count: remote.ahead_count,
        behind_count: remote.behind_count,
        ahead_of_default_count: remote.ahead_of_default_count,
        pr: remote.pr,
    }
}

fn to_remote_status_part(status: &VcsStatusResult) -> VcsStatusRemoteResult {
    VcsStatusRemoteResult {
        has_upstream: status.has_upstream,
        ahead_count: status.ahead_count,
        behind_count: status.behind_count,
        ahead_of_default_count: status.ahead_of_default_count,
        pr: status.pr.clone(),
    }
}

fn to_local_status_part(status: &VcsStatusResult) -> VcsStatusLocalResult {
    VcsStatusLocalResult {
        is_repo: status.is_repo,
        source_control_provider: status.source_control_provider.clone(),
        has_primary_remote: status.has_primary_remote,
        is_default_ref: status.is_default_ref,
        ref_name: status.ref_name.clone(),
        has_working_tree_changes: status.has_working_tree_changes,
        working_tree: status.working_tree.clone(),
        branch_changes: status.branch_changes.clone(),
    }
}

fn empty_local() -> VcsStatusLocalResult {
    VcsStatusLocalResult {
        is_repo: true,
        source_control_provider: None,
        has_primary_remote: false,
        is_default_ref: false,
        ref_name: None,
        has_working_tree_changes: false,
        working_tree: VcsStatusWorkingTree {
            files: Vec::new(),
            insertions: 0,
            deletions: 0,
        },
        branch_changes: None,
    }
}

pub fn apply_git_status_stream_event(
    current: Option<VcsStatusResult>,
    event: &VcsStatusStreamEvent,
) -> VcsStatusResult {
    match event {
        VcsStatusStreamEvent::Snapshot { local, remote } => {
            merge_git_status_parts(local.clone(), remote.clone())
        }
        VcsStatusStreamEvent::LocalUpdated { local } => {
            merge_git_status_parts(local.clone(), current.as_ref().map(to_remote_status_part))
        }
        VcsStatusStreamEvent::RemoteUpdated { remote } => match current {
            None => merge_git_status_parts(empty_local(), remote.clone()),
            Some(current) => merge_git_status_parts(to_local_status_part(&current), remote.clone()),
        },
    }
}

fn js_slice_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[allow(dead_code)]
fn _file_path(value: &str) -> GitFilePath {
    GitFilePath::new(value).expect("path")
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::PositiveInt;
    use turin_contracts::git::VcsStatusChangeRequest;

    #[test]
    fn normalizes_equivalent_github_remotes() {
        assert_eq!(
            normalize_git_remote_url("git@github.com:T3Tools/T3Code.git"),
            "github.com/t3tools/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("https://github.com/T3Tools/T3Code.git"),
            "github.com/t3tools/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("ssh://git@github.com/T3Tools/T3Code"),
            "github.com/t3tools/t3code"
        );
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
    fn drops_explicit_ports() {
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
    fn normalizes_scp_with_non_git_users() {
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
    fn azure_ssh_matches_https() {
        assert_eq!(
            normalize_git_remote_url("git@ssh.dev.azure.com:v3/T3Tools/Platform/T3Code"),
            "dev.azure.com/t3tools/platform/_git/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("ssh://git@ssh.dev.azure.com:22/v3/T3Tools/Platform/T3Code"),
            "dev.azure.com/t3tools/platform/_git/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("https://T3Tools@dev.azure.com/T3Tools/Platform/_git/T3Code"),
            "dev.azure.com/t3tools/platform/_git/t3code"
        );
        assert_eq!(
            normalize_git_remote_url("T3Tools@vs-ssh.visualstudio.com:v3/T3Tools/Platform/T3Code"),
            "t3tools.visualstudio.com/platform/_git/t3code"
        );
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
    fn parses_origin_url_from_config() {
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
        assert_eq!(
            parse_origin_url_from_git_config("[remote.origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            Some("git@github.com:a/b.git")
        );
        assert_eq!(
            parse_origin_url_from_git_config("[remote.Origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            Some("git@github.com:a/b.git")
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"]\n\turl = https://github.com/acme/\\\n\t\trepo.git\n"
            )
            .as_deref(),
            Some("https://github.com/acme/repo.git")
        );
        let fallback = [
            "[remote \"upstream\"]",
            "\turl = https://github.com/acme/repo.git",
            "[remote \"fork\"]",
            "\turl = https://github.com/me/repo.git",
        ]
        .join("\n");
        assert_eq!(
            parse_origin_url_from_git_config(&fallback).as_deref(),
            Some("https://github.com/acme/repo.git")
        );
        assert_eq!(
            parse_origin_url_from_git_config("[core]\n\tbare = false\n"),
            None
        );
        assert_eq!(parse_origin_url_from_git_config(""), None);
    }

    #[test]
    fn parses_github_owner_repo() {
        assert_eq!(
            parse_github_repository_name_with_owner_from_remote_url(Some(
                "git@github.com:T3Tools/T3Code.git"
            ))
            .as_deref(),
            Some("T3Tools/T3Code")
        );
        assert_eq!(
            parse_github_repository_name_with_owner_from_remote_url(Some(
                "https://github.com/T3Tools/T3Code.git"
            ))
            .as_deref(),
            Some("T3Tools/T3Code")
        );
        assert_eq!(
            parse_github_repository_name_with_owner_from_remote_url(Some(
                "ssh://github.com/T3Tools/T3Code.git"
            ))
            .as_deref(),
            Some("T3Tools/T3Code")
        );
    }

    #[test]
    fn temporary_worktree_names() {
        let built = build_temporary_worktree_branch_name(|bytes| {
            assert_eq!(bytes, 4);
            "DEADBEEF".to_owned()
        });
        assert!(is_temporary_worktree_branch(&built));
        assert!(is_temporary_worktree_branch("t3/deadbeef"));
        assert!(is_temporary_worktree_branch(" t3/deadbeef "));
        assert!(is_temporary_worktree_branch("t3/DEADBEEF"));
        assert_eq!(
            build_temporary_worktree_branch_name(|_| "f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12".into()),
            "t3/f4ae4e0e"
        );
        assert!(is_temporary_worktree_branch("t3code/deadbeef"));
        assert!(is_temporary_worktree_branch("t3code-deadbeef"));
        assert!(is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"
        ));
        assert!(!is_temporary_worktree_branch(
            "t3/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"
        ));
        assert!(!is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-1d48-b4f2-9cf0aa54ab12"
        ));
        assert!(!is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-4d48-c4f2-9cf0aa54ab12"
        ));
        assert_eq!(
            flatten_temporary_worktree_branch_name("t3/deadbeef"),
            "t3-deadbeef"
        );
        assert!(is_temporary_worktree_branch("t3-deadbeef"));
        assert!(!is_temporary_worktree_branch("t3-deadbeef-extra"));
        assert!(!is_temporary_worktree_branch("t3-feature"));
        assert_eq!(
            flatten_temporary_worktree_branch_name("t3code/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"),
            "t3-f4ae4e0e"
        );
        assert!(!is_temporary_worktree_branch("t3/feature/demo"));
        assert!(!is_temporary_worktree_branch("main"));
    }

    #[test]
    fn feature_branch_names() {
        assert_eq!(
            sanitize_feature_branch_name("feature/refine-toolbar"),
            "feature/refine-toolbar"
        );
        assert_eq!(
            sanitize_feature_branch_name(" \"FIX//Calendar recruitment filter\" "),
            "fix/calendar-recruitment-filter"
        );
        assert_eq!(
            sanitize_feature_branch_name("refine toolbar"),
            "feature/refine-toolbar"
        );
        assert_eq!(sanitize_feature_branch_name(""), "feature/update");
        assert_eq!(
            sanitize_feature_branch_name(&format!("fix/{}", "x".repeat(80))),
            format!("fix/{}", "x".repeat(60))
        );
        assert_eq!(
            resolve_auto_feature_branch_name(
                &["FIX/refine-toolbar".into(), "fix/refine-toolbar-2".into()],
                Some("fix/refine-toolbar")
            ),
            "fix/refine-toolbar-3"
        );
        assert_eq!(
            resolve_auto_feature_branch_name(&["feature/update".into()], None),
            "feature/update-2"
        );
        assert_eq!(
            resolve_auto_feature_branch_name(&["fix".into()], Some("fix/name")),
            "fix-2/name"
        );
        assert_eq!(
            resolve_auto_feature_branch_name(&["fix/name/child".into()], Some("fix/name")),
            "fix/name-2"
        );
    }

    #[test]
    fn generated_branch_prefixes() {
        let static_naming = |prefix: &str| BranchNamingOptions {
            mode: BranchNamingMode::Static,
            prefix: prefix.to_owned(),
            instructions: String::new(),
        };
        assert_eq!(
            format_generated_branch_name("Add Search", Some(&static_naming("t3code"))),
            "t3code/add-search"
        );
        assert_eq!(
            format_generated_branch_name("Add Search", Some(&static_naming(""))),
            "add-search"
        );
        assert_eq!(
            format_generated_branch_name("Add Search", Some(&static_naming("Team/Julius/"))),
            "Team/Julius/add-search"
        );
        assert_eq!(
            format_generated_branch_name(
                "feat/Add Search",
                Some(&BranchNamingOptions {
                    mode: BranchNamingMode::Semantic,
                    prefix: "t3code".into(),
                    instructions: String::new(),
                })
            ),
            "feat/add-search"
        );
        let branch = format!("Julius/ABC-123/release.v2-{}", "x".repeat(70));
        assert_eq!(
            format_generated_branch_name(
                &format!(" {branch} "),
                Some(&BranchNamingOptions {
                    mode: BranchNamingMode::Custom,
                    prefix: "ignored".into(),
                    instructions: String::new(),
                })
            ),
            branch
        );
    }

    #[test]
    fn github_detection_is_github_only() {
        let github =
            detect_source_control_provider_from_git_remote_url("git@github.com:owner/repo.git")
                .unwrap();
        assert_eq!(github.kind.as_str(), "github");
        assert_eq!(github.name.as_str(), "GitHub");
        assert_eq!(github.base_url, "https://github.com");
        let enterprise = detect_source_control_provider_from_git_remote_url(
            "https://github.example.com/owner/repo.git",
        )
        .unwrap();
        assert_eq!(enterprise.name.as_str(), "GitHub Self-Hosted");
        assert!(
            detect_source_control_provider_from_git_remote_url("https://gitlab.com/group/repo.git")
                .is_none()
        );
        assert!(
            detect_source_control_provider_from_git_remote_url(
                "https://notgithub.example.com/owner/repo.git"
            )
            .is_none()
        );
    }

    #[test]
    fn status_stream_remote_update_without_local_state() {
        let remote = VcsStatusRemoteResult {
            has_upstream: true,
            ahead_count: 2,
            behind_count: 1,
            ahead_of_default_count: None,
            pr: None,
        };
        let merged = apply_git_status_stream_event(
            None,
            &VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            },
        );
        assert!(merged.is_repo);
        assert!(!merged.has_primary_remote);
        assert_eq!(merged.ahead_count, 2);
        assert_eq!(merged.behind_count, 1);
        assert!(merged.pr.is_none());
        assert!(merged.working_tree.files.is_empty());
    }

    #[test]
    fn status_stream_preserves_local_fields() {
        let current = VcsStatusResult {
            is_repo: true,
            source_control_provider: detect_source_control_provider_from_git_remote_url(
                "https://github.com/acme/repo.git",
            ),
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: TrimmedNonEmptyString::new("feature/demo").ok(),
            has_working_tree_changes: true,
            working_tree: VcsStatusWorkingTree {
                files: vec![turin_contracts::git::VcsStatusWorkingTreeFile {
                    path: GitFilePath::new("src/demo.ts").unwrap(),
                    insertions: 1,
                    deletions: 0,
                }],
                insertions: 1,
                deletions: 0,
            },
            branch_changes: None,
            has_upstream: false,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: None,
            pr: None,
        };
        let remote = VcsStatusRemoteResult {
            has_upstream: true,
            ahead_count: 2,
            behind_count: 1,
            ahead_of_default_count: None,
            pr: None,
        };
        let merged = apply_git_status_stream_event(
            Some(current.clone()),
            &VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            },
        );
        assert_eq!(merged.ref_name, current.ref_name);
        assert!(merged.has_working_tree_changes);
        assert!(merged.has_upstream);
        assert_eq!(merged.ahead_count, 2);
        let _ = VcsStatusChangeRequest {
            number: PositiveInt::new(1).unwrap(),
            title: TrimmedNonEmptyString::new("t").unwrap(),
            url: String::new(),
            base_ref: TrimmedNonEmptyString::new("main").unwrap(),
            head_ref: TrimmedNonEmptyString::new("feature").unwrap(),
            state: turin_contracts::git::GitPullRequestState::Open,
            is_draft: None,
            updated_at: None,
        };
    }
}
