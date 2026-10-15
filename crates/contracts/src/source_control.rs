//! Port of upstream/packages/contracts/src/sourceControl.ts

use crate::base_schemas::{
    ContractError, EffectOption, PositiveInt, TrimmedNonEmptyString, UtcDateTime, js_trim,
};
use crate::vcs::VcsDriverKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

const SOURCE_CONTROL_KIND_MAX_CHARS: usize = 64;

/// `SourceControlProviderKind` — open branded slug naming a source control
/// host, such as `github`.
///
/// Open on purpose, like `ProviderDriverKind`: a server can report a host
/// this client does not ship a definition for, and clients fall back to a
/// generic presentation instead of failing to decode. `unknown` is what the
/// server reports for a remote it cannot attribute to any host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceControlProviderKind(String);

impl SourceControlProviderKind {
    /// Validates like upstream's branded slug: trimmed (JS `trim`), non-empty,
    /// `^[a-z][a-z0-9-]*$`, at most 64 chars (ASCII, so bytes == UTF-16 units).
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        let valid = !trimmed.is_empty()
            && trimmed.len() <= SOURCE_CONTROL_KIND_MAX_CHARS
            && trimmed
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase())
            && trimmed
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(ContractError {
                expected: "a source control provider kind matching ^[a-z][a-z0-9-]*$ (<= 64 chars)",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for SourceControlProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SourceControlProviderKind {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for SourceControlProviderKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceControlProviderKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::new(raw.as_ref()).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderInfo {
    pub kind: SourceControlProviderKind,
    pub name: TrimmedNonEmptyString,
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeRequestState {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "merged")]
    Merged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequest {
    pub provider: SourceControlProviderKind,
    pub number: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub url: String,
    pub base_ref_name: TrimmedNonEmptyString,
    pub head_ref_name: TrimmedNonEmptyString,
    /// The head commit, when the provider's read includes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<TrimmedNonEmptyString>,
    pub state: ChangeRequestState,
    /// Present when the provider can tell that an open change request is still a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    /// `Schema.optional(Schema.NullOr(Schema.String))`: absent, null, or a string.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub closed_at: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub merged_at: Option<Option<String>>,
    pub updated_at: EffectOption<UtcDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_cross_repository: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub head_repository_name_with_owner: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub head_repository_owner_login: Option<Option<TrimmedNonEmptyString>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlRepositoryCloneUrls {
    pub name_with_owner: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub ssh_url: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceControlRepositoryVisibility {
    Private,
    Public,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceControlCloneProtocol {
    Auto,
    Ssh,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlRepositoryInfo {
    pub provider: SourceControlProviderKind,
    pub name_with_owner: TrimmedNonEmptyString,
    pub url: TrimmedNonEmptyString,
    pub ssh_url: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlRepositoryLookupInput {
    pub provider: SourceControlProviderKind,
    pub repository: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlCloneRepositoryInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<SourceControlProviderKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<TrimmedNonEmptyString>,
    pub destination_path: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<SourceControlCloneProtocol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlCloneRepositoryResult {
    pub cwd: TrimmedNonEmptyString,
    pub remote_url: TrimmedNonEmptyString,
    pub repository: Option<SourceControlRepositoryInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlPublishRepositoryInput {
    pub cwd: TrimmedNonEmptyString,
    pub provider: SourceControlProviderKind,
    pub repository: TrimmedNonEmptyString,
    pub visibility: SourceControlRepositoryVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<SourceControlCloneProtocol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceControlPublishStatus {
    Pushed,
    RemoteAdded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlPublishRepositoryResult {
    pub repository: SourceControlRepositoryInfo,
    pub remote_name: TrimmedNonEmptyString,
    pub remote_url: TrimmedNonEmptyString,
    pub branch: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_branch: Option<TrimmedNonEmptyString>,
    pub status: SourceControlPublishStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceControlDiscoveryStatus {
    Available,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceControlProviderAuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderAccount {
    pub host: TrimmedNonEmptyString,
    pub account: TrimmedNonEmptyString,
    pub active: bool,
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrimmedNonEmptyString>,
    /// Set when the login comes from a token variable such as `GH_TOKEN`,
    /// which wins over Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variable: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderAuth {
    pub status: SourceControlProviderAuthStatus,
    pub account: EffectOption<TrimmedNonEmptyString>,
    pub host: EffectOption<TrimmedNonEmptyString>,
    pub detail: EffectOption<TrimmedNonEmptyString>,
    /// Every login the provider CLI holds, across hosts. Only GitHub reports these today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<SourceControlProviderAccount>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsDiscoveryItem {
    pub kind: VcsDriverKind,
    pub implemented: bool,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<TrimmedNonEmptyString>,
    pub status: SourceControlDiscoveryStatus,
    pub version: EffectOption<TrimmedNonEmptyString>,
    pub install_hint: TrimmedNonEmptyString,
    pub detail: EffectOption<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderDiscoveryItem {
    pub kind: SourceControlProviderKind,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<TrimmedNonEmptyString>,
    pub status: SourceControlDiscoveryStatus,
    pub version: EffectOption<TrimmedNonEmptyString>,
    pub install_hint: TrimmedNonEmptyString,
    pub detail: EffectOption<TrimmedNonEmptyString>,
    pub auth: SourceControlProviderAuth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlDiscoveryResult {
    pub version_control_systems: Vec<VcsDiscoveryItem>,
    pub source_control_providers: Vec<SourceControlProviderDiscoveryItem>,
}

/// `SourceControlProviderError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SourceControlProviderError {
    #[serde(rename = "SourceControlProviderError")]
    #[error("Source control provider {provider} failed in {operation}: {detail}")]
    SourceControlProviderError {
        provider: SourceControlProviderKind,
        operation: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `SourceControlRepositoryError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SourceControlRepositoryError {
    #[serde(rename = "SourceControlRepositoryError")]
    #[error("Source control repository operation {operation} failed for {provider}: {detail}")]
    SourceControlRepositoryError {
        provider: SourceControlProviderKind,
        operation: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn round_trip<T: Serialize + serde::de::DeserializeOwned>(value: Value) -> T {
        let parsed: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        parsed
    }

    #[test]
    fn provider_kind_is_a_trimmed_lowercase_slug() {
        let kind: SourceControlProviderKind =
            serde_json::from_value(json!("  github\u{FEFF}")).unwrap();
        assert_eq!(kind.as_str(), "github");
        assert_eq!(serde_json::to_value(&kind).unwrap(), json!("github"));
        assert!(SourceControlProviderKind::new("git-lab2").is_ok());
        assert!(SourceControlProviderKind::new("a".repeat(64)).is_ok());
        for bad in [
            "", "   ", "GitHub", "1abc", "-abc", "a_b", "a b", "é", "a\u{85}",
        ] {
            assert!(SourceControlProviderKind::new(bad).is_err(), "{bad:?}");
        }
        assert!(SourceControlProviderKind::new("a".repeat(65)).is_err());
        assert!(serde_json::from_value::<SourceControlProviderKind>(json!(5)).is_err());
    }

    fn provider_info() -> Value {
        json!({ "kind": "github", "name": "GitHub", "baseUrl": "https://github.com" })
    }

    #[test]
    fn change_request_optional_nullable_and_option_fields() {
        let minimal = json!({
            "provider": "github", "number": 12, "title": "t", "url": "u",
            "baseRefName": "main", "headRefName": "feat", "state": "open",
            "updatedAt": { "_tag": "None" },
        });
        let parsed: ChangeRequest = round_trip(minimal.clone());
        assert!(parsed.closed_at.is_none() && parsed.merged_at.is_none());
        assert_eq!(parsed.updated_at, EffectOption::None);

        let full = json!({
            "provider": "github", "number": 12, "title": "t", "url": "u",
            "baseRefName": "main", "headRefName": "feat", "headSha": "abc123", "state": "merged",
            "isDraft": false, "closedAt": null, "mergedAt": "2026-01-01T00:00:00Z",
            "updatedAt": { "_tag": "Some", "value": "2026-01-02T03:04:05.678Z" },
            "isCrossRepository": true,
            "headRepositoryNameWithOwner": null,
            "headRepositoryOwnerLogin": "octo",
        });
        let parsed: ChangeRequest = round_trip(full);
        assert_eq!(parsed.closed_at, Some(None));
        assert_eq!(
            parsed.merged_at,
            Some(Some("2026-01-01T00:00:00Z".to_owned()))
        );
        assert_eq!(parsed.head_repository_name_with_owner, Some(None));
        assert!(parsed.updated_at.as_ref_option().is_some());

        // `updatedAt` is required (absence is not `None`), `number` is positive,
        // and the state set is closed.
        for (key, bad) in [
            ("updatedAt", Value::Null),
            ("number", json!(0)),
            ("state", json!("draft")),
        ] {
            let mut value = minimal.clone();
            if bad.is_null() {
                value.as_object_mut().unwrap().remove(key);
            } else {
                value[key] = bad;
            }
            assert!(
                serde_json::from_value::<ChangeRequest>(value).is_err(),
                "{key}"
            );
        }
        let mut bad = minimal;
        bad["headRefName"] = json!("  ");
        assert!(serde_json::from_value::<ChangeRequest>(bad).is_err());
    }

    #[test]
    fn repository_inputs_and_results_round_trip() {
        let repo = json!({
            "provider": "github", "nameWithOwner": "o/r",
            "url": "https://github.com/o/r", "sshUrl": "git@github.com:o/r.git",
        });
        let _: SourceControlRepositoryInfo = round_trip(repo.clone());
        let _: SourceControlRepositoryCloneUrls = round_trip(json!({
            "nameWithOwner": "o/r", "url": "u", "sshUrl": "s",
        }));
        let _: SourceControlRepositoryLookupInput =
            round_trip(json!({ "provider": "github", "repository": "o/r", "cwd": "/r" }));
        let minimal: SourceControlCloneRepositoryInput =
            round_trip(json!({ "destinationPath": "/dst" }));
        assert!(minimal.provider.is_none() && minimal.protocol.is_none());
        for protocol in ["auto", "ssh", "https"] {
            let _: SourceControlCloneRepositoryInput = round_trip(json!({
                "provider": "github", "repository": "o/r", "remoteUrl": "u",
                "destinationPath": "/dst", "protocol": protocol,
            }));
        }
        assert!(
            serde_json::from_value::<SourceControlCloneRepositoryInput>(
                json!({ "destinationPath": "/d", "protocol": "git" })
            )
            .is_err()
        );
        let cloned: SourceControlCloneRepositoryResult =
            round_trip(json!({ "cwd": "/dst", "remoteUrl": "u", "repository": null }));
        assert!(cloned.repository.is_none());
        let _: SourceControlCloneRepositoryResult =
            round_trip(json!({ "cwd": "/dst", "remoteUrl": "u", "repository": repo }));

        let _: SourceControlPublishRepositoryInput = round_trip(json!({
            "cwd": "/r", "provider": "github", "repository": "o/r", "visibility": "private",
            "remoteName": "origin", "protocol": "ssh",
        }));
        assert!(
            serde_json::from_value::<SourceControlPublishRepositoryInput>(json!({
                "cwd": "/r", "provider": "github", "repository": "o/r", "visibility": "internal",
            }))
            .is_err()
        );
        for status in ["pushed", "remote_added"] {
            let _: SourceControlPublishRepositoryResult = round_trip(json!({
                "repository": repo, "remoteName": "origin", "remoteUrl": "u",
                "branch": "main", "upstreamBranch": "origin/main", "status": status,
            }));
        }
        assert!(
            serde_json::from_value::<SourceControlPublishRepositoryResult>(json!({
                "repository": repo, "remoteName": "origin", "remoteUrl": "u",
                "branch": "main", "status": "remote-added",
            }))
            .is_err()
        );
    }

    fn some(value: &str) -> Value {
        json!({ "_tag": "Some", "value": value })
    }

    #[test]
    fn discovery_result_round_trips_effect_options() {
        let value = json!({
            "versionControlSystems": [
                {
                    "kind": "git", "implemented": true, "label": "Git", "executable": "git",
                    "status": "available", "version": some("2.45.0"),
                    "installHint": "brew install git", "detail": { "_tag": "None" },
                },
                {
                    "kind": "jj", "implemented": false, "label": "Jujutsu",
                    "status": "missing", "version": { "_tag": "None" },
                    "installHint": "brew install jj", "detail": some("not installed"),
                },
            ],
            "sourceControlProviders": [{
                "kind": "github", "label": "GitHub", "status": "available",
                "version": some("2.60.0"), "installHint": "brew install gh",
                "detail": { "_tag": "None" },
                "auth": {
                    "status": "authenticated", "account": some("octo"),
                    "host": some("github.com"), "detail": { "_tag": "None" },
                    "accounts": [
                        {
                            "host": "github.com", "account": "octo", "active": true,
                            "authenticated": true, "environmentVariable": "GH_TOKEN",
                        },
                        {
                            "host": "ghe.example", "account": "me", "active": false,
                            "authenticated": false, "error": "token expired",
                        },
                    ],
                },
            }],
        });
        let parsed: SourceControlDiscoveryResult = round_trip(value.clone());
        assert!(parsed.version_control_systems[0].executable.is_some());
        assert!(parsed.version_control_systems[1].executable.is_none());

        // Option fields are required on the wire; `accounts` may be absent.
        let mut no_accounts = value.clone();
        no_accounts["sourceControlProviders"][0]["auth"]
            .as_object_mut()
            .unwrap()
            .remove("accounts");
        let parsed: SourceControlDiscoveryResult = round_trip(no_accounts);
        assert!(parsed.source_control_providers[0].auth.accounts.is_none());

        let mut missing_version = value;
        missing_version["versionControlSystems"][0]
            .as_object_mut()
            .unwrap()
            .remove("version");
        assert!(serde_json::from_value::<SourceControlDiscoveryResult>(missing_version).is_err());

        for status in ["authenticated", "unauthenticated", "unknown"] {
            let parsed: SourceControlProviderAuthStatus =
                serde_json::from_value(json!(status)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json!(status));
        }
        assert!(serde_json::from_value::<SourceControlDiscoveryStatus>(json!("broken")).is_err());
    }

    #[test]
    fn provider_info_and_errors() {
        let _: SourceControlProviderInfo = round_trip(provider_info());

        let error: SourceControlProviderError = round_trip(json!({
            "_tag": "SourceControlProviderError", "provider": "github", "operation": "createPr",
            "cwd": "/r", "command": "gh", "repository": "o/r", "reference": "#1",
            "detail": "boom", "cause": { "code": 1 },
        }));
        assert_eq!(
            error.to_string(),
            "Source control provider github failed in createPr: boom"
        );
        let repo: SourceControlRepositoryError = round_trip(json!({
            "_tag": "SourceControlRepositoryError", "provider": "github",
            "operation": "lookup", "detail": "nope",
        }));
        assert_eq!(
            repo.to_string(),
            "Source control repository operation lookup failed for github: nope"
        );
        assert!(
            serde_json::from_value::<SourceControlProviderError>(json!({
                "_tag": "SourceControlRepositoryError", "provider": "github",
                "operation": "o", "cwd": "/r", "detail": "d",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SourceControlProviderError>(json!({
                "_tag": "SourceControlProviderError", "provider": "GitHub",
                "operation": "o", "cwd": "/r", "detail": "d",
            }))
            .is_err()
        );
    }
}
