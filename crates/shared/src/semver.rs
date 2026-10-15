//! Port of upstream/packages/shared/src/semver.ts

use std::cmp::Ordering;

use crate::js_string::{is_js_whitespace_char, locale_compare};
use turin_contracts::base_schemas::js_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSemver {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub prerelease: Vec<String>,
}

fn is_number_segment(segment: &str) -> bool {
    // Upstream `/^\d+$/` with JS `\d` (ASCII digits only).
    !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit())
}

/// Pads shorthand versions ("20", "20.1") up to three segments, preserving a
/// `-prerelease` suffix. Empty/garbage input stays empty; inputs with more
/// than three segments are left untouched.
pub fn normalize_semver_version(version: &str) -> String {
    let trimmed = js_trim(version);
    let (main, prerelease) = split_two_on_dash(trimmed);
    let mut segments: Vec<String> = Vec::new();
    for segment in main.split('.') {
        let trimmed_segment = js_trim(segment);
        if !trimmed_segment.is_empty() {
            segments.push(trimmed_segment.to_owned());
        }
    }

    // Pad shorthand versions ("20" or "20.1") up to three segments so major-only
    // and minor-only inputs parse and compare numerically. This matches
    // satisfies_semver_range, which already treats a missing minor/patch as 0. The
    // length > 0 guard keeps empty/garbage input empty (parse_semver still
    // rejects it), and inputs with more than three segments are left untouched.
    while !segments.is_empty() && segments.len() < 3 {
        segments.push("0".to_owned());
    }

    let joined = segments.join(".");
    match prerelease.filter(|prerelease| !prerelease.is_empty()) {
        Some(prerelease) => format!("{joined}-{prerelease}"),
        None => joined,
    }
}

/// JS `value.split("-", 2)`: the limit drops everything after the second
/// piece, it does not keep the remainder.
fn split_two_on_dash(value: &str) -> (&str, Option<&str>) {
    let mut pieces = value.split('-');
    let main = pieces.next().unwrap_or("");
    (main, pieces.next())
}

pub fn parse_semver(value: &str) -> Option<ParsedSemver> {
    let normalized = normalize_semver_version(value);
    let normalized = normalized
        .strip_prefix('v')
        .unwrap_or(&normalized)
        .to_owned();
    let (main, prerelease) = split_two_on_dash(&normalized);
    let segments: Vec<&str> = main.split('.').collect();
    if segments.len() != 3 {
        return None;
    }
    let [major_segment, minor_segment, patch_segment] = [segments[0], segments[1], segments[2]];
    if !is_number_segment(major_segment)
        || !is_number_segment(minor_segment)
        || !is_number_segment(patch_segment)
    {
        return None;
    }

    let major = parse_int(major_segment)?;
    let minor = parse_int(minor_segment)?;
    let patch = parse_int(patch_segment)?;

    Some(ParsedSemver {
        major,
        minor,
        patch,
        prerelease: parse_prerelease_segments(prerelease),
    })
}

/// JS `Number.parseInt(segment, 10)` for digit-only segments. Deviation:
/// digit runs that overflow an `i64` (far past JS's 2^53 safe range) fail to
/// parse instead of clamping.
fn parse_int(segment: &str) -> Option<i64> {
    segment.parse::<i64>().ok()
}

