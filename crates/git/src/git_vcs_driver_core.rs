//! Port of upstream/apps/server/src/vcs/GitVcsDriverCore.ts
//!
//! Pure parsers and failure classification. Stderr never leaves this module:
//! it echoes argv and remote URLs, which can carry credentials. Matching it
//! against fixed patterns yields a tag and a message that quotes none of the
//! matched text.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::git::{GitCommandFailureReason, VcsRef};
use turin_contracts::review::ReviewDiffFileStat;

use crate::remote_refs::parse_remote_ref_with_remote_names;

pub const GIT_LIST_BRANCHES_DEFAULT_LIMIT: usize = 100;

static BRANCH_AB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\+(\d+)\s+-(\d+)$").unwrap());
static REVIEW_NUMSTAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+|-)\t(\d+|-)\t([\s\S]*)$").unwrap());
static REMOTE_VERBOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+(\S+)\s+\((fetch|push)\)$").unwrap());
static CHECKOUT_PROGRESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Updating files:\s+(\d+)%\s+\((\d+)/(\d+)\)").unwrap());
static DIAGNOSTIC_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:fatal|error):|^!\s|^\s+!\s").unwrap());
static SSH_TRANSPORT_REFUSAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)Permission denied \((?:publickey|password|keyboard-interactive)|Permission denied, please try again|Host key verification failed",
    )
    .unwrap()
});
static REMOTE_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^remote:").unwrap());
static GIT_CONFIG_COUNT_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t\r\n\x0B\x0C]*(?:\+?\d+|-0+)").unwrap());

struct FailurePattern {
    pattern: Regex,
    reason: GitCommandFailureReason,
}

static FAILURE_PATTERNS: LazyLock<Vec<FailurePattern>> = LazyLock::new(|| {
    let pairs = [
        (
            r"(?i)would clobber existing tag",
            GitCommandFailureReason::TagWouldBeClobbered,
        ),
        (
            r"(?i)is already (?:used by worktree at|checked out at)",
            GitCommandFailureReason::BranchCheckedOutInWorktree,
        ),
        (
            r"(?i)a branch named .+ already exists",
            GitCommandFailureReason::BranchAlreadyExists,
        ),
        (
            r"(?i)(?:authentication failed|could not read Username|could not read Password|permission denied \([a-z-]+(?:,[a-z-]+)*\)|permission denied, please try again)",
            GitCommandFailureReason::AuthenticationFailed,
        ),
        (
            r"(?i)host key verification failed",
            GitCommandFailureReason::HostKeyUnverified,
        ),
        (
            r"(?i)(?:could not read from remote repository|does not appear to be a git repository|repository .+ not found)",
            GitCommandFailureReason::RemoteUnreachable,
        ),
        (
            r"(?i)fatal: '[^']+' already exists|destination path .+ already exists",
            GitCommandFailureReason::PathAlreadyExists,
        ),
    ];
    pairs
        .into_iter()
        .map(|(pattern, reason)| FailurePattern {
            pattern: Regex::new(pattern).unwrap(),
            reason,
        })
        .collect()
});

