//! Port of upstream/packages/contracts/src/composerContext.ts
//!
//! Inline context records: the typed payload behind every composer chip. A
//! message's `text` carries position through canonical reference links
//! (`[label](t3-context://v1/<kind>/<contextId>)`); these records carry the
//! payload, keyed by `contextId`. Bytes never live here: image and file
//! records bind to a `ChatAttachment` by id.

use crate::base_schemas::{ContractError, EnvironmentId, NonNegativeInt, ThreadId, TrimmedNonEmptyString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
use std::fmt;

pub const COMPOSER_CONTEXT_KINDS: &[&str] = &[
    "image",
    "file",
    "terminal",
    "element",
    "preview-annotation",
    "review-comment",
    "mention",
    "skill",
    "thread",
];

const CONTEXT_ID_MAX_CHARS: usize = 128;
pub const COMPOSER_CONTEXT_LABEL_MAX_CHARS: usize = 200;
pub const COMPOSER_CONTEXT_TERMINAL_TEXT_MAX_CHARS: usize = 64_000;
// Upstream bounds these fields with isMaxLength; kept for reference (see
// PORTING.md deviations).
#[allow(dead_code)]
const COMPOSER_CONTEXT_ELEMENT_HTML_MAX_CHARS: usize = 8_000;
#[allow(dead_code)]
const COMPOSER_CONTEXT_ELEMENT_STYLES_MAX_CHARS: usize = 8_000;
/// Exported so producers can clamp to the same boundary the schema enforces,
/// rather than minting a record the send path would fail to encode.
pub const COMPOSER_CONTEXT_REVIEW_TEXT_MAX_CHARS: usize = 16_000;
pub const COMPOSER_CONTEXT_REVIEW_DIFF_MAX_CHARS: usize = 32_000;
#[allow(dead_code)]
const COMPOSER_CONTEXT_PREVIEW_COMMENT_MAX_CHARS: usize = 8_000;

fn is_context_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= CONTEXT_ID_MAX_CHARS
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

macro_rules! context_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validates like upstream: trimmed, non-empty,
            /// `^[a-zA-Z0-9_-]+$`, at most 128 chars.
            pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
                let trimmed = value.as_ref().trim();
                if !is_context_id(trimmed) {
                    return Err(ContractError {
                        expected: concat!(stringify!($name), ": ^[a-zA-Z0-9_-]+$ (<= 128 chars)"),
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

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <::std::borrow::Cow<'de, str>>::deserialize(d)?;
                Self::new(raw.as_ref()).map_err(::serde::de::Error::custom)
            }
        }
    };
}

context_id! {
    /// Durable identity of one payload. Shared by every chip that points at it.
    ComposerContextId
}
context_id! {
    /// Identity of one occurrence in a document. Changes when a chip is duplicated.
    ComposerContextReferenceId
}

/// `ComposerContextKind`: open kind — known kinds get typed records,
/// everything else preserves its payload. `^[a-z][a-z0-9-]{0,39}$`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComposerContextKind(String);

