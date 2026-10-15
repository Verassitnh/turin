//! Port of upstream/packages/contracts/src/project.ts

use crate::base_schemas::{
    CommandId, ContractError, IsoDateTime, NonNegativeInt, PositiveInt, ProjectId,
    TrimmedNonEmptyString, TrimmedString, js_len,
};
use crate::environment::{RepositoryIdentity, ThreadEnvMode};
use crate::model_selection::ModelSelection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

crate::max_len!(max_200, 200);
crate::max_len!(max_256, 256);

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
        let valid = js_len(s) <= 64
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

/// `ProjectMonogramText`: `^[\p{L}\p{N}][\p{L}\p{N}\p{M}\u200c\u200d]*$`, at
/// most 32 UTF-16 units. Grapheme-count validation belongs to the server
/// command boundary, not snapshot decoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectMonogramText(TrimmedNonEmptyString);

impl ProjectMonogramText {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"^[\p{L}\p{N}][\p{L}\p{N}\p{M}\x{200c}\x{200d}]*$")
                .expect("monogram pattern is valid")
        });
        let value = TrimmedNonEmptyString::new(value)?;
        let s = value.as_str();
        if js_len(s) > 32 || !PATTERN.is_match(s) {
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
        if js_len(value.as_str()) > 32 {
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
        // `/\.(?:avif|…)$/i`: the extension must follow a dot.
        let valid = js_len(s) <= 1024
            && lower
                .rsplit_once('.')
                .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext));
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
    if raw.get("kind").and_then(|k| k.as_str()) != Some("lucide") {
        return serde_json::from_value(raw.clone()).map_err(|e| e.to_string());
    }
    // `monogramText` / `monogram` are `Schema.optional(ProjectMonogramText)`: absent
    // (or null here) is fine, any other non-string value fails the member.
    let monogram_field = |key: &str| -> Result<Option<ProjectMonogramText>, String> {
        match raw.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(text)) => ProjectMonogramText::new(text)
                .map(Some)
                .map_err(|e| e.to_string()),
            Some(_) => Err(format!("`{key}` must be a string")),
        }
    };
    let monogram_text = monogram_field("monogramText")?;
    let monogram = match monogram_text {
        Some(text) => Some(text),
        None => monogram_field("monogram")?,
    };
    // The lucide fields (`name`, `color`) must decode either way.
    let name_and_color =
        |raw: &serde_json::Value| -> Result<(serde_json::Value, serde_json::Value), String> {
            Ok((
                raw.get("name").cloned().unwrap_or(serde_json::Value::Null),
                raw.get("color").cloned().unwrap_or(serde_json::Value::Null),
            ))
        };
    let (name, color) = name_and_color(raw)?;
    let name: ProjectLucideIconName = serde_json::from_value(name).map_err(|e| e.to_string())?;
    let color: ProjectIconColor = serde_json::from_value(color).map_err(|e| e.to_string())?;
    Ok(match monogram {
        Some(text) => ProjectIconOverride::Monogram { text, color },
        None => ProjectIconOverride::Lucide { name, color },
    })
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

const PROJECT_SEARCH_ENTRIES_MAX_LIMIT: u64 = 200;
const PROJECT_SEARCH_CONTENTS_MAX_LIMIT: u64 = 500;

/// `PositiveInt.check(isLessThanOrEqualTo(max))` and the content query bound.
mod limits {
    use super::*;

    fn bounded<'de, D: Deserializer<'de>>(d: D, max: u64) -> Result<PositiveInt, D::Error> {
        let value = PositiveInt::deserialize(d)?;
        if value.get() > max {
            Err(serde::de::Error::custom(format!("expected at most {max}")))
        } else {
            Ok(value)
        }
    }

    pub fn entries<'de, D: Deserializer<'de>>(d: D) -> Result<PositiveInt, D::Error> {
        bounded(d, PROJECT_SEARCH_ENTRIES_MAX_LIMIT)
    }

    pub fn contents<'de, D: Deserializer<'de>>(d: D) -> Result<PositiveInt, D::Error> {
        bounded(d, PROJECT_SEARCH_CONTENTS_MAX_LIMIT)
    }

