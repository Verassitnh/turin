//! Port of upstream/packages/contracts/src/chatAttachment.ts

use crate::base_schemas::{
    ContractError, IsoDateTime, MessageId, NonNegativeInt, PositiveInt, ThreadId,
    TrimmedNonEmptyString, TrimmedString, js_len, js_trim,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const PROVIDER_SEND_TURN_MAX_INPUT_CHARS: usize = 120_000;
pub const PROVIDER_SEND_TURN_MAX_ATTACHMENTS: usize = 100;
pub const PROVIDER_SEND_TURN_MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES: &[&str] =
    &["image/gif", "image/jpeg", "image/png", "image/webp"];
const PROVIDER_SEND_TURN_MAX_IMAGE_DATA_URL_CHARS: usize = 14_000_000;
const CHAT_ATTACHMENT_ID_MAX_CHARS: usize = 128;

/// Whether a pasted or picked image mime type can be sent on a provider turn.
pub fn is_provider_send_turn_supported_image_mime_type(mime_type: &str) -> bool {
    PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type.to_lowercase().as_str())
}

/// `ChatAttachmentId`: trimmed non-empty, `^[a-zA-Z0-9_-]+$`, at most 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChatAttachmentId(String);

impl ChatAttachmentId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        let valid = !trimmed.is_empty()
            && js_len(trimmed) <= CHAT_ATTACHMENT_ID_MAX_CHARS
            && trimmed
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid {
            return Err(ContractError {
                expected: "an attachment id matching ^[a-zA-Z0-9_-]+$ (<= 128 chars)",
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

impl fmt::Display for ChatAttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ChatAttachmentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for ChatAttachmentId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ChatAttachmentId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::new(raw.as_ref()).map_err(serde::de::Error::custom)
    }
}

pub const SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS: usize = 32_000;
pub const SNAP_SHOT_ACCESSIBILITY_MAX_NODES: usize = 10_000;
pub const SNAP_SHOT_ACCESSIBILITY_MAX_SERIALIZED_CHARS: usize = 32_000;

/// `TrimmedNonEmptyString.check(isMaxLength(n))` field decoders (UTF-16 length,
/// like JS). `required` for plain fields, `optional` for `Schema.optional*`.
macro_rules! trimmed_max {
    ($name:ident, $max:expr) => {
        mod $name {
            use super::*;

            fn check(value: TrimmedNonEmptyString) -> Result<TrimmedNonEmptyString, String> {
                if js_len(value.as_str()) > $max {
                    Err(format!("expected at most {} characters", $max))
                } else {
                    Ok(value)
                }
            }

            #[allow(dead_code)]
            pub fn required<'de, D: Deserializer<'de>>(
                d: D,
            ) -> Result<TrimmedNonEmptyString, D::Error> {
                check(TrimmedNonEmptyString::deserialize(d)?).map_err(serde::de::Error::custom)
            }

            #[allow(dead_code)]
            pub fn optional<'de, D: Deserializer<'de>>(
                d: D,
            ) -> Result<Option<TrimmedNonEmptyString>, D::Error> {
                Option::<TrimmedNonEmptyString>::deserialize(d)?
                    .map(check)
                    .transpose()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

trimmed_max!(max_50, 50);
trimmed_max!(max_100, 100);
trimmed_max!(max_255, 255);
trimmed_max!(max_1000, 1_000);
trimmed_max!(max_2000, 2_000);
trimmed_max!(max_8000, 8_000);
trimmed_max!(max_32000, SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS);

/// `Schema.isMaxLength(n)` on a plain array (checked on the raw input).
fn max_items<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
    max: usize,
) -> Result<Vec<T>, D::Error> {
    let values = Vec::<T>::deserialize(d)?;
    if values.len() > max {
        Err(serde::de::Error::custom(format!(
            "expected at most {max} items"
        )))
    } else {
        Ok(values)
    }
}

fn actions_field<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<TrimmedNonEmptyString>>, D::Error> {
    #[derive(Deserialize)]
    struct Action(#[serde(deserialize_with = "max_100::required")] TrimmedNonEmptyString);
    Option::<Vec<Action>>::deserialize(d)?
        .map(|actions| {
            if actions.len() > 32 {
                Err(serde::de::Error::custom("expected at most 32 items"))
            } else {
                Ok(actions.into_iter().map(|action| action.0).collect())
            }
        })
        .transpose()
}

fn children_field<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<SnapShotAccessibilityNode>, D::Error> {
    max_items(d, SNAP_SHOT_ACCESSIBILITY_MAX_NODES)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotAccessibilityBounds {
    pub x: NonNegativeInt,
    pub y: NonNegativeInt,
    pub width: PositiveInt,
    pub height: PositiveInt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotAccessibilityState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<SnapShotAccessibilityChecked>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapShotAccessibilityChecked {
    #[serde(rename = "on")]
    On,
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "mixed")]
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotAccessibilityNode {
    #[serde(deserialize_with = "max_100::required")]
    pub role: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_1000::optional"
    )]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_8000::optional"
    )]
    pub value: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_2000::optional"
    )]
    pub description: Option<TrimmedNonEmptyString>,
    pub bounds: Option<SnapShotAccessibilityBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SnapShotAccessibilityState>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "actions_field"
    )]
    pub actions: Option<Vec<TrimmedNonEmptyString>>,
    #[serde(deserialize_with = "children_field")]
    pub children: Vec<SnapShotAccessibilityNode>,
}

