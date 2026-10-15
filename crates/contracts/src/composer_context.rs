//! Port of upstream/packages/contracts/src/composerContext.ts
//!
//! Inline context records: the typed payload behind every composer chip. A
//! message's `text` carries position through canonical reference links
//! (`[label](t3-context://v1/<kind>/<contextId>)`); these records carry the
//! payload, keyed by `contextId`. Bytes never live here: image and file
//! records bind to a `ChatAttachment` by id.

use crate::base_schemas::{
    ContractError, EnvironmentId, NonNegativeInt, PositiveInt, ThreadId, TrimmedNonEmptyString,
    js_len, js_trim,
};
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

crate::max_len!(max_64, 64);
crate::max_len!(max_100, 100);
crate::max_len!(max_200, 200);
crate::max_len!(max_255, 255);
crate::max_len!(max_2048, 2_048);
crate::max_len!(max_8000, 8_000);
crate::max_len!(max_16000, 16_000);
crate::max_len!(max_32000, 32_000);
crate::max_len!(max_64000, 64_000);
crate::max_len_vec!(short_strings_200, 2_048, 200);
crate::max_len_vec!(short_strings_50, 2_048, 50);
crate::max_items!(items_50, 50);
crate::max_items!(items_200, 200);

const CONTEXT_ID_MAX_CHARS: usize = 128;
pub const COMPOSER_CONTEXT_LABEL_MAX_CHARS: usize = 200;
pub const COMPOSER_CONTEXT_TERMINAL_TEXT_MAX_CHARS: usize = 64_000;
// The 8_000 element/preview bounds are applied through `max_8000` above.
/// Exported so producers can clamp to the same boundary the schema enforces,
/// rather than minting a record the send path would fail to encode.
pub const COMPOSER_CONTEXT_REVIEW_TEXT_MAX_CHARS: usize = 16_000;
pub const COMPOSER_CONTEXT_REVIEW_DIFF_MAX_CHARS: usize = 32_000;

fn is_context_id(value: &str) -> bool {
    !value.is_empty()
        && js_len(value) <= CONTEXT_ID_MAX_CHARS
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
                let trimmed = js_trim(value.as_ref());
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
        let trimmed = js_trim(value.as_ref());
        let valid = !trimmed.is_empty()
            && trimmed.len() <= 40
            && trimmed
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase())
            && trimmed
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
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
    pub number: PositiveInt,
    #[serde(deserialize_with = "max_2048::required")]
    pub title: String,
    #[serde(deserialize_with = "max_2048::required")]
    pub url: String,
    #[serde(deserialize_with = "max_2048::required")]
    pub head_branch: String,
    #[serde(deserialize_with = "max_2048::required")]
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
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: ImageContextKind,
    pub attachment_id: ContextAttachmentId,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_100::required")]
    pub mime_type: TrimmedNonEmptyString,
    pub size_bytes: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: FileContextKind,
    pub attachment_id: ContextAttachmentId,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_100::required")]
    pub mime_type: TrimmedNonEmptyString,
    pub size_bytes: NonNegativeInt,
}

/// Upstream attaches a `lineEnd >= lineStart` check to the terminal record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: TerminalContextKind,
    #[serde(deserialize_with = "max_255::required")]
    pub terminal_id: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_255::required")]
    pub terminal_label: TrimmedNonEmptyString,
    pub line_start: NonNegativeInt,
    pub line_end: NonNegativeInt,
    #[serde(deserialize_with = "max_64000::required")]
    pub text: String,
}