    /// `Schema.String.check(isNonEmpty(), isMaxLength(256))`, deliberately untrimmed.
    pub fn contents_query<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let value = String::deserialize(d)?;
        if value.is_empty() || js_len(&value) > 256 {
            Err(serde::de::Error::custom("expected 1..=256 characters"))
        } else {
            Ok(value)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesInput {
    pub cwd: TrimmedNonEmptyString,
    // An empty query is a bounded browse: the index returns frecency-ordered
    // entries, which the file picker uses for its initial results.
    #[serde(deserialize_with = "max_256::required")]
    pub query: TrimmedString,
    #[serde(deserialize_with = "limits::entries")]
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
    #[serde(deserialize_with = "limits::contents_query")]
    pub query: String,
    #[serde(deserialize_with = "limits::contents")]
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
    use crate::base_schemas::js_len;

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<crate::base_schemas::TrimmedNonEmptyString, D::Error> {
        let value =
            <crate::base_schemas::TrimmedNonEmptyString as serde::Deserialize>::deserialize(d)?;
        if js_len(value.as_str()) > MAX {
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
    #[serde(deserialize_with = "max_200::required")]
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

#[cfg(test)]
mod upstream_tests {
    use super::*;
    use serde_json::{Value, json};

    fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn search_inputs_trim_entries_query_but_not_content_query() {
        let entries: ProjectSearchEntriesInput =
            decode(json!({ "cwd": "/workspace", "query": "   ", "limit": 10, "kind": "file" }))
                .unwrap();
        assert_eq!(entries.query.as_str(), "");
        let contents: ProjectSearchContentsInput = decode(json!({
            "cwd": "/workspace", "query": " foo ", "limit": 10,
            "caseSensitive": false, "wholeWord": false, "useRegex": false,
        }))
        .unwrap();
        assert_eq!(contents.query, " foo ");
    }

    #[test]
    fn search_input_bounds() {
        let entries = |limit: u64, query: String| {
            decode::<ProjectSearchEntriesInput>(
                json!({ "cwd": "/w", "query": query, "limit": limit }),
            )
        };
        assert!(entries(200, "a".repeat(256)).is_ok());
        assert!(entries(201, String::new()).is_err());
        assert!(entries(0, String::new()).is_err());
        assert!(entries(1, "a".repeat(257)).is_err());
        let contents = |limit: u64, query: String| {
            decode::<ProjectSearchContentsInput>(json!({
                "cwd": "/w", "query": query, "limit": limit,
                "caseSensitive": false, "wholeWord": false, "useRegex": false,
            }))
        };
        assert!(contents(500, "a".repeat(256)).is_ok());
        assert!(contents(501, "a".into()).is_err());
        assert!(contents(1, String::new()).is_err());
        assert!(contents(1, "😀".repeat(129)).is_err());
    }

    #[test]
    fn legacy_message_only_errors_decode() {
        let search: ProjectSearchEntriesError = decode(json!({
            "_tag": "ProjectSearchEntriesError",
            "message": "Legacy project search failure.",
            "query": "legacy sensitive query",
        }))
        .unwrap();
        let ProjectSearchEntriesError::Diagnostics {
            cwd,
            query_length,
            failure,
            message,
            ..
        } = &search;
        assert_eq!(message.as_str(), "Legacy project search failure.");
        assert!(cwd.is_none() && query_length.is_none() && failure.is_none());
        // The legacy `query` key is not carried forward.
        assert!(
            serde_json::to_value(&search)
                .unwrap()
                .get("query")
                .is_none()
        );
        let write: ProjectWriteFileError = decode(json!({
            "_tag": "ProjectWriteFileError", "message": "Legacy project write failure.",
        }))
        .unwrap();
        assert_eq!(write.to_string(), "Legacy project write failure.");
    }

    #[test]
    fn constructor_messages_do_not_leak_causes() {
        assert_eq!(
            ProjectSearchEntriesError::new("/workspace").to_string(),
            "Failed to search workspace entries in '/workspace'."
        );
        assert_eq!(
            ProjectSearchContentsError::new("/workspace").to_string(),
            "Failed to search workspace contents in '/workspace'."
        );
        assert_eq!(
            ProjectListEntriesError::new("/workspace").to_string(),
            "Failed to list workspace entries in '/workspace'."
        );
        assert_eq!(
            ProjectReadFileError::new("/workspace", "src/index.ts").to_string(),
            "Failed to read workspace file 'src/index.ts' in '/workspace'."
        );
        assert_eq!(
            ProjectWriteFileError::new("/workspace", "a.txt").to_string(),
            "Failed to write workspace file 'a.txt' in '/workspace'."
        );
    }

    #[test]
    fn list_entries_error_has_no_search_fields() {
        let encoded = serde_json::to_value(ProjectListEntriesError::new("/w")).unwrap();
        assert!(encoded.get("queryLength").is_none() && encoded.get("limit").is_none());
    }

    #[test]
    fn payloads_preserve_omitted_false_and_null() {
        let create: ProjectCreatePayload = decode(json!({
            "title": " Example ", "workspaceRoot": "/workspace", "createWorkspaceRootIfMissing": false,
        }))
        .unwrap();
        assert_eq!(create.scripts, None);
        let update: ProjectUpdatePayload = decode(json!({
            "autoPull": false, "defaultModelSelection": null, "faviconPath": null,
        }))
        .unwrap();
        assert_eq!(update.title, None);
        assert_eq!(update.default_model_selection, Some(None));
        assert_eq!(update.favicon_path, Some(None));
        assert_eq!(update.auto_pull, Some(false));
        let mutation: ProjectMutation = decode(json!({
            "type": "project.update", "commandId": "command", "projectId": "project",
            "autoPull": false, "defaultModelSelection": null, "faviconPath": null,
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&mutation).unwrap(),
            json!({
                "type": "project.update", "commandId": "command", "projectId": "project",
                "autoPull": false, "defaultModelSelection": null, "faviconPath": null,
            })
        );
        let created: ProjectMutation = decode(json!({
            "type": "project.create", "commandId": "c", "projectId": "p",
            "title": " Example ", "workspaceRoot": "/workspace", "createWorkspaceRootIfMissing": false,
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&created).unwrap()["title"],
            json!("Example")
        );
    }

    #[test]
    fn favicon_paths_accept_only_supported_image_files() {
        assert_eq!(
            ProjectFaviconPath::new("brand/icon.svg").unwrap().as_str(),
            "brand/icon.svg"
        );
        assert!(ProjectFaviconPath::new("Resources/AppIcon.icns").is_ok());
        assert!(ProjectFaviconPath::new("a/B.JPEG").is_ok());
        assert!(ProjectFaviconPath::new(".env").is_err());
        // The extension must follow a dot.
        assert!(ProjectFaviconPath::new("png").is_err());
        assert!(ProjectFaviconPath::new(format!("{}.png", "a".repeat(1021))).is_err());
        assert!(ProjectFaviconPath::new(format!("{}.png", "a".repeat(1020))).is_ok());
    }

    fn update_icon(icon: Value) -> Result<ProjectIconOverride, serde_json::Error> {
        let update: ProjectUpdatePayload = decode(json!({ "projectIcon": icon }))?;
        Ok(update.project_icon.unwrap().unwrap())
    }

    #[test]
    fn icon_overrides_accept_lucide_colors_and_emoji() {
        for icon in [
            json!({ "kind": "lucide", "name": "alarm-clock", "color": "violet" }),
            json!({ "kind": "emoji", "emoji": "👩🏽‍💻" }),
        ] {
            assert_eq!(
                serde_json::to_value(update_icon(icon.clone()).unwrap()).unwrap(),
                icon
            );
        }
        assert!(
            update_icon(json!({ "kind": "lucide", "name": "Alarm Clock", "color": "ultraviolet" }))
                .is_err()
        );
        assert!(update_icon(json!({ "kind": "lucide", "name": "a--b", "color": "blue" })).is_err());
        // 16 astral characters are 32 UTF-16 units; 17 are 34.
        assert!(update_icon(json!({ "kind": "emoji", "emoji": "😀".repeat(16) })).is_ok());
        assert!(update_icon(json!({ "kind": "emoji", "emoji": "😀".repeat(17) })).is_err());
    }

    #[test]
    fn monograms_validate_text_and_palette_colors() {
        for text in ["A", "T3", "É", "文書", "कि", "किखि", "e\u{301}"] {
            let icon = json!({ "kind": "monogram", "color": "violet", "text": text });
            assert_eq!(
                serde_json::to_value(update_icon(icon.clone()).unwrap()).unwrap(),
                icon,
                "{text}"
            );
        }
        for icon in [
            json!({ "kind": "monogram", "text": "", "color": "blue" }),
            json!({ "kind": "monogram", "text": "\u{301}", "color": "blue" }),
            json!({ "kind": "monogram", "text": "A B", "color": "blue" }),
            json!({ "kind": "monogram", "text": "🚀", "color": "blue" }),
            json!({ "kind": "monogram", "text": "T3", "color": "ultraviolet" }),
            json!({ "kind": "monogram", "text": "A".repeat(33), "color": "blue" }),
        ] {
            assert!(update_icon(icon.clone()).is_err(), "{icon}");
        }
    }

    #[test]
    fn icons_send_and_store_in_their_plain_shape() {
        for icon in [
            json!({ "kind": "monogram", "text": "क्ष्म", "color": "violet" }),
            json!({ "kind": "lucide", "name": "alarm-clock", "color": "blue" }),
            json!({ "kind": "emoji", "emoji": "🚀" }),
        ] {
            let received: ReceivedProjectIcon = decode(icon.clone()).unwrap();
            assert_eq!(serde_json::to_value(&received).unwrap(), icon);
            let stored: StoredProjectIcon = decode(icon.clone()).unwrap();
            assert_eq!(serde_json::to_value(&stored).unwrap(), icon);
        }
    }

    #[test]
    fn reads_monograms_stored_in_the_pre_v2_fallback_shape() {
        let monogram = json!({ "kind": "monogram", "text": "T3", "color": "violet" });
        let fallback = json!({ "kind": "lucide", "name": "folder-code", "color": "violet" });
        for extra in [json!({ "monogramText": "T3" }), json!({ "monogram": "T3" })] {
            let mut legacy = fallback.clone();
            legacy
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let stored: StoredProjectIcon = decode(legacy.clone()).unwrap();
            assert_eq!(serde_json::to_value(&stored).unwrap(), monogram);
            let received: ReceivedProjectIcon = decode(legacy).unwrap();
            assert_eq!(serde_json::to_value(&received).unwrap(), monogram);
        }
        // A broken legacy monogram fails the member.
        let mut broken = fallback.clone();
        broken["monogramText"] = json!("A B");
        assert!(decode::<StoredProjectIcon>(broken).is_err());
        let mut non_string = fallback;
        non_string["monogramText"] = json!(5);
        assert!(decode::<StoredProjectIcon>(non_string).is_err());
        // The lucide fields are validated even with a monogram.
        let invalid_name = json!({ "kind": "lucide", "name": "Bad Name", "color": "violet", "monogramText": "T3" });
        assert!(decode::<StoredProjectIcon>(invalid_name).is_err());
    }

    #[test]
    fn icon_kind_from_a_newer_server_shows_the_default_icon() {
        let unknown: ReceivedProjectIcon =
            decode(json!({ "kind": "image", "url": "https://example.com/a.png" })).unwrap();
        assert_eq!(unknown.0, None);
        let shell: crate::orchestration_project::OrchestrationProjectShell = decode(json!({
            "id": "project-1", "title": "Project", "workspaceRoot": "/tmp/project",
            "defaultModelSelection": null, "scripts": [],
            "projectIcon": { "kind": "image", "url": "https://example.com/a.png" },
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
        }))
        .unwrap();
        assert!(matches!(
            shell.project_icon,
            Some(Some(ReceivedProjectIcon(None)))
        ));
        // A known kind with a broken payload still fails.
        assert!(decode::<ReceivedProjectIcon>(json!({ "kind": "emoji" })).is_err());
        // A non-string kind fails: it is not an unknown tag.
        assert!(decode::<ReceivedProjectIcon>(json!({ "kind": 5 })).is_err());
    }

    #[test]
    fn project_round_trips_with_tri_state_fields() {
        let base = json!({
            "id": "p", "title": "T", "workspaceRoot": "/w", "defaultModelSelection": null,
            "scripts": [], "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z", "deletedAt": null,
        });
        let project: Project = decode(base.clone()).unwrap();
        assert_eq!(project.favicon_path, None);
        assert_eq!(serde_json::to_value(&project).unwrap(), base);
        let mut nulls = base;
        nulls["faviconPath"] = json!(null);
        nulls["defaultThreadEnvMode"] = json!(null);
        let project: Project = decode(nulls.clone()).unwrap();
        assert_eq!(project.favicon_path, Some(None));
        assert_eq!(serde_json::to_value(&project).unwrap(), nulls);
    }

    #[test]
    fn script_async_key_and_create_new_input() {
        let script: ProjectScript = decode(json!({
            "id": "s", "name": "n", "command": "c", "icon": "play",
            "runOnWorktreeCreate": false, "async": true,
        }))
        .unwrap();
        assert_eq!(script.is_async, Some(true));
        assert_eq!(serde_json::to_value(&script).unwrap()["async"], json!(true));
        assert!(decode::<ProjectCreateNewInput>(json!({ "name": "n".repeat(200) })).is_ok());
        assert!(decode::<ProjectCreateNewInput>(json!({ "name": "n".repeat(201) })).is_err());
    }

    #[test]
    fn read_write_paths_are_bounded_in_utf16_units() {
        let read = |path: String| {
            decode::<ProjectReadFileInput>(json!({ "cwd": "/w", "relativePath": path }))
        };
        assert!(read("a".repeat(512)).is_ok());
        assert!(read("a".repeat(513)).is_err());
        assert!(read("😀".repeat(257)).is_err());
    }
}