string_literal!(
    /// Constant `"captured-image"` coordinate space of an element tree.
    CapturedImageCoordinateSpace,
    "captured-image"
);

/// Wire members of `SnapShotAccessibility` (union on `format`), plus the
/// checks upstream attaches to the whole union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "format",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SnapShotAccessibilityWire {
    #[serde(rename = "flat-text")]
    FlatText {
        #[serde(deserialize_with = "max_32000::required")]
        text: TrimmedNonEmptyString,
        truncated: bool,
    },
    #[serde(rename = "element-tree")]
    ElementTree {
        coordinate_space: CapturedImageCoordinateSpace,
        image_size: SnapShotImageSize,
        truncated: bool,
        root: SnapShotAccessibilityNode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotImageSize {
    pub width: PositiveInt,
    pub height: PositiveInt,
}

impl SnapShotAccessibilityWire {
    /// Upstream's whole-union filter: element trees must stay within the node
    /// and serialized-size budgets.
    fn validate(&self) -> Result<(), String> {
        if let Self::ElementTree { root, .. } = self {
            let mut nodes = 0;
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                nodes += 1;
                if nodes > SNAP_SHOT_ACCESSIBILITY_MAX_NODES {
                    return Err(format!(
                        "Accessibility trees must not exceed {SNAP_SHOT_ACCESSIBILITY_MAX_NODES} nodes."
                    ));
                }
                stack.extend(node.children.iter());
            }
            if serde_json::to_string(self)
                .map(|s| js_len(&s))
                .unwrap_or(usize::MAX)
                > SNAP_SHOT_ACCESSIBILITY_MAX_SERIALIZED_CHARS
            {
                return Err(format!(
                    "Accessibility trees must not exceed {SNAP_SHOT_ACCESSIBILITY_MAX_SERIALIZED_CHARS} serialized characters."
                ));
            }
        }
        Ok(())
    }
}

/// `SnapShotAccessibility`: the wire union plus the tree budget checks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SnapShotAccessibility(pub SnapShotAccessibilityWire);

impl<'de> Deserialize<'de> for SnapShotAccessibility {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = SnapShotAccessibilityWire::deserialize(d)?;
        wire.validate().map_err(serde::de::Error::custom)?;
        Ok(Self(wire))
    }
}

impl SnapShotAccessibility {
    pub fn as_wire(&self) -> &SnapShotAccessibilityWire {
        &self.0
    }
}