impl<'de> Deserialize<'de> for TerminalContextRecord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            version: ComposerContextRecordVersion,
            context_id: ComposerContextId,
            #[serde(deserialize_with = "max_200::required")]
            label: String,
            kind: TerminalContextKind,
            #[serde(deserialize_with = "max_255::required")]
            terminal_id: TrimmedNonEmptyString,
            #[serde(deserialize_with = "max_255::required")]
            terminal_label: TrimmedNonEmptyString,
            line_start: NonNegativeInt,
            line_end: NonNegativeInt,
            #[serde(deserialize_with = "max_64000::required")]
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
    #[serde(deserialize_with = "max_2048::optional")]
    pub function_name: Option<String>,
    #[serde(deserialize_with = "max_2048::optional")]
    pub file_name: Option<String>,
    pub line_number: Option<NonNegativeInt>,
    pub column_number: Option<NonNegativeInt>,
}

/// What a picked page element looks like to the agent; shared by element and
/// annotation records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContextDetails {
    #[serde(deserialize_with = "max_2048::required")]
    pub page_url: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub page_title: Option<String>,
    #[serde(deserialize_with = "max_255::required")]
    pub tag_name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_2048::optional")]
    pub selector: Option<String>,
    #[serde(deserialize_with = "max_8000::required")]
    pub html_preview: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub component_name: Option<String>,
    pub source: Option<ElementContextSource>,
    #[serde(deserialize_with = "max_8000::required")]
    pub styles: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: ElementContextKind,
    #[serde(deserialize_with = "max_2048::required")]
    pub page_url: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub page_title: Option<String>,
    #[serde(deserialize_with = "max_255::required")]
    pub tag_name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_2048::optional")]
    pub selector: Option<String>,
    #[serde(deserialize_with = "max_8000::required")]
    pub html_preview: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub component_name: Option<String>,
    pub source: Option<ElementContextSource>,
    #[serde(deserialize_with = "max_8000::required")]
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
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: PreviewAnnotationContextKind,
    #[serde(deserialize_with = "max_2048::required")]
    pub annotation_id: String,
    #[serde(deserialize_with = "max_2048::required")]
    pub page_url: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub page_title: Option<String>,
    #[serde(deserialize_with = "max_8000::required")]
    pub comment: String,
    #[serde(deserialize_with = "max_2048::required")]
    pub target_summary: String,
    #[serde(deserialize_with = "short_strings_200::required")]
    pub style_changes: Vec<String>,
    /// Picked elements inside the annotation, with the detail the agent
    /// needs to find them.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "items_50::optional"
    )]
    pub elements: Option<Vec<ElementContextDetails>>,
    /// Original target ids and edits allow pasted annotations to retain
    /// exact style changes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "short_strings_50::optional"
    )]
    pub element_ids: Option<Vec<String>>,
    /// Region and stroke geometry is lossy on purpose, but their counts feed
    /// the target summary, so a pasted annotation still says what it marked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_count: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_count: Option<NonNegativeInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "items_200::optional"
    )]
    pub style_change_details: Option<Vec<PreviewAnnotationStyleChange>>,
    /// The screenshot travels as its own image record; this links the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_context_id: Option<ComposerContextId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewAnnotationStyleChange {
    #[serde(deserialize_with = "max_2048::required")]
    pub target_id: String,
    #[serde(deserialize_with = "max_2048::optional")]
    pub selector: Option<String>,
    #[serde(deserialize_with = "max_2048::required")]
    pub property: String,
    #[serde(deserialize_with = "max_8000::required")]
    pub previous_value: String,
    #[serde(deserialize_with = "max_8000::required")]
    pub value: String,
}