impl ComposerContextKind {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = value.as_ref().trim();
        let valid = !trimmed.is_empty()
            && trimmed.len() <= 40
            && trimmed.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && trimmed.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(ContractError {
                expected: "a context kind matching ^[a-z][a-z0-9-]{0,39}$",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ComposerContextKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ComposerContextKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ComposerContextKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

// Same rules as the private `ChatAttachmentId` in orchestration.ts;
// duplicated here because orchestration.ts imports this module.
context_id! {
    /// Binding id of the `ChatAttachment` an image/file record points at.
    ContextAttachmentId
}

crate::int_literal!(
    /// Constant `version: 1` marker of every context record.
    ComposerContextRecordVersion,
    1
);

/// Snapshot used to identify and present a pull request carried by a
/// review-context record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestContextMetadata {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: crate::source_control::ChangeRequestState,
    pub is_draft: bool,
}

string_literal!(
    /// Constant `"image"` context record kind.
    ImageContextKind,
    "image"
);
string_literal!(
    /// Constant `"file"` context record kind.
    FileContextKind,
    "file"
);
string_literal!(
    /// Constant `"terminal"` context record kind.
    TerminalContextKind,
    "terminal"
);
string_literal!(
    /// Constant `"element"` context record kind.
    ElementContextKind,
    "element"
);
string_literal!(
    /// Constant `"preview-annotation"` context record kind.
    PreviewAnnotationContextKind,
    "preview-annotation"
);
string_literal!(
    /// Constant `"review-comment"` context record kind.
    ReviewCommentContextKind,
    "review-comment"
);
string_literal!(
    /// Constant `"mention"` context record kind.
    MentionContextKind,
    "mention"
);
string_literal!(
    /// Constant `"skill"` context record kind.
    SkillContextKind,
    "skill"
);
string_literal!(
    /// Constant `"thread"` context record kind.
    ThreadContextKind,
    "thread"
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: ImageContextKind,
    pub attachment_id: ContextAttachmentId,
    pub name: TrimmedNonEmptyString,
    pub mime_type: TrimmedNonEmptyString,
    pub size_bytes: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: FileContextKind,
    pub attachment_id: ContextAttachmentId,
    pub name: TrimmedNonEmptyString,
    pub mime_type: TrimmedNonEmptyString,
    pub size_bytes: NonNegativeInt,
}

/// Upstream attaches a `lineEnd >= lineStart` check to the terminal record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: TerminalContextKind,
    pub terminal_id: TrimmedNonEmptyString,
    pub terminal_label: TrimmedNonEmptyString,
    pub line_start: NonNegativeInt,
    pub line_end: NonNegativeInt,
    pub text: String,
}

impl<'de> Deserialize<'de> for TerminalContextRecord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            version: ComposerContextRecordVersion,
            context_id: ComposerContextId,
            label: String,
            kind: TerminalContextKind,
            terminal_id: TrimmedNonEmptyString,
            terminal_label: TrimmedNonEmptyString,
            line_start: NonNegativeInt,
            line_end: NonNegativeInt,
            text: String,
        }
        let raw = Raw::deserialize(d)?;
        if raw.line_end < raw.line_start {
            return Err(serde::de::Error::custom(
                "terminal context lineEnd must be >= lineStart",
            ));
        }
        Ok(Self {
            version: raw.version,
            context_id: raw.context_id,
            label: raw.label,
            kind: raw.kind,
            terminal_id: raw.terminal_id,
            terminal_label: raw.terminal_label,
            line_start: raw.line_start,
            line_end: raw.line_end,
            text: raw.text,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContextSource {
    pub function_name: Option<String>,
    pub file_name: Option<String>,
    pub line_number: Option<NonNegativeInt>,
    pub column_number: Option<NonNegativeInt>,
}

/// What a picked page element looks like to the agent; shared by element and
/// annotation records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContextDetails {
    pub page_url: String,
    pub page_title: Option<String>,
    pub tag_name: TrimmedNonEmptyString,
    pub selector: Option<String>,
    pub html_preview: String,
    pub component_name: Option<String>,
    pub source: Option<ElementContextSource>,
    pub styles: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: ElementContextKind,
    pub page_url: String,
    pub page_title: Option<String>,
    pub tag_name: TrimmedNonEmptyString,
    pub selector: Option<String>,
    pub html_preview: String,
    pub component_name: Option<String>,
    pub source: Option<ElementContextSource>,
    pub styles: String,
}

// Optional record fields are `optionalKey`: a record holding an explicit
// `undefined` cannot be sent as JSON, so it fails its own check and is
// dropped alone instead of failing the whole message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAnnotationContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: PreviewAnnotationContextKind,
    pub annotation_id: String,
    pub page_url: String,
    pub page_title: Option<String>,
    pub comment: String,
    pub target_summary: String,
    pub style_changes: Vec<String>,
    /// Picked elements inside the annotation, with the detail the agent
    /// needs to find them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<ElementContextDetails>>,
    /// Original target ids and edits allow pasted annotations to retain
    /// exact style changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_ids: Option<Vec<String>>,
    /// Region and stroke geometry is lossy on purpose, but their counts feed
    /// the target summary, so a pasted annotation still says what it marked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_count: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_count: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_change_details: Option<Vec<PreviewAnnotationStyleChange>>,
    /// The screenshot travels as its own image record; this links the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_context_id: Option<ComposerContextId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAnnotationStyleChange {
    pub target_id: String,
    pub selector: Option<String>,
    pub property: String,
    pub previous_value: String,
    pub value: String,
}

