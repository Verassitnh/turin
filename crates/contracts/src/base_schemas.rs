//! Port of upstream/packages/contracts/src/baseSchemas.ts
//!
//! Effect Schema codecs become serde types. Branded IDs are newtypes that
//! enforce the same "trimmed, non-empty" rule on deserialize.
//! Cut-feature exports intentionally omitted: AuthSessionId, RpcClientId,
//! SecretRef, DpopFailureReason, ClientOs, ClientDeviceType,
//! ClientWebDeployment, and ClientConnectionMethod.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, num::NonZeroU64};

/// ECMAScript `String.prototype.trim` whitespace and line terminators.
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(|c: char| (c.is_whitespace() && c != '\u{0085}') || c == '\u{FEFF}')
}

#[doc(hidden)]
pub mod __private {
    pub use serde;
    pub use uuid;
}

/// Error for a value that fails a contract check.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{expected}: got {got:?}")]
pub struct ContractError {
    pub expected: &'static str,
    pub got: String,
}

/// `TrimmedString`: trims on both decode and encode.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrimmedString(String);

impl TrimmedString {
    pub fn new(value: impl AsRef<str>) -> Self {
        Self(js_trim(value.as_ref()).to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Serialize for TrimmedString {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(js_trim(&self.0))
    }
}

impl<'de> Deserialize<'de> for TrimmedString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(d)?))
    }
}

/// `TrimmedNonEmptyString`: non-blank once trimmed.
///
/// Upstream notes that `make`/encode see the untrimmed value, so a plain
/// non-empty check would accept `" "` and encode it to `""`. Here the value is
/// trimmed at construction, so there is no untrimmed state to get wrong.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrimmedNonEmptyString(String);

impl TrimmedNonEmptyString {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        if trimmed.is_empty() {
            return Err(ContractError {
                expected: "a non-blank string",
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

impl fmt::Display for TrimmedNonEmptyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for TrimmedNonEmptyString {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TrimmedNonEmptyString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `NonNegativeInt`
pub type NonNegativeInt = u64;
/// `PositiveInt`: an integer of at least one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PositiveInt(pub NonZeroU64);

impl PositiveInt {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// `IsoDateTime`: an ISO-8601 string. Kept as a string at the contract layer
/// so persisted values round-trip byte for byte.
pub type IsoDateTime = String;

/// `DateTimeUtc` / `DateTimeUtcFromString`: UTC milliseconds on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDateTime(pub DateTime<Utc>);

impl UtcDateTime {
    pub fn now() -> Self {
        Self(Utc::now())
    }
}

impl fmt::Display for UtcDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

impl Serialize for UtcDateTime {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for UtcDateTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&value)
            .map(|value| Self(value.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

/// Defines a branded entity id (`makeEntityId`): a trimmed, non-empty string.
#[macro_export]
macro_rules! id {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(::std::sync::Arc<str>);

        impl $name {
            /// Validates like upstream's branded schema: trimmed, non-empty.
            pub fn new(value: impl AsRef<str>) -> Result<Self, $crate::base_schemas::ContractError> {
                let trimmed = $crate::base_schemas::js_trim(value.as_ref());
                if trimmed.is_empty() {
                    return Err($crate::base_schemas::ContractError {
                        expected: concat!("a non-blank ", stringify!($name)),
                        got: value.as_ref().to_owned(),
                    });
                }
                Ok(Self(::std::sync::Arc::from(trimmed)))
            }
            /// Fresh random id (upstream mints ids with `crypto.randomUUID()`).
            pub fn random() -> Self {
                Self(::std::sync::Arc::from($crate::__private::uuid::Uuid::new_v4().to_string()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
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

        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S: $crate::__private::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct IdVisitor;
                impl<'de> $crate::__private::serde::de::Visitor<'de> for IdVisitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str(concat!("a non-blank ", stringify!($name)))
                    }
                    fn visit_str<E: $crate::__private::serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        $name::new(value).map_err(E::custom)
                    }
                }
                d.deserialize_str(IdVisitor)
            }
        }
    )+};
}

id!(
    ThreadId,
    ProjectId,
    EnvironmentId,
    CommandId,
    EventId,
    MessageId,
    TurnId,
    RunId,
    RunAttemptId,
    NodeId,
    ProviderItemId,
    ProviderSessionId,
    ProviderThreadId,
    ProviderTurnId,
    RuntimeSessionId,
    RuntimeItemId,
    TurnItemId,
    RuntimeRequestId,
    RuntimeTaskId,
    ScheduledTaskId,
    ApprovalRequestId,
    CheckpointRef,
    CheckpointId,
    CheckpointScopeId,
    ContextHandoffId,
    ContextTransferId,
    RawEventId,
    PlanId,
);

/// Which client surface a command comes from. Turin only ever sends
/// `Desktop`, but persisted upstream rows may carry the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientSurface {
    Web,
    Desktop,
    Mobile,
    Cli,
}

/// Value of a forward-compatible tagged union whose tag this build does not
/// know (`UnknownUnionMember`). Decode-only: serializing one fails, exactly
/// like upstream's forbidden encoder.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownUnionMember {
    pub tag: String,
    pub value: String,
    pub raw: serde_json::Value,
}

/// `ForwardCompatibleUnion`: known members decode strictly; an unknown tag
/// decodes to [`UnknownUnionMember`].
#[derive(Debug, Clone, PartialEq)]
pub enum ForwardCompatible<T> {
    Known(T),
    Unknown(UnknownUnionMember),
}

/// Implemented by tagged unions so [`ForwardCompatible`] knows the tag field
/// and the set of tags this build understands.
pub trait TaggedUnion: Sized {
    const TAG: &'static str;
    fn known_tags() -> &'static [&'static str];
}

/// Assert that samples cover exactly the serialized tags. Every `TaggedUnion`
/// implementation needs a test calling this with one sample per variant.
pub fn assert_known_tags<T: TaggedUnion + Serialize>(samples: &[T]) {
    use std::collections::BTreeSet;
    let actual: BTreeSet<String> = samples
        .iter()
        .map(|sample| {
            let value = serde_json::to_value(sample).expect("tagged sample must serialize");
            value
                .get(T::TAG)
                .and_then(serde_json::Value::as_str)
                .expect("tagged sample must have a string tag")
                .to_owned()
        })
        .collect();
    let known: BTreeSet<String> = T::known_tags().iter().map(|tag| (*tag).to_owned()).collect();
    assert_eq!(actual, known, "TaggedUnion::known_tags must match serialized tags");
}

impl<T: Serialize> Serialize for ForwardCompatible<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(value) => value.serialize(s),
            Self::Unknown(member) => Err(serde::ser::Error::custom(format!(
                "Unknown {} values are never sent.",
                member.tag
            ))),
        }
    }
}