static FETCH_AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:fatal: (?:Authentication failed|could not read (?:Username|Password))\b|\S+: Permission denied \(publickey)",
    )
    .unwrap()
});
static FETCH_NETWORK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:fatal: |ssh: )?Could not resolve host(?:name)?\b|fatal: unable to access .+: (?:Could not resolve host|Failed to connect)\b|ssh: connect to host \S+ port \d+: (?:Connection timed out|Connection refused|Network is unreachable)\b)",
    )
    .unwrap()
});
static FETCH_MISSING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:remote: Repository not found\.?$|fatal: repository .+ not found$|fatal: .+ does not appear to be a git repository$)",
    )
    .unwrap()
});
static REF_LOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)^(?:(?:error|fatal): cannot lock ref\b|fatal: Unable to create ['"].+\.lock['"]:|error: fetching ref \S+ failed: (?:incorrect old value provided|reference already exists)$)"#,
    )
    .unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchAheadBehind {
    pub ahead: u64,
    pub behind: u64,
}

pub fn parse_branch_ab(value: &str) -> BranchAheadBehind {
    let Some(caps) = BRANCH_AB.captures(value) else {
        return BranchAheadBehind {
            ahead: 0,
            behind: 0,
        };
    };
    BranchAheadBehind {
        ahead: caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0),
        behind: caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumstatEntry {
    pub path: String,
    pub insertions: i64,
    pub deletions: i64,
}

pub fn parse_numstat_entries(stdout: &str) -> Vec<NumstatEntry> {
    let mut entries = Vec::new();
    for line in stdout.split(['\r', '\n']) {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let added_raw = parts.next().unwrap_or("0");
        let deleted_raw = parts.next().unwrap_or("0");
        let path_parts: Vec<&str> = parts.collect();
        let joined = path_parts.join("\t");
        let raw_path = if path_parts.len() > 1 {
            path_parts.last().copied().unwrap_or("").trim()
        } else {
            joined.trim()
        };
        if raw_path.is_empty() {
            continue;
        }
        let normalized = match raw_path.find(" => ") {
            Some(index) => raw_path[index + " => ".len()..].trim(),
            None => raw_path,
        };
        entries.push(NumstatEntry {
            path: if normalized.is_empty() {
                raw_path.to_owned()
            } else {
                normalized.to_owned()
            },
            insertions: js_parse_int(added_raw),
            deletions: js_parse_int(deleted_raw),
        });
    }
    // A `\r\n` split on both separators yields an empty field between them.
    entries.retain(|entry| !entry.path.is_empty());
    entries
}

static SANITIZE_REMOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9._-]+").unwrap());
static SANITIZE_REMOTE_EDGES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-+|-+$").unwrap());

/// `-z` preserves tabs and newlines in paths and gives renames two path fields.
pub fn parse_review_numstat(stdout: &str) -> Vec<ReviewDiffFileStat> {
    let fields: Vec<&str> = stdout.split('\0').collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        let Some(caps) = REVIEW_NUMSTAT.captures(field) else {
            index += 1;
            continue;
        };
        let path_field = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let (previous_path, path) = if path_field.is_empty() {
            index += 1;
            let previous = (*fields.get(index).unwrap_or(&"")).to_owned();
            index += 1;
            let path = (*fields.get(index).unwrap_or(&"")).to_owned();
            (Some(previous), path)
        } else {
            (None, path_field.to_owned())
        };
        let additions = caps.get(1).map(|m| m.as_str()).unwrap_or("-");
        let deletions = caps.get(2).map(|m| m.as_str()).unwrap_or("-");
        files.push(ReviewDiffFileStat {
            path,
            previous_path,
            additions: if additions == "-" {
                0.0
            } else {
                additions.parse().unwrap_or(0.0)
            },
            deletions: if deletions == "-" {
                0.0
            } else {
                deletions.parse().unwrap_or(0.0)
            },
        });
        index += 1;
    }
    files
}

pub fn parse_porcelain_path(line: &str) -> Option<String> {
    if line.starts_with("? ") || line.starts_with("! ") {
        let simple = line[2..].trim();
        return if simple.is_empty() {
            None
        } else {
            Some(simple.to_owned())
        };
    }
    if !(line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("u ")) {
        return None;
    }
    if let Some(tab_index) = line.find('\t') {
        let from_tab = &line[tab_index + 1..];
        let file_path = from_tab.split('\t').next().unwrap_or("");
        let trimmed = file_path.trim();
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        };
    }
    let file_path = line.split_whitespace().next_back().unwrap_or("");
    if file_path.is_empty() {
        None
    } else {
        Some(file_path.to_owned())
    }
}

pub fn filter_branches_for_list_query(refs: &[VcsRef], query: Option<&str>) -> Vec<VcsRef> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return refs.to_vec();
    };
    let normalized = query.to_lowercase();
    refs.iter()
        .filter(|reference| reference.name.as_str().to_lowercase().contains(&normalized))
        .cloned()
        .collect()
}

pub struct PaginatedBranches {
    pub refs: Vec<VcsRef>,
    pub next_cursor: Option<usize>,
    pub total_count: usize,
}

pub fn paginate_branches(
    refs: &[VcsRef],
    cursor: Option<usize>,
    limit: Option<usize>,
) -> PaginatedBranches {
    let cursor = cursor.unwrap_or(0);
    let limit = limit.unwrap_or(GIT_LIST_BRANCHES_DEFAULT_LIMIT);
    let total_count = refs.len();
    let end = cursor.saturating_add(limit).min(total_count);
    let page = if cursor >= total_count {
        Vec::new()
    } else {
        refs[cursor..end].to_vec()
    };
    let next_cursor = if cursor + page.len() < total_count {
        Some(cursor + page.len())
    } else {
        None
    };
    PaginatedBranches {
        refs: page,
        next_cursor,
        total_count,
    }
}

