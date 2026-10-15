//! The branch-name helpers from upstream/packages/shared/src/git.ts that text generation
//! uses: `sanitizeBranchFragment`, `formatGeneratedBranchName`, `sanitizeFeatureBranchName`.
//!
//! Copied from the T06 `shared_git.rs` branch because `turin-git` is not on `stage` yet.
//! Dedupe when it lands. The copy keeps the regexes and their order.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::js_trim;
use turin_contracts::settings::{BranchNamingMode, BranchNamingOptions};

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

/// Upstream `sanitizeBranchFragment`: a lowercase git ref fragment of at most 64 characters.
pub fn sanitize_branch_fragment(raw: &str) -> String {
    let normalized = EDGE_SEPARATORS
        .replace_all(&QUOTES.replace_all(&js_trim(raw).to_lowercase(), ""), "")
        .into_owned();
    let fragment = FRAGMENT_UNSAFE.replace_all(&normalized, "-");
    let fragment = MULTI_SLASH.replace_all(&fragment, "/");
    let fragment = MULTI_DASH.replace_all(&fragment, "-");
    let fragment = EDGE_DOT_DASH.replace_all(&fragment, "");
    let capped: String = fragment.chars().take(64).collect();
    let collapsed = TRAILING_DOT_DASH.replace(&capped, "").into_owned();
    if collapsed.is_empty() {
        "update".to_owned()
    } else {
        collapsed
    }
}

/// Upstream `formatGeneratedBranchName`. Custom naming keeps the model's complete ref;
/// Git validates it on rename.
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
                    let unsafe_free = PREFIX_UNSAFE.replace_all(part, "-");
                    let single_dash = MULTI_DASH.replace_all(&unsafe_free, "-");
                    PREFIX_EDGE.replace_all(&single_dash, "").into_owned()
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

/// Upstream `sanitizeFeatureBranchName`: keeps a slash-separated namespace, otherwise `feature/`.
pub fn sanitize_feature_branch_name(raw: &str) -> String {
    let sanitized = sanitize_branch_fragment(raw);
    if sanitized.contains('/') {
        sanitized
    } else {
        format!("feature/{sanitized}")
    }
}