/// Upstream attaches an `endIndex >= startIndex` check to the review record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: ReviewCommentContextKind,
    pub section_id: TrimmedNonEmptyString,
    pub section_title: String,
    pub file_path: TrimmedNonEmptyString,
    pub start_index: NonNegativeInt,
    pub end_index: NonNegativeInt,
    pub range_label: String,
    pub text: String,
    pub diff: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestContextMetadata>,
}

impl<'de> Deserialize<'de> for ReviewCommentContextRecord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            version: ComposerContextRecordVersion,
            context_id: ComposerContextId,
            label: String,
            kind: ReviewCommentContextKind,
            section_id: TrimmedNonEmptyString,
            section_title: String,
            file_path: TrimmedNonEmptyString,
            start_index: NonNegativeInt,
            end_index: NonNegativeInt,
            range_label: String,
            text: String,
            diff: String,
            #[serde(default)]
            fence_language: Option<String>,
            #[serde(default)]
            pull_request: Option<PullRequestContextMetadata>,
        }
        let raw = Raw::deserialize(d)?;
        if raw.end_index < raw.start_index {
            return Err(serde::de::Error::custom(
                "review context endIndex must be >= startIndex",
            ));
        }
        Ok(Self {
            version: raw.version,
            context_id: raw.context_id,
            label: raw.label,
            kind: raw.kind,
            section_id: raw.section_id,
            section_title: raw.section_title,
            file_path: raw.file_path,
            start_index: raw.start_index,
            end_index: raw.end_index,
            range_label: raw.range_label,
            text: raw.text,
            diff: raw.diff,
            fence_language: raw.fence_language,
            pull_request: raw.pull_request,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: MentionContextKind,
    pub path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: SkillContextKind,
    pub name: TrimmedNonEmptyString,
}

/// Another thread on the same server, attached so the agent can read its
/// history through `t3_thread_read`. Only identity travels; the title is a
/// display snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: ThreadContextKind,
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
}

/// Kind value of [`UnknownContextRecord`]: a well-formed open kind that is
/// none of the known kinds (upstream uses a negative-lookahead pattern).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownContextKind(ComposerContextKind);

impl UnknownContextKind {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let kind = ComposerContextKind::new(value)?;
        if COMPOSER_CONTEXT_KINDS.contains(&kind.as_str()) {
            return Err(ContractError {
                expected: "a context kind other than the known kinds",
                got: kind.as_str().to_owned(),
            });
        }
        Ok(Self(kind))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for UnknownContextKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for UnknownContextKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for UnknownContextKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Catch-all for kinds this build does not know. Known discriminators are
/// excluded so a malformed known record fails its own schema instead of
/// sliding through unchecked. Mirrors `ChatUnknownAttachment`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    pub label: String,
    pub kind: UnknownContextKind,
    #[serde(deserialize_with = "payload_field::deserialize")]
    pub payload: serde_json::Value,
}

/// Payload bound: only JSON values the wire can carry, at most 64k
/// serialized chars, so a payload this build cannot read fails this record
/// alone instead of the whole message.
mod payload_field {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<serde_json::Value, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match serde_json::to_string(&value) {
            Ok(encoded) if encoded.len() <= 64_000 => Ok(value),
            _ => Err(serde::de::Error::custom(
                "context payloads must serialize to at most 64,000 chars",
            )),
        }
    }
}

