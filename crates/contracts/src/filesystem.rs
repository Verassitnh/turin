//! Port of upstream/packages/contracts/src/filesystem.ts

use crate::base_schemas::{NonNegativeInt, TrimmedNonEmptyString};
use serde::{Deserialize, Serialize};

const FILESYSTEM_PATH_MAX_LENGTH: usize = 512;

pub const FILESYSTEM_METADATA_BATCH_LIMIT: usize = 64;

/// Serde validator for upstream's path fields: trimmed non-empty, ≤ 512 chars.
mod path_field {
    use super::*;

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<TrimmedNonEmptyString, D::Error> {
        let value = TrimmedNonEmptyString::deserialize(d)?;
        if value.as_str().len() > FILESYSTEM_PATH_MAX_LENGTH {
            Err(serde::de::Error::custom(
                "filesystem paths can be at most 512 chars",
            ))
        } else {
            Ok(value)
        }
    }

    pub fn deserialize_vec<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<TrimmedNonEmptyString>, D::Error> {
        let values = Vec::<serde_json::Value>::deserialize(d)?;
        values
            .into_iter()
            .map(|value| deserialize(value).map_err(serde::de::Error::custom))
            .collect()
    }

    pub fn deserialize_option<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<TrimmedNonEmptyString>, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(d)?;
        value
            .map(|value| deserialize(value).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemGetMetadataInput {
    // Absolute host paths, including ~/ paths. Relative paths need a workspace
    // anchor on the client; the server never resolves them against its own cwd.
    #[serde(deserialize_with = "path_field::deserialize_vec")]
    pub paths: Vec<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilesystemEntryKind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemEntryMetadata {
    pub kind: FilesystemEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_length: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<TrimmedNonEmptyString>,
}

/// Results follow request order so paths do not have to travel back over the
/// wire. A null entry means the path is missing, unreadable, or unsupported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemGetMetadataResult {
    pub entries: Vec<Option<FilesystemEntryMetadata>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseInput {
    #[serde(deserialize_with = "path_field::deserialize")]
    pub partial_path: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "path_field::deserialize_option"
    )]
    pub cwd: Option<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseEntry {
    pub name: TrimmedNonEmptyString,
    pub full_path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseResult {
    pub parent_path: TrimmedNonEmptyString,
    pub entries: Vec<FilesystemBrowseEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemBrowseFailure {
    WindowsPathUnsupported,
    CurrentProjectRequired,
    ReadDirectoryFailed,
}

/// `FilesystemBrowseError` (Schema.TaggedError). Structured diagnostics stay
/// optional for rolling compatibility with legacy message-only payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FilesystemBrowseError {
    #[serde(rename = "FilesystemBrowseError")]
    #[error("{message}")]
    FilesystemBrowseError {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_path: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<FilesystemBrowseFailure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_path: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        platform: Option<TrimmedNonEmptyString>,
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl FilesystemBrowseError {
    /// Upstream's constructor default: builds the human message when the
    /// caller did not supply one.
    pub fn new(partial_path: &str, cwd: Option<&str>, failure: FilesystemBrowseFailure) -> Self {
        let cwd_part = cwd.map(|c| format!(" from '{c}'")).unwrap_or_default();
        Self::FilesystemBrowseError {
            partial_path: Some(
                TrimmedNonEmptyString::new(partial_path).expect("caller passes non-empty"),
            ),
            cwd: cwd.map(|c| TrimmedNonEmptyString::new(c).expect("caller passes non-empty")),
            failure: Some(failure),
            parent_path: None,
            platform: None,
            message: TrimmedNonEmptyString::new(format!(
                "Failed to browse filesystem path '{partial_path}'{cwd_part}."
            ))
            .expect("message is non-empty"),
            cause: None,
        }
    }
}
