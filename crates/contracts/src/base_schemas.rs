//! Port of upstream/packages/contracts/src/baseSchemas.ts
//!
//! Effect Schema codecs become serde types. Branded IDs are newtypes that
//! enforce the same "trimmed, non-empty" rule on deserialize.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

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
        Self(value.as_ref().trim().to_owned())
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
        s.serialize_str(self.0.trim())
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
        let trimmed = value.as_ref().trim();
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
/// `PositiveInt` (validated where upstream validates; plain integer otherwise)
pub type PositiveInt = u64;
/// `PortSchema`
pub type Port = u16;

/// `IsoDateTime`: an ISO-8601 string. Kept as a string at the contract layer
/// so persisted values round-trip byte for byte.
pub type IsoDateTime = String;

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
                let trimmed = value.as_ref().trim();
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
                Self(::std::sync::Arc::from(::uuid::Uuid::new_v4().to_string()))
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
        values.serialize(s)
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

/// `ForwardCompatibleOptional` / `ForwardCompatibleNullable` / `OmittedWhenNull`:
/// a value this build does not understand decodes as absent. Use with
/// `#[serde(default, deserialize_with = "lenient_option::deserialize")]`.
pub mod lenient_option {
    use super::*;

    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: serde::de::DeserializeOwned,
    {
        let raw = Option::<serde_json::Value>::deserialize(d)?;
        Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
    }
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
        #[derive(Deserialize)]
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
    }
}