impl<'de, T> Deserialize<'de> for ForwardCompatible<T>
where
    T: TaggedUnion + serde::de::DeserializeOwned,
{
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        match unknown_union_tag(&raw, T::TAG, T::known_tags()) {
            Some(value) => Ok(Self::Unknown(UnknownUnionMember {
                tag: T::TAG.to_owned(),
                value,
                raw,
            })),
            // A known tag (or no tag) must decode in full: broken known
            // payloads still fail loudly.
            None => serde_json::from_value(raw)
                .map(Self::Known)
                .map_err(serde::de::Error::custom),
        }
    }
}

impl<T> ForwardCompatible<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown(_) => None,
        }
    }
    pub fn into_known(self) -> Option<T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown(_) => None,
        }
    }
}

/// `hasUnknownUnionTag`: the unknown tag value if `value` carries a string
/// `tag` that is not in `known`.
pub fn unknown_union_tag(value: &serde_json::Value, tag: &str, known: &[&str]) -> Option<String> {
    let found = value.as_object()?.get(tag)?.as_str()?;
    (!known.contains(&found)).then(|| found.to_owned())
}

/// `hasUnknownUnionTag`: a present tag not in `known` is unknown, including
/// a non-string tag. Missing tags and non-object values return false.
pub fn has_unknown_union_tag(value: &serde_json::Value, tag: &str, known: &[&str]) -> bool {
    value
        .as_object()
        .and_then(|object| object.get(tag))
        .is_some_and(|found| !found.as_str().is_some_and(|s| known.contains(&s)))
}

/// `ForwardCompatibleUnionArray`: decodes an array, dropping members whose tag
/// this build does not know.
pub mod forward_compatible_union_array {
    use super::*;

    pub fn serialize<S, T>(values: &[T], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        values.serialize(s)
    }

    pub fn deserialize<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: TaggedUnion + serde::de::DeserializeOwned,
    {
        let values = Vec::<ForwardCompatible<T>>::deserialize(d)?;
        Ok(values
            .into_iter()
            .filter_map(ForwardCompatible::into_known)
            .collect())
    }
}

/// `ForwardCompatibleArray`: decodes an array, dropping any element that fails
/// to decode instead of failing the whole payload.
pub mod forward_compatible_array {
    use super::*;

    pub fn serialize<S, T>(values: &[T], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let valid: Vec<serde_json::Value> = values
            .iter()
            .filter_map(|value| serde_json::to_value(value).ok())
            .collect();
        valid.serialize(s)
    }