/// `KnownComposerContextRecord`: the nine typed records, untagged like
/// upstream's union (each member carries its own `kind` literal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KnownComposerContextRecord {
    Image(ImageContextRecord),
    File(FileContextRecord),
    Terminal(TerminalContextRecord),
    Element(ElementContextRecord),
    PreviewAnnotation(PreviewAnnotationContextRecord),
    ReviewComment(ReviewCommentContextRecord),
    Mention(MentionContextRecord),
    Skill(SkillContextRecord),
    Thread(ThreadContextRecord),
}

/// `ComposerContextRecord`: known records plus the open catch-all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComposerContextRecord {
    Known(KnownComposerContextRecord),
    Unknown(UnknownContextRecord),
}

pub const COMPOSER_CONTEXT_MAX_RECORDS: usize = 200;
const COMPOSER_CONTEXT_MAX_SERIALIZED_CHARS: usize = 16_000_000;

/// Serde codec for the `records` field shared by the message context and the
/// clipboard fragment: bounded raw input, then per-record tolerant decoding
/// (an undecodable record is dropped, not fatal), then the caller-specific
/// extra checks.
pub(crate) mod records_field {
    use super::*;

    pub fn serialize<S: Serializer>(
        records: &[ComposerContextRecord],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        records.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ComposerContextRecord>, D::Error> {
        let raw = Vec::<serde_json::Value>::deserialize(d)?;
        if raw.len() > COMPOSER_CONTEXT_MAX_RECORDS {
            return Err(serde::de::Error::custom(format!(
                "context records are limited to {COMPOSER_CONTEXT_MAX_RECORDS}"
            )));
        }
        let records: Vec<ComposerContextRecord> = raw
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();
        Ok(records)
    }
}

/// Structured context riding on a user message. Undecodable records are
/// dropped, not fatal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationMessageContext {
    pub version: ComposerContextRecordVersion,
    #[serde(with = "message_records")]
    pub records: Vec<ComposerContextRecord>,
}

mod message_records {
    use super::*;

