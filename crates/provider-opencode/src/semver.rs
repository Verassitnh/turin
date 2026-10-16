//! Local copy of the slice of `upstream/packages/shared/src/semver.ts` the
//! OpenCode probe needs. `turin-shared` does not export these yet.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSemver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<String>,
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Upstream does `split("-", 2)`, so a prerelease keeps only what precedes the
/// second hyphen (`1.2.3-beta-1` gives `beta`). An empty prerelease is absent,
/// as the JS truthiness check treats it.
fn split_prerelease(version: &str) -> (&str, Option<&str>) {
    let mut parts = version.splitn(3, '-');
    let main = parts.next().unwrap_or("");
    let prerelease = parts.next().filter(|prerelease| !prerelease.is_empty());
    (main, prerelease)
}

/// Pad shorthand versions (`"20"`, `"20.1"`) to three segments. Inputs with
/// more than three segments are left untouched.
pub fn normalize_semver_version(version: &str) -> String {
    let (main, prerelease) = split_prerelease(version.trim());
    let mut segments: Vec<&str> = main
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    while !segments.is_empty() && segments.len() < 3 {
        segments.push("0");
    }
    match prerelease {
        Some(prerelease) => format!("{}-{prerelease}", segments.join(".")),
        None => segments.join("."),
    }
}

pub fn parse_semver(value: &str) -> Option<ParsedSemver> {
    let normalized = normalize_semver_version(value);
    let normalized = normalized.strip_prefix('v').unwrap_or(&normalized);
    let (main, prerelease) = split_prerelease(normalized);
    let segments: Vec<&str> = main.split('.').collect();
    if segments.len() != 3 {
        return None;
    }
    let (major, minor, patch) = (segments[0], segments[1], segments[2]);
    if !is_digits(major) || !is_digits(minor) || !is_digits(patch) {
        return None;
    }
    Some(ParsedSemver {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        patch: patch.parse().ok()?,
        prerelease: parse_prerelease_segments(prerelease),
    })
}

fn parse_prerelease_segments(prerelease: Option<&str>) -> Vec<String> {
    let Some(prerelease) = prerelease else {
        return Vec::new();
    };
    prerelease
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn compare_prerelease_identifier(left: &str, right: &str) -> i32 {
    let left_numeric = is_digits(left);
    let right_numeric = is_digits(right);
    if left_numeric && right_numeric {
        let left: u64 = left.parse().unwrap_or(0);
        let right: u64 = right.parse().unwrap_or(0);
        return match left.cmp(&right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
    }
    if left_numeric {
        return -1;
    }
    if right_numeric {
        return 1;
    }
    match left.cmp(right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Numeric compare when both sides parse. Otherwise JS `localeCompare` on the
/// raw strings, which for ASCII versions matches byte order.
pub fn compare_semver_versions(left: &str, right: &str) -> i32 {
    let Some(parsed_left) = parse_semver(left) else {
        return str_cmp(left, right);
    };
    let Some(parsed_right) = parse_semver(right) else {
        return str_cmp(left, right);
    };
    if parsed_left.major != parsed_right.major {
        return cmp_u64(parsed_left.major, parsed_right.major);
    }
    if parsed_left.minor != parsed_right.minor {
        return cmp_u64(parsed_left.minor, parsed_right.minor);
    }
    if parsed_left.patch != parsed_right.patch {
        return cmp_u64(parsed_left.patch, parsed_right.patch);
    }
    if parsed_left.prerelease.is_empty() && parsed_right.prerelease.is_empty() {
        return 0;
    }
    if parsed_left.prerelease.is_empty() {
        return 1;
    }
    if parsed_right.prerelease.is_empty() {
        return -1;
    }
    let length = parsed_left
        .prerelease
        .len()
        .max(parsed_right.prerelease.len());
    for index in 0..length {
        let Some(left_id) = parsed_left.prerelease.get(index) else {
            return -1;
        };
        let Some(right_id) = parsed_right.prerelease.get(index) else {
            return 1;
        };
        let comparison = compare_prerelease_identifier(left_id, right_id);
        if comparison != 0 {
            return comparison;
        }
    }
    0
}

fn cmp_u64(left: u64, right: u64) -> i32 {
    match left.cmp(&right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn str_cmp(left: &str, right: &str) -> i32 {
    match left.cmp(right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// First `major.minor.patch` in CLI output. The optional `v` is consumed so
/// `opencode v2.0.3` yields `2.0.3` and `build2.0.3artifact` yields nothing.
pub fn parse_generic_cli_version(output: &str) -> Option<String> {
    let bytes = output.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(version) = match_version_at(output, index) {
            return Some(version);
        }
        index += 1;
    }
    None
}

fn match_version_at(output: &str, index: usize) -> Option<String> {
    let rest = output.get(index..)?;
    let boundary_ok = index == 0
        || output
            .get(index - 1..index)
            .is_some_and(|previous| !is_word(previous));
    if !boundary_ok {
        return None;
    }
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    let (major, rest) = take_digits(rest)?;
    let rest = rest.strip_prefix('.')?;
    let (minor, rest) = take_digits(rest)?;
    let rest = rest.strip_prefix('.')?;
    let (patch, rest) = take_digits(rest)?;
    if rest.starts_with(|ch: char| is_word_char(ch)) {
        return None;
    }
    Some(format!("{major}.{minor}.{patch}"))
}

fn take_digits(input: &str) -> Option<(&str, &str)> {
    let end = input
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(input.len());
    if end == 0 {
        return None;
    }
    Some(input.split_at(end))
}

fn is_word(text: &str) -> bool {
    text.chars().all(is_word_char)
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_versions_the_way_snapshot_probe_does() {
        assert_eq!(
            parse_generic_cli_version("1.14.19").as_deref(),
            Some("1.14.19")
        );
        assert_eq!(
            parse_generic_cli_version("opencode v2.0.3").as_deref(),
            Some("2.0.3")
        );
        assert_eq!(
            parse_generic_cli_version("v22.19.0").as_deref(),
            Some("22.19.0")
        );
        assert_eq!(
            parse_generic_cli_version("codex-cli 0.53.0 (build abc)").as_deref(),
            Some("0.53.0")
        );
        assert_eq!(parse_generic_cli_version("no version here"), None);
        assert_eq!(parse_generic_cli_version("build2.0.3artifact"), None);
    }

    #[test]
    fn a_second_hyphen_ends_the_prerelease_like_upstream_split() {
        assert_eq!(normalize_semver_version("1.2.3-beta-1"), "1.2.3-beta");
        assert_eq!(normalize_semver_version("1.2-"), "1.2.0");
        assert_eq!(
            compare_semver_versions("1.14.19-rc-1", "1.14.19-rc-2"),
            0,
            "upstream truncates both to rc"
        );
        assert_eq!(
            parse_semver("1.2.3-beta-1").map(|parsed| parsed.prerelease),
            parse_semver("1.2.3-beta").map(|parsed| parsed.prerelease)
        );
    }

    #[test]
    fn compares_minimum_opencode_version() {
        assert!(compare_semver_versions("1.19.0", "1.14.19") > 0);
        assert!(compare_semver_versions("1.14.18", "1.14.19") < 0);
        assert_eq!(compare_semver_versions("1.14.19", "1.14.19"), 0);
        assert!(compare_semver_versions("1.14.19", "1.14.19-rc.1") > 0);
    }
}