    pub fn deserialize<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: serde::de::DeserializeOwned,
    {
        let values = Vec::<serde_json::Value>::deserialize(d)?;
        Ok(values
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect())
    }
}

/// `Schema.optional(NullOr(X))` / `optionalKey(NullOr(X))`: absence, null, and
/// a value remain distinct. Use with `default, skip_serializing_if =
/// "Option::is_none", with = "double_option"` on `Option<Option<T>>`.
pub mod double_option {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> {
        value.as_ref().and_then(Option::as_ref).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(d)?))
    }
}

fn decode_lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(raw).ok())
}

/// Same idea for one optional value whose literal set grows over time: a
/// member this build does not know decodes as absent rather than failing the
/// enclosing struct. Encoding is the plain encoding. Use
/// `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional")]`.
pub mod forward_compatible_optional {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<T>, s: S) -> Result<S::Ok, S::Error> { value.serialize(s) }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> { decode_lenient(d) }
}

/// Optional nullable value: unknown values decode as absent, while explicit
/// null remains null. Use `#[serde(default, skip_serializing_if = "Option::is_none",
/// with = "forward_compatible_optional_nullable")]` on `Option<Option<T>>`.
pub mod forward_compatible_optional_nullable {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> { double_option::serialize(value, s) }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<Option<T>>, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        if raw.is_null() { Ok(Some(None)) } else { Ok(serde_json::from_value(raw).ok().map(Some)) }
    }
}

/// The nullable form, for a persisted setting whose literal set grows over
/// time: a member this build does not know (or a missing key) decodes as null
/// rather than failing the enclosing struct. Encoding is the plain encoding. Use
/// `#[serde(default, with = "forward_compatible_nullable")]` without skip.
pub mod forward_compatible_nullable {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<T>, s: S) -> Result<S::Ok, S::Error> { value.serialize(s) }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> { decode_lenient(d) }
}

/// A nullable setting whose null is "unset" and never crosses the wire: it
/// decodes from a missing or unknown key and encodes back to a missing key.
/// For a field that older clients decode as a required literal, so a null
/// on the wire would fail their whole settings snapshot. Use `#[serde(default, skip_serializing_if = "Option::is_none",
/// with = "omitted_when_null")]`.
pub mod omitted_when_null {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<T>, s: S) -> Result<S::Ok, S::Error> { value.serialize(s) }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> { decode_lenient(d) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn ids_trim_and_reject_blank() {
        assert_eq!(ThreadId::new("  abc ").unwrap().as_str(), "abc");
        assert!(ThreadId::new("   ").is_err());
        let parsed: ThreadId = serde_json::from_str("\" t1 \"").unwrap();
        assert_eq!(parsed.as_str(), "t1");
        assert!(serde_json::from_str::<ThreadId>("\"\"").is_err());
        assert!(ScheduledTaskId::new("\u{FEFF}").is_err());
    }

    #[test]
    fn js_whitespace_matches_upstream_cases() {
        for padding in [" ", "\t", "\n", "\r\n", "\u{000b}", "\u{000c}", "\u{00a0}", "\u{2028}", "\u{3000}", "\u{feff}"] {
            let padded = format!("{padding}a b{padding}");
            assert_eq!(js_trim(&padded), "a b");
            assert_eq!(TrimmedString::new(&padded).as_str(), "a b");
            assert_eq!(TrimmedNonEmptyString::new(&padded).unwrap().as_str(), "a b");
            assert_eq!(ThreadId::new(&padded).unwrap().as_str(), "a b");
            assert!(TrimmedNonEmptyString::new(padding).is_err());
            assert!(ThreadId::new(padding).is_err());
        }
        assert_eq!(js_trim("\u{0085}a\u{0085}"), "\u{0085}a\u{0085}");
        assert_eq!(ThreadId::new("a\u{0085}").unwrap().as_str(), "a\u{0085}");
    }

    #[test]
    fn all_kept_entity_ids_round_trip_trimmed_values() {
        macro_rules! check_id {
            ($($name:ident),+ $(,)?) => {$(
                let value = $name::new("\u{FEFF} id \u{3000}").unwrap();
                assert_eq!(value.as_str(), "id", stringify!($name));
                assert_eq!(serde_json::to_string(&value).unwrap(), "\"id\"", stringify!($name));
                assert_eq!(serde_json::from_str::<$name>("\" id \"").unwrap(), value, stringify!($name));
                assert!(serde_json::from_str::<$name>("\"\u{FEFF}\"").is_err(), stringify!($name));
            )+};
        }
        check_id!(ThreadId, ProjectId, EnvironmentId, CommandId, EventId,
            MessageId, TurnId, RunId, RunAttemptId, NodeId, ProviderItemId,
            ProviderSessionId, ProviderThreadId, ProviderTurnId, RuntimeSessionId,
            RuntimeItemId, TurnItemId, RuntimeRequestId, RuntimeTaskId,
            ScheduledTaskId, ApprovalRequestId, CheckpointRef, CheckpointId,
            CheckpointScopeId, ContextHandoffId, ContextTransferId, RawEventId,
            PlanId);
    }

