//! Port of upstream/packages/contracts/src/project.ts

use crate::base_schemas::{
    CommandId, ContractError, IsoDateTime, NonNegativeInt, PositiveInt, ProjectId,
    TrimmedNonEmptyString, TrimmedString,
};
use crate::environment::{RepositoryIdentity, ThreadEnvMode};
use crate::model_selection::ModelSelection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectScriptIcon {
    Play,
    Test,
    Lint,
    Configure,
    Build,
    Debug,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: TrimmedNonEmptyString,
    pub name: TrimmedNonEmptyString,
    pub command: TrimmedNonEmptyString,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    /// Run in the thread's worktree each time the thread settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    /// Start the agent while setup runs unless explicitly disabled.
    #[serde(rename = "async", default, skip_serializing_if = "Option::is_none")]
    pub is_async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectIconColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

/// `ProjectLucideIconName`: trimmed non-empty, ≤ 64 chars, kebab-case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectLucideIconName(TrimmedNonEmptyString);

impl ProjectLucideIconName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        let s = value.as_str();
        let valid = s.len() <= 64
            && s.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            });
        if !valid {
            return Err(ContractError {
                expected: "a kebab-case lucide icon name (<= 64 chars)",
                got: s.to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ProjectLucideIconName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for ProjectLucideIconName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProjectLucideIconName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `ProjectMonogramText`: letters/numbers/marks only, ≤ 32 chars. Grapheme-
/// count validation belongs to the server command boundary, not snapshot
/// decoding. (Marks are recognized by canonical combining class; the rare
/// spacing/enclosing marks with ccc 0 are rejected — see PORTING.md.)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectMonogramText(TrimmedNonEmptyString);

impl ProjectMonogramText {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        let s = value.as_str();
        let mut chars = s.chars();
        let valid = s.len() <= 32
            && chars.next().is_some_and(|c| c.is_alphanumeric())
            && s.chars().skip(1).all(|c| {
                c.is_alphanumeric()
                    || matches!(c, '\u{200c}' | '\u{200d}')
                    || unicode_ccc::get_canonical_combining_class(c)
                        != unicode_ccc::CanonicalCombiningClass::NotReordered
            });
        if !valid {
            return Err(ContractError {
                expected: "a monogram of letters, numbers, and marks (<= 32 chars)",
                got: s.to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ProjectMonogramText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for ProjectMonogramText {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProjectMonogramText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `ProjectEmoji`: trimmed non-empty, ≤ 32 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectEmoji(TrimmedNonEmptyString);

impl ProjectEmoji {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        if value.as_str().len() > 32 {
            return Err(ContractError {
                expected: "an emoji of at most 32 chars",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ProjectEmoji {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for ProjectEmoji {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProjectEmoji {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

string_literal!(
    /// Constant `"lucide"` icon kind.
    LucideIconKind,
    "lucide"
);
string_literal!(
    /// Constant `"emoji"` icon kind.
    EmojiIconKind,
    "emoji"
);
string_literal!(
    /// Constant `"monogram"` icon kind.
    MonogramIconKind,
    "monogram"
);

/// A workspace-relative image a project may use as its favicon:
/// ends in an image extension, ≤ 1024 chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectFaviconPath(TrimmedNonEmptyString);

impl ProjectFaviconPath {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        let s = value.as_str();
        const EXTENSIONS: &[&str] = &[
            "avif", "gif", "icns", "ico", "jpg", "jpeg", "png", "svg", "webp",
        ];
        let lower = s.to_lowercase();
        let valid = s.len() <= 1024
            && lower
                .rsplit('.')
                .next()
                .is_some_and(|ext| EXTENSIONS.contains(&ext));
        if !valid {
            return Err(ContractError {
                expected: "a workspace-relative image path (.avif/.gif/.icns/.ico/.jpe?g/.png/.svg/.webp)",
                got: s.to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ProjectFaviconPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for ProjectFaviconPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for ProjectFaviconPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// A project's chosen icon, as clients set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProjectIconOverride {
    #[serde(rename = "lucide")]
    Lucide {
        name: ProjectLucideIconName,
        color: ProjectIconColor,
    },
    #[serde(rename = "emoji")]
    Emoji { emoji: ProjectEmoji },
    #[serde(rename = "monogram")]
    Monogram {
        text: ProjectMonogramText,
        color: ProjectIconColor,
    },
}

/// Upstream's legacy-monogram migration: before v2, servers sent a monogram
/// as a lucide icon carrying its text, so older clients showed a folder.
/// Icons stored then, and v2 servers released before this change, still use
/// that shape; it is read here, never written.
fn from_legacy_monogram(raw: &serde_json::Value) -> Result<ProjectIconOverride, String> {
    match raw.get("kind").and_then(|k| k.as_str()) {
        Some("lucide") => {
            let monogram_text = raw
                .get("monogramText")
                .and_then(|v| v.as_str())
                .map(ProjectMonogramText::new)
                .transpose()
                .map_err(|e| e.to_string())?;
            let monogram = match monogram_text {
                Some(text) => Some(text),
                None => raw
                    .get("monogram")
                    .and_then(|v| v.as_str())
                    .map(ProjectMonogramText::new)
                    .transpose()
                    .map_err(|e| e.to_string())?,
            };
            match monogram {
                Some(text) => Ok(ProjectIconOverride::Monogram {
                    text,
                    color: serde_json::from_value(
                        raw.get("color").cloned().unwrap_or(serde_json::Value::Null),
                    )
                    .map_err(|e| e.to_string())?,
                }),
                None => serde_json::from_value(raw.clone()).map_err(|e| e.to_string()),
            }
        }
        _ => serde_json::from_value(raw.clone()).map_err(|e| e.to_string()),
    }
}

/// An icon the server stores; it reads legacy monograms and writes the plain
/// shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProjectIcon(pub ProjectIconOverride);

impl Serialize for StoredProjectIcon {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for StoredProjectIcon {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        from_legacy_monogram(&raw)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// An icon as clients receive it. A kind from a newer server decodes as no
/// override (null), so the client shows the project's default icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedProjectIcon(pub Option<ProjectIconOverride>);

impl Serialize for ReceivedProjectIcon {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Some(icon) => icon.serialize(s),
            None => s.serialize_none(),
        }
    }
}

impl ReceivedProjectIcon {
    pub fn known_tags() -> &'static [&'static str] {
        &["lucide", "emoji", "monogram"]
    }
}

impl crate::base_schemas::TaggedUnion for ReceivedProjectIcon {
    const TAG: &'static str = "kind";
    fn known_tags() -> &'static [&'static str] {
        Self::known_tags()
    }
}

impl<'de> Deserialize<'de> for ReceivedProjectIcon {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        match crate::base_schemas::unknown_union_tag(&raw, "kind", Self::known_tags()) {
            Some(_) => Ok(Self(None)),
            None => from_legacy_monogram(&raw)
                .map(|icon| Self(Some(icon)))
                .map_err(serde::de::Error::custom),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub favicon_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub project_icon: Option<Option<ReceivedProjectIcon>>,
    /// `Schema.NullOr(ModelSelection)`: required key, `null` when unset.
    pub default_model_selection: Option<ModelSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    // Opt-in because background sync performs network I/O and may move the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    pub scripts: Vec<ProjectScript>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub deleted_at: Option<IsoDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub projects: Vec<Project>,
    pub updated_at: IsoDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProjectChange {
    #[serde(rename = "project.upserted")]
    ProjectUpserted { project: Project },
    #[serde(rename = "project.deleted")]
    ProjectDeleted {
        project_id: ProjectId,
        deleted_at: IsoDateTime,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreatePayload {
    pub title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_workspace_root_if_missing: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<ProjectScript>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdatePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub project_icon: Option<Option<ProjectIconOverride>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub favicon_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<ProjectScript>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProjectMutation {
    #[serde(rename = "project.create")]
    ProjectCreate {
        command_id: CommandId,
        project_id: ProjectId,
        title: TrimmedNonEmptyString,
        workspace_root: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_workspace_root_if_missing: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        default_model_selection: Option<Option<ModelSelection>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scripts: Option<Vec<ProjectScript>>,
    },
    #[serde(rename = "project.update")]
    ProjectUpdate {
        command_id: CommandId,
        project_id: ProjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_root: Option<TrimmedNonEmptyString>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        default_model_selection: Option<Option<ModelSelection>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_pull: Option<bool>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        project_icon: Option<Option<ProjectIconOverride>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        favicon_path: Option<Option<TrimmedNonEmptyString>>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::base_schemas::double_option"
        )]
        default_thread_env_mode: Option<Option<ThreadEnvMode>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scripts: Option<Vec<ProjectScript>>,
    },
    #[serde(rename = "project.delete")]
    ProjectDelete {
        command_id: CommandId,
        project_id: ProjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
}

/// `ProjectMutationError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProjectMutationError {
    #[serde(rename = "ProjectMutationError")]
    #[error("{message}")]
    ProjectMutationError {
        command_id: CommandId,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesInput {
    pub cwd: TrimmedNonEmptyString,
    // An empty query is a bounded browse: the index returns frecency-ordered
    // entries, which the file picker uses for its initial results.
    pub query: TrimmedString,
    pub limit: PositiveInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProjectEntryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub path: TrimmedNonEmptyString,
    pub kind: ProjectEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesResult {
    pub entries: Vec<ProjectEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchContentsInput {
    pub cwd: TrimmedNonEmptyString,
    // Whitespace is significant in content queries (" foo", regex trailing
    // spaces), so the query is deliberately not trimmed on the wire.
    pub query: String,
    pub limit: PositiveInt,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContentMatchRange {
    pub start: NonNegativeInt,
    pub end: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContentMatch {
    pub path: TrimmedNonEmptyString,
    pub line_number: PositiveInt,
    pub line_content: String,
    pub match_ranges: Vec<ProjectContentMatchRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchContentsResult {
    pub matches: Vec<ProjectContentMatch>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex_fallback_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListEntriesInput {
    pub cwd: TrimmedNonEmptyString,
    // Present for immediate filesystem children, including ignored entries;
    // empty means root. Omitted preserves the indexed recursive listing used
    // by older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_path: Option<TrimmedString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListEntriesResult {
    pub entries: Vec<ProjectEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectEntriesFailure {
    WorkspaceRootNotFound,
    WorkspaceRootCreateFailed,
    WorkspaceRootStatFailed,
    WorkspaceRootNotDirectory,
    SearchIndexCreateFailed,
    SearchIndexScanTimedOut,
    SearchIndexSearchFailed,
    DirectoryListFailed,
}

/// Shared optional-diagnostics shape of the project entries errors. The
/// structured fields are optional on the wire so newer peers can decode
/// legacy message-only failures; the `message` default mirrors upstream's
/// constructors.
macro_rules! project_entries_error {
    ($(#[$meta:meta])* $name:ident, $tag:literal, $default:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
        #[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
        pub enum $name {
            #[serde(rename = $tag)]
            #[error("{message}")]
            Diagnostics {
                #[serde(default, skip_serializing_if = "Option::is_none")]
                cwd: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                query_length: Option<NonNegativeInt>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                limit: Option<PositiveInt>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                failure: Option<ProjectEntriesFailure>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                normalized_cwd: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                timeout: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                detail: Option<TrimmedNonEmptyString>,
                message: TrimmedNonEmptyString,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                cause: Option<serde_json::Value>,
            },
        }

        impl $name {
            /// Upstream's constructor: fills the default message when the
            /// caller did not supply one.
            #[allow(clippy::new_without_default)]
            pub fn new(cwd: &str) -> Self {
                Self::Diagnostics {
                    cwd: Some(TrimmedNonEmptyString::new(cwd).expect("caller passes non-empty")),
                    query_length: None,
                    limit: None,
                    failure: None,
                    normalized_cwd: None,
                    timeout: None,
                    detail: None,
                    message: TrimmedNonEmptyString::new(format!($default, cwd))
                        .expect("message is non-empty"),
                    cause: None,
                }
            }
        }
    };
}

project_entries_error! {
    /// `ProjectSearchEntriesError` (Schema.TaggedError).
    ProjectSearchEntriesError,
    "ProjectSearchEntriesError",
    "Failed to search workspace entries in '{}'."
}
project_entries_error! {
    /// `ProjectSearchContentsError` (Schema.TaggedError).
    ProjectSearchContentsError,
    "ProjectSearchContentsError",
    "Failed to search workspace contents in '{}'."
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProjectListEntriesError {
    #[serde(rename = "ProjectListEntriesError")]
    #[error("{message}")]
    Diagnostics {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query_length: Option<NonNegativeInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<PositiveInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<ProjectEntriesFailure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normalized_cwd: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<TrimmedNonEmptyString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<TrimmedNonEmptyString>,
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

impl ProjectListEntriesError {
    #[allow(clippy::new_without_default)]
    pub fn new(cwd: &str) -> Self {
        Self::Diagnostics {
            cwd: Some(TrimmedNonEmptyString::new(cwd).expect("caller passes non-empty")),
            query_length: None,
            limit: None,
            failure: None,
            normalized_cwd: None,
            timeout: None,
            detail: None,
            message: TrimmedNonEmptyString::new(format!(
                "Failed to list workspace entries in '{cwd}'."
            ))
            .expect("message is non-empty"),
            cause: None,
        }
    }
}

/// Path-length bound shared by the read/write file inputs (≤ 512 chars).
mod relative_path {
    pub const MAX: usize = 512;

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<crate::base_schemas::TrimmedNonEmptyString, D::Error> {
        let value =
            <crate::base_schemas::TrimmedNonEmptyString as serde::Deserialize>::deserialize(d)?;
        if value.as_str().len() > MAX {
            Err(serde::de::Error::custom(
                "workspace-relative paths can be at most 512 chars",
            ))
        } else {
            Ok(value)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileInput {
    pub cwd: TrimmedNonEmptyString,
    // Workspace-relative, or an absolute host path for a file outside the
    // workspace. Only workspace-relative paths can be written back.
    #[serde(deserialize_with = "relative_path::deserialize")]
    pub relative_path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileResult {
    pub relative_path: TrimmedNonEmptyString,
    pub contents: String,
    pub byte_length: NonNegativeInt,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFileFailure {
    WorkspacePathOutsideRoot,
    ResolvedPathOutsideRoot,
    PathNotFile,
    BinaryFile,
    OperationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectFileOperation {
    RealpathWorkspaceRoot,
    RealpathTarget,
    Open,
    Stat,
    Read,
    Close,
    MakeDirectory,
    WriteFile,
}

macro_rules! project_file_error {
    ($(#[$meta:meta])* $name:ident, $tag:literal, $default:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
        #[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
        pub enum $name {
            #[serde(rename = $tag)]
            #[error("{message}")]
            Diagnostics {
                #[serde(default, skip_serializing_if = "Option::is_none")]
                cwd: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                relative_path: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                failure: Option<ProjectFileFailure>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                resolved_path: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                resolved_workspace_root: Option<TrimmedNonEmptyString>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                operation: Option<ProjectFileOperation>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                operation_path: Option<TrimmedNonEmptyString>,
                message: TrimmedNonEmptyString,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                cause: Option<serde_json::Value>,
            },
        }

        impl $name {
            /// Upstream's constructor: fills the default message when the
            /// caller did not supply one.
            pub fn new(cwd: &str, relative_path: &str) -> Self {
                Self::Diagnostics {
                    cwd: Some(TrimmedNonEmptyString::new(cwd).expect("caller passes non-empty")),
                    relative_path: Some(
                        TrimmedNonEmptyString::new(relative_path).expect("caller passes non-empty"),
                    ),
                    failure: None,
                    resolved_path: None,
                    resolved_workspace_root: None,
                    operation: None,
                    operation_path: None,
                    message: TrimmedNonEmptyString::new(format!($default, relative_path, cwd))
                        .expect("message is non-empty"),
                    cause: None,
                }
            }
        }
    };
}

project_file_error! {
    /// `ProjectReadFileError` (Schema.TaggedError).
    ProjectReadFileError,
    "ProjectReadFileError",
    "Failed to read workspace file '{}' in '{}'."
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWriteFileInput {
    pub cwd: TrimmedNonEmptyString,
    #[serde(deserialize_with = "relative_path::deserialize")]
    pub relative_path: TrimmedNonEmptyString,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWriteFileResult {
    pub relative_path: TrimmedNonEmptyString,
}

/// The environment's Scratch project, created on first request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEnsureScratchResult {
    pub project_id: ProjectId,
}

/// A project started from just a name, in a new folder the server makes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateNewInput {
    pub name: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateNewResult {
    pub project_id: ProjectId,
    pub workspace_root: TrimmedNonEmptyString,
    /// Why the first commit failed. The project and its files exist either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_error: Option<TrimmedNonEmptyString>,
}

project_file_error! {
    /// `ProjectWriteFileError` (Schema.TaggedError).
    ProjectWriteFileError,
    "ProjectWriteFileError",
    "Failed to write workspace file '{}' in '{}'."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_override_decodes_each_kind() {
        let lucide: ProjectIconOverride = serde_json::from_value(serde_json::json!({
            "kind": "lucide", "name": "folder", "color": "blue"
        }))
        .unwrap();
        assert_eq!(
            lucide,
            ProjectIconOverride::Lucide {
                name: ProjectLucideIconName::new("folder").unwrap(),
                color: ProjectIconColor::Blue,
            }
        );

        let emoji: ProjectIconOverride =
            serde_json::from_value(serde_json::json!({"kind": "emoji", "emoji": "🦀"})).unwrap();
        assert_eq!(
            emoji,
            ProjectIconOverride::Emoji {
                emoji: ProjectEmoji::new("🦀").unwrap()
            }
        );
    }

    #[test]
    fn stored_icon_reads_legacy_monogram() {
        // A lucide icon carrying monogram text becomes a monogram.
        let stored: StoredProjectIcon = serde_json::from_value(serde_json::json!({
            "kind": "lucide", "name": "folder", "color": "rose", "monogramText": "A1"
        }))
        .unwrap();
        assert_eq!(
            stored.0,
            ProjectIconOverride::Monogram {
                text: ProjectMonogramText::new("A1").unwrap(),
                color: ProjectIconColor::Rose,
            }
        );

        // Without monogram fields it stays lucide.
        let plain: StoredProjectIcon = serde_json::from_value(serde_json::json!({
            "kind": "lucide", "name": "folder", "color": "rose"
        }))
        .unwrap();
        assert!(matches!(plain.0, ProjectIconOverride::Lucide { .. }));

        // And it re-encodes as the plain override shape.
        let encoded = serde_json::to_value(&plain).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({"kind":"lucide","name":"folder","color":"rose"})
        );
    }

    #[test]
    fn received_icon_decodes_unknown_kind_as_none() {
        let unknown: ReceivedProjectIcon = serde_json::from_value(serde_json::json!({
            "kind": "hologram", "h": 1
        }))
        .unwrap();
        assert_eq!(unknown.0, None);

        let known: ReceivedProjectIcon = serde_json::from_value(serde_json::json!({
            "kind": "monogram", "text": "X9", "color": "pink"
        }))
        .unwrap();
        assert!(matches!(
            known.0,
            Some(ProjectIconOverride::Monogram { .. })
        ));
    }

    #[test]
    fn mutation_round_trips() {
        let mutation = ProjectMutation::ProjectDelete {
            command_id: CommandId::new("c1").unwrap(),
            project_id: ProjectId::new("p1").unwrap(),
            force: Some(true),
        };
        let json = serde_json::to_value(&mutation).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type":"project.delete","commandId":"c1","projectId":"p1","force":true})
        );
        let back: ProjectMutation = serde_json::from_value(json).unwrap();
        assert_eq!(back, mutation);
    }

    #[test]
    fn project_change_decodes() {
        let change: ProjectChange = serde_json::from_value(serde_json::json!({
            "type": "project.deleted", "projectId": "p1", "deletedAt": "2026-10-14T00:00:00.000Z"
        }))
        .unwrap();
        assert!(matches!(change, ProjectChange::ProjectDeleted { .. }));
    }
}

#[cfg(test)]
mod tagged_union_tests {
    use super::*;

    #[test]
    fn received_project_icon_known_tags() {
        use crate::base_schemas::assert_known_tags;
        // One sample per known variant (lucide, emoji, monogram).
        assert_known_tags(&[
            ReceivedProjectIcon(Some(ProjectIconOverride::Lucide {
                name: ProjectLucideIconName::new("folder").unwrap(),
                color: ProjectIconColor::Blue,
            })),
            ReceivedProjectIcon(Some(ProjectIconOverride::Emoji {
                emoji: ProjectEmoji::new("x").unwrap(),
            })),
            ReceivedProjectIcon(Some(ProjectIconOverride::Monogram {
                text: ProjectMonogramText::new("A1").unwrap(),
                color: ProjectIconColor::Rose,
            })),
        ]);
    }
}
