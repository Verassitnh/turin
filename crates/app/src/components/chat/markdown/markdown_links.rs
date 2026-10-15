//! Port of the path classification in upstream/packages/shared/src/markdownLinks.ts
//! (`inlineCodeFilePathCandidate`, `parseMarkdownFileLink` / `looksLikeFilePath`)
//! and `fileLinkLabel` from fileLinks.ts: which inline code spans and link
//! destinations ChatMarkdown draws as file chips.

use std::sync::LazyLock;

use regex::Regex;

static RELATIVE_PATH_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(~/|\.{1,2}/)").expect("static regex"));
static RELATIVE_FILE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*/)+[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*(?::\d+){0,2}$")
        .expect("static regex")
});
static RELATIVE_FILE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*\.[A-Za-z0-9_-]+(?::\d+){0,2}$")
        .expect("static regex")
});
static EXTERNAL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z][A-Za-z0-9+.-]*):(.*)$").expect("static regex"));
static POSITION_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+(?::\d+)?$").expect("static regex"));
static POSITION_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(?::\d+)?$").expect("static regex"));
static FILE_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[A-Za-z0-9_-]+$").expect("static regex"));
// A final dot between digits marks a version or model id (`glm-5.3`,
// `Qwen2.5-Coder`), not an extension. `ls.1` and `libfoo.so.1` stay files.
static VERSION_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d\.\d[^.]*$").expect("static regex"));
static NUMERIC_DOTTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(?:\.\d+)+$").expect("static regex"));

// Standard OS and dev-container roots; deliberately excludes app-route-ish
// prefixes like /app/ or /chat/ so SPA routes never read as files.
const POSIX_FILE_ROOT_PREFIXES: &[&str] = &[
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/",
    "/etc/",
    "/opt/",
    "/mnt/",
    "/Volumes/",
    "/private/",
    "/root/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/lib/",
    "/lib64/",
    "/srv/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/run/",
    "/boot/",
    "/media/",
    "/workspace/",
    "/workspaces/",
];

// `Name:digits` also matches `error:1`, `port:3000`, and `TODO:12`.
const EXTENSIONLESS_FILE_NAMES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Procfile",
    "Brewfile",
    "Caddyfile",
    "Vagrantfile",
    "Jenkinsfile",
    "Podfile",
    "Fastfile",
    "BUILD",
    "WORKSPACE",
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTORS",
    "CHANGELOG",
    "README",
    "CODEOWNERS",
];

const GENERIC_HOSTNAME_TLDS: &[&str] = &[
    "com", "net", "org", "io", "dev", "app", "ai", "co", "edu", "gov", "mil", "info", "biz", "xyz",
    "me", "tv", "cc", "gg", "chat", "cloud", "site", "online", "tech", "store", "link",
];

// Country codes also name file extensions. A :line suffix makes `.pl`
// and `.pt` files more likely than hostnames.
const COUNTRY_HOSTNAME_TLDS: &[&str] = &[
    "uk", "de", "fr", "nl", "se", "no", "fi", "dk", "pl", "ch", "at", "be", "es", "it", "pt", "eu",
    "us", "ca", "au", "nz", "jp", "kr", "cn", "br", "ru", "mx", "ie", "cz", "tr", "sg", "hk",
];

fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with("\\\\")
}

fn looks_like_hostname(segment: &str, has_position: bool) -> bool {
    if segment.starts_with('.') {
        return false;
    }
    let lowered = segment.to_lowercase();
    if lowered == "localhost" || NUMERIC_DOTTED.is_match(segment) {
        return true;
    }
    let labels: Vec<&str> = lowered.split('.').collect();
    let Some(last) = labels.last().filter(|_| labels.len() >= 2) else {
        return false;
    };
    if GENERIC_HOSTNAME_TLDS.contains(last) {
        return true;
    }
    !has_position && COUNTRY_HOSTNAME_TLDS.contains(last)
}

/// `inlineCodeFilePathCandidate`: path-shaped inline code, or `None`.
pub fn inline_code_file_path_candidate(code_text: &str) -> Option<String> {
    let trimmed = code_text.trim();
    if trimmed.is_empty() || trimmed.contains(|c: char| c.is_whitespace() || c == '`') {
        return None;
    }
    let candidate = if is_windows_absolute_path(trimmed) {
        trimmed.to_string()
    } else {
        trimmed.replace('\\', "/")
    };
    let has_position = POSITION_SUFFIX.is_match(&candidate);
    if !has_position && !candidate.contains(['/', '\\']) {
        return None;
    }
    let explicit = RELATIVE_PATH_PREFIX.is_match(&candidate)
        || candidate.starts_with('/')
        || is_windows_absolute_path(&candidate);
    if !explicit {
        let without_position = POSITION_SUFFIX.replace(&candidate, "").to_string();
        let first_segment = without_position
            .split('/')
            .next()
            .unwrap_or(&without_position);
        if looks_like_hostname(first_segment, has_position) {
            return None;
        }
        let basename = without_position
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        if VERSION_SUFFIX.is_match(basename) {
            return None;
        }
        if !has_position && !FILE_EXTENSION.is_match(basename) {
            return None;
        }
    }
    Some(candidate)
}