    #[test]
    fn utc_datetime_normalizes_to_milliseconds() {
        let at: UtcDateTime = serde_json::from_str("\"2026-01-02T03:04:05.123456+02:00\"").unwrap();
        assert_eq!(serde_json::to_string(&at).unwrap(), "\"2026-01-02T01:04:05.123Z\"");
        let whole: UtcDateTime = serde_json::from_str("\"2026-01-02T01:04:05Z\"").unwrap();
        assert_eq!(whole.to_string(), "2026-01-02T01:04:05.000Z");
        assert!(at > whole);
        assert!(serde_json::from_str::<UtcDateTime>("\"yesterday\"").is_err());
    }

    #[test]
    fn positive_int_rejects_zero() {
        assert!(PositiveInt::new(0).is_none());
        assert!(serde_json::from_str::<PositiveInt>("0").is_err());
        assert_eq!(serde_json::from_str::<PositiveInt>("1").unwrap().get(), 1);
        assert_eq!(serde_json::to_string(&PositiveInt::new(42).unwrap()).unwrap(), "42");
    }

    #[test]
    fn trimmed_non_empty_never_encodes_blank() {
        assert!(TrimmedNonEmptyString::new(" ").is_err());
        let value = TrimmedNonEmptyString::new(" x ").unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), "\"x\"");
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum Shape {
        Circle { r: u32 },
        Square { s: u32 },
    }

    impl TaggedUnion for Shape {
        const TAG: &'static str = "type";
        fn known_tags() -> &'static [&'static str] {
            &["circle", "square"]
        }
    }

    #[test]
    fn forward_compatible_union() {
        assert_known_tags(&[Shape::Circle { r: 1 }, Shape::Square { s: 2 }]);
        let known: ForwardCompatible<Shape> =
            serde_json::from_str(r#"{"type":"circle","r":1}"#).unwrap();
        assert_eq!(known, ForwardCompatible::Known(Shape::Circle { r: 1 }));

        let unknown: ForwardCompatible<Shape> =
            serde_json::from_str(r#"{"type":"hexagon","n":6}"#).unwrap();
        assert!(matches!(unknown, ForwardCompatible::Unknown(ref m) if m.value == "hexagon"));
        assert!(serde_json::to_string(&unknown).is_err());

        // A known member with a broken payload still fails loudly.
        assert!(serde_json::from_str::<ForwardCompatible<Shape>>(r#"{"type":"circle"}"#).is_err());
    }

    #[test]
    fn forward_compatible_union_array_drops_unknown() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(with = "forward_compatible_union_array")]
            shapes: Vec<Shape>,
        }
        let holder: Holder = serde_json::from_str(
            r#"{"shapes":[{"type":"circle","r":1},{"type":"hexagon"},{"type":"square","s":2}]}"#,
        )
        .unwrap();
        assert_eq!(
            holder.shapes,
            vec![Shape::Circle { r: 1 }, Shape::Square { s: 2 }]
        );
        assert_eq!(serde_json::to_value(&holder).unwrap(), serde_json::json!({"shapes":[{"type":"circle","r":1},{"type":"square","s":2}]}));
        assert!(serde_json::from_str::<Holder>(r#"{"shapes":[{"type":"circle"}]}"#).is_err());
    }

    #[test]
    fn union_array_keeps_known_member_with_unusual_fields() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "kind")]
        enum Flagged {
            #[serde(rename = "flag")]
            Flag { _unknown: bool, tag: String, value: String },
        }
        impl TaggedUnion for Flagged {
            const TAG: &'static str = "kind";
            fn known_tags() -> &'static [&'static str] { &["flag"] }
        }
        assert_known_tags(&[Flagged::Flag { _unknown: true, tag: "kind".into(), value: "x".into() }]);
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(with = "forward_compatible_union_array")]
            values: Vec<Flagged>,
        }
        let raw = serde_json::json!({"values":[{"kind":"flag","_unknown":true,"tag":"kind","value":"x"}]});
        let holder: Holder = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&holder).unwrap(), raw);
    }

    #[test]
    fn has_unknown_union_tag_matches_js() {
        for value in [serde_json::json!({"type":"new"}), serde_json::json!({"type":5}), serde_json::json!({"type":null})] {
            assert!(has_unknown_union_tag(&value, "type", &["circle"]));
        }
        for value in [serde_json::json!({"type":"circle"}), serde_json::json!({}), serde_json::json!(null)] {
            assert!(!has_unknown_union_tag(&value, "type", &["circle"]));
        }
        assert!(serde_json::from_value::<ForwardCompatible<Shape>>(serde_json::json!({"type":5})).is_err());
    }

    #[test]
    fn optional_nullable_fields_round_trip_all_states() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Holder {
            #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
            plain: Option<Option<String>>,
            #[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional_nullable")]
            lenient: Option<Option<u32>>,
        }
        let absent: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(absent, Holder { plain: None, lenient: None });
        assert_eq!(serde_json::to_string(&absent).unwrap(), "{}");
        let null: Holder = serde_json::from_str(r#"{"plain":null,"lenient":null}"#).unwrap();
        assert_eq!(null, Holder { plain: Some(None), lenient: Some(None) });
        assert_eq!(serde_json::to_value(&null).unwrap(), serde_json::json!({"plain":null,"lenient":null}));
        let value: Holder = serde_json::from_str(r#"{"plain":"x","lenient":3}"#).unwrap();
        assert_eq!(value, Holder { plain: Some(Some("x".into())), lenient: Some(Some(3)) });
        assert_eq!(serde_json::to_value(&value).unwrap(), serde_json::json!({"plain":"x","lenient":3}));
        let unknown: Holder = serde_json::from_str(r#"{"lenient":"future"}"#).unwrap();
        assert_eq!(unknown.lenient, None);
        assert_eq!(serde_json::to_string(&unknown).unwrap(), "{}");
    }

    #[test]
    fn lenient_helpers_have_distinct_encode_rules() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Holder {
            #[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional")]
            optional: Option<u32>,
            #[serde(default, with = "forward_compatible_nullable")]
            nullable: Option<u32>,
            #[serde(default, skip_serializing_if = "Option::is_none", with = "omitted_when_null")]
            omitted: Option<u32>,
        }
        let absent: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(serde_json::to_value(&absent).unwrap(), serde_json::json!({"nullable":null}));
        let unknown: Holder = serde_json::from_str(r#"{"optional":"future","nullable":"future","omitted":"future"}"#).unwrap();
        assert_eq!(unknown, absent);
        let null: Holder = serde_json::from_str(r#"{"optional":null,"nullable":null,"omitted":null}"#).unwrap();
        assert_eq!(null, absent);
        let values: Holder = serde_json::from_str(r#"{"optional":1,"nullable":2,"omitted":3}"#).unwrap();
        assert_eq!(serde_json::to_value(&values).unwrap(), serde_json::json!({"optional":1,"nullable":2,"omitted":3}));
    }

    #[test]
    fn forward_compatible_array_drops_bad_decode_and_encode_elements() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Named { name: TrimmedNonEmptyString }
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(with = "forward_compatible_array")]
            values: Vec<Named>,
        }
        let decoded: Holder = serde_json::from_str(r#"{"values":[{"name":" a "},null,{"name":" "},{"name":"b"}]}"#).unwrap();
        assert_eq!(decoded.values.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);

        struct SometimesBad(&'static str);
        impl Serialize for SometimesBad {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                if self.0.is_empty() { Err(serde::ser::Error::custom("bad")) } else { s.serialize_str(self.0) }
            }
        }
        #[derive(Serialize)]
        struct Outbound {
            #[serde(with = "forward_compatible_array")]
            values: Vec<SometimesBad>,
        }
        let wire = serde_json::to_value(Outbound { values: vec![SometimesBad("a"), SometimesBad(""), SometimesBad("b")] }).unwrap();
        assert_eq!(wire, serde_json::json!({"values":["a","b"]}));
    }

    #[test]
    fn forward_compatible_array_applies_element_defaults() {
        #[derive(Serialize, Deserialize)]
        struct WithDefault { name: String, #[serde(default = "seven")] count: u32 }
        fn seven() -> u32 { 7 }
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(with = "forward_compatible_array")]
            values: Vec<WithDefault>,
        }
        let decoded: Holder = serde_json::from_str(r#"{"values":[{"name":"a"},{"name":5}]}"#).unwrap();
        assert_eq!(decoded.values.len(), 1);
        assert_eq!(decoded.values[0].count, 7);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), serde_json::json!({"values":[{"name":"a","count":7}]}));
    }
}