    pub fn serialize<S: Serializer>(
        records: &[ComposerContextRecord],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        records_field::serialize(records, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ComposerContextRecord>, D::Error> {
        let records = records_field::deserialize(d)?;
        // The bound applies to the raw input, as in upstream: forward-
        // compatible decoding drops unknown records first, so a checked
        // output length never sees the flood.
        if serde_json::to_string(&records).map(|s| s.len()).unwrap_or(usize::MAX)
            > COMPOSER_CONTEXT_MAX_SERIALIZED_CHARS
        {
            return Err(serde::de::Error::custom(format!(
                "context records must not exceed {COMPOSER_CONTEXT_MAX_SERIALIZED_CHARS} serialized chars"
            )));
        }
        // Duplicate context ids would make chips alias each other.
        let mut seen = HashSet::new();
        for record in &records {
            let id = match record {
                ComposerContextRecord::Known(known) => match known {
                    KnownComposerContextRecord::Image(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::File(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::Terminal(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::Element(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::PreviewAnnotation(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::ReviewComment(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::Mention(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::Skill(r) => r.context_id.as_str(),
                    KnownComposerContextRecord::Thread(r) => r.context_id.as_str(),
                },
                ComposerContextRecord::Unknown(r) => r.context_id.as_str(),
            };
            if !seen.insert(id) {
                return Err(serde::de::Error::custom(
                    "context records must have unique contextIds",
                ));
            }
        }
        Ok(records)
    }
}

/// `ComposerContextId` accessor shared by all records (helper for callers).
impl ComposerContextRecord {
    pub fn context_id(&self) -> &ComposerContextId {
        match self {
            Self::Known(known) => match known {
                KnownComposerContextRecord::Image(r) => &r.context_id,
                KnownComposerContextRecord::File(r) => &r.context_id,
                KnownComposerContextRecord::Terminal(r) => &r.context_id,
                KnownComposerContextRecord::Element(r) => &r.context_id,
                KnownComposerContextRecord::PreviewAnnotation(r) => &r.context_id,
                KnownComposerContextRecord::ReviewComment(r) => &r.context_id,
                KnownComposerContextRecord::Mention(r) => &r.context_id,
                KnownComposerContextRecord::Skill(r) => &r.context_id,
                KnownComposerContextRecord::Thread(r) => &r.context_id,
            },
            Self::Unknown(r) => &r.context_id,
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Self::Known(known) => match known {
                KnownComposerContextRecord::Image(_) => "image",
                KnownComposerContextRecord::File(_) => "file",
                KnownComposerContextRecord::Terminal(_) => "terminal",
                KnownComposerContextRecord::Element(_) => "element",
                KnownComposerContextRecord::PreviewAnnotation(_) => "preview-annotation",
                KnownComposerContextRecord::ReviewComment(_) => "review-comment",
                KnownComposerContextRecord::Mention(_) => "mention",
                KnownComposerContextRecord::Skill(_) => "skill",
                KnownComposerContextRecord::Thread(_) => "thread",
            },
            Self::Unknown(r) => r.kind.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_records_decode_by_kind() {
        let image: ComposerContextRecord = serde_json::from_value(serde_json::json!({
            "version": 1,
            "contextId": "ctx-1",
            "label": "shot",
            "kind": "image",
            "attachmentId": "att-1",
            "name": "shot.png",
            "mimeType": "image/png",
            "sizeBytes": 12,
        }))
        .unwrap();
        assert_eq!(image.kind(), "image");
        assert_eq!(image.context_id().as_str(), "ctx-1");

        let mention: ComposerContextRecord = serde_json::from_value(serde_json::json!({
            "version": 1,
            "contextId": "ctx-2",
            "label": "file",
            "kind": "mention",
            "path": "src/main.rs",
        }))
        .unwrap();
        assert_eq!(mention.kind(), "mention");
    }

    #[test]
    fn unknown_kinds_keep_their_payload() {
        let unknown: ComposerContextRecord = serde_json::from_value(serde_json::json!({
            "version": 1,
            "contextId": "ctx-3",
            "label": "later",
            "kind": "hologram",
            "payload": { "n": 6 },
        }))
        .unwrap();
        match unknown {
            ComposerContextRecord::Unknown(record) => {
                assert_eq!(record.kind.as_str(), "hologram");
                assert_eq!(record.payload, serde_json::json!({"n": 6}));
            }
            other => panic!("expected unknown record, got {other:?}"),
        }
    }

    #[test]
    fn terminal_range_is_validated() {
        assert!(serde_json::from_value::<ComposerContextRecord>(serde_json::json!({
            "version": 1,
            "contextId": "ctx-4",
            "label": "term",
            "kind": "terminal",
            "terminalId": "t1",
            "terminalLabel": "bash",
            "lineStart": 10,
            "lineEnd": 2,
            "text": "output",
        }))
        .is_err());
    }

    #[test]
    fn message_context_drops_undecodable_and_rejects_duplicate_ids() {
        let context: OrchestrationMessageContext = serde_json::from_value(serde_json::json!({
            "version": 1,
            "records": [
                { "version": 1, "contextId": "a", "label": "l", "kind": "mention", "path": "x" },
                { "version": 1, "contextId": "b", "label": "l", "kind": "hologram", "payload": 1 },
                { "version": 1, "label": "l", "kind": "mention", "path": "y" }
            ]
        }))
        .unwrap();
        assert_eq!(context.records.len(), 2);

        assert!(serde_json::from_value::<OrchestrationMessageContext>(serde_json::json!({
            "version": 1,
            "records": [
                { "version": 1, "contextId": "a", "label": "l", "kind": "mention", "path": "x" },
                { "version": 1, "contextId": "a", "label": "l", "kind": "mention", "path": "y" }
            ]
        }))
        .is_err());
    }
}