/// Upstream attaches an `endIndex >= startIndex` check to the review record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCommentContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: ReviewCommentContextKind,
    #[serde(deserialize_with = "max_255::required")]
    pub section_id: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_2048::required")]
    pub section_title: String,
    #[serde(deserialize_with = "max_2048::required")]
    pub file_path: TrimmedNonEmptyString,
    pub start_index: NonNegativeInt,
    pub end_index: NonNegativeInt,
    #[serde(deserialize_with = "max_2048::required")]
    pub range_label: String,
    #[serde(deserialize_with = "max_16000::required")]
    pub text: String,
    #[serde(deserialize_with = "max_32000::required")]
    pub diff: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_64::optional"
    )]
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
            #[serde(deserialize_with = "max_200::required")]
            label: String,
            kind: ReviewCommentContextKind,
            #[serde(deserialize_with = "max_255::required")]
            section_id: TrimmedNonEmptyString,
            #[serde(deserialize_with = "max_2048::required")]
            section_title: String,
            #[serde(deserialize_with = "max_2048::required")]
            file_path: TrimmedNonEmptyString,
            start_index: NonNegativeInt,
            end_index: NonNegativeInt,
            #[serde(deserialize_with = "max_2048::required")]
            range_label: String,
            #[serde(deserialize_with = "max_16000::required")]
            text: String,
            #[serde(deserialize_with = "max_32000::required")]
            diff: String,
            #[serde(default, deserialize_with = "max_64::optional")]
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
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: MentionContextKind,
    #[serde(deserialize_with = "max_2048::required")]
    pub path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillContextRecord {
    pub version: ComposerContextRecordVersion,
    pub context_id: ComposerContextId,
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: SkillContextKind,
    #[serde(deserialize_with = "max_255::required")]
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
    #[serde(deserialize_with = "max_200::required")]
    pub label: String,
    pub kind: ThreadContextKind,
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    #[serde(deserialize_with = "max_200::required")]
    pub title: String,
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
    #[serde(deserialize_with = "max_200::required")]
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

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<serde_json::Value, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match serde_json::to_string(&value) {
            Ok(encoded) if js_len(&encoded) <= 64_000 => Ok(value),
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
        check_count(&raw)?;
        Ok(decode(raw))
    }

    /// The bound applies to the raw input, as in upstream: forward-compatible
    /// decoding drops unknown records first, so a checked output length never
    /// sees the flood.
    pub fn check_count<E: serde::de::Error>(raw: &[serde_json::Value]) -> Result<(), E> {
        if raw.len() > COMPOSER_CONTEXT_MAX_RECORDS {
            return Err(E::custom(format!(
                "context records are limited to {COMPOSER_CONTEXT_MAX_RECORDS}"
            )));
        }
        Ok(())
    }

    /// `ForwardCompatibleArray(ComposerContextRecord)`: undecodable records
    /// are dropped, not fatal.
    pub fn decode(raw: Vec<serde_json::Value>) -> Vec<ComposerContextRecord> {
        raw.into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect()
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
        let raw = Vec::<serde_json::Value>::deserialize(d)?;
        records_field::check_count(&raw)?;
        // `JSON.stringify(records).length` of the raw input (UTF-16 units).
        if serde_json::to_string(&raw)
            .map(|encoded| js_len(&encoded))
            .unwrap_or(usize::MAX)
            > COMPOSER_CONTEXT_MAX_SERIALIZED_CHARS
        {
            return Err(serde::de::Error::custom(format!(
                "context records must not exceed {COMPOSER_CONTEXT_MAX_SERIALIZED_CHARS} serialized chars"
            )));
        }
        let records = records_field::decode(raw);
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
        assert!(
            serde_json::from_value::<ComposerContextRecord>(serde_json::json!({
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
            .is_err()
        );
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

    use serde_json::{Value, json};

    fn base() -> Value {
        json!({ "version": 1, "contextId": "ctx_1" })
    }

    fn with(mut record: Value, extra: Value) -> Value {
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        record
    }

    fn known_records() -> Vec<(&'static str, Value)> {
        let b = base();
        vec![
            (
                "image",
                with(
                    b.clone(),
                    json!({
                        "kind": "image", "label": "checkout-error.png", "attachmentId": "att_1",
                        "name": "checkout-error.png", "mimeType": "image/png", "sizeBytes": 1234,
                    }),
                ),
            ),
            (
                "file",
                with(
                    b.clone(),
                    json!({
                        "kind": "file", "label": "notes.txt", "attachmentId": "att_2",
                        "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 12,
                    }),
                ),
            ),
            (
                "terminal",
                with(
                    b.clone(),
                    json!({
                        "kind": "terminal", "label": "Terminal 1 lines 509-514", "terminalId": "term-1",
                        "terminalLabel": "Terminal 1", "lineStart": 509, "lineEnd": 514,
                        "text": "error: boom\n  at main.ts:1",
                    }),
                ),
            ),
            (
                "element",
                with(
                    b.clone(),
                    json!({
                        "kind": "element", "label": "<Button>", "pageUrl": "http://localhost:3000/checkout",
                        "pageTitle": "Checkout", "tagName": "button", "selector": "#pay",
                        "htmlPreview": "<button id=\"pay\">Pay</button>", "componentName": "Button",
                        "source": { "functionName": "Button", "fileName": "Button.tsx", "lineNumber": 12, "columnNumber": 3 },
                        "styles": "color: red;",
                    }),
                ),
            ),
            (
                "preview-annotation",
                with(
                    b.clone(),
                    json!({
                        "kind": "preview-annotation", "label": "Checkout", "annotationId": "ann_1",
                        "pageUrl": "http://localhost:3000/checkout", "pageTitle": "Checkout",
                        "comment": "Make this bigger", "targetSummary": "1 selected element",
                        "styleChanges": ["font-size: 20px"],
                        "elements": [{
                            "pageUrl": "http://localhost:3000/checkout", "pageTitle": "Checkout",
                            "tagName": "button", "selector": "#pay", "htmlPreview": "<button>Pay</button>",
                            "componentName": "Button", "source": null, "styles": "",
                        }],
                        "screenshotContextId": "ctx_2",
                    }),
                ),
            ),
            (
                "review-comment",
                with(
                    b.clone(),
                    json!({
                        "kind": "review-comment", "label": "ChatComposer.tsx L4118", "sectionId": "diff-1",
                        "sectionTitle": "Changes", "filePath": "apps/web/src/ChatComposer.tsx",
                        "startIndex": 4118, "endIndex": 4118, "rangeLabel": "L4118",
                        "text": "Why is this here?", "diff": "+ const x = 1;", "fenceLanguage": "diff",
                        "pullRequest": {
                            "number": 42, "title": "Improve context chips",
                            "url": "https://github.com/pingdotgg/t3code/pull/42",
                            "headBranch": "feat/context-chips", "baseBranch": "main",
                            "state": "open", "isDraft": false,
                        },
                    }),
                ),
            ),
            (
                "mention",
                with(
                    b.clone(),
                    json!({ "kind": "mention", "label": "@src/index.ts", "path": "src/index.ts" }),
                ),
            ),
            (
                "skill",
                with(
                    b.clone(),
                    json!({ "kind": "skill", "label": "$pinchtab", "name": "pinchtab" }),
                ),
            ),
            (
                "thread",
                with(
                    b,
                    json!({
                        "kind": "thread", "label": "Fix login flow", "environmentId": "environment-1",
                        "threadId": "thread-1", "title": "Fix login flow",
                    }),
                ),
            ),
        ]
    }

    fn record(kind: &str) -> Value {
        known_records()
            .into_iter()
            .find(|(k, _)| *k == kind)
            .unwrap()
            .1
    }

    #[test]
    fn known_kinds_match_the_record_table() {
        let kinds: Vec<&str> = known_records().iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, COMPOSER_CONTEXT_KINDS);
    }

    #[test]
    fn every_known_record_round_trips() {
        for (kind, value) in known_records() {
            let decoded: ComposerContextRecord = serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("{kind}: {error}"));
            assert!(matches!(decoded, ComposerContextRecord::Known(_)), "{kind}");
            assert_eq!(decoded.kind(), kind);
            assert_eq!(serde_json::to_value(&decoded).unwrap(), value, "{kind}");
        }
    }

    #[test]
    fn unknown_kind_round_trips_with_payload() {
        let value = with(
            base(),
            json!({
                "kind": "future-thing", "label": "Future", "payload": { "anything": [1, 2, 3] },
            }),
        );
        let decoded: ComposerContextRecord = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(decoded, ComposerContextRecord::Unknown(_)));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    }

    #[test]
    fn malformed_known_kind_does_not_slide_through_as_unknown() {
        let value = with(base(), json!({ "kind": "image", "label": "x" }));
        assert!(serde_json::from_value::<ComposerContextRecord>(value).is_err());
    }

    #[test]
    fn unknown_payload_is_bounded_by_serialized_length() {
        let unknown = with(base(), json!({ "kind": "future-thing", "label": "Future" }));
        let decode = |payload: Value| {
            serde_json::from_value::<ComposerContextRecord>(with(
                unknown.clone(),
                json!({ "payload": payload }),
            ))
        };
        assert!(decode(json!("x".repeat(63_998))).is_ok());
        assert!(decode(json!("x".repeat(63_999))).is_err());
        // Each quote escapes to two characters.
        assert!(decode(json!("\"".repeat(32_000))).is_err());
        let context: OrchestrationMessageContext = serde_json::from_value(json!({
            "version": 1,
            "records": [
                with(unknown, json!({ "payload": "x".repeat(64_000) })),
                with(record("skill"), json!({ "contextId": "ctx_skill" })),
            ],
        }))
        .unwrap();
        assert_eq!(context.records.len(), 1);
        assert_eq!(context.records[0].kind(), "skill");
    }

    #[test]
    fn rejects_bad_ids_versions_and_kinds() {
        let skill = record("skill");
        let decode = |value: Value| serde_json::from_value::<ComposerContextRecord>(value).is_err();
        assert!(decode(with(
            skill.clone(),
            json!({ "contextId": "has space" })
        )));
        assert!(decode(with(skill.clone(), json!({ "version": 2 }))));
        assert!(decode(with(skill, json!({ "kind": "Bad Kind" }))));
        // 129 chars is over the id cap.
        assert!(decode(with(
            record("skill"),
            json!({ "contextId": "a".repeat(129) })
        )));
    }

    #[test]
    fn string_bounds_use_utf16_length() {
        let decode = |value: Value| serde_json::from_value::<ComposerContextRecord>(value).is_ok();
        assert!(decode(with(
            record("skill"),
            json!({ "label": "x".repeat(200) })
        )));
        assert!(!decode(with(
            record("skill"),
            json!({ "label": "x".repeat(201) })
        )));
        // 100 astral characters are 200 UTF-16 units; 101 are 202.
        assert!(decode(with(
            record("skill"),
            json!({ "label": "😀".repeat(100) })
        )));
        assert!(!decode(with(
            record("skill"),
            json!({ "label": "😀".repeat(101) })
        )));
        assert!(!decode(with(
            record("review-comment"),
            json!({ "text": "x".repeat(16_001) })
        )));
        assert!(!decode(with(
            record("review-comment"),
            json!({ "diff": "x".repeat(32_001) })
        )));
        assert!(!decode(with(
            record("review-comment"),
            json!({ "fenceLanguage": "x".repeat(65) })
        )));
        assert!(!decode(with(
            record("terminal"),
            json!({ "text": "x".repeat(64_001) })
        )));
        assert!(!decode(with(
            record("element"),
            json!({ "htmlPreview": "x".repeat(8_001) })
        )));
        assert!(!decode(with(
            record("element"),
            json!({ "pageUrl": "x".repeat(2_049) })
        )));
        assert!(!decode(with(
            record("preview-annotation"),
            json!({
                "styleChanges": vec!["a"; 201],
            })
        )));
        assert!(!decode(with(
            record("preview-annotation"),
            json!({
                "elementIds": vec!["a"; 51],
            })
        )));
    }

    #[test]
    fn range_checks_and_pull_request_number() {
        let decode = |value: Value| serde_json::from_value::<ComposerContextRecord>(value).is_ok();
        assert!(!decode(with(
            record("review-comment"),
            json!({ "startIndex": 5, "endIndex": 4 })
        )));
        let mut pr = record("review-comment");
        pr["pullRequest"]["number"] = json!(0);
        assert!(!decode(pr));
        let mut pr = record("review-comment");
        pr["pullRequest"]["state"] = json!("draft");
        assert!(!decode(pr));
    }

    #[test]
    fn rejects_aggregate_context_size_even_when_each_record_is_valid() {
        let details: Vec<Value> = (0..200)
            .map(|_| {
                json!({
                    "targetId": "target", "selector": null, "property": "content",
                    "previousValue": "x".repeat(8_000), "value": "y".repeat(8_000),
                })
            })
            .collect();
        let big = with(
            record("preview-annotation"),
            json!({ "styleChangeDetails": details }),
        );
        assert!(serde_json::from_value::<ComposerContextRecord>(big.clone()).is_ok());
        assert!(
            serde_json::from_value::<OrchestrationMessageContext>(
                json!({ "version": 1, "records": [big.clone()] })
            )
            .is_ok()
        );
        let six: Vec<Value> = (0..6)
            .map(|index| with(big.clone(), json!({ "contextId": format!("ctx_{index}") })))
            .collect();
        assert!(
            serde_json::from_value::<OrchestrationMessageContext>(
                json!({ "version": 1, "records": six })
            )
            .is_err()
        );
    }

    #[test]
    fn normalizes_decoded_identifiers() {
        let context: OrchestrationMessageContext = serde_json::from_value(json!({
            "version": 1,
            "records": [with(record("skill"), json!({ "contextId": "  ctx_1  ", "name": "  review  " }))],
        }))
        .unwrap();
        let ComposerContextRecord::Known(KnownComposerContextRecord::Skill(skill)) =
            &context.records[0]
        else {
            panic!("expected skill record")
        };
        assert_eq!(skill.context_id.as_str(), "ctx_1");
        assert_eq!(skill.name.as_str(), "review");
    }

    #[test]
    fn rejects_oversized_arrays_before_dropping_malformed_records() {
        let records: Vec<Value> = (0..201).map(|_| json!({})).collect();
        assert!(
            serde_json::from_value::<OrchestrationMessageContext>(
                json!({ "version": 1, "records": records })
            )
            .is_err()
        );
        let records: Vec<Value> = (0..200).map(|_| json!({})).collect();
        let context: OrchestrationMessageContext =
            serde_json::from_value(json!({ "version": 1, "records": records })).unwrap();
        assert!(context.records.is_empty());
    }

    #[test]
    fn drops_undecodable_records_and_keeps_valid_siblings() {
        let context: OrchestrationMessageContext = serde_json::from_value(json!({
            "version": 1,
            "records": [
                record("skill"),
                { "version": 1, "kind": "image" },
                with(record("terminal"), json!({ "contextId": "ctx_2" })),
            ],
        }))
        .unwrap();
        let kinds: Vec<&str> = context.records.iter().map(|record| record.kind()).collect();
        assert_eq!(kinds, ["skill", "terminal"]);
    }

    #[test]
    fn rejects_duplicate_normalized_context_identities() {
        assert!(serde_json::from_value::<OrchestrationMessageContext>(json!({
            "version": 1,
            "records": [record("skill"), with(record("terminal"), json!({ "contextId": " ctx_1 " }))],
        }))
        .is_err());
    }

    #[test]
    fn optional_record_fields_are_omitted_not_null_on_encode() {
        let decoded: ComposerContextRecord =
            serde_json::from_value(record("preview-annotation")).unwrap();
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert!(encoded.get("elementIds").is_none());
        assert!(encoded.get("regionCount").is_none());
        assert_eq!(encoded["elements"][0]["source"], Value::Null);
    }
}