string_literal!(
    /// Constant `"snap-shot"` kind of a screenshot attachment source.
    SnapShotKind,
    "snap-shot"
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapShotSource {
    pub kind: SnapShotKind,
    pub captured_at: IsoDateTime,
    #[serde(deserialize_with = "max_255::required")]
    pub app_name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::window_title")]
    pub window_title: TrimmedString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_32000::optional"
    )]
    pub accessible_text: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<SnapShotAccessibility>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "max_255::optional"
    )]
    pub app_identifier: Option<TrimmedNonEmptyString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "field_checks::app_icon_data_url"
    )]
    pub app_icon_data_url: Option<TrimmedNonEmptyString>,
}

/// Field validators shared by the attachment structs (upstream inline checks).
mod field_checks {
    use super::*;

    pub fn window_title<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedString, D::Error> {
        let value = TrimmedString::deserialize(d)?;
        if js_len(value.as_str()) > 1_000 {
            Err(serde::de::Error::custom(
                "window titles can be at most 1,000 characters",
            ))
        } else {
            Ok(value)
        }
    }

    pub fn app_icon_data_url<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<TrimmedNonEmptyString>, D::Error> {
        let Some(value) = Option::<TrimmedNonEmptyString>::deserialize(d)? else {
            return Ok(None);
        };
        let has_png_prefix = value
            .as_str()
            .get(.."data:image/png;base64,".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/png;base64,"));
        if js_len(value.as_str()) > 100_000 || !has_png_prefix {
            Err(serde::de::Error::custom(
                "expected a PNG data url of at most 100,000 characters",
            ))
        } else {
            Ok(Some(value))
        }
    }

    pub fn image_size_bytes<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let value = u64::deserialize(d)?;
        if value > PROVIDER_SEND_TURN_MAX_IMAGE_BYTES {
            Err(serde::de::Error::custom(format!(
                "image attachments can be at most {PROVIDER_SEND_TURN_MAX_IMAGE_BYTES} bytes"
            )))
        } else {
            Ok(value)
        }
    }

    pub fn file_size_bytes<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let value = u64::deserialize(d)?;
        if !(1..=PROVIDER_SEND_TURN_MAX_FILE_BYTES).contains(&value) {
            Err(serde::de::Error::custom(format!(
                "file attachments must be between 1 and {PROVIDER_SEND_TURN_MAX_FILE_BYTES} bytes"
            )))
        } else {
            Ok(value)
        }
    }

    pub fn image_mime_type<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<TrimmedNonEmptyString, D::Error> {
        let value = TrimmedNonEmptyString::deserialize(d)?;
        if js_len(value.as_str()) > 100 || !value.as_str().to_lowercase().starts_with("image/") {
            Err(serde::de::Error::custom(
                "image attachment mime types must start with \"image/\" and be at most 100 characters",
            ))
        } else {
            Ok(value)
        }
    }
}

string_literal!(
    /// Constant `"image"` type tag of an image attachment.
    ImageType,
    "image"
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatImageAttachment {
    #[serde(rename = "type")]
    pub type_: ImageType,
    pub id: ChatAttachmentId,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::image_mime_type")]
    pub mime_type: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::image_size_bytes")]
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SnapShotSource>,
}

/// `PastedTextAttachmentSource` (TaggedStruct "pasted-text" with no fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedTextAttachmentSource;

impl Serialize for PastedTextAttachmentSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(1))?;
        map.serialize_entry("_tag", "pasted-text")?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for PastedTextAttachmentSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        match raw.get("_tag") {
            Some(tag) if tag == "pasted-text" => Ok(Self),
            _ => Err(serde::de::Error::custom("expected _tag \"pasted-text\"")),
        }
    }
}

