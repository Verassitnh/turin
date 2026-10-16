//! Port of upstream/apps/server/src/provider/providerCompatibility.ts.
//!
//! A compatibility policy maps a provider version to a status for one T3 Code
//! release range. Policies come from the model manifest (remote, then the
//! bundled copy). The advisory rides beside the probe result; it never changes
//! `status`, `message`, or `models`.
//!
//! `t3_code_version` is passed in, not read from a package file: upstream reads
//! `apps/server/package.json`, and the Turin app wiring owns that version.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use turin_contracts::{TrimmedNonEmptyString, provider_instance::ProviderDriverKind};

use crate::{
    semver::satisfies_semver_range,
    server_provider::{
        ServerProvider, ServerProviderCompatibilityAdvisory, ServerProviderCompatibilityStatus,
    },
};

/// One status for a version range inside a policy. Ranges are matched in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCompatibilityRange {
    pub range: String,
    pub status: ServerProviderCompatibilityStatus,
}

/// Upstream `ProviderCompatibilityPolicy`. Decode with
/// [`decode_provider_compatibility_policy`], which enforces the schema filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCompatibilityPolicy {
    pub driver: ProviderDriverKind,
    pub t3_code_range: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_version: Option<String>,
    pub ranges: Vec<ProviderCompatibilityRange>,
}

/// Same filter as upstream's `StableVersion`: exactly `major.minor.patch`.
fn is_stable_version(value: &str) -> bool {
    stable_version_regex().is_match(value)
}

fn stable_version_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\.\d+\.\d+$").expect("valid regex"))
}

/// Same filter as upstream's `VersionRange`. Comparator groups are joined by
/// `||`; each token is an optional operator, an optional `v`, then one to
/// three numeric parts. A group with no token ("1.0.0||", "||") fails: upstream
/// splits the trimmed group on whitespace, and `"".split(/\s+/)` is `[""]`, which
/// is not a token.
fn is_version_range(value: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let token = RE.get_or_init(|| {
        Regex::new(r"^(?:\^|>=|>|<=|<|=)?v?\d+(?:\.\d+){0,2}$").expect("valid regex")
    });
    !value.trim().is_empty()
        && value.split("||").all(|group| {
            let group = group.trim();
            !group.is_empty() && group.split_whitespace().all(|part| token.is_match(part))
        })
}

/// Decodes one policy from manifest JSON and applies the upstream schema
/// filters. Invalid input returns the failed field's name.
pub fn decode_provider_compatibility_policy(
    value: &Value,
) -> Result<ProviderCompatibilityPolicy, String> {
    let policy: ProviderCompatibilityPolicy =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if !is_version_range(&policy.t3_code_range) {
        return Err("t3CodeRange".to_owned());
    }
    if let Some(range) = &policy.recommended_range
        && !is_version_range(range)
    {
        return Err("recommendedRange".to_owned());
    }
    if let Some(version) = &policy.recommended_version
        && !is_stable_version(version)
    {
        return Err("recommendedVersion".to_owned());
    }
    if policy
        .ranges
        .iter()
        .any(|entry| !is_version_range(&entry.range))
    {
        return Err("ranges".to_owned());
    }
    // The recommended version must sit in the first range it matches, and that
    // range must be supported.
    if let Some(version) = &policy.recommended_version {
        let recommended_ok = policy
            .recommended_range
            .as_ref()
            .is_none_or(|range| satisfies_semver_range(version, range));
        let first_status = policy
            .ranges
            .iter()
            .find(|entry| satisfies_semver_range(version, &entry.range))
            .map(|entry| entry.status);
        if !recommended_ok || first_status != Some(ServerProviderCompatibilityStatus::Supported) {
            return Err("a recommended version in a supported range".to_owned());
        }
    }
    Ok(policy)
}

/// Decodes a manifest's `compatibility` array. Entries that fail the schema are
/// skipped, the way the manifest reader drops an invalid catalog entry.
pub fn decode_provider_compatibility_policies(
    values: &[Value],
) -> Vec<ProviderCompatibilityPolicy> {
    values
        .iter()
        .filter_map(|value| decode_provider_compatibility_policy(value).ok())
        .collect()
}

