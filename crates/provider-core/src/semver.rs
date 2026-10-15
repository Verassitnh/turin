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