/// Branch name → worktree path. Prunable worktrees are omitted.
pub fn parse_worktree_branch_paths(stdout: &str) -> HashMap<String, String> {
    let mut worktree_paths = HashMap::new();
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut current_prunable = false;
    let flush = |paths: &mut HashMap<String, String>,
                 path: &mut Option<String>,
                 branch: &mut Option<String>,
                 prunable: &mut bool| {
        if let (Some(path), Some(branch)) = (path.take(), branch.take())
            && !*prunable
        {
            paths.insert(branch, path);
        }
        *prunable = false;
    };
    for field in stdout.split('\0') {
        if field.is_empty() {
            flush(
                &mut worktree_paths,
                &mut current_path,
                &mut current_branch,
                &mut current_prunable,
            );
        } else if let Some(path) = field.strip_prefix("worktree ") {
            current_path = Some(path.to_owned());
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_owned());
        } else if field == "prunable" || field.starts_with("prunable ") {
            current_prunable = true;
        }
    }
    flush(
        &mut worktree_paths,
        &mut current_path,
        &mut current_branch,
        &mut current_prunable,
    );
    worktree_paths
}

pub fn split_null_separated_paths(input: &str, truncated: bool) -> Vec<String> {
    let mut parts: Vec<&str> = input.split('\0').collect();
    if parts.is_empty() {
        return Vec::new();
    }
    if truncated && parts.last().is_some_and(|part| !part.is_empty()) {
        parts.pop();
    }
    parts
        .into_iter()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn sanitize_remote_name(value: &str) -> String {
    let sanitized = SANITIZE_REMOTE.replace_all(value.trim(), "-");
    let sanitized = SANITIZE_REMOTE_EDGES.replace_all(&sanitized, "");
    if sanitized.is_empty() {
        "fork".to_owned()
    } else {
        sanitized.into_owned()
    }
}

pub fn parse_remote_fetch_urls(stdout: &str) -> HashMap<String, String> {
    let mut remotes = HashMap::new();
    for line in stdout.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(caps) = REMOTE_VERBOSE.captures(trimmed) else {
            continue;
        };
        let remote_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let remote_url = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let direction = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        if direction != "fetch" || remote_name.is_empty() || remote_url.is_empty() {
            continue;
        }
        remotes.insert(remote_name.to_owned(), remote_url.to_owned());
    }
    remotes
}

pub struct ParsedUpstreamRef {
    pub upstream_ref: String,
    pub remote_name: String,
    pub branch_name: String,
}

pub fn parse_upstream_ref_with_remote_names(
    upstream_ref: &str,
    remote_names: &[String],
) -> Option<ParsedUpstreamRef> {
    let parsed = parse_remote_ref_with_remote_names(upstream_ref, remote_names)?;
    Some(ParsedUpstreamRef {
        upstream_ref: upstream_ref.to_owned(),
        remote_name: parsed.remote_name,
        branch_name: parsed.branch_name,
    })
}

pub fn parse_upstream_ref_by_first_separator(upstream_ref: &str) -> Option<ParsedUpstreamRef> {
    let separator = upstream_ref.find('/')?;
    if separator == 0 || separator + 1 == upstream_ref.len() {
        return None;
    }
    let remote_name = upstream_ref[..separator].trim();
    let branch_name = upstream_ref[separator + 1..].trim();
    if remote_name.is_empty() || branch_name.is_empty() {
        return None;
    }
    Some(ParsedUpstreamRef {
        upstream_ref: upstream_ref.to_owned(),
        remote_name: remote_name.to_owned(),
        branch_name: branch_name.to_owned(),
    })
}

pub fn parse_tracking_branch_by_upstream_ref(stdout: &str, upstream_ref: &str) -> Option<String> {
    for line in stdout.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split('\t');
        let branch_name = parts.next().unwrap_or("").trim();
        let candidate = parts.next().unwrap_or("").trim();
        if branch_name.is_empty() || candidate.is_empty() {
            continue;
        }
        if candidate == upstream_ref {
            return Some(branch_name.to_owned());
        }
    }
    None
}