/// Upstream `resolveProviderCompatibility`: the advisory for one version of a
/// driver, or `None` when no policy covers this T3 Code release.
pub fn resolve_provider_compatibility(
    policies: Option<&[ProviderCompatibilityPolicy]>,
    driver: &ProviderDriverKind,
    version: Option<&str>,
    t3_code_version: &str,
) -> Option<ServerProviderCompatibilityAdvisory> {
    let policy = policies?.iter().find(|entry| {
        &entry.driver == driver && satisfies_semver_range(t3_code_version, &entry.t3_code_range)
    })?;
    let unprefixed = version.map(|value| value.strip_prefix('v').unwrap_or(value));
    // Cursor appends a build hash to its date; Google's ACP runtime uses a release
    // prefix; Muse appends a release revision. Strip only these driver-specific
    // forms, keeping semver prereleases unknown.
    let stable: Option<String> = match (driver.as_str(), unprefixed) {
        ("cursor", Some(value)) => Some(strip_cursor_build_hash(value)),
        ("antigravity", Some(value)) => Some(strip_antigravity_prefix(value)),
        ("muse", Some(value)) => Some(strip_muse_revision(value)),
        (_, value) => value.map(str::to_owned),
    };
    let status = match stable {
        Some(stable) if is_stable_version(&stable) => policy
            .ranges
            .iter()
            .find(|entry| satisfies_semver_range(&stable, &entry.range))
            .map_or(ServerProviderCompatibilityStatus::Unknown, |entry| {
                entry.status
            }),
        _ => ServerProviderCompatibilityStatus::Unknown,
    };
    let message = match status {
        ServerProviderCompatibilityStatus::Broken => {
            Some("This provider version is known to be incompatible with this T3 Code release.")
        }
        ServerProviderCompatibilityStatus::Unsupported => {
            Some("This provider version is outside the supported range for this T3 Code release.")
        }
        ServerProviderCompatibilityStatus::Graceful => {
            Some("This provider version has limited compatibility with this T3 Code release.")
        }
        _ => None,
    };
    let recommended_version = policy.recommended_version.clone();
    let recommended_range = policy.recommended_range.clone();
    let recommendation = recommended_version
        .as_deref()
        .or(recommended_range.as_deref());
    let message = match (message, recommendation) {
        (Some(message), Some(recommendation)) => Some(format!("{message} Use {recommendation}.")),
        (message, _) => message.map(str::to_owned),
    };
    Some(ServerProviderCompatibilityAdvisory {
        status,
        latest_version_status: None,
        message: message.map(|text| TrimmedNonEmptyString::new(text).expect("non-blank message")),
        recommended_version: recommended_version
            .map(|value| TrimmedNonEmptyString::new(value).expect("non-blank version")),
        recommended_range: recommended_range
            .map(|value| TrimmedNonEmptyString::new(value).expect("non-blank range")),
    })
}

fn strip_cursor_build_hash(value: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(\d{4}\.\d{2}\.\d{2})-[a-f0-9]+$").expect("valid"));
    re.replace(value, "$1").into_owned()
}

fn strip_antigravity_prefix(value: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^agy_acp_server_(\d+\.\d+\.\d+)$").expect("valid regex"));
    re.replace(value, "$1").into_owned()
}

fn strip_muse_revision(value: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^(\d+\.\d+\.\d+)-R\d+(?:\.\d+)?$").expect("valid regex"));
    re.replace(value, "$1").into_owned()
}

