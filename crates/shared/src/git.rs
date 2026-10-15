//! Port of upstream/packages/shared/src/git.ts
//!
//! Branch naming, temporary worktree refs, remote URL keys, `.git/config` origin reading, remote
//! ref dedupe, and merging the local and remote halves of a VCS status.
//!
//! Deviation: the Azure DevOps SSH-to-web key in `normalizeGitRemoteUrl` is cut with its host (see
//! PORTING.md, cut list). Its test cases are dropped with it.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::git::{
    VcsRef, VcsStatusLocalResult, VcsStatusRemoteResult, VcsStatusResult, VcsStatusStreamEvent,
    VcsStatusWorkingTree,
};
use turin_contracts::settings::{BranchNamingMode, BranchNamingOptions};
use turin_contracts::source_control::SourceControlProviderInfo;

use crate::js_string::JS_WS;
use crate::source_control::detect_source_control_provider_from_remote_url;

pub const WORKTREE_BRANCH_PREFIX: &str = "t3";
const TEMP_WORKTREE_HEX_TOKEN: &str = "[0-9a-f]{8}";
const TEMP_WORKTREE_UUID_V4_TOKEN: &str =
    "[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}";
const AUTO_FEATURE_BRANCH_FALLBACK: &str = "feature/update";
const BRANCH_FRAGMENT_MAX_CHARS: usize = 64;

static QUOTES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"`]"#).expect("static regex"));

static EDGE_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^[./{JS_WS}_-]+|[./{JS_WS}_-]+$")).expect("static regex")
});

static NON_BRANCH_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9/_-]+").expect("static regex"));

static SLASH_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/+").expect("static regex"));

static DASH_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-+").expect("static regex"));

static FRAGMENT_EDGES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[./_-]+|[./_-]+$").expect("static regex"));

static FRAGMENT_TRAILING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[./_-]+$").expect("static regex"));

static STATIC_PREFIX_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9_-]+").expect("static regex"));

static DASH_EDGES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-+|-+$").expect("static regex"));

static TEMP_WORKTREE_BRANCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?:{WORKTREE_BRANCH_PREFIX}[-/]{TEMP_WORKTREE_HEX_TOKEN}|t3code(?:[-/]{TEMP_WORKTREE_HEX_TOKEN}|/{TEMP_WORKTREE_UUID_V4_TOKEN}))$"
    ))
    .expect("static regex")
});

static SCP_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^[a-zA-Z0-9._-]+@([^:/{JS_WS}]+):([^/{JS_WS}]+(?:/[^/{JS_WS}]+)+)$"
    ))
    .expect("static regex")
});

static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:ssh|https?|git)://").expect("static regex"));

static GIT_CONFIG_CONTINUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\r?\n[ \t]*").expect("static regex"));

static GIT_CONFIG_REMOTE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)^\[[{JS_WS}]*remote(?:[{JS_WS}]+"([^"]+)"|\.([^\]{JS_WS}]+))[{JS_WS}]*\](?:[{JS_WS}]*[#;].*)?$"#
    ))
    .expect("static regex")
});

static GIT_CONFIG_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)^url[{JS_WS}]*=[{JS_WS}]*(.*)$")).expect("static regex")
});

static GITHUB_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^(?:git@github\.com:|ssh://(?:git@)?github\.com/|https://github\.com/|git://github\.com/)([^/{JS_WS}]+/[^/{JS_WS}]+?)(?:\.git)?/?$"
    ))
    .expect("static regex")
});

static REF_NAME_WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t\n\r\x0C\x0B]+").expect("static regex"));

/// Sanitize an arbitrary string into a valid, lowercase git refName fragment.
/// Strips quotes, collapses separators, limits to 64 chars.
pub fn sanitize_branch_fragment(raw: &str) -> String {
    let lowered = js_trim(raw).to_lowercase();
    let unquoted = QUOTES.replace_all(&lowered, "");
    let normalized = EDGE_SEPARATORS.replace_all(&unquoted, "");

    let fragment = NON_BRANCH_CHARS.replace_all(&normalized, "-");
    let fragment = SLASH_RUN.replace_all(&fragment, "/");
    let fragment = DASH_RUN.replace_all(&fragment, "-");
    let fragment = FRAGMENT_EDGES.replace_all(&fragment, "");
    // Everything left is ASCII, so byte and UTF-16 truncation agree.
    let cut = &fragment[..fragment.len().min(BRANCH_FRAGMENT_MAX_CHARS)];
    let fragment = FRAGMENT_TRAILING.replace_all(cut, "").into_owned();

    if fragment.is_empty() {
        "update".to_owned()
    } else {
        fragment
    }
}