pub fn derive_local_branch_name_from_remote_ref(branch_name: &str) -> Option<String> {
    let separator = branch_name.find('/')?;
    if separator == 0 || separator + 1 == branch_name.len() {
        return None;
    }
    let local = branch_name[separator + 1..].trim();
    if local.is_empty() {
        None
    } else {
        Some(local.to_owned())
    }
}

/// Classifies git's own diagnostic lines. Hook text and `remote:` lines are ignored,
/// except an unprefixed local ssh refusal, which git only summarizes generically.
pub fn classify_git_failure(stderr: &str) -> Option<GitCommandFailureReason> {
    let diagnostics = stderr
        .split(['\r', '\n'])
        .filter(|line| {
            DIAGNOSTIC_LINE.is_match(line)
                || (!REMOTE_PREFIX.is_match(line) && SSH_TRANSPORT_REFUSAL.is_match(line))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if diagnostics.is_empty() {
        return None;
    }
    if is_non_repository_git_stderr(&diagnostics) {
        return Some(GitCommandFailureReason::NotARepository);
    }
    for pattern in FAILURE_PATTERNS.iter() {
        if pattern.pattern.is_match(&diagnostics) {
            return Some(pattern.reason);
        }
    }
    None
}

pub fn parse_default_branch_from_remote_head_ref(value: &str, remote_name: &str) -> Option<String> {
    let trimmed = value.trim();
    let prefix = format!("refs/remotes/{remote_name}/");
    let ref_name = trimmed.strip_prefix(&prefix)?.trim();
    if ref_name.is_empty() {
        None
    } else {
        Some(ref_name.to_owned())
    }
}

pub fn is_non_repository_git_stderr(stderr: &str) -> bool {
    stderr.to_lowercase().contains("not a git repository")
}

pub fn is_unborn_head_stderr(stderr: &str) -> bool {
    let normalized = stderr.to_lowercase();
    normalized.contains("bad revision 'head'")
        || (normalized.contains("unknown revision")
            && normalized.contains("path not in the working tree"))
}

pub fn is_missing_worktree_stderr(stderr: &str) -> bool {
    let normalized = stderr.to_lowercase();
    normalized.contains("is not a working tree")
        || normalized.contains("cannot remove working tree")
}

/// Fetch stderr can contain remote credentials. Only these fixed diagnoses may
/// enter persisted errors.
pub fn fetch_failure_detail(stderr: &str) -> Option<&'static str> {
    let lines: Vec<&str> = stderr.split(['\r', '\n']).map(str::trim).collect();
    if lines.iter().any(|line| FETCH_AUTH.is_match(line)) {
        return Some(
            "Git could not authenticate with the remote. Check Git credentials or SSH access on the server, then retry.",
        );
    }
    if lines.iter().any(|line| FETCH_NETWORK.is_match(line)) {
        return Some(
            "Git could not reach the remote. Check the server's network connection and remote host, then retry.",
        );
    }
    if lines.iter().any(|line| FETCH_MISSING.is_match(line)) {
        return Some(
            "Git could not access the remote repository. Check the remote URL and repository permissions on the server.",
        );
    }
    if is_ref_lock_failure(stderr) {
        return Some(
            "Git could not update a local reference. Another Git operation or a stale lock may be blocking the fetch; check the repository on the server, then retry.",
        );
    }
    None
}

pub fn is_ref_lock_failure(stderr: &str) -> bool {
    stderr
        .split(['\r', '\n'])
        .any(|line| REF_LOCK.is_match(line.trim()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckoutProgress {
    pub percent: u64,
    pub completed: u64,
    pub total: u64,
}

pub fn parse_git_checkout_progress_line(line: &str) -> Option<CheckoutProgress> {
    let caps = CHECKOUT_PROGRESS.captures(line)?;
    let percent = caps.get(1)?.as_str().parse::<u64>().ok()?;
    let completed = caps.get(2)?.as_str().parse::<u64>().ok()?;
    let total = caps.get(3)?.as_str().parse::<u64>().ok()?;
    Some(CheckoutProgress {
        percent: percent.min(100),
        completed,
        total,
    })
}

/// Appends `core.longpaths=true` through `GIT_CONFIG_*` on Windows. A count git
/// would reject is left alone so git still reports it.
pub fn windows_long_path_config_env(
    platform: &str,
    env: &HashMap<String, String>,
) -> HashMap<String, String> {
    if platform != "win32" {
        return HashMap::new();
    }
    let count_key = env
        .keys()
        .find(|key| key.eq_ignore_ascii_case("GIT_CONFIG_COUNT"))
        .cloned()
        .unwrap_or_else(|| "GIT_CONFIG_COUNT".to_owned());
    let inherited = env.get(&count_key).map(String::as_str);
    if let Some(inherited) = inherited
        && !inherited.is_empty()
    {
        let matched = GIT_CONFIG_COUNT_VALUE.find(inherited);
        if matched.is_none_or(|found| found.as_str() != inherited) {
            return HashMap::new();
        }
    }
    let count = match inherited {
        None | Some("") => 0,
        Some(value) => js_parse_int(value).max(0) as u64,
    };
    let mut next = HashMap::new();
    next.insert(count_key, (count + 1).to_string());
    next.insert(
        format!("GIT_CONFIG_KEY_{count}"),
        "core.longpaths".to_owned(),
    );
    next.insert(format!("GIT_CONFIG_VALUE_{count}"), "true".to_owned());
    next
}

fn js_parse_int(value: &str) -> i64 {
    let rest = value.trim_start_matches(|c: char| {
        matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{000B}' | '\u{000C}')
    });
    let (negative, digits) = if let Some(rest) = rest.strip_prefix('+') {
        (false, rest)
    } else if let Some(rest) = rest.strip_prefix('-') {
        (true, rest)
    } else {
        (false, rest)
    };
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return 0;
    }
    let magnitude: i64 = digits.parse().unwrap_or(0);
    if negative { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    fn reason(stderr: &str) -> Option<&'static str> {
        classify_git_failure(stderr).map(|reason| match reason {
            GitCommandFailureReason::AuthenticationFailed => "authentication_failed",
            GitCommandFailureReason::BranchAlreadyExists => "branch_already_exists",
            GitCommandFailureReason::BranchCheckedOutInWorktree => "branch_checked_out_in_worktree",
            GitCommandFailureReason::HostKeyUnverified => "host_key_unverified",
            GitCommandFailureReason::NotARepository => "not_a_repository",
            GitCommandFailureReason::PathAlreadyExists => "path_already_exists",
            GitCommandFailureReason::RemoteUnreachable => "remote_unreachable",
            GitCommandFailureReason::TagWouldBeClobbered => "tag_would_be_clobbered",
        })
    }

    #[test]
    fn classifies_only_git_diagnostics_and_local_ssh() {
        assert_eq!(
            reason("fatal: would clobber existing tag 'v1'"),
            Some("tag_would_be_clobbered")
        );
        assert_eq!(
            reason("fatal: 'feature' is already used by worktree at /secret"),
            Some("branch_checked_out_in_worktree")
        );
        assert_eq!(
            reason("fatal: a branch named 'feature' already exists"),
            Some("branch_already_exists")
        );
        assert_eq!(
            reason("fatal: Authentication failed for 'https://user:token@github.com/x'"),
            Some("authentication_failed")
        );
        assert_eq!(
            reason("Permission denied (publickey)."),
            Some("authentication_failed")
        );
        assert_eq!(reason("remote: Permission denied (publickey)."), None);
        assert_eq!(reason("authentication failed"), None);
        assert_eq!(
            reason("fatal: Host key verification failed."),
            Some("host_key_unverified")
        );
        assert_eq!(
            reason("fatal: repository 'git@github.com:a/b.git' not found"),
            Some("remote_unreachable")
        );
        assert_eq!(
            reason("fatal: '/tmp/worktree' already exists"),
            Some("path_already_exists")
        );
        assert_eq!(reason("fatal: something already exists"), None);
        assert_eq!(
            reason("fatal: not a git repository (or any of the parent directories): .git"),
            Some("not_a_repository")
        );
    }

    #[test]
    fn fetch_details_are_fixed_sentences() {
        assert!(
            fetch_failure_detail("fatal: Authentication failed for 'https://token@host/x'")
                .unwrap()
                .contains("authenticate")
        );
        assert!(fetch_failure_detail("ssh: Could not resolve hostname github.com").is_some());
        assert!(fetch_failure_detail("fatal: repository 'x' not found").is_some());
        assert!(
            fetch_failure_detail(
                "error: fetching ref refs/heads/main failed: incorrect old value provided"
            )
            .unwrap()
            .contains("local reference")
        );
        assert!(fetch_failure_detail("fatal: user:token@host").is_none());
    }

    #[test]
    fn parses_checkout_progress() {
        assert_eq!(
            parse_git_checkout_progress_line("Updating files:  78% (2104/2700)"),
            Some(CheckoutProgress {
                percent: 78,
                completed: 2104,
                total: 2700,
            })
        );
        assert_eq!(
            parse_git_checkout_progress_line("Updating files: 100% (2700/2700), done."),
            Some(CheckoutProgress {
                percent: 100,
                completed: 2700,
                total: 2700,
            })
        );
        assert_eq!(
            parse_git_checkout_progress_line("Preparing worktree (new branch 'x')"),
            None
        );
    }

    #[test]
    fn windows_long_paths_append_after_the_inherited_count() {
        assert!(windows_long_path_config_env("linux", &HashMap::new()).is_empty());
        let mut env = HashMap::new();
        env.insert("git_config_count".to_owned(), "2".to_owned());
        let next = windows_long_path_config_env("win32", &env);
        assert_eq!(next.get("git_config_count").map(String::as_str), Some("3"));
        assert_eq!(
            next.get("GIT_CONFIG_KEY_2").map(String::as_str),
            Some("core.longpaths")
        );
        assert_eq!(
            next.get("GIT_CONFIG_VALUE_2").map(String::as_str),
            Some("true")
        );

        for (count, expected) in [
            (None, "1"),
            (Some(""), "1"),
            (Some("0"), "1"),
            (Some("-0"), "1"),
            (Some("2"), "3"),
            (Some(" 2"), "3"),
            (Some("+2"), "3"),
            (Some("02"), "3"),
        ] {
            let env = match count {
                None => HashMap::new(),
                Some(count) => HashMap::from([("GIT_CONFIG_COUNT".to_owned(), count.to_owned())]),
            };
            assert_eq!(
                windows_long_path_config_env("win32", &env)
                    .get("GIT_CONFIG_COUNT")
                    .map(String::as_str),
                Some(expected),
                "{count:?}"
            );
        }
        for count in ["nope", "2x", "-1", "1.5", "  ", "1 "] {
            let env = HashMap::from([("GIT_CONFIG_COUNT".to_owned(), count.to_owned())]);
            assert!(
                windows_long_path_config_env("win32", &env).is_empty(),
                "{count}"
            );
        }
    }

    #[test]
    fn review_numstat_keeps_rename_paths() {
        let files = parse_review_numstat("1\t2\tnote.txt\0-\t-\t\0old.txt\0new.txt\0");
        assert_eq!(files[0].path, "note.txt");
        assert_eq!(files[0].additions, 1.0);
        assert_eq!(files[0].previous_path, None);
        assert_eq!(files[1].previous_path.as_deref(), Some("old.txt"));
        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].additions, 0.0);
    }

    #[test]
    fn porcelain_and_worktrees() {
        assert_eq!(
            parse_porcelain_path("? note.txt").as_deref(),
            Some("note.txt")
        );
        assert_eq!(
            parse_porcelain_path("1 .M N... 100644 100644 100644 a b\tnote.txt").as_deref(),
            Some("note.txt")
        );
        let paths = parse_worktree_branch_paths(
            "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /gone\0branch refs/heads/old\0prunable\0\0",
        );
        assert_eq!(paths.get("main").map(String::as_str), Some("/repo"));
        assert!(!paths.contains_key("old"));
    }

    #[test]
    fn paginates_branch_names() {
        let refs: Vec<VcsRef> = ["main", "feature"]
            .into_iter()
            .map(|name| VcsRef {
                name: TrimmedNonEmptyString::new(name).unwrap(),
                is_remote: None,
                remote_name: None,
                current: false,
                is_default: false,
                worktree_path: None,
            })
            .collect();
        let page = paginate_branches(&refs, Some(0), Some(1));
        assert_eq!(page.refs.len(), 1);
        assert_eq!(page.next_cursor, Some(1));
        assert_eq!(page.total_count, 2);
        assert_eq!(sanitize_remote_name("  My Fork!! "), "My-Fork");
        assert_eq!(sanitize_remote_name("!!!"), "fork");
        assert_eq!(
            parse_branch_ab("+3 -1"),
            BranchAheadBehind {
                ahead: 3,
                behind: 1
            }
        );
    }
}