string_literal!(
    /// Constant `"file"` type tag of a file attachment.
    FileType,
    "file"
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileAttachment {
    #[serde(rename = "type")]
    pub type_: FileType,
    pub id: ChatAttachmentId,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_100::required")]
    pub mime_type: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::file_size_bytes")]
    pub size_bytes: u64,
    /// Clipboard text folded by a client. Providers keep these path-only so the
    /// agent can inspect the file selectively instead of eagerly spending the
    /// same context the fold is intended to preserve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PastedTextAttachmentSource>,
}

/// Value of an unknown attachment's `type`: any trimmed non-empty string of
/// at most 50 chars that is not exactly `image` or `file` (upstream uses a
/// negative-lookahead pattern), so a malformed known attachment fails its own
/// schema instead of sliding through the open one unchecked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAttachmentType(TrimmedNonEmptyString);

impl UnknownAttachmentType {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let value = TrimmedNonEmptyString::new(value)?;
        if js_len(value.as_str()) > 50 {
            return Err(ContractError {
                expected: "an attachment type of at most 50 chars",
                got: value.as_str().to_owned(),
            });
        }
        if matches!(value.as_str(), "image" | "file") {
            return Err(ContractError {
                expected: "an attachment type other than \"image\" or \"file\"",
                got: value.as_str().to_owned(),
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Serialize for UnknownAttachmentType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for UnknownAttachmentType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(<std::borrow::Cow<'de, str>>::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Catch-all for attachment types this build does not know. Attachments ride
/// on persisted events and thread streams, so a newer server or client must
/// be able to introduce a type without making older readers fail to decode
/// the whole message. Consumers skip these or render them as unsupported.
/// Mirrors how `OrchestrationThreadActivity` keeps `kind` open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUnknownAttachment {
    #[serde(rename = "type")]
    pub type_: UnknownAttachmentType,
    pub id: ChatAttachmentId,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "max_100::required")]
    pub mime_type: TrimmedNonEmptyString,
    pub size_bytes: NonNegativeInt,
}

/// Field validators for the upload attachment.
mod upload_field_checks {
    use super::*;

    pub fn data_url<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedNonEmptyString, D::Error> {
        let value = TrimmedNonEmptyString::deserialize(d)?;
        if js_len(value.as_str()) > PROVIDER_SEND_TURN_MAX_IMAGE_DATA_URL_CHARS {
            Err(serde::de::Error::custom(
                "image data urls can be at most 14,000,000 chars",
            ))
        } else {
            Ok(value)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadChatImageAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ChatAttachmentId>,
    #[serde(rename = "type")]
    pub type_: ImageType,
    #[serde(deserialize_with = "max_255::required")]
    pub name: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::image_mime_type")]
    pub mime_type: TrimmedNonEmptyString,
    #[serde(deserialize_with = "field_checks::image_size_bytes")]
    pub size_bytes: u64,
    #[serde(deserialize_with = "upload_field_checks::data_url")]
    pub data_url: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SnapShotSource>,
}

/// `ChatAttachment`: image, file, or a type this build does not know. The
/// untagged order matches upstream's union order: the unknown member's `type`
/// excludes `image`/`file`, so a malformed known attachment fails its own
/// schema instead of falling through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatAttachment {
    Image(ChatImageAttachment),
    File(ChatFileAttachment),
    Unknown(ChatUnknownAttachment),
}

/// Minimal fields of an attachment used by the limit check.
struct AttachmentLike<'a> {
    type_is_image: bool,
    mime_type: &'a str,
    size_bytes: u64,
}

impl<'a> AttachmentLike<'a> {
    fn of(attachment: &'a ChatAttachment) -> Self {
        match attachment {
            ChatAttachment::Image(image) => Self {
                type_is_image: true,
                mime_type: image.mime_type.as_str(),
                size_bytes: image.size_bytes,
            },
            ChatAttachment::File(file) => Self {
                type_is_image: false,
                mime_type: file.mime_type.as_str(),
                size_bytes: file.size_bytes,
            },
            ChatAttachment::Unknown(unknown) => Self {
                type_is_image: false,
                mime_type: unknown.mime_type.as_str(),
                size_bytes: unknown.size_bytes,
            },
        }
    }
}

/// Upstream `getProviderAttachmentLimitError`, generalized over the
/// attachment kinds (all of which expose type, mimeType, and sizeBytes).
pub fn get_provider_attachment_limit_error(attachments: &[ChatAttachment]) -> Option<String> {
    check_attachment_limits(
        attachments
            .iter()
            .map(AttachmentLike::of)
            .collect::<Vec<_>>()
            .as_slice(),
    )
}

fn check_attachment_limits(attachments: &[AttachmentLike<'_>]) -> Option<String> {
    if attachments.len() > PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
        return Some(format!(
            "You can attach up to {PROVIDER_SEND_TURN_MAX_ATTACHMENTS} files per message or question response."
        ));
    }
    let image_bytes = attachments.iter().fold(0u64, |total, attachment| {
        total
            + if attachment.type_is_image
                || is_provider_send_turn_supported_image_mime_type(attachment.mime_type)
            {
                attachment.size_bytes
            } else {
                0
            }
    });
    if image_bytes > PROVIDER_SEND_TURN_MAX_TOTAL_IMAGE_BYTES {
        return Some(
            "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
                .to_owned(),
        );
    }
    None
}

/// `UploadChatAttachment`: currently only the image member.
pub type UploadChatAttachment = UploadChatImageAttachment;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistChatAttachmentsInput {
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub attachments: Vec<UploadChatAttachment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistChatAttachmentsResult {
    pub attachments: Vec<ChatAttachment>,
}

/// `PersistChatAttachmentsError` (Schema.TaggedError): `{"_tag": …}` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag")]
pub enum PersistChatAttachmentsError {
    #[serde(rename = "PersistChatAttachmentsError")]
    #[error("{message}")]
    PersistChatAttachmentsError {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerates_attachment_types_from_newer_builds() {
        // Attachments ride on persisted events and thread streams with no
        // client version negotiation. A type this build does not know must
        // decode instead of failing the whole message.
        let attachment: ChatAttachment = serde_json::from_value(serde_json::json!({
            "type": "somethingnew",
            "id": "thread-1-00000000-0000-4000-8000-000000000003-glb",
            "name": "scene.glb",
            "mimeType": "model/gltf-binary",
            "sizeBytes": 12,
        }))
        .unwrap();
        assert!(
            matches!(attachment, ChatAttachment::Unknown(ref unknown) if unknown.type_.as_str() == "somethingnew")
        );
    }

    #[test]
    fn rejects_malformed_known_attachment_types() {
        // The tolerant member must not catch malformed known attachments: a
        // file over the size cap or an image with a bad mime has to fail its
        // own schema, not slide through the open one with those constraints
        // unchecked.
        let base = serde_json::json!({
            "id": "thread-1-00000000-0000-4000-8000-000000000003-pdf",
            "name": "report.pdf",
            "mimeType": "application/pdf",
        });
        let mut oversized_file = base.clone();
        oversized_file["type"] = serde_json::json!("file");
        oversized_file["sizeBytes"] = serde_json::json!(PROVIDER_SEND_TURN_MAX_FILE_BYTES + 1);
        assert!(serde_json::from_value::<ChatAttachment>(oversized_file).is_err());

        let mut bad_mime_image = base;
        bad_mime_image["type"] = serde_json::json!("image");
        bad_mime_image["sizeBytes"] = serde_json::json!(12);
        assert!(serde_json::from_value::<ChatAttachment>(bad_mime_image).is_err());
    }

    #[test]
    fn rejects_accessibility_trees_above_the_serialized_limit() {
        let children: Vec<SnapShotAccessibilityNode> = (0..10)
            .map(|_| SnapShotAccessibilityNode {
                role: TrimmedNonEmptyString::new("text").unwrap(),
                name: None,
                value: Some(TrimmedNonEmptyString::new(&"x".repeat(8_000)).unwrap()),
                description: None,
                bounds: None,
                state: None,
                actions: None,
                children: Vec::new(),
            })
            .collect();
        let root = SnapShotAccessibilityNode {
            role: TrimmedNonEmptyString::new("window").unwrap(),
            name: None,
            value: None,
            description: None,
            bounds: Some(SnapShotAccessibilityBounds {
                x: 0,
                y: 0,
                width: PositiveInt::new(800).unwrap(),
                height: PositiveInt::new(600).unwrap(),
            }),
            state: None,
            actions: None,
            children,
        };
        let wire = SnapShotAccessibilityWire::ElementTree {
            coordinate_space: CapturedImageCoordinateSpace,
            image_size: SnapShotImageSize {
                width: PositiveInt::new(800).unwrap(),
                height: PositiveInt::new(600).unwrap(),
            },
            truncated: false,
            root,
        };
        assert!(wire.validate().is_err());
        assert!(
            serde_json::from_value::<SnapShotAccessibility>(serde_json::to_value(&wire).unwrap())
                .is_err()
        );
    }

    #[test]
    fn image_mime_type_check_accepts_raster_and_rejects_svg() {
        assert!(is_provider_send_turn_supported_image_mime_type("image/png"));
        assert!(is_provider_send_turn_supported_image_mime_type(
            "IMAGE/JPEG"
        ));
        assert!(!is_provider_send_turn_supported_image_mime_type(
            "image/svg+xml"
        ));
    }

    #[test]
    fn attachment_limit_error() {
        let attachment = ChatAttachment::Image(ChatImageAttachment {
            type_: ImageType,
            id: ChatAttachmentId::new("a").unwrap(),
            name: TrimmedNonEmptyString::new("a.png").unwrap(),
            mime_type: TrimmedNonEmptyString::new("image/png").unwrap(),
            size_bytes: 81 * 1024 * 1024,
            source: None,
        });
        assert_eq!(
            get_provider_attachment_limit_error(&[attachment]),
            Some("Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once.".to_owned())
        );
        let small = ChatAttachment::File(ChatFileAttachment {
            type_: FileType,
            id: ChatAttachmentId::new("b").unwrap(),
            name: TrimmedNonEmptyString::new("b.txt").unwrap(),
            mime_type: TrimmedNonEmptyString::new("text/plain").unwrap(),
            size_bytes: 10,
            source: None,
        });
        assert_eq!(get_provider_attachment_limit_error(&[small]), None);
    }

    fn image(extra: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "type": "image",
            "id": "img-1",
            "name": "a.png",
            "mimeType": "image/png",
            "sizeBytes": 12,
        });
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        base
    }

    #[test]
    fn enforces_string_length_checks_in_utf16_units() {
        assert!(serde_json::from_value::<ChatAttachment>(image(serde_json::json!({}))).is_ok());
        // 128 is the id cap; 129 fails and can never fall through to Unknown
        // because `type` is "image".
        let long_id = "a".repeat(129);
        assert!(
            serde_json::from_value::<ChatAttachment>(image(serde_json::json!({ "id": long_id })))
                .is_err()
        );
        // 128 astral characters are 256 UTF-16 units but also fail the id pattern.
        assert!(
            serde_json::from_value::<ChatAttachment>(image(
                serde_json::json!({ "id": "a".repeat(128) })
            ))
            .is_ok()
        );
        let name_255 = "😀".repeat(127);
        assert!(
            serde_json::from_value::<ChatAttachment>(image(
                serde_json::json!({ "name": name_255 })
            ))
            .is_ok()
        );
        let name_256 = "😀".repeat(128);
        assert!(
            serde_json::from_value::<ChatAttachment>(image(
                serde_json::json!({ "name": name_256 })
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChatAttachment>(image(
                serde_json::json!({ "mimeType": format!("image/{}", "x".repeat(95)) })
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ChatAttachment>(image(
                serde_json::json!({ "sizeBytes": PROVIDER_SEND_TURN_MAX_IMAGE_BYTES + 1 })
            ))
            .is_err()
        );
    }

    #[test]
    fn file_size_must_be_at_least_one_byte() {
        let file = |size: u64| {
            serde_json::json!({
                "type": "file", "id": "f", "name": "n", "mimeType": "text/plain", "sizeBytes": size,
            })
        };
        assert!(serde_json::from_value::<ChatAttachment>(file(0)).is_err());
        assert!(serde_json::from_value::<ChatAttachment>(file(1)).is_ok());
    }

    #[test]
    fn unknown_type_excludes_known_discriminators_and_caps_length() {
        let unknown = |ty: &str| {
            serde_json::json!({
                "type": ty, "id": "u", "name": "n", "mimeType": "x/y", "sizeBytes": 1,
            })
        };
        assert!(serde_json::from_value::<ChatAttachment>(unknown("images")).is_ok());
        // `image` with a non-image mime and `file` with zero bytes fail their
        // own schema and must not slide into the open member.
        assert!(serde_json::from_value::<ChatAttachment>(unknown("image")).is_err());
        let mut empty_file = unknown("file");
        empty_file["sizeBytes"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ChatAttachment>(empty_file).is_err());
        assert!(serde_json::from_value::<ChatAttachment>(unknown(&"t".repeat(51))).is_err());
        assert!(serde_json::from_value::<ChatAttachment>(unknown(&"t".repeat(50))).is_ok());
    }

    #[test]
    fn pasted_text_source_round_trips_with_tag() {
        let file: ChatAttachment = serde_json::from_value(serde_json::json!({
            "type": "file", "id": "f", "name": "n", "mimeType": "text/plain", "sizeBytes": 3,
            "source": { "_tag": "pasted-text" },
        }))
        .unwrap();
        let ChatAttachment::File(ref inner) = file else {
            panic!("expected a file")
        };
        assert_eq!(inner.source, Some(PastedTextAttachmentSource));
        assert_eq!(
            serde_json::to_value(&file).unwrap()["source"],
            serde_json::json!({ "_tag": "pasted-text" })
        );
    }

    #[test]
    fn snap_shot_source_checks() {
        let source = |extra: serde_json::Value| {
            let mut base = serde_json::json!({
                "kind": "snap-shot",
                "capturedAt": "2026-01-01T00:00:00.000Z",
                "appName": "App",
                "windowTitle": "  Title  ",
            });
            for (key, value) in extra.as_object().unwrap() {
                base[key] = value.clone();
            }
            base
        };
        let decoded: SnapShotSource =
            serde_json::from_value(source(serde_json::json!({}))).unwrap();
        assert_eq!(decoded.window_title.as_str(), "Title");
        // windowTitle may be empty (TrimmedString, not non-empty).
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "windowTitle": "" })
            ))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "windowTitle": "w".repeat(1_001) })
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "appName": "a".repeat(256) })
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "appIconDataUrl": "data:image/png;base64,AAAA" })
            ))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "appIconDataUrl": "DATA:IMAGE/PNG;BASE64,AAAA" })
            ))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "appIconDataUrl": "data:image/jpeg;base64,AAAA" })
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<SnapShotSource>(source(
                serde_json::json!({ "kind": "other" })
            ))
            .is_err()
        );
    }

    #[test]
    fn accessibility_bounds_require_positive_dimensions() {
        let tree = |width: u64| {
            serde_json::json!({
                "format": "element-tree",
                "coordinateSpace": "captured-image",
                "imageSize": { "width": width, "height": 600 },
                "truncated": false,
                "root": { "role": "window", "bounds": null, "children": [] },
            })
        };
        assert!(serde_json::from_value::<SnapShotAccessibility>(tree(800)).is_ok());
        assert!(serde_json::from_value::<SnapShotAccessibility>(tree(0)).is_err());
        let flat = serde_json::json!({ "format": "flat-text", "text": "hi", "truncated": true });
        let decoded: SnapShotAccessibility = serde_json::from_value(flat.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), flat);
    }
}
