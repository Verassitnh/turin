//! Version comparison from `@t3tools/shared/semver` (`compareSemverVersions`
//! and what it needs).
//!
//! Local shim; swap for `turin_shared::semver` when `turin-shared` is merged.
//! `localeCompare` fallbacks use plain byte order.

use std::cmp::Ordering;

use turin_contracts::js_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSemver {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Vec<String>,
}

fn is_number_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
}

/// JS `value.split("-", 2)`: the limit drops everything after the second piece.
fn split_dash_two(value: &str) -> (&str, Option<&str>) {
    let mut pieces = value.split('-');
    (pieces.next().unwrap_or(""), pieces.next())
}

/// Pads shorthand versions ("20", "20.1") up to three segments.
pub fn normalize_semver_version(version: &str) -> String {
    let (main, prerelease) = split_dash_two(js_trim(version));
    let mut segments: Vec<String> = main
        .split('.')
        .map(|segment| js_trim(segment).to_owned())
        .filter(|segment| !segment.is_empty())
        .collect();
    while !segments.is_empty() && segments.len() < 3 {
        segments.push("0".to_owned());
    }
    let joined = segments.join(".");
    match prerelease.filter(|prerelease| !prerelease.is_empty()) {
        Some(prerelease) => format!("{joined}-{prerelease}"),
        None => joined,
    }
}

pub(crate) fn is_valid_semver(value: &str) -> bool {
    parse_semver(value).is_some()
}

fn parse_semver(value: &str) -> Option<ParsedSemver> {
    let normalized = normalize_semver_version(value);
    let normalized = normalized.strip_prefix('v').unwrap_or(&normalized);
    let (main, prerelease) = split_dash_two(normalized);
    let segments: Vec<&str> = main.split('.').collect();
    let [major, minor, patch] = segments.as_slice() else {
        return None;
    };
    if ![major, minor, patch]
        .iter()
        .all(|segment| is_number_segment(segment))
    {
        return None;
    }
    Some(ParsedSemver {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        patch: patch.parse().ok()?,
        prerelease: prerelease
            .map(|prerelease| {
                prerelease
                    .split('.')
                    .map(|segment| js_trim(segment).to_owned())
                    .filter(|segment| !segment.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn compare_prerelease_identifier(left: &str, right: &str) -> Ordering {
    match (is_number_segment(left), is_number_segment(right)) {
        (true, true) => left
            .parse::<u64>()
            .unwrap_or(u64::MAX)
            .cmp(&right.parse::<u64>().unwrap_or(u64::MAX)),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left.cmp(right),
    }
}

/// Semver ordering with upstream's lenient parsing; unparsable input falls
/// back to string order.
pub fn compare_semver_versions(left: &str, right: &str) -> Ordering {
    let (Some(parsed_left), Some(parsed_right)) = (parse_semver(left), parse_semver(right)) else {
        return left.cmp(right);
    };
    let by_numbers = (parsed_left.major, parsed_left.minor, parsed_left.patch).cmp(&(
        parsed_right.major,
        parsed_right.minor,
        parsed_right.patch,
    ));
    if by_numbers != Ordering::Equal {
        return by_numbers;
    }
    match (
        parsed_left.prerelease.is_empty(),
        parsed_right.prerelease.is_empty(),
    ) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let length = parsed_left
        .prerelease
        .len()
        .max(parsed_right.prerelease.len());
    for index in 0..length {
        let (Some(left_id), Some(right_id)) = (
            parsed_left.prerelease.get(index),
            parsed_right.prerelease.get(index),
        ) else {
            return if parsed_left.prerelease.get(index).is_none() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        };
        let comparison = compare_prerelease_identifier(left_id, right_id);
        if comparison != Ordering::Equal {
            return comparison;
        }
    }
    Ordering::Equal
}

/// Small semver range checker for CLI and runtime gates.
///
/// Port of `satisfiesSemverRange` in `@t3tools/shared/semver`. The body stays
/// the same decision table: `||` groups of space-separated comparators.
pub fn satisfies_semver_range(raw_version: &str, range: &str) -> bool {
    let normalized = js_trim(raw_version).trim_start_matches('v');
    let Some(version) = parse_range_version(normalized) else {
        return false;
    };
    range.split("||").any(|group| {
        let comparators: Vec<&str> = group
            .split_whitespace()
            .filter(|part| !part.is_empty())
            .collect();
        if comparators.is_empty() {
            return false;
        }
        comparators.iter().all(|comparator| {
            let comparator = js_trim(comparator);
            let (operator, target_raw) = split_comparator(comparator);
            let Some(target_raw) = target_raw else {
                return false;
            };
            let Some(target) = parse_range_version(target_raw.trim_start_matches('v')) else {
                return false;
            };
            // Prerelease tails are ignored here, matching the upstream regex.
            let compared = cmp_triple(version, target);
            match operator {
                "^" => {
                    if compared < 0 {
                        return false;
                    }
                    if target.0 > 0 {
                        version.0 == target.0
                    } else if target.1 > 0 {
                        version.0 == 0 && version.1 == target.1
                    } else {
                        version.0 == 0 && version.1 == 0 && version.2 == target.2
                    }
                }
                ">=" => compared >= 0,
                ">" => compared > 0,
                "<=" => compared <= 0,
                "<" => compared < 0,
                "=" => compared == 0,
                _ => false,
            }
        })
    })
}

fn parse_range_version(value: &str) -> Option<(u64, u64, u64)> {
    let numeric = value.split_once('-').map(|(head, _)| head).unwrap_or(value);
    let mut parts = numeric.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    if numeric.is_empty()
        || !numeric
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    Some((major, minor, patch))
}

fn split_comparator(comparator: &str) -> (&str, Option<&str>) {
    let operators = ["^", ">=", ">", "<=", "<", "="];
    for operator in operators {
        if let Some(rest) = comparator.strip_prefix(operator) {
            let rest = js_trim(rest);
            return (operator, if rest.is_empty() { None } else { Some(rest) });
        }
    }
    (
        "=",
        if comparator.is_empty() {
            None
        } else {
            Some(comparator)
        },
    )
}

fn cmp_triple(version: (u64, u64, u64), target: (u64, u64, u64)) -> i32 {
    if version.0 != target.0 {
        return if version.0 > target.0 { 1 } else { -1 };
    }
    if version.1 != target.1 {
        return if version.1 > target.1 { 1 } else { -1 };
    }
    if version.2 != target.2 {
        return if version.2 > target.2 { 1 } else { -1 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_versions_like_upstream() {
        assert_eq!(compare_semver_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_semver_versions("1.2.3", "1.10.0"), Ordering::Less);
        assert_eq!(compare_semver_versions("v2", "1.9.9"), Ordering::Greater);
        assert_eq!(
            compare_semver_versions("1.0.0-beta.2", "1.0.0"),
            Ordering::Less
        );
        assert_eq!(
            compare_semver_versions("1.0.0-beta.2", "1.0.0-beta.11"),
            Ordering::Less
        );
        assert_eq!(
            compare_semver_versions("1.0.0-1", "1.0.0-alpha"),
            Ordering::Less
        );
        assert_eq!(compare_semver_versions("junk", "junk"), Ordering::Equal);
    }

    #[test]
    fn pads_shorthand() {
        assert_eq!(normalize_semver_version("20"), "20.0.0");
        assert_eq!(normalize_semver_version(" 20.1-rc.1 "), "20.1.0-rc.1");
        assert_eq!(normalize_semver_version(""), "");
    }
}
