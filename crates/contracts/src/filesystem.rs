//! Port of upstream/packages/contracts/src/filesystem.ts

use crate::base_schemas::{NonNegativeInt, TrimmedNonEmptyString, js_len};
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
        if js_len(value.as_str()) > FILESYSTEM_PATH_MAX_LENGTH {
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
        #[derive(Deserialize)]
        struct Path(#[serde(deserialize_with = "deserialize")] TrimmedNonEmptyString);
        let values = Vec::<Path>::deserialize(d)?;
        if values.is_empty() || values.len() > FILESYSTEM_METADATA_BATCH_LIMIT {
            return Err(serde::de::Error::custom(format!(
                "expected between 1 and {FILESYSTEM_METADATA_BATCH_LIMIT} paths"
            )));
        }
        Ok(values.into_iter().map(|path| path.0).collect())
    }

    pub fn deserialize_option<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<TrimmedNonEmptyString>, D::Error> {
        #[derive(Deserialize)]
        struct Path(#[serde(deserialize_with = "deserialize")] TrimmedNonEmptyString);
        Ok(Option::<Path>::deserialize(d)?.map(|path| path.0))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_input_bounds() {
        let decode = |value| serde_json::from_value::<FilesystemGetMetadataInput>(value);
        assert!(decode(json!({ "paths": [] })).is_err());
        assert!(decode(json!({ "paths": ["  ~/a  "] })).is_ok());
        assert!(decode(json!({ "paths": vec!["/a"; 64] })).is_ok());
        assert!(decode(json!({ "paths": vec!["/a"; 65] })).is_err());
        assert!(decode(json!({ "paths": ["   "] })).is_err());
        // 512 UTF-16 units pass; 513 fail (astral chars count as two).
        assert!(decode(json!({ "paths": ["a".repeat(512)] })).is_ok());
        assert!(decode(json!({ "paths": ["a".repeat(513)] })).is_err());
        assert!(decode(json!({ "paths": ["😀".repeat(256)] })).is_ok());
        assert!(decode(json!({ "paths": ["😀".repeat(257)] })).is_err());
    }

    #[test]
    fn browse_input_cwd_is_optional_and_bounded() {
        let decoded: FilesystemBrowseInput =
            serde_json::from_value(json!({ "partialPath": "~/" })).unwrap();
        assert_eq!(decoded.cwd, None);
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            json!({ "partialPath": "~/" })
        );
        assert!(
            serde_json::from_value::<FilesystemBrowseInput>(
                json!({ "partialPath": "~/", "cwd": "c".repeat(513) })
            )
            .is_err()
        );
    }

    #[test]
    fn metadata_result_keeps_null_entries() {
        let result: FilesystemGetMetadataResult = serde_json::from_value(
            json!({ "entries": [null, { "kind": "file", "byteLength": 3 }] }),
        )
        .unwrap();
        assert_eq!(result.entries[0], None);
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "entries": [null, { "kind": "file", "byteLength": 3 }] })
        );
    }

    #[test]
    fn browse_failure_literals() {
        assert_eq!(
            serde_json::to_value(FilesystemBrowseFailure::WindowsPathUnsupported).unwrap(),
            json!("windows_path_unsupported")
        );
        let error = FilesystemBrowseError::new(
            "~/x",
            Some("/p"),
            FilesystemBrowseFailure::ReadDirectoryFailed,
        );
        assert_eq!(
            error.to_string(),
            "Failed to browse filesystem path '~/x' from '/p'."
        );
    }
}
