//! Port of upstream/packages/contracts/src/sourceControl.ts

use crate::base_schemas::{ContractError, EffectOption, TrimmedNonEmptyString};
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
    /// Validates like upstream's branded slug: trimmed, non-empty,
    /// `^[a-z][a-z0-9-]*$`, at most 64 chars.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = value.as_ref().trim();
        let valid = !trimmed.is_empty()
            && trimmed.len() <= SOURCE_CONTROL_KIND_MAX_CHARS
            && trimmed.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && trimmed.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
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
    pub number: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<Option<String>>,
    pub updated_at: EffectOption<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_cross_repository: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_repository_name_with_owner: Option<Option<TrimmedNonEmptyString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
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
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
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