/// Upstream `applyProviderCompatibility`: a remote policy replaces its matching
/// bundled policy, and omission keeps the bundle.
pub fn apply_provider_compatibility(
    mut snapshot: ServerProvider,
    policies: Option<&[ProviderCompatibilityPolicy]>,
    fallback: Option<&[ProviderCompatibilityPolicy]>,
    t3_code_version: &str,
) -> ServerProvider {
    snapshot.compatibility_advisory = None;
    if !snapshot.enabled || !snapshot.installed {
        return snapshot;
    }
    let advisory = resolve_provider_compatibility(
        policies,
        &snapshot.driver,
        snapshot.version.as_ref().map(|value| value.as_str()),
        t3_code_version,
    )
    .or_else(|| {
        resolve_provider_compatibility(
            fallback,
            &snapshot.driver,
            snapshot.version.as_ref().map(|value| value.as_str()),
            t3_code_version,
        )
    });
    let latest_version = snapshot
        .version_advisory
        .as_ref()
        .and_then(|advisory| advisory.latest_version.as_ref())
        .map(|value| value.as_str().to_owned());
    let latest_advisory = latest_version.as_deref().and_then(|latest| {
        resolve_provider_compatibility(policies, &snapshot.driver, Some(latest), t3_code_version)
            .or_else(|| {
                resolve_provider_compatibility(
                    fallback,
                    &snapshot.driver,
                    Some(latest),
                    t3_code_version,
                )
            })
    });
    if let Some(mut advisory) = advisory {
        advisory.latest_version_status = latest_advisory.map(|latest| latest.status);
        snapshot.compatibility_advisory = Some(advisory);
    }
    snapshot
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/providerCompatibility.test.ts,
    //! minus the registry-stream case, which lands with `provider_registry`.

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{model_manifest::bundled_model_manifest, server_provider::ServerProviderState};

    const V2_RELEASE: &str = "0.0.46";

    fn kind(value: &str) -> ProviderDriverKind {
        ProviderDriverKind::new(value).expect("valid driver kind")
    }

    fn range(text: &str, status: ServerProviderCompatibilityStatus) -> ProviderCompatibilityRange {
        ProviderCompatibilityRange {
            range: text.to_owned(),
            status,
        }
    }

    fn policy() -> ProviderCompatibilityPolicy {
        use ServerProviderCompatibilityStatus::*;
        ProviderCompatibilityPolicy {
            driver: kind("codex"),
            t3_code_range: ">=0.0.42 <0.1.0".to_owned(),
            recommended_version: Some("2.0.0".to_owned()),
            recommended_range: Some(">=2.0.0 <3.0.0".to_owned()),
            ranges: vec![
                range("<1.0.0", Broken),
                range(">=1.0.0 <1.5.0", Unsupported),
                range(">=1.5.0 <2.0.0", Graceful),
                range(">=2.0.0 <3.0.0", Supported),
            ],
        }
    }

    fn bundled() -> Vec<ProviderCompatibilityPolicy> {
        decode_provider_compatibility_policies(
            &bundled_model_manifest().compatibility.unwrap_or_default(),
        )
    }

    fn status_of(
        policies: &[ProviderCompatibilityPolicy],
        driver: &ProviderDriverKind,
        version: Option<&str>,
        t3: &str,
    ) -> Option<ServerProviderCompatibilityStatus> {
        resolve_provider_compatibility(Some(policies), driver, version, t3).map(|a| a.status)
    }

    /// A flat snapshot as the probe leaves it: enabled, installed, and erroring.
    fn provider(status: &str, message: Option<&str>) -> ServerProvider {
        serde_json::from_value(json!({
            "instanceId": "codex-work",
            "driver": "codex",
            "enabled": true,
            "installed": true,
            "version": "0.9.0",
            "status": status,
            "message": message,
            "checkedAt": "2026-09-22T00:00:00Z",
            "auth": { "status": "unauthenticated" },
            "models": [],
            "skills": [],
            "slashCommands": [],
        }))
        .expect("provider fixture")
    }

    #[test]
    fn bundles_a_compatibility_policy_for_every_built_in_harness() {
        let policies = bundled();
        for driver in ["codex", "claudeAgent", "cursor", "opencode"] {
            assert!(
                status_of(&policies, &kind(driver), None, V2_RELEASE).is_some(),
                "Missing bundled compatibility policy for {driver}"
            );
        }
    }

    #[test]
    fn keeps_releases_before_v2_on_the_opencode_1_policy_they_shipped_with() {
        let policies = bundled();
        let opencode = kind("opencode");
        for (version, expected) in [
            ("1.14.19", ServerProviderCompatibilityStatus::Supported),
            ("2.0.18", ServerProviderCompatibilityStatus::Broken),
            ("1.14.18", ServerProviderCompatibilityStatus::Broken),
        ] {
            let advisory =
                resolve_provider_compatibility(Some(&policies), &opencode, Some(version), "0.0.45")
                    .expect("advisory");
            assert_eq!(advisory.status, expected, "OpenCode {version} on 0.0.45");
            assert_eq!(
                advisory.recommended_version.as_ref().map(|v| v.as_str()),
                Some("1.14.19")
            );
        }
        // Pi has no driver before V2.
        assert_eq!(
            status_of(&policies, &kind("pi"), Some("1.0.0"), "0.0.45"),
            None
        );
    }

    #[test]
    fn supports_opencode_2_and_gives_opencode_1x_limited_support() {
        use ServerProviderCompatibilityStatus::*;
        let policies = bundled();
        let opencode = kind("opencode");
        for (version, expected) in [
            ("2.0.18", Supported),
            ("2.1.0", Supported),
            // Early OpenCode 2 releases predate the API the adapter was built against.
            ("2.0.17", Unsupported),
            ("2.0.0", Unsupported),
            ("1.99.0", Graceful),
            ("1.14.19", Graceful),
            ("1.14.18", Broken),
        ] {
            for t3 in [V2_RELEASE, "0.0.46-preview.20261002.2598"] {
                let advisory =
                    resolve_provider_compatibility(Some(&policies), &opencode, Some(version), t3)
                        .expect("advisory");
                assert_eq!(advisory.status, expected, "OpenCode {version} on {t3}");
                assert_eq!(
                    advisory.recommended_range.as_ref().map(|v| v.as_str()),
                    Some(">=2.0.18")
                );
            }
        }
        // The advisory rides beside the probe: a ready 1.x instance stays ready.
        let mut ready = provider("ready", None);
        ready.driver = opencode.clone();
        ready.version = Some(TrimmedNonEmptyString::new("1.18.33").expect("version"));
        let ready = apply_provider_compatibility(ready, None, Some(&policies), V2_RELEASE);
        assert_eq!(ready.status, ServerProviderState::Ready);
        assert_eq!(
            ready.compatibility_advisory.as_ref().map(|a| a.status),
            status_of(&policies, &opencode, Some("1.18.33"), V2_RELEASE)
        );
    }

    #[test]
    fn compares_cursor_build_dates_without_treating_semver_prereleases_as_stable() {
        use ServerProviderCompatibilityStatus::*;
        let cursor = kind("cursor");
        let cursor_policy = ProviderCompatibilityPolicy {
            driver: cursor.clone(),
            t3_code_range: policy().t3_code_range,
            recommended_range: None,
            recommended_version: None,
            ranges: vec![
                range("<2026.05.09", Unsupported),
                range(">=2026.05.09", Supported),
            ],
        };
        for (version, expected) in [
            ("2026.05.08-a1b2c3d", Unsupported),
            ("2026.05.09-a1b2c3d", Supported),
            ("2026.09.22-f2b0fcd", Supported),
            ("2026.05.09", Supported),
            ("2026.05.09-beta.1", Unknown),
        ] {
            assert_eq!(
                status_of(
                    std::slice::from_ref(&cursor_policy),
                    &cursor,
                    Some(version),
                    "0.0.46"
                ),
                Some(expected),
                "{version}"
            );
        }
        assert_eq!(
            status_of(&[policy()], &kind("codex"), Some("2.0.0-a1b2c3d"), "0.0.46"),
            Some(Unknown)
        );
    }

    #[test]
    fn compares_muse_versions_by_their_release_ignoring_the_revision_suffix() {
        use ServerProviderCompatibilityStatus::*;
        let policies = bundled();
        let muse = kind("muse");
        for (version, expected) in [
            ("1.4.3-R5018.1", Supported),
            ("1.4.2-R4684", Supported),
            ("1.4.1-R4100.2", Unknown),
            ("1.4.3-beta.1", Unknown),
        ] {
            assert_eq!(
                status_of(&policies, &muse, Some(version), V2_RELEASE),
                Some(expected),
                "{version}"
            );
        }
    }

    #[test]
    fn recognizes_antigravity_semver_release_tags_while_keeping_dated_candidates_unknown() {
        use ServerProviderCompatibilityStatus::*;
        let antigravity = kind("antigravity");
        let tagged = ProviderCompatibilityPolicy {
            driver: antigravity.clone(),
            ranges: vec![range("=2.0.0", Supported)],
            ..policy()
        };
        for (version, expected) in [
            ("agy_acp_server_2.0.0", Supported),
            ("2.0.0", Supported),
            ("agy_acp_server_2.0.1", Unknown),
            ("agy_acp_server_2.0.0-beta.1", Unknown),
            ("agy_acp_server_20260818_01_RC01", Unknown),
        ] {
            assert_eq!(
                status_of(
                    std::slice::from_ref(&tagged),
                    &antigravity,
                    Some(version),
                    "0.0.46"
                ),
                Some(expected),
                "{version}"
            );
        }
    }

    #[test]
    fn classifies_boundaries_and_treats_unlisted_versions_and_release_tags_as_unknown() {
        use ServerProviderCompatibilityStatus::*;
        let codex = kind("codex");
        let policies = [policy()];
        for (version, expected) in [
            (Some("0.9.9"), Broken),
            (Some("1.0.0"), Unsupported),
            (Some("1.5.0"), Graceful),
            (Some("2.0.0"), Supported),
            (Some("v2.0.0"), Supported),
            (Some("3.0.0"), Unknown),
            (Some("2.0.0-beta.1"), Unknown),
            (Some("agy_acp_server_20260818_01_RC01"), Unknown),
            (None, Unknown),
        ] {
            assert_eq!(
                status_of(&policies, &codex, version, "0.0.46"),
                Some(expected),
                "{version:?}"
            );
        }
        assert_eq!(status_of(&policies, &codex, Some("0.9.0"), "0.1.0"), None);
    }

    #[test]
    fn supports_every_driver_without_inventing_policies_for_uncovered_adapters() {
        for name in [
            "codex",
            "claudeAgent",
            "cursor",
            "grok",
            "opencode",
            "antigravity",
            "customDriver",
        ] {
            let adapter = kind(name);
            let mut own = policy();
            own.driver = adapter.clone();
            assert_eq!(
                status_of(&[own], &adapter, Some("2.0.0"), "0.0.46"),
                Some(ServerProviderCompatibilityStatus::Supported)
            );
            assert_eq!(status_of(&[], &adapter, Some("2.0.0"), "0.0.46"), None);
        }
    }

    #[test]
    fn relaxes_remote_policy_without_losing_probe_errors_and_falls_back_when_omitted() {
        use ServerProviderCompatibilityStatus::*;
        let fallback = [policy()];
        let snapshot = provider("error", Some("Authentication failed"));
        let broken = apply_provider_compatibility(snapshot, Some(&[]), Some(&fallback), "0.0.46");
        assert_eq!(
            broken.compatibility_advisory.as_ref().map(|a| a.status),
            Some(Broken)
        );

        let mut relaxed = policy();
        relaxed.ranges = vec![range(">=0.0.0", Supported)];
        let supported = apply_provider_compatibility(
            broken.clone(),
            Some(std::slice::from_ref(&relaxed)),
            Some(&fallback),
            "0.0.46",
        );
        assert_eq!(
            supported.compatibility_advisory.as_ref().map(|a| a.status),
            Some(Supported)
        );
        assert_eq!(supported.status, ServerProviderState::Error);
        assert_eq!(
            supported.message.as_ref().map(|m| m.as_str()),
            Some("Authentication failed")
        );

        let mut moved = policy();
        moved.t3_code_range = ">=9.0.0".to_owned();
        assert_eq!(
            apply_provider_compatibility(
                supported.clone(),
                Some(&[moved]),
                Some(&fallback),
                "0.0.46"
            )
            .compatibility_advisory
            .as_ref()
            .map(|a| a.status),
            Some(Broken)
        );
        let removed = apply_provider_compatibility(supported, Some(&[]), Some(&[]), "0.0.46");
        assert!(removed.compatibility_advisory.is_none());
        assert_eq!(removed.status, ServerProviderState::Error);
        assert_eq!(
            removed.message.as_ref().map(|m| m.as_str()),
            Some("Authentication failed")
        );

        let mut disabled = broken.clone();
        disabled.enabled = false;
        assert!(
            apply_provider_compatibility(disabled, Some(&[]), Some(&fallback), "0.0.46")
                .compatibility_advisory
                .is_none()
        );
        let mut uninstalled = broken;
        uninstalled.installed = false;
        assert!(
            apply_provider_compatibility(uninstalled, Some(&[]), Some(&fallback), "0.0.46")
                .compatibility_advisory
                .is_none()
        );
    }

    #[test]
    fn rejects_invalid_ranges_and_recommendations_outside_the_first_supported_match() {
        use ServerProviderCompatibilityStatus::*;
        let valid = policy_json();
        assert!(decode_provider_compatibility_policy(&valid).is_ok());
        let prefixed = decode_provider_compatibility_policy(&json!({
            "driver": "codex",
            "t3CodeRange": ">=v0.0.42 <v0.1",
            "recommendedRange": "^v2",
            "recommendedVersion": "2.0.0",
            "ranges": [{ "range": ">=v2.0 <v3", "status": "supported" }],
        }))
        .expect("prefixed policy decodes");
        assert_eq!(
            status_of(
                std::slice::from_ref(&prefixed),
                &kind("codex"),
                Some("2.0.0"),
                "0.0.46"
            ),
            Some(Supported)
        );
        assert_eq!(
            status_of(
                std::slice::from_ref(&prefixed),
                &kind("codex"),
                Some("3.0.0"),
                "0.0.46"
            ),
            Some(Unknown)
        );
        let mut invalid = policy_json();
        let invalid_cases = [
            ("t3CodeRange", json!("*")),
            ("recommendedVersion", json!("3.0.0")),
            ("recommendedVersion", json!("2.0.0; echo unsafe")),
        ];
        for (field, value) in invalid_cases {
            invalid[field] = value;
            assert!(
                decode_provider_compatibility_policy(&invalid).is_err(),
                "{field}"
            );
            invalid = policy_json();
        }
        // An empty comparator group fails the filter, as upstream's `[""]` token does.
        for range in ["1.0.0||", "||", ">=1 || ", "||>=1", ">=1 ||  || <3"] {
            for field in ["t3CodeRange", "recommendedRange"] {
                let mut empty_group = policy_json();
                empty_group[field] = json!(range);
                assert!(
                    decode_provider_compatibility_policy(&empty_group).is_err(),
                    "{field}: {range:?}"
                );
            }
            let mut empty_group = policy_json();
            empty_group["ranges"] = json!([{ "range": range, "status": "supported" }]);
            assert!(
                decode_provider_compatibility_policy(&empty_group).is_err(),
                "ranges: {range:?}"
            );
        }
        assert!(
            decode_provider_compatibility_policy(&{
                let mut multi_group = policy_json();
                multi_group["t3CodeRange"] = json!(">=0.0.42 <0.1.0 || >=1.0.0");
                multi_group
            })
            .is_ok()
        );
        let mut garbage = policy_json();
        garbage["ranges"] = json!([{ "range": ">=2.0.0 garbage", "status": "supported" }]);
        assert!(decode_provider_compatibility_policy(&garbage).is_err());
        // A catch-all broken range listed first shadows the recommended version.
        let mut shadowed = policy_json();
        let mut ranges = vec![json!({ "range": ">=0.0.0", "status": "broken" })];
        ranges.extend(
            policy_json()["ranges"]
                .as_array()
                .expect("ranges")
                .iter()
                .cloned(),
        );
        shadowed["ranges"] = Value::Array(ranges);
        assert!(decode_provider_compatibility_policy(&shadowed).is_err());
    }

    #[test]
    fn recomputes_latest_version_compatibility_without_losing_the_underlying_update_advisory() {
        use ServerProviderCompatibilityStatus::*;
        let mut snapshot = provider("ready", None);
        snapshot.version = Some(TrimmedNonEmptyString::new("2.0.0").expect("version"));
        snapshot.version_advisory = serde_json::from_value(json!({
            "status": "behind_latest",
            "currentVersion": "2.0.0",
            "latestVersion": "4.0.0",
            "canUpdate": true,
            "updateCommand": "npm install -g fixture@latest",
            "checkedAt": "2026-09-22T00:00:00Z",
            "message": null,
        }))
        .expect("version advisory fixture");
        let mut blocked = policy();
        blocked.ranges.push(range(">=3.0.0", Broken));
        let blocked = apply_provider_compatibility(
            snapshot.clone(),
            Some(std::slice::from_ref(&blocked)),
            Some(&[]),
            "0.0.46",
        );
        assert_eq!(
            blocked
                .compatibility_advisory
                .as_ref()
                .and_then(|a| a.latest_version_status),
            Some(Broken)
        );
        let mut relaxed = policy();
        relaxed.ranges = vec![range(">=2.0.0", Supported)];
        let relaxed =
            apply_provider_compatibility(blocked.clone(), Some(&[relaxed]), Some(&[]), "0.0.46");
        assert_eq!(
            relaxed
                .compatibility_advisory
                .as_ref()
                .and_then(|a| a.latest_version_status),
            Some(Supported)
        );
        assert_eq!(relaxed.version_advisory, snapshot.version_advisory);
    }

    fn policy_json() -> Value {
        json!({
            "driver": "codex",
            "t3CodeRange": ">=0.0.42 <0.1.0",
            "recommendedVersion": "2.0.0",
            "recommendedRange": ">=2.0.0 <3.0.0",
            "ranges": [
                { "range": "<1.0.0", "status": "broken" },
                { "range": ">=1.0.0 <1.5.0", "status": "unsupported" },
                { "range": ">=1.5.0 <2.0.0", "status": "graceful" },
                { "range": ">=2.0.0 <3.0.0", "status": "supported" },
            ],
        })
    }
}