/// Custom naming preserves the model's complete ref; Git validates it on rename.
pub fn format_generated_branch_name(raw: &str, naming: Option<&BranchNamingOptions>) -> String {
    let Some(naming) = naming else {
        return sanitize_branch_fragment(raw);
    };
    if naming.mode == BranchNamingMode::Custom {
        return js_trim(raw).to_owned();
    }
    let branch = sanitize_branch_fragment(raw);
    if naming.mode != BranchNamingMode::Static {
        return branch;
    }
    let prefix = naming
        .prefix
        .split('/')
        .map(|part| {
            let part = STATIC_PREFIX_CHARS.replace_all(part, "-");
            let part = DASH_RUN.replace_all(&part, "-");
            DASH_EDGES.replace_all(&part, "").into_owned()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if prefix.is_empty() {
        branch
    } else {
        format!("{prefix}/{branch}")
    }
}

/// Sanitize a generated branch name, preserving any slash-separated namespace.
/// Unprefixed fragments use the `feature/` namespace.
pub fn sanitize_feature_branch_name(raw: &str) -> String {
    let sanitized = sanitize_branch_fragment(raw);
    if sanitized.contains('/') {
        sanitized
    } else {
        format!("feature/{sanitized}")
    }
}

/// Resolve a unique generated refName, suffixing only the path component blocked by an existing ref
/// or, for the final component, its descendants.
pub fn resolve_auto_feature_branch_name(
    existing_branch_names: &[String],
    preferred_branch: Option<&str>,
) -> String {
    let preferred = preferred_branch
        .map(js_trim)
        .filter(|preferred| !preferred.is_empty());
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
        let is_last = index == parts.len() - 1;
        let mut candidate = base.clone();
        let mut suffix = 2;
        while existing_refs.contains(candidate.as_str())
            || (is_last
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
pub fn derive_local_branch_name_from_remote_ref(branch_name: &str) -> &str {
    match branch_name.find('/') {
        Some(index) if index > 0 && index < branch_name.len() - 1 => &branch_name[index + 1..],
        _ => branch_name,
    }
}

pub fn build_temporary_worktree_branch_name(random_hex: impl FnOnce(usize) -> String) -> String {
    // Normalize to exactly 8 lowercase hex chars so a UUID-shaped callback still produces the
    // canonical temporary branch form.
    let token: String = random_hex(4)
        .to_lowercase()
        .chars()
        .filter(|ch| matches!(ch, '0'..='9' | 'a'..='f'))
        .take(8)
        .collect();
    format!("{WORKTREE_BRANCH_PREFIX}/{token}")
}

/// Git stores refs as paths, so a plain `t3` branch makes every `t3/<hex>` ref impossible. This
/// moves a temporary name to the flat `t3-<hex>` sibling.
pub fn flatten_temporary_worktree_branch_name(ref_name: &str) -> String {
    // Keep only the canonical 8-hex token so legacy `t3code/` and UUID names map cleanly.
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

/// Normalize a git remote URL into a stable comparison key.
pub fn normalize_git_remote_url(value: &str) -> String {
    let mut normalized = js_trim(value).trim_end_matches('/').to_owned();
    if normalized.len() >= 4
        && normalized
            .get(normalized.len() - 4..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".git"))
    {
        normalized.truncate(normalized.len() - 4);
    }
    let normalized = normalized.to_lowercase();

    if URL_SCHEME.is_match(&normalized) {
        if let Ok(url) = url::Url::parse(&normalized) {
            let repository_segments: Vec<&str> = url
                .path()
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect();
            if let Some(host) = url.host_str().filter(|host| !host.is_empty())
                && repository_segments.len() > 1
            {
                return format!("{host}/{}", repository_segments.join("/"));
            }
        } else {
            return normalized;
        }
    }

    if let Some(caps) = SCP_REMOTE.captures(&normalized) {
        let (host, path) = (&caps[1], &caps[2]);
        return format!("{host}/{path}");
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
        let ch = chars[index];
        if ch == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        } else if !quoted && (ch == '#' || ch == ';') {
            break;
        } else {
            out.push(ch);
        }
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
    let joined = GIT_CONFIG_CONTINUATION.replace_all(config_text, "");
    for raw_line in joined.split('\n') {
        let line = js_trim(raw_line.strip_suffix('\r').unwrap_or(raw_line));
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Both `[remote "origin"]` and the legacy `[remote.origin]` form, with an optional trailing
        // comment. Git keeps quoted subsections case-sensitive but folds the dotted form to
        // lowercase.
        if let Some(header) = GIT_CONFIG_REMOTE_HEADER.captures(line) {
            section = header
                .get(1)
                .map(|quoted| quoted.as_str().to_owned())
                .or_else(|| header.get(2).map(|dotted| dotted.as_str().to_lowercase()));
            continue;
        }
        if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(current) = section.as_deref() else {
            continue;
        };
        let Some(caps) = GIT_CONFIG_URL.captures(line) else {
            continue;
        };
        let url = parse_git_config_value(caps.get(1).map_or("", |m| m.as_str()));
        if url.is_empty() {
            continue;
        }
        if current == "origin" {
            origin_url.get_or_insert(url);
        } else {
            first_remote_url.get_or_insert(url);
        }
    }
    origin_url.or(first_remote_url)
}

/// Best-effort parse of a GitHub `owner/repo` identifier from common remote URL shapes.
pub fn parse_github_repository_name_with_owner_from_remote_url(
    url: Option<&str>,
) -> Option<String> {
    let trimmed = js_trim(url?);
    if trimmed.is_empty() {
        return None;
    }
    let name = GITHUB_REMOTE
        .captures(trimmed)
        .and_then(|caps| caps.get(1))
        .map(|m| js_trim(m.as_str()))
        .unwrap_or("");
    (!name.is_empty()).then(|| name.to_owned())
}

fn derive_local_branch_name_candidates_from_remote_ref(
    branch_name: &str,
    remote_name: Option<&str>,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let first_slash = derive_local_branch_name_from_remote_ref(branch_name);
    if !first_slash.is_empty() {
        candidates.push(first_slash.to_owned());
    }
    if let Some(remote_name) = remote_name.filter(|name| !name.is_empty()) {
        let remote_prefix = format!("{remote_name}/");
        if branch_name.starts_with(&remote_prefix) && branch_name.len() > remote_prefix.len() {
            let candidate = branch_name[remote_prefix.len()..].to_owned();
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

/// Sanitize the picker's "Create new ref" entry: runs of ASCII whitespace git rejects become a
/// dash. Other characters, case, and existing dashes are left alone.
pub fn sanitize_new_ref_name(raw_name: &str) -> String {
    REF_NAME_WHITESPACE
        .replace_all(js_trim(raw_name), "-")
        .into_owned()
}

/// Hide `origin/*` remote refs when a matching local refName already exists.
pub fn dedupe_remote_branches_with_local_matches(refs: &[VcsRef]) -> Vec<VcsRef> {
    let local_branch_names: HashSet<&str> = refs
        .iter()
        .filter(|ref_name| ref_name.is_remote != Some(true))
        .map(|ref_name| ref_name.name.as_str())
        .collect();

    refs.iter()
        .filter(|ref_name| {
            if ref_name.is_remote != Some(true) {
                return true;
            }
            if ref_name
                .remote_name
                .as_ref()
                .map(TrimmedNonEmptyString::as_str)
                != Some("origin")
            {
                return true;
            }
            let candidates = derive_local_branch_name_candidates_from_remote_ref(
                ref_name.name.as_str(),
                ref_name
                    .remote_name
                    .as_ref()
                    .map(TrimmedNonEmptyString::as_str),
            );
            !candidates
                .iter()
                .any(|candidate| local_branch_names.contains(candidate.as_str()))
        })
        .cloned()
        .collect()
}

pub fn detect_source_control_provider_from_git_remote_url(
    remote_url: &str,
) -> Option<SourceControlProviderInfo> {
    detect_source_control_provider_from_remote_url(remote_url)
}

fn empty_remote_status() -> VcsStatusRemoteResult {
    VcsStatusRemoteResult {
        has_upstream: false,
        ahead_count: 0,
        behind_count: 0,
        ahead_of_default_count: None,
        pr: None,
    }
}

pub fn merge_git_status_parts(
    local: &VcsStatusLocalResult,
    remote: Option<&VcsStatusRemoteResult>,
) -> VcsStatusResult {
    let remote = remote.cloned().unwrap_or_else(empty_remote_status);
    VcsStatusResult {
        is_repo: local.is_repo,
        source_control_provider: local.source_control_provider.clone(),
        has_primary_remote: local.has_primary_remote,
        is_default_ref: local.is_default_ref,
        ref_name: local.ref_name.clone(),
        has_working_tree_changes: local.has_working_tree_changes,
        working_tree: local.working_tree.clone(),
        branch_changes: local.branch_changes.clone(),
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

pub fn apply_git_status_stream_event(
    current: Option<&VcsStatusResult>,
    event: &VcsStatusStreamEvent,
) -> VcsStatusResult {
    match event {
        VcsStatusStreamEvent::Snapshot { local, remote } => {
            merge_git_status_parts(local, remote.as_ref())
        }
        VcsStatusStreamEvent::LocalUpdated { local } => {
            merge_git_status_parts(local, current.map(to_remote_status_part).as_ref())
        }
        VcsStatusStreamEvent::RemoteUpdated { remote } => match current {
            None => merge_git_status_parts(
                &VcsStatusLocalResult {
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
                },
                remote.as_ref(),
            ),
            Some(current) => {
                merge_git_status_parts(&to_local_status_part(current), remote.as_ref())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::git::VcsStatusWorkingTreeFile;
    use turin_contracts::source_control::SourceControlProviderKind;

    fn naming(mode: BranchNamingMode, prefix: &str) -> BranchNamingOptions {
        BranchNamingOptions {
            mode,
            prefix: prefix.to_owned(),
            instructions: String::new(),
        }
    }

    #[test]
    fn normalizes_equivalent_github_remotes_across_protocol_variants() {
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
    fn preserves_nested_group_paths_for_providers_like_gitlab() {
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
        let origin = Some("git@github.com:a/b.git");
        assert_eq!(
            parse_origin_url_from_git_config("[remote.origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            origin
        );
        // Git folds the dotted form to lowercase but keeps quoted names as written.
        assert_eq!(
            parse_origin_url_from_git_config("[remote.Origin]\n\turl = git@github.com:a/b.git\n")
                .as_deref(),
            origin
        );
        assert_eq!(
            parse_origin_url_from_git_config("[remote \"Origin\"]\n\turl = git@github.com:x/y.git\n[remote \"origin\"]\n\turl = git@github.com:a/b.git\n").as_deref(),
            origin
        );
        assert_eq!(
            parse_origin_url_from_git_config(
                "[remote \"origin\"] # primary\n\turl = git@github.com:a/b.git\n"
            )
            .as_deref(),
            origin
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
        assert_eq!(
            parse_origin_url_from_git_config("[core]\n\tbare = false\n"),
            None
        );
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
                Some("T3Tools/T3Code")
            );
        }
    }

    #[test]
    fn matches_the_generated_temporary_worktree_ref_format() {
        assert!(is_temporary_worktree_branch(
            &build_temporary_worktree_branch_name(|byte_length| {
                assert_eq!(byte_length, 4);
                "DEADBEEF".to_owned()
            })
        ));
        assert!(is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}/deadbeef"
        )));
        assert!(is_temporary_worktree_branch(&format!(
            " {WORKTREE_BRANCH_PREFIX}/deadbeef "
        )));
        assert!(is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}/DEADBEEF"
        )));
    }

    #[test]
    fn normalizes_a_uuid_shaped_random_callback_to_the_canonical_8_hex_form() {
        assert_eq!(
            build_temporary_worktree_branch_name(
                |_| "f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12".to_owned()
            ),
            format!("{WORKTREE_BRANCH_PREFIX}/f4ae4e0e")
        );
    }

    #[test]
    fn matches_legacy_t3code_temporary_worktree_refs() {
        assert!(is_temporary_worktree_branch("t3code/deadbeef"));
        assert!(is_temporary_worktree_branch("t3code-deadbeef"));
        assert!(is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"
        ));
        assert!(!is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"
        )));
    }

    #[test]
    fn rejects_uuid_shaped_refs_that_are_not_rfc_4122_v4() {
        // version nibble is not 4
        assert!(!is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-1d48-b4f2-9cf0aa54ab12"
        ));
        // variant nibble is not [89ab]
        assert!(!is_temporary_worktree_branch(
            "t3code/f4ae4e0e-f971-4d48-c4f2-9cf0aa54ab12"
        ));
    }

    #[test]
    fn matches_the_flat_fallback_used_when_a_plain_t3_branch_exists() {
        let flat =
            flatten_temporary_worktree_branch_name(&format!("{WORKTREE_BRANCH_PREFIX}/deadbeef"));
        assert_eq!(flat, format!("{WORKTREE_BRANCH_PREFIX}-deadbeef"));
        assert!(is_temporary_worktree_branch(&flat));
        assert!(!is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}-deadbeef-extra"
        )));
        assert!(!is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}-feature"
        )));
        assert_eq!(
            flatten_temporary_worktree_branch_name("t3code/f4ae4e0e-f971-4d48-b4f2-9cf0aa54ab12"),
            format!("{WORKTREE_BRANCH_PREFIX}-f4ae4e0e")
        );
        assert_eq!(
            flatten_temporary_worktree_branch_name("t3code-deadbeef"),
            format!("{WORKTREE_BRANCH_PREFIX}-deadbeef")
        );
        assert_eq!(
            flatten_temporary_worktree_branch_name("t3code/deadbeef"),
            format!("{WORKTREE_BRANCH_PREFIX}-deadbeef")
        );
    }

    #[test]
    fn rejects_non_temporary_ref_names() {
        assert!(!is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}/feature/demo"
        )));
        assert!(!is_temporary_worktree_branch("main"));
        assert!(!is_temporary_worktree_branch(&format!(
            "{WORKTREE_BRANCH_PREFIX}/deadbeef-extra"
        )));
    }

    #[test]
    fn treats_a_remote_only_update_as_a_repository_when_local_state_is_missing() {
        let remote = VcsStatusRemoteResult {
            has_upstream: true,
            ahead_count: 2,
            behind_count: 1,
            ahead_of_default_count: None,
            pr: None,
        };
        let status = apply_git_status_stream_event(
            None,
            &VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            },
        );
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "isRepo": true,
                "hasPrimaryRemote": false,
                "isDefaultRef": false,
                "refName": null,
                "hasWorkingTreeChanges": false,
                "workingTree": { "files": [], "insertions": 0, "deletions": 0 },
                "hasUpstream": true,
                "aheadCount": 2,
                "behindCount": 1,
                "pr": null,
            })
        );
    }

    #[test]
    fn preserves_local_only_fields_when_applying_a_remote_update() {
        let current = VcsStatusResult {
            is_repo: true,
            source_control_provider: Some(SourceControlProviderInfo {
                kind: SourceControlProviderKind::new("github").unwrap(),
                name: TrimmedNonEmptyString::new("GitHub").unwrap(),
                base_url: "https://github.com".to_owned(),
            }),
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some(TrimmedNonEmptyString::new("feature/demo").unwrap()),
            has_working_tree_changes: true,
            working_tree: VcsStatusWorkingTree {
                files: vec![VcsStatusWorkingTreeFile {
                    path: serde_json::from_value(json!("src/demo.ts")).unwrap(),
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
        let status = apply_git_status_stream_event(
            Some(&current),
            &VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            },
        );
        assert_eq!(
            status,
            VcsStatusResult {
                has_upstream: true,
                ahead_count: 2,
                behind_count: 1,
                pr: None,
                ..current
            }
        );
    }

    #[test]
    fn preserves_the_namespace_of_feature_branches() {
        for namespace in ["feature", "fix", "team/jules"] {
            assert_eq!(
                sanitize_feature_branch_name(&format!("{namespace}/refine-toolbar")),
                format!("{namespace}/refine-toolbar")
            );
        }
    }

    #[test]
    fn sanitizes_an_explicit_namespace_before_preserving_it() {
        assert_eq!(
            sanitize_feature_branch_name(" \"FIX//Calendar recruitment filter\" "),
            "fix/calendar-recruitment-filter"
        );
    }

    #[test]
    fn keeps_the_fallback_for_unprefixed_input() {
        for (input, expected) in [
            ("refine toolbar", "feature/refine-toolbar"),
            ("", "feature/update"),
            (" /?. / ", "feature/update"),
            ("fix///", "feature/fix"),
        ] {
            assert_eq!(sanitize_feature_branch_name(input), expected, "{input}");
        }
    }

    #[test]
    fn keeps_the_existing_length_limit_for_namespaced_branches() {
        assert_eq!(
            sanitize_feature_branch_name(&format!("fix/{}", "x".repeat(80))),
            format!("fix/{}", "x".repeat(60))
        );
    }

    #[test]
    fn resolves_case_insensitive_collisions_within_the_generated_namespace() {
        assert_eq!(
            resolve_auto_feature_branch_name(
                &[
                    "FIX/refine-toolbar".to_owned(),
                    "fix/refine-toolbar-2".to_owned()
                ],
                Some("fix/refine-toolbar"),
            ),
            "fix/refine-toolbar-3"
        );
    }

    #[test]
    fn keeps_the_fallback_when_no_preferred_branch_is_supplied() {
        assert_eq!(
            resolve_auto_feature_branch_name(&["feature/update".to_owned()], None),
            "feature/update-2"
        );
    }

    #[test]
    fn resolves_against_namespace_blockers() {
        let cases: [(Option<&str>, &[&str], &str); 6] = [
            (Some("fix/name"), &["fix"], "fix-2/name"),
            (
                Some("team/jules/fix/name"),
                &["team/jules"],
                "team/jules-2/fix/name",
            ),
            (Some("fix/name"), &["fix/name/child"], "fix/name-2"),
            (Some("FIX/name"), &["FIX", "FIX-2"], "fix-3/name"),
            (
                Some("fix/name"),
                &["fixes", "fix/name-extra/child"],
                "fix/name",
            ),
            (None, &["feature", "feature-2"], "feature-3/update"),
        ];
        for (preferred, existing, expected) in cases {
            let existing: Vec<String> = existing.iter().map(|name| (*name).to_owned()).collect();
            assert_eq!(
                resolve_auto_feature_branch_name(&existing, preferred),
                expected,
                "{preferred:?}"
            );
        }
    }

    #[test]
    fn joins_static_prefixes_with_one_slash() {
        for prefix in ["t3code", "t3code/"] {
            assert_eq!(
                format_generated_branch_name(
                    "Add Search",
                    Some(&naming(BranchNamingMode::Static, prefix))
                ),
                "t3code/add-search"
            );
        }
    }

    #[test]
    fn supports_an_empty_prefix_and_preserves_user_prefix_casing() {
        assert_eq!(
            format_generated_branch_name("Add Search", Some(&naming(BranchNamingMode::Static, ""))),
            "add-search"
        );
        assert_eq!(
            format_generated_branch_name(
                "Add Search",
                Some(&naming(BranchNamingMode::Static, "Team/Julius/"))
            ),
            "Team/Julius/add-search"
        );
    }

    #[test]
    fn normalizes_invalid_static_prefixes() {
        for (prefix, expected) in [
            ("release..candidate", "release-candidate/add-search"),
            (" Team / Jules.lock/", "Team/Jules-lock/add-search"),
            ("-team//feature@{new}", "team/feature-new/add-search"),
            (" /?. / ", "add-search"),
        ] {
            assert_eq!(
                format_generated_branch_name(
                    "Add Search",
                    Some(&naming(BranchNamingMode::Static, prefix))
                ),
                expected,
                "{prefix}"
            );
        }
    }

    #[test]
    fn uses_the_models_semantic_prefix_without_the_stored_static_prefix() {
        assert_eq!(
            format_generated_branch_name(
                "feat/Add Search",
                Some(&naming(BranchNamingMode::Semantic, "t3code"))
            ),
            "feat/add-search"
        );
    }

    #[test]
    fn preserves_the_full_custom_name_including_case_dots_and_length() {
        let branch = format!("Julius/ABC-123/release.v2-{}", "x".repeat(70));
        assert_eq!(
            format_generated_branch_name(
                &format!(" {branch} "),
                Some(&naming(BranchNamingMode::Custom, "ignored"))
            ),
            branch
        );
    }

    #[test]
    fn sanitizes_new_ref_names_by_dashing_only_whitespace_git_rejects() {
        assert_eq!(
            sanitize_new_ref_name("  new branch\tname  "),
            "new-branch-name"
        );
        // U+00A0 is valid in a ref name, so it survives.
        assert_eq!(sanitize_new_ref_name("a\u{a0}b"), "a\u{a0}b");
    }

    #[test]
    fn hides_origin_remote_refs_that_have_a_local_match() {
        let remote = |name: &str, remote: &str| VcsRef {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            is_remote: Some(true),
            remote_name: Some(TrimmedNonEmptyString::new(remote).unwrap()),
            current: false,
            is_default: false,
            worktree_path: None,
        };
        let local = |name: &str| VcsRef {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            is_remote: None,
            remote_name: None,
            current: false,
            is_default: false,
            worktree_path: None,
        };
        let refs = vec![
            local("feature/demo"),
            remote("origin/feature/demo", "origin"),
            remote("origin/feature/other", "origin"),
            remote("upstream/feature/demo", "upstream"),
        ];
        let deduped = dedupe_remote_branches_with_local_matches(&refs);
        let names: Vec<&str> = deduped
            .iter()
            .map(|ref_name| ref_name.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "feature/demo",
                "origin/feature/other",
                "upstream/feature/demo"
            ]
        );
    }
}