fn parse_prerelease_segments(prerelease: Option<&str>) -> Vec<String> {
    let prerelease = match prerelease {
        Some(value) => value,
        None => return Vec::new(),
    };
    prerelease
        .split('.')
        .map(js_trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

fn compare_prerelease_identifier(left: &str, right: &str) -> Ordering {
    let left_numeric = is_number_segment(left);
    let right_numeric = is_number_segment(right);

    if left_numeric && right_numeric {
        return parse_int(left)
            .unwrap_or(i64::MAX)
            .cmp(&parse_int(right).unwrap_or(i64::MAX));
    }
    if left_numeric {
        return Ordering::Less;
    }
    if right_numeric {
        return Ordering::Greater;
    }
    locale_compare(left, right)
}

pub fn compare_semver_versions(left: &str, right: &str) -> Ordering {
    let parsed_left = parse_semver(left);
    let parsed_right = parse_semver(right);
    let (parsed_left, parsed_right) = match (parsed_left, parsed_right) {
        (Some(left), Some(right)) => (left, right),
        // Upstream falls back to localeCompare on either side failing to parse.
        _ => return locale_compare(left, right),
    };

    match parsed_left.major.cmp(&parsed_right.major) {
        Ordering::Equal => {}
        other => return other,
    }
    match parsed_left.minor.cmp(&parsed_right.minor) {
        Ordering::Equal => {}
        other => return other,
    }
    match parsed_left.patch.cmp(&parsed_right.patch) {
        Ordering::Equal => {}
        other => return other,
    }

    if parsed_left.prerelease.is_empty() && parsed_right.prerelease.is_empty() {
        return Ordering::Equal;
    }
    if parsed_left.prerelease.is_empty() {
        return Ordering::Greater;
    }
    if parsed_right.prerelease.is_empty() {
        return Ordering::Less;
    }

    let length = parsed_left
        .prerelease
        .len()
        .max(parsed_right.prerelease.len());
    for index in 0..length {
        let left_identifier = parsed_left.prerelease.get(index);
        let right_identifier = parsed_right.prerelease.get(index);
        let (Some(left_identifier), Some(right_identifier)) = (left_identifier, right_identifier)
        else {
            return match (left_identifier, right_identifier) {
                (None, _) => Ordering::Less,
                (_, None) => Ordering::Greater,
                _ => unreachable!(),
            };
        };
        match compare_prerelease_identifier(left_identifier, right_identifier) {
            Ordering::Equal => {}
            other => return other,
        }
    }

    Ordering::Equal
}

/// Small semver range checker for CLI/runtime gates.
///
/// Supports space-separated comparators with `^ >= > <= < =` operators and
/// `||` range groups, mirroring upstream's supported syntax exactly.
pub fn satisfies_semver_range(raw_version: &str, range: &str) -> bool {
    let trimmed_version = js_trim(raw_version);
    let normalized_version = trimmed_version.strip_prefix('v').unwrap_or(trimmed_version);
    let Some(version) = parse_installed_version(normalized_version) else {
        return false;
    };

    range.split("||").any(|group| {
        let comparators: Vec<&str> = group
            .trim()
            .split(is_js_whitespace_char)
            .filter(|token| !token.is_empty())
            .collect();
        if comparators.is_empty() {
            return false;
        }
        comparators.iter().all(|comparator| {
            let Some((operator, target)) = parse_comparator(comparator.trim()) else {
                return false;
            };
            let compared = compare_components(&version, &target);
            match operator {
                "^" => {
                    if compared == Ordering::Less {
                        return false;
                    }
                    if target.major > 0 {
                        return version.major == target.major;
                    }
                    if target.minor > 0 {
                        return version.major == 0 && version.minor == target.minor;
                    }
                    version.major == 0 && version.minor == 0 && version.patch == target.patch
                }
                ">=" => compared != Ordering::Less,
                ">" => compared == Ordering::Greater,
                "<=" => compared != Ordering::Greater,
                "<" => compared == Ordering::Less,
                "=" => compared == Ordering::Equal,
                _ => false,
            }
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeVersion {
    major: i64,
    minor: i64,
    patch: i64,
}

/// `/^(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:-[0-9A-Za-z.-]+)?$/`
fn parse_installed_version(normalized: &str) -> Option<RangeVersion> {
    let (main, prerelease) = match normalized.split_once('-') {
        Some((main, prerelease)) => (main, Some(prerelease)),
        None => (normalized, None),
    };
    if let Some(prerelease) = prerelease {
        // The regex requires at least one `[0-9A-Za-z.-]` character after `-`.
        if prerelease.is_empty()
            || !prerelease
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return None;
        }
    }
    parse_dotted_numbers(main)
}

/// `\d+(?:\.\d+){0,2}`, missing minor/patch read as 0.
fn parse_dotted_numbers(value: &str) -> Option<RangeVersion> {
    let segments: Vec<&str> = value.split('.').collect();
    if segments.len() > 3 || !segments.iter().all(|segment| is_number_segment(segment)) {
        return None;
    }
    let number = |index: usize| -> Option<i64> {
        segments
            .get(index)
            .map_or(Some(0), |segment| parse_int(segment))
    };
    Some(RangeVersion {
        major: number(0)?,
        minor: number(1)?,
        patch: number(2)?,
    })
}

/// `/^(\^|>=|>|<=|<|=)?\s*v?(\d+(?:\.\d+){0,2})$/`
fn parse_comparator(comparator: &str) -> Option<(&'static str, RangeVersion)> {
    let (operator, rest) = [
        ("^", "^"),
        (">=", ">="),
        (">", ">"),
        ("<=", "<="),
        ("<", "<"),
        ("=", "="),
    ]
    .iter()
    .find_map(|(prefix, operator)| {
        comparator
            .strip_prefix(prefix)
            .map(|rest| (*operator, rest))
    })
    .unwrap_or(("=", comparator));
    let rest = rest.trim_start_matches(is_js_whitespace_char);
    let rest = rest.strip_prefix('v').unwrap_or(rest);
    Some((operator, parse_dotted_numbers(rest)?))
}

fn compare_components(version: &RangeVersion, target: &RangeVersion) -> Ordering {
    version.major.cmp(&target.major).then_with(|| {
        version
            .minor
            .cmp(&target.minor)
            .then_with(|| version.patch.cmp(&target.patch))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_supported_range_groups() {
        let range = "^22.16 || ^23.11 || >=24.10";

        assert!(satisfies_semver_range("22.16.0", range));
        assert!(satisfies_semver_range("23.11.1", range));
        assert!(satisfies_semver_range("24.10.0", range));
        assert!(!satisfies_semver_range("22.15.9", range));
        assert!(!satisfies_semver_range("23.10.9", range));
        assert!(!satisfies_semver_range("24.9.9", range));
    }

    #[test]
    fn normalizes_versions_with_a_missing_patch_segment() {
        assert_eq!(normalize_semver_version("2.1"), "2.1.0");
    }

    #[test]
    fn normalizes_and_parses_shorthand_major_only_versions() {
        assert_eq!(normalize_semver_version("20"), "20.0.0");
        assert_eq!(normalize_semver_version("v18"), "v18.0.0");
        assert_eq!(normalize_semver_version("20-rc.1"), "20.0.0-rc.1");
        assert_eq!(
            parse_semver("20"),
            Some(ParsedSemver {
                major: 20,
                minor: 0,
                patch: 0,
                prerelease: vec![]
            })
        );
    }

    #[test]
    fn compares_shorthand_versions_numerically_instead_of_lexically() {
        // Regression: "20" vs "9" previously fell back to string comparison, which
        // ordered "20" before "9" ("2" < "9").
        assert_eq!(compare_semver_versions("20", "9"), Ordering::Greater);
        assert_eq!(compare_semver_versions("18", "18.0.0"), Ordering::Equal);
    }

    #[test]
    fn still_rejects_non_numeric_shorthand_and_keeps_empty_input_empty() {
        assert_eq!(parse_semver("abc"), None);
        assert_eq!(normalize_semver_version(""), "");
    }

    #[test]
    fn compares_prerelease_versions_before_stable_versions() {
        assert_eq!(
            compare_semver_versions("2.1.111-beta.1", "2.1.111"),
            Ordering::Less
        );
    }

    #[test]
    fn falls_back_to_lexical_comparison_for_malformed_numeric_segments() {
        assert_eq!(
            compare_semver_versions("1.2.3abc", "1.2.10"),
            Ordering::Greater
        );
    }

    #[test]
    fn supports_comparison_comparators() {
        assert!(satisfies_semver_range("24.9.0", ">=24.0 <24.10"));
        assert!(!satisfies_semver_range("24.10.0", ">=24.0 <24.10"));
    }

    #[test]
    fn honors_caret_range_upper_bounds_for_zero_major_versions() {
        assert!(satisfies_semver_range("0.2.3", "^0.2.3"));
        assert!(satisfies_semver_range("0.2.9", "^0.2.3"));
        assert!(!satisfies_semver_range("0.3.0", "^0.2.3"));
        assert!(!satisfies_semver_range("0.5.0", "^0.2.3"));
        assert!(satisfies_semver_range("0.0.3", "^0.0.3"));
        assert!(!satisfies_semver_range("0.0.4", "^0.0.3"));
    }

    #[test]
    fn rejects_invalid_versions_and_unsupported_range_syntax() {
        assert!(!satisfies_semver_range("not-a-version", ">=24.0"));
        assert!(!satisfies_semver_range("24.10.0", "~24.10"));
    }
}

#[cfg(test)]
mod divergence_tests {
    use super::*;

    #[test]
    fn split_limit_drops_the_remainder_like_js() {
        assert_eq!(normalize_semver_version("1.2.3-beta-1"), "1.2.3-beta");
        assert_eq!(normalize_semver_version("1.2.3-"), "1.2.3");
    }

    #[test]
    fn range_trims_before_stripping_v_and_rejects_prerelease_targets() {
        assert!(satisfies_semver_range(" v24.10.0 ", ">=24.10"));
        assert!(!satisfies_semver_range("24.10.0", ">=24.10.0-beta"));
        assert!(satisfies_semver_range("24.10.0-rc.1", ">=v24"));
    }
}