fn looks_like_posix_filesystem_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if POSIX_FILE_ROOT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || POSITION_SUFFIX.is_match(path)
    {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or("");
    EXTENSIONLESS_FILE_NAMES.contains(&basename) || FILE_EXTENSION.is_match(basename)
}

/// `parseMarkdownFileLink` (reduced): the file path a link destination names.
pub fn parse_markdown_file_link(href: &str) -> Option<String> {
    let trimmed = href.trim();
    let normalized = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed);
    if normalized.is_empty() || normalized.starts_with('#') || normalized.starts_with("//") {
        return None;
    }
    let path = if let Some(rest) = normalized
        .strip_prefix("file://")
        .or_else(|| normalized.strip_prefix("FILE://"))
    {
        rest.to_string()
    } else {
        let without_hash = normalized.split('#').next().unwrap_or(normalized);
        without_hash
            .split('?')
            .next()
            .unwrap_or(without_hash)
            .to_string()
    };
    let path = path.trim().to_string();
    if path.is_empty() {
        return None;
    }
    if !is_windows_absolute_path(&path)
        && let Some(captures) = EXTERNAL_SCHEME.captures(&path)
    {
        let rest = captures.get(2).map_or("", |m| m.as_str());
        if rest.starts_with("//") || !POSITION_ONLY.is_match(rest) {
            return None;
        }
    }
    let path_only = POSITION_SUFFIX.replace(&path, "").to_string();
    let looks = if is_windows_absolute_path(&path_only) || RELATIVE_PATH_PREFIX.is_match(&path_only)
    {
        true
    } else if path_only.starts_with('/') {
        looks_like_posix_filesystem_path(&path)
    } else if EXTENSIONLESS_FILE_NAMES.contains(&path_only.as_str()) {
        true
    } else {
        RELATIVE_FILE_PATH.is_match(&path) || RELATIVE_FILE_NAME.is_match(&path_only)
    };
    looks.then_some(path)
}

/// `isMarkdownFileLinkLabel`: the link text names the destination (its path
/// or a suffix of it), so the chip alone stands for it.
pub fn is_markdown_file_link_label(label: &str, destination: &str) -> bool {
    let normalize = |path: &str| {
        path.replace('\\', "/")
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string()
    };
    let label_path = normalize(&POSITION_SUFFIX.replace(label.trim(), ""));
    let destination_path = normalize(&POSITION_SUFFIX.replace(destination, ""));
    label_path.is_empty()
        || destination_path == label_path
        || destination_path.ends_with(&format!("/{label_path}"))
}

/// `fileLinkLabel`: basename plus `L12:C3`. (The parent-directory suffix that
/// disambiguates equal basenames is not ported.)
pub fn file_link_label(path_with_position: &str) -> String {
    let mut path = path_with_position;
    let mut line = None;
    let mut column = None;
    if let Some(m) = POSITION_SUFFIX.find(path_with_position) {
        let parts: Vec<&str> = m.as_str().trim_start_matches(':').split(':').collect();
        line = parts.first().copied();
        column = parts.get(1).copied();
        path = &path_with_position[..m.start()];
    }
    let basename = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path);
    match (line, column) {
        (Some(line), Some(column)) => format!("{basename} · L{line}:C{column}"),
        (Some(line), None) => format!("{basename} · L{line}"),
        _ => basename.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_candidates() {
        assert_eq!(
            inline_code_file_path_candidate("src/main.ts").as_deref(),
            Some("src/main.ts")
        );
        assert_eq!(
            inline_code_file_path_candidate("design.md:12").as_deref(),
            Some("design.md:12")
        );
        assert_eq!(
            inline_code_file_path_candidate("./run.sh").as_deref(),
            Some("./run.sh")
        );
        assert_eq!(inline_code_file_path_candidate("node.meta"), None);
        assert_eq!(inline_code_file_path_candidate("origin/main"), None);
        assert_eq!(inline_code_file_path_candidate("example.com/path.ts"), None);
        assert_eq!(inline_code_file_path_candidate("models/glm-5.3"), None);
        assert_eq!(inline_code_file_path_candidate("a b/c.ts"), None);
    }

    #[test]
    fn link_destinations() {
        assert_eq!(
            parse_markdown_file_link("src/app.rs").as_deref(),
            Some("src/app.rs")
        );
        assert_eq!(
            parse_markdown_file_link("/Users/me/x").as_deref(),
            Some("/Users/me/x")
        );
        assert_eq!(parse_markdown_file_link("https://github.com/x"), None);
        assert_eq!(parse_markdown_file_link("/chat/settings"), None);
        assert_eq!(parse_markdown_file_link("#section"), None);
    }

    #[test]
    fn file_link_labels_match_destinations() {
        assert!(is_markdown_file_link_label(
            "main.rs",
            "crates/app/src/main.rs"
        ));
        assert!(is_markdown_file_link_label("src/main.rs", "./src/main.rs"));
        assert!(!is_markdown_file_link_label(
            "the entry point",
            "src/main.rs"
        ));
    }

    #[test]
    fn labels() {
        assert_eq!(file_link_label("crates/app/src/main.rs"), "main.rs");
        assert_eq!(file_link_label("src/a.ts:12"), "a.ts · L12");
        assert_eq!(file_link_label("src/a.ts:12:3"), "a.ts · L12:C3");
    }
}
