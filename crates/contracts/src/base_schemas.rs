//! Port of upstream/packages/contracts/src/baseSchemas.ts
//!
//! Effect Schema codecs become serde types. Branded IDs are newtypes that
//! enforce the same "trimmed, non-empty" rule on deserialize.
//! Cut-feature exports intentionally omitted: AuthSessionId, RpcClientId,
//! SecretRef, DpopFailureReason, PortSchema, ClientOs, ClientDeviceType,
//! ClientWebDeployment, and ClientConnectionMethod. All belong to cut features.
//! `isUnknownUnionMember` has no port: match on [`ForwardCompatible::Unknown`].

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, num::NonZeroU64, str::FromStr};

/// ECMAScript WhiteSpace and LineTerminator code points, i.e. what
/// `String.prototype.trim` strips. Differs from Rust's `char::is_whitespace`:
/// U+FEFF is included and U+0085 is not.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trim` exactly. Use this, never `str::trim`, anywhere
/// upstream calls `.trim()`.
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// JS `string.length` (UTF-16 code units). `Schema.isMaxLength` and friends
/// count this, not bytes or `char`s.
pub fn js_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Largest integer a JS number holds exactly (`Number.MAX_SAFE_INTEGER`).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Writes a `Schema.Number` the way `JSON.stringify` does: a whole number has
/// no fractional part (`2`, not `2.0`), so re-saved JSON stays byte-identical
/// to what the TS server writes.
pub fn serialize_js_number<S: Serializer>(value: f64, s: S) -> Result<S::Ok, S::Error> {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER {
        s.serialize_i64(value as i64)
    } else {
        s.serialize_f64(value)
    }
}

/// `Schema.Number` field codec (`#[serde(with = "js_number")] f64`); decodes
/// any JSON number, encodes whole numbers without a fraction.
pub mod js_number {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &f64, s: S) -> Result<S::Ok, S::Error> {
        super::serialize_js_number(*value, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        f64::deserialize(d)
    }
}

/// `Option<f64>` counterpart of [`js_number`]: `NullOr(Schema.Number)` (encodes
/// `null`) or, with `skip_serializing_if`, `optional(Schema.Number)`.
pub mod js_number_nullable {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => super::serialize_js_number(*value, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Option::<f64>::deserialize(d)
    }
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

impl AsRef<str> for TrimmedString {
    fn as_ref(&self) -> &str {
        &self.0
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

impl AsRef<str> for TrimmedNonEmptyString {
    fn as_ref(&self) -> &str {
        &self.0
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
    /// `None` for zero.
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for PositiveInt {
    type Error = ContractError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value).ok_or_else(|| ContractError {
            expected: "an integer of at least 1",
            got: value.to_string(),
        })
    }
}

impl From<PositiveInt> for u64 {
    fn from(value: PositiveInt) -> Self {
        value.get()
    }
}

impl fmt::Display for PositiveInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `IsoDateTime`: an ISO-8601 string. Kept as a string at the contract layer
/// so persisted values round-trip byte for byte.
pub type IsoDateTime = String;

/// `DateTimeUtc` / `DateTimeUtcFromString`: always serializes as
/// `YYYY-MM-DDTHH:MM:SS.sssZ`. Upstream compares these strings as text
/// (`ORDER BY created_at`), so the format must never drift.
///
/// Parses the ISO forms upstream's `DateTime.makeUnsafe` (`new Date(s)`, with
/// `Z` appended when `hasZone` fails) accepts, with V8's rules:
/// `YYYY[-MM[-DD]]` or `±YYYYYY[-MM[-DD]]`, then optionally `T`/`t` +
/// `HH:mm[:ss[.f+]]`, then optionally `Z` or `±HH[:]mm` (date-only forms take
/// `Z` only). No zone means UTC. Days 1..=31 roll over (`02-30` is `03-01`),
/// `24:00[:00[.000]]` is the next midnight, `:60` and lowercase `z` fail, and
/// digits past milliseconds are truncated. Unlike upstream, the instant must
/// fall in years 0..=9999 so the fixed format always round-trips; V8's
/// non-ISO fallback forms (`Jan 2 2024`, `2024/01/02`, space separators) are
/// not accepted.
///
/// Precision is truncated to milliseconds (like a JS `Date`) by [`Self::now`],
/// [`Self::from_datetime`], parsing and deserializing, so equality agrees with
/// the wire form. Build values through those, not the public field, unless the
/// `DateTime` is already millisecond-aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDateTime(pub DateTime<Utc>);

fn utc_year_bounds() -> (DateTime<Utc>, DateTime<Utc>) {
    let min = chrono::NaiveDate::from_ymd_opt(0, 1, 1).expect("year 0 is valid");
    let max = chrono::NaiveDate::from_ymd_opt(9999, 12, 31).expect("year 9999 is valid");
    (
        min.and_hms_milli_opt(0, 0, 0, 0)
            .expect("valid time")
            .and_utc(),
        max.and_hms_milli_opt(23, 59, 59, 999)
            .expect("valid time")
            .and_utc(),
    )
}

impl UtcDateTime {
    pub fn now() -> Self {
        Self::from_datetime(Utc::now())
    }
    /// Truncates to milliseconds and clamps to years 0..=9999.
    pub fn from_datetime(value: DateTime<Utc>) -> Self {
        let (min, max) = utc_year_bounds();
        let value = value.clamp(min, max);
        let sub_milli = value.timestamp_subsec_nanos() % 1_000_000;
        Self(value - chrono::Duration::nanoseconds(i64::from(sub_milli)))
    }
}

/// Byte cursor for [`parse_js_iso_date`].
struct IsoCursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl IsoCursor<'_> {
    fn eat(&mut self, byte: u8) -> bool {
        let matched = self.bytes.get(self.at) == Some(&byte);
        self.at += usize::from(matched);
        matched
    }
    fn sign(&mut self) -> Option<i64> {
        if self.eat(b'+') {
            Some(1)
        } else if self.eat(b'-') {
            Some(-1)
        } else {
            None
        }
    }
    fn digits(&mut self, count: usize) -> Option<i64> {
        let run = self.bytes.get(self.at..self.at + count)?;
        if !run.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.at += count;
        Some(run.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    }
    /// Milliseconds from one or more fraction digits, truncated.
    fn fraction_millis(&mut self) -> Option<i64> {
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        let run = &self.bytes[start..self.at];
        if run.is_empty() {
            return None;
        }
        Some(
            run.iter()
                .chain([b'0'; 3].iter())
                .take(3)
                .fold(0, |n, d| n * 10 + i64::from(d - b'0')),
        )
    }
}

/// The instant `new Date(s)` yields for the ISO forms documented on
/// [`UtcDateTime`], before the year bound.
fn parse_js_iso_date(value: &str) -> Option<DateTime<Utc>> {
    let mut at = IsoCursor {
        bytes: value.as_bytes(),
        at: 0,
    };
    let year = match at.sign() {
        Some(sign) => {
            let year = at.digits(6)?;
            if sign < 0 && year == 0 {
                return None;
            }
            sign * year
        }
        None => at.digits(4)?,
    };
    let (mut month, mut day) = (1, 1);
    if at.eat(b'-') {
        month = at.digits(2)?;
        if at.eat(b'-') {
            day = at.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (mut hour, mut minute, mut second, mut millis) = (0, 0, 0, 0);
    let has_time = at.eat(b'T') || at.eat(b't');
    if has_time {
        hour = at.digits(2)?;
        if !at.eat(b':') {
            return None;
        }
        minute = at.digits(2)?;
        if at.eat(b':') {
            second = at.digits(2)?;
            if at.eat(b'.') {
                millis = at.fraction_millis()?;
            }
        }
    }
    if hour > 24 || minute > 59 || second > 59 || (hour == 24 && minute + second + millis > 0) {
        return None;
    }

    let mut offset_minutes = 0;
    if !at.eat(b'Z')
        && has_time
        && let Some(sign) = at.sign()
    {
        let hours = at.digits(2)?;
        at.eat(b':');
        let minutes = at.digits(2)?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        offset_minutes = sign * (hours * 60 + minutes);
    }
    if at.at != at.bytes.len() {
        return None;
    }

    let first_of_month =
        chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, u32::try_from(month).ok()?, 1)?;
    let midnight = first_of_month.and_hms_opt(0, 0, 0)?.and_utc();
    let offset = chrono::Duration::days(day - 1)
        + chrono::Duration::hours(hour)
        + chrono::Duration::minutes(minute - offset_minutes)
        + chrono::Duration::seconds(second)
        + chrono::Duration::milliseconds(millis);
    midnight.checked_add_signed(offset)
}

impl FromStr for UtcDateTime {
    type Err = ContractError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (min, max) = utc_year_bounds();
        parse_js_iso_date(value)
            .filter(|instant| (min..=max).contains(instant))
            .map(Self)
            .ok_or_else(|| ContractError {
                expected: "an ISO 8601 timestamp in years 0000-9999",
                got: value.to_owned(),
            })
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
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Defines branded entity ids (`makeEntityId`): trimmed, non-empty strings
/// held as `Arc<str>`. Works from any crate depending on `turin-contracts`
/// (serde and uuid are reached through `$crate::__private`). Never hand-write
/// an id newtype.
#[macro_export]
macro_rules! id {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(::std::sync::Arc<str>);

        impl $name {
            /// Validates like upstream's branded schema: trimmed, non-empty.
            pub fn new(value: impl AsRef<str>) -> ::std::result::Result<Self, $crate::base_schemas::ContractError> {
                let trimmed = $crate::base_schemas::js_trim(value.as_ref());
                if trimmed.is_empty() {
                    return ::std::result::Result::Err($crate::base_schemas::ContractError {
                        expected: concat!("a non-blank ", stringify!($name)),
                        got: value.as_ref().to_owned(),
                    });
                }
                ::std::result::Result::Ok(Self(::std::sync::Arc::from(trimmed)))
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
            fn serialize<S: $crate::__private::serde::Serializer>(&self, s: S) -> ::std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(d: D) -> ::std::result::Result<Self, D::Error> {
                struct IdVisitor;
                impl<'de> $crate::__private::serde::de::Visitor<'de> for IdVisitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str(concat!("a non-blank ", stringify!($name)))
                    }
                    fn visit_str<E: $crate::__private::serde::de::Error>(self, value: &str) -> ::std::result::Result<Self::Value, E> {
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

/// `ForwardCompatibleUnion`: wire codec for a server→client tagged union a
/// newer server may add members to. A value whose tag is one this build does
/// not know decodes to [`UnknownUnionMember`] instead of failing; a known
/// member decodes through its own schema, so a broken known payload still
/// fails. Unknown members are decode-only; encoding one fails.
#[derive(Debug, Clone, PartialEq)]
pub enum ForwardCompatible<T> {
    Known(T),
    Unknown(UnknownUnionMember),
}

/// Implemented by tagged unions so [`ForwardCompatible`] knows the tag field
/// and the set of tags this build understands. Every impl needs a test calling
/// [`assert_known_tags`]: a tag missing from `known_tags` makes that known
/// variant decode as unknown and be silently dropped from arrays.
pub trait TaggedUnion: Sized {
    const TAG: &'static str;
    fn known_tags() -> &'static [&'static str];
}

/// Panics unless the serialized `T::TAG` values of `samples` equal
/// `T::known_tags()` as a set. Every `TaggedUnion` implementation needs a test
/// calling this with one sample per variant (upstream derives the set from the
/// members and throws on mismatch; here it is a hand-written list).
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
    let known: BTreeSet<String> = T::known_tags()
        .iter()
        .map(|tag| (*tag).to_owned())
        .collect();
    assert_eq!(
        actual, known,
        "TaggedUnion::known_tags must match serialized tags"
    );
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

/// The unknown tag value if `value` carries a *string* `tag` that is not in
/// `known`. Backs the strict [`ForwardCompatible`] decode; for the upstream
/// envelope check use [`has_unknown_union_tag`].
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

/// `ForwardCompatibleUnionArray`: an array of a growing tagged union that drops
/// members this build does not know. Known members decode through their own
/// schema and still fail when broken. Encoding is the plain array encoding.
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

    /// `optional`/`optionalKey(ForwardCompatibleUnionArray(X))`: use on
    /// `Option<Vec<X>>` with `#[serde(default, skip_serializing_if =
    /// "Option::is_none", with = "forward_compatible_union_array::optional")]`.
    /// An absent key is `None`; `null` fails, as upstream.
    pub mod optional {
        use super::*;

        pub fn serialize<S, T>(values: &Option<Vec<T>>, s: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
            T: Serialize,
        {
            match values {
                Some(values) => super::serialize(values, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Vec<T>>, D::Error>
        where
            D: Deserializer<'de>,
            T: TaggedUnion + serde::de::DeserializeOwned,
        {
            super::deserialize(d).map(Some)
        }
    }
}

/// `ForwardCompatibleArray`: wire codec for a server→client array whose
/// element shape grows over time. Clients keep decoding payloads from servers
/// newer than themselves instead of failing the connection over data they
/// could not act on anyway.
///
/// Decoding runs each element through its own `Deserialize`, so trimming,
/// defaults and date normalization apply as usual. Only elements that fail it
/// are dropped, on either side (an element that fails to encode is omitted).
///
/// For a tagged union, prefer [`forward_compatible_union_array`]: it drops
/// only values whose tag this build does not know, so a known member with a
/// broken payload still fails loudly.
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

    /// `optional`/`optionalKey(ForwardCompatibleArray(X))`: use on
    /// `Option<Vec<X>>` with `#[serde(default, skip_serializing_if =
    /// "Option::is_none", with = "forward_compatible_array::optional")]`.
    /// An absent key is `None`; `null` fails, as upstream.
    pub mod optional {
        use super::*;

        pub fn serialize<S, T>(values: &Option<Vec<T>>, s: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
            T: Serialize,
        {
            match values {
                Some(values) => super::serialize(values, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Vec<T>>, D::Error>
        where
            D: Deserializer<'de>,
            T: serde::de::DeserializeOwned,
        {
            super::deserialize(d).map(Some)
        }
    }
}

/// `Schema.optional(NullOr(X))` / `optionalKey(NullOr(X))`: absence, null, and
/// a value stay distinct (absent = `None`, `null` = `Some(None)`, value =
/// `Some(Some(v))`; `Some(None)` encodes `null`). Use on `Option<Option<T>>`
/// with `#[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]`.
pub mod double_option {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().and_then(Option::as_ref).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(d)?))
    }
}

fn decode_lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
    d: D,
) -> Result<Option<T>, D::Error> {
    let raw = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(raw).ok())
}

/// `ForwardCompatibleOptional`: one optional value whose literal set grows
/// over time: a member this build does not know decodes as absent rather than
/// failing the enclosing struct. Encoding is the plain encoding. Use on
/// `Option<T>` with `#[serde(default, skip_serializing_if = "Option::is_none", with = "forward_compatible_optional")]`.
pub mod forward_compatible_optional {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        decode_lenient(d)
    }
}

/// `ForwardCompatibleOptional(NullOr(X))`: as [`forward_compatible_optional`],
/// but an explicit `null` stays `null` (`Some(None)`), absent and unknown are
/// `None`. Use on `Option<Option<T>>` with `#[serde(default,
/// skip_serializing_if = "Option::is_none", with = "forward_compatible_optional_nullable")]`.
pub mod forward_compatible_optional_nullable {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        double_option::serialize(value, s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        if raw.is_null() {
            Ok(Some(None))
        } else {
            Ok(serde_json::from_value(raw).ok().map(Some))
        }
    }
}

/// `ForwardCompatibleNullable`: the nullable form, for a persisted setting
/// whose literal set grows over time: a member this build does not know (or a
/// missing key) decodes as null rather than failing the enclosing struct.
/// Encoding is the plain encoding, so `None` writes `null`. Use on `Option<T>`
/// with `#[serde(default, with = "forward_compatible_nullable")]`, no skip.
pub mod forward_compatible_nullable {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        decode_lenient(d)
    }
}

/// `OmittedWhenNull`: a nullable setting whose null is "unset" and never
/// crosses the wire: it decodes from a missing or unknown key and encodes back
/// to a missing key. For a field that older clients decode as a required
/// literal, so a null on the wire would fail their whole settings snapshot.
/// Use on `Option<T>` with `#[serde(default, skip_serializing_if =
/// "Option::is_none", with = "omitted_when_null")]`.
pub mod omitted_when_null {
    use super::*;
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        decode_lenient(d)
    }
}

/// Wire form of upstream `Schema.Option(X)`: `Some` encodes as
/// `{"_tag":"Some","value":…}`, `None` as `{"_tag":"None"}`.
///
/// T01-local: upstream `baseSchemas.ts` carries `Schema.Option` (a plain
/// codec, unlike the forward-compat helpers above); it appears inside struct
/// fields of `sourceControl.ts` and `vcs.ts` only. Distinct from
/// `Option<T>` (which maps `Schema.optionalKey` / `Schema.NullOr`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
#[derive(Default)]
pub enum EffectOption<T> {
    #[serde(rename = "Some")]
    Some { value: Box<T> },
    #[serde(rename = "None")]
    #[default]
    None,
}

impl<T> EffectOption<T> {
    pub fn some(value: T) -> Self {
        Self::Some {
            value: Box::new(value),
        }
    }
    pub fn as_ref_option(&self) -> Option<&T> {
        match self {
            Self::Some { value } => Some(value),
            Self::None => None,
        }
    }
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Some { value } => Some(*value),
            Self::None => None,
        }
    }
}

impl<T> From<Option<T>> for EffectOption<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => Self::some(value),
            None => Self::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whitespace list from upstream baseSchemas.test.ts.
    const PADDINGS: [&str; 10] = [
        " ", "\t", "\n", "\r\n", "\u{000b}", "\u{000c}", "\u{00a0}", "\u{2028}", "\u{3000}",
        "\u{feff}",
    ];

    macro_rules! for_each_id {
        ($callback:ident) => {
            $callback!(
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
                PlanId
            )
        };
    }

    // --- trimmed non-empty strings -------------------------------------

    #[test]
    fn js_trim_matches_ecmascript_for_every_code_point() {
        // Independent oracle: Rust's White_Space, with the two known differences.
        for c in char::MIN..=char::MAX {
            let expected = (c.is_whitespace() && c != '\u{0085}') || c == '\u{feff}';
            assert_eq!(is_js_whitespace(c), expected, "U+{:04X}", c as u32);
        }
        assert_eq!(js_trim("\u{0085}a\u{0085}"), "\u{0085}a\u{0085}");
        assert_eq!(js_trim("\u{feff}a\u{feff}"), "a");
        assert_eq!(js_trim("\u{180e}a"), "\u{180e}a");
    }

    #[test]
    fn padded_values_trim_and_blank_values_are_rejected() {
        // Upstream pads with every kind of whitespace `trim()` removes.
        for outer in PADDINGS {
            for inner in PADDINGS {
                let padded = format!("{outer}{inner}a b{inner}{outer}");
                assert_eq!(js_trim(&padded), "a b");
                assert_eq!(TrimmedString::new(&padded).as_str(), "a b");
                assert_eq!(TrimmedNonEmptyString::new(&padded).unwrap().as_str(), "a b");
                assert_eq!(ThreadId::new(&padded).unwrap().as_str(), "a b");
                let blank = format!("{outer}{inner}");
                assert!(TrimmedNonEmptyString::new(&blank).is_err());
                assert!(ThreadId::new(&blank).is_err());
            }
        }
        assert!(ThreadId::new("").is_err());
        assert_eq!(ThreadId::new("a\u{0085}").unwrap().as_str(), "a\u{0085}");
    }

    #[test]
    fn trimmed_values_encode_trimmed_and_round_trip() {
        let untrimmed = TrimmedString::new("  a b  ");
        assert_eq!(serde_json::to_string(&untrimmed).unwrap(), "\"a b\"");
        assert_eq!(
            serde_json::from_str::<TrimmedString>("\"\\u000b x \"")
                .unwrap()
                .as_str(),
            "x"
        );
        let non_empty = TrimmedNonEmptyString::new(" x ").unwrap();
        assert_eq!(serde_json::to_string(&non_empty).unwrap(), "\"x\"");
        assert!(serde_json::from_str::<TrimmedNonEmptyString>("\" \"").is_err());
    }

    #[test]
    fn all_entity_ids_reject_whitespace_only_and_round_trip() {
        macro_rules! check_ids {
            ($($name:ident),+) => {$(
                for value in [" ", "\t\n", "\u{00a0}\u{3000}", "\u{feff}", ""] {
                    assert!($name::new(value).is_err(), stringify!($name));
                    let json = serde_json::to_string(value).unwrap();
                    assert!(serde_json::from_str::<$name>(&json).is_err(), stringify!($name));
                }
                let value = $name::new("\u{FEFF} id \u{3000}").unwrap();
                assert_eq!(value.as_str(), "id", stringify!($name));
                assert_eq!(value.to_string(), "id");
                assert_eq!(serde_json::to_string(&value).unwrap(), "\"id\"", stringify!($name));
                assert_eq!(serde_json::from_str::<$name>("\" id \"").unwrap(), value);
                assert_eq!(serde_json::from_str::<$name>("\"\\ufeffid\"").unwrap(), value);
                assert!(serde_json::from_str::<$name>("5").is_err());
                assert_ne!($name::random(), $name::random());
            )+};
        }
        for_each_id!(check_ids);
    }

    // --- numbers and timestamps ----------------------------------------

    #[test]
    fn positive_int_rejects_zero_everywhere() {
        assert!(PositiveInt::new(0).is_none());
        assert!(PositiveInt::try_from(0).is_err());
        assert!(serde_json::from_str::<PositiveInt>("0").is_err());
        assert!(serde_json::from_str::<PositiveInt>("-1").is_err());
        assert!(serde_json::from_str::<PositiveInt>("1.5").is_err());
        assert!(serde_json::from_str::<PositiveInt>("\"1\"").is_err());
        assert_eq!(serde_json::from_str::<PositiveInt>("1").unwrap().get(), 1);
        assert_eq!(u64::from(PositiveInt::try_from(7).unwrap()), 7);
        assert_eq!(
            serde_json::to_string(&PositiveInt::new(42).unwrap()).unwrap(),
            "42"
        );
    }

    #[test]
    fn utc_datetime_always_encodes_millisecond_utc() {
        let at: UtcDateTime = serde_json::from_str("\"2026-01-02T03:04:05.123456+02:00\"").unwrap();
        assert_eq!(
            serde_json::to_string(&at).unwrap(),
            "\"2026-01-02T01:04:05.123Z\""
        );
        let whole: UtcDateTime = serde_json::from_str("\"2026-01-02T01:04:05Z\"").unwrap();
        assert_eq!(whole.to_string(), "2026-01-02T01:04:05.000Z");
        assert!(at > whole);
        assert!(serde_json::from_str::<UtcDateTime>("\"yesterday\"").is_err());
        assert!(serde_json::from_str::<UtcDateTime>("5").is_err());

        // Text order equals time order, which upstream's `ORDER BY` relies on.
        let earlier: UtcDateTime = "2026-10-14T12:00:00.500Z".parse().unwrap();
        let later: UtcDateTime = "2026-10-14T12:00:01Z".parse().unwrap();
        assert!(earlier < later && earlier.to_string() < later.to_string());

        // Equality agrees with the wire form (sub-millisecond digits are dropped).
        let a: UtcDateTime = "2026-01-02T01:04:05.123456Z".parse().unwrap();
        let b: UtcDateTime = "2026-01-02T01:04:05.123999Z".parse().unwrap();
        assert_eq!(a, b);
        let now = UtcDateTime::now().to_string();
        assert!(now.ends_with('Z') && now.len() == 24, "{now}");
        assert_eq!(IsoDateTime::from("kept as-is"), "kept as-is");
    }

    /// Expected values are what effect 4.0.4 `DateTime.make(s)` + `formatIso`
    /// produce under Node 22 (TZ=America/Los_Angeles, so a local-time
    /// misparse would show).
    #[test]
    fn utc_datetime_accepts_what_upstream_make_accepts() {
        let cases = [
            ("2024-01-02T03:04:05.123Z", "2024-01-02T03:04:05.123Z"),
            ("2024-01-02T03:04:05Z", "2024-01-02T03:04:05.000Z"),
            (
                "2024-01-02T03:04:05.123456789012345678Z",
                "2024-01-02T03:04:05.123Z",
            ),
            ("2024-01-02T03:04:05.9995Z", "2024-01-02T03:04:05.999Z"),
            ("2024-01-02T03:04:05.1", "2024-01-02T03:04:05.100Z"),
            // No zone means UTC.
            ("2024-01-02T03:04:05", "2024-01-02T03:04:05.000Z"),
            ("2024-01-02T03:04:05.12345", "2024-01-02T03:04:05.123Z"),
            // Minute precision.
            ("2024-01-02T03:04Z", "2024-01-02T03:04:00.000Z"),
            ("2024-01-02T03:04", "2024-01-02T03:04:00.000Z"),
            ("2024T03:04", "2024-01-01T03:04:00.000Z"),
            ("2024-01T03:04Z", "2024-01-01T03:04:00.000Z"),
            // Date-only forms, with or without `Z`.
            ("2024-01-02", "2024-01-02T00:00:00.000Z"),
            ("2024-01", "2024-01-01T00:00:00.000Z"),
            ("2024", "2024-01-01T00:00:00.000Z"),
            ("2024-01-02Z", "2024-01-02T00:00:00.000Z"),
            ("2024-01Z", "2024-01-01T00:00:00.000Z"),
            ("2024Z", "2024-01-01T00:00:00.000Z"),
            // Offsets, with and without the colon.
            ("2024-01-02T03:04:05+02:00", "2024-01-02T01:04:05.000Z"),
            ("2024-01-02T03:04:05+0200", "2024-01-02T01:04:05.000Z"),
            ("2024-01-02T03:04+0200", "2024-01-02T01:04:00.000Z"),
            ("2024-01-02T03:04:05-0530", "2024-01-02T08:34:05.000Z"),
            ("2024-01-02T03:04:05.5+02:00", "2024-01-02T01:04:05.500Z"),
            ("2024-01-02T03:04:05.123-00:00", "2024-01-02T03:04:05.123Z"),
            ("2024-01-02T03:04:05+23:59", "2024-01-01T03:05:05.000Z"),
            ("2024-01-02T03:04:05-23:59", "2024-01-03T03:03:05.000Z"),
            ("2024T03:04:05+0200", "2024-01-01T01:04:05.000Z"),
            // Lowercase `t` is accepted upstream; lowercase `z` is not.
            ("2024-01-02t03:04:05Z", "2024-01-02T03:04:05.000Z"),
            ("2024-01-02t03:04", "2024-01-02T03:04:00.000Z"),
            ("2024-01-02t03:04:05+02:00", "2024-01-02T01:04:05.000Z"),
            // 24:00 is the next midnight; days past month end roll over.
            ("2024-01-02T24:00:00Z", "2024-01-03T00:00:00.000Z"),
            ("2024-01-02T24:00Z", "2024-01-03T00:00:00.000Z"),
            ("2024-01-02T24:00", "2024-01-03T00:00:00.000Z"),
            ("2024-01-02T24:00:00.000Z", "2024-01-03T00:00:00.000Z"),
            ("2024-12-31T24:00:00Z", "2025-01-01T00:00:00.000Z"),
            ("2024-02-29T24:00", "2024-03-01T00:00:00.000Z"),
            ("2024-02-29", "2024-02-29T00:00:00.000Z"),
            ("2024-02-30T00:00:00Z", "2024-03-01T00:00:00.000Z"),
            ("2023-02-29", "2023-03-01T00:00:00.000Z"),
            ("2023-02-31", "2023-03-03T00:00:00.000Z"),
            ("2024-04-31T00:00:00Z", "2024-05-01T00:00:00.000Z"),
            // Expanded years, and the year bounds.
            ("+002024-01-02T00:00:00Z", "2024-01-02T00:00:00.000Z"),
            ("+002024-01-02T03:04+02:00", "2024-01-02T01:04:00.000Z"),
            ("+000000-01-01T00:00:00Z", "0000-01-01T00:00:00.000Z"),
            ("0000-01-01T00:00:00Z", "0000-01-01T00:00:00.000Z"),
            ("0001-01-01", "0001-01-01T00:00:00.000Z"),
            ("9999-12-31T23:59:59.999Z", "9999-12-31T23:59:59.999Z"),
        ];
        for (wire, expected) in cases {
            let parsed: UtcDateTime = wire.parse().unwrap_or_else(|e| panic!("{wire}: {e}"));
            assert_eq!(parsed.to_string(), expected, "{wire}");
            let json = serde_json::to_string(wire).unwrap();
            assert_eq!(serde_json::from_str::<UtcDateTime>(&json).unwrap(), parsed);
            assert_eq!(expected.parse::<UtcDateTime>().unwrap(), parsed, "{wire}");
        }
    }

    #[test]
    fn utc_datetime_rejects_what_upstream_make_rejects() {
        for wire in [
            "",
            "yesterday",
            "2024-01-02T23:59:60Z",
            "2024-01-02T00:00:60",
            "2024-01-02T03:04:05z",
            "2024-01-02T03:04:05+02",
            "2024-01-02T03:04:05+2:00",
            "2024-01-02T03:04:05+02:0",
            "2024-01-02T03:04:05+0",
            "2024-01-02T03:04:05+020",
            "2024-01-02T03:04:05+02000",
            "2024-01-02T03:04:05+24:00",
            "2024-01-02T03:04:05+02:60",
            "2024-01-02T03:04:05.123+02:00:00",
            "2024-01-02T03:04:05.123+02:00Z",
            "2024-01-02T03:04:05.123ZZ",
            "2024-01-02T03:04:05GMT",
            "2024-01-02T03:04:05ZGMT",
            "2024-01-02+02:00",
            "2024-01-02T24:00:01Z",
            "2024-01-02T24:00:00.001Z",
            "2024-01-02T25:00Z",
            "2024-01-02T03:60Z",
            "2024-13-01",
            "2024-00-01",
            "2024-01-00",
            "2024-01-32",
            "20240102",
            "2024-01-02T3:04:05Z",
            "2024-01-02T03:04:5Z",
            "2024-01-02T03Z",
            "2024-01-02T03",
            "2024-01-02T03:04:05.Z",
            "2024-01-02T03:04:05,123Z",
            "2024-01-02TZ",
            "2024-01-02T",
            "-000000-01-01T00:00:00Z",
            " 2024-01-02T03:04:05Z",
            "2024-01-02T03:04:05Z ",
        ] {
            assert!(wire.parse::<UtcDateTime>().is_err(), "{wire}");
            let json = serde_json::to_string(wire).unwrap();
            assert!(
                serde_json::from_str::<UtcDateTime>(&json).is_err(),
                "{wire}"
            );
        }
    }

    #[test]
    fn utc_datetime_rejects_years_outside_0000_to_9999() {
        // Upstream accepts these but encodes them as `+010000-…` / `-000001-…`,
        // which the fixed `YYYY-…` format cannot carry.
        for wire in [
            "+010000-01-01T00:00:00Z",
            "-000001-01-01T00:00:00Z",
            "9999-12-31T23:59:59.999-01:00",
            "9999-12-31T24:00Z",
            "0000-01-01T00:00:00+01:00",
        ] {
            assert!(wire.parse::<UtcDateTime>().is_err(), "{wire}");
        }
    }

    #[test]
    fn utc_datetime_from_datetime_clamps_to_the_encodable_years() {
        let far_future = DateTime::<Utc>::MAX_UTC;
        assert_eq!(
            UtcDateTime::from_datetime(far_future).to_string(),
            "9999-12-31T23:59:59.999Z"
        );
        assert_eq!(
            UtcDateTime::from_datetime(DateTime::<Utc>::MIN_UTC).to_string(),
            "0000-01-01T00:00:00.000Z"
        );
        for value in [far_future, DateTime::<Utc>::MIN_UTC] {
            let at = UtcDateTime::from_datetime(value);
            assert_eq!(at.to_string().parse::<UtcDateTime>().unwrap(), at);
        }
    }

    // --- tagged unions --------------------------------------------------

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "camelCase")]
    enum Shape {
        Circle { radius: f64, at: UtcDateTime },
        Square { side: f64 },
    }

    impl TaggedUnion for Shape {
        const TAG: &'static str = "kind";
        fn known_tags() -> &'static [&'static str] {
            &["circle", "square"]
        }
    }

    fn circle() -> Shape {
        Shape::Circle {
            radius: 1.0,
            at: "2026-10-05T00:00:00.000Z".parse().unwrap(),
        }
    }

    #[test]
    fn assert_known_tags_accepts_exact_coverage() {
        assert_known_tags(&[circle(), Shape::Square { side: 2.0 }]);
    }

    #[test]
    #[should_panic(expected = "known_tags must match")]
    fn assert_known_tags_panics_when_a_variant_is_unsampled() {
        assert_known_tags(&[Shape::Square { side: 2.0 }]);
    }

    #[test]
    #[should_panic(expected = "known_tags must match")]
    fn assert_known_tags_panics_when_the_list_has_a_stale_tag() {
        #[derive(Serialize)]
        #[serde(tag = "kind")]
        enum Stale {
            #[serde(rename = "a")]
            A,
        }
        impl TaggedUnion for Stale {
            const TAG: &'static str = "kind";
            fn known_tags() -> &'static [&'static str] {
                &["a", "gone"]
            }
        }
        assert_known_tags(&[Stale::A]);
    }

    #[test]
    #[should_panic(expected = "known_tags must match")]
    fn assert_known_tags_panics_when_the_list_misses_a_tag() {
        #[derive(Serialize)]
        #[serde(tag = "kind")]
        enum Missing {
            #[serde(rename = "a")]
            A,
            #[serde(rename = "b")]
            B,
        }
        impl TaggedUnion for Missing {
            const TAG: &'static str = "kind";
            fn known_tags() -> &'static [&'static str] {
                &["a"]
            }
        }
        assert_known_tags(&[Missing::A, Missing::B]);
    }

    #[test]
    fn union_decodes_unknown_member_from_a_newer_server() {
        let raw = json!({"kind": "triangle", "corners": 3});
        let decoded: ForwardCompatible<Shape> = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            decoded,
            ForwardCompatible::Unknown(UnknownUnionMember {
                tag: "kind".into(),
                value: "triangle".into(),
                raw,
            })
        );
        assert!(decoded.known().is_none());
    }

    #[test]
    fn union_decodes_known_members_in_full_transformations_included() {
        let decoded: ForwardCompatible<Shape> = serde_json::from_value(
            json!({"kind": "circle", "radius": 1, "at": "2026-10-05T01:00:00+01:00"}),
        )
        .unwrap();
        assert_eq!(decoded, ForwardCompatible::Known(circle()));
    }

    #[test]
    fn union_still_fails_a_known_member_with_a_broken_payload() {
        let broken = json!({"kind": "square", "side": "wide"});
        assert!(serde_json::from_value::<ForwardCompatible<Shape>>(broken).is_err());
        assert!(
            serde_json::from_value::<ForwardCompatible<Shape>>(json!({"kind": "circle"})).is_err()
        );
    }

    #[test]
    fn union_refuses_to_encode_an_unknown_member() {
        let unknown: ForwardCompatible<Shape> =
            serde_json::from_value(json!({"kind": "triangle"})).unwrap();
        let error = serde_json::to_string(&unknown).unwrap_err().to_string();
        assert!(
            error.contains("Unknown kind values are never sent."),
            "{error}"
        );
        let known = ForwardCompatible::Known(Shape::Square { side: 2.0 });
        assert_eq!(
            serde_json::to_value(&known).unwrap(),
            json!({"kind": "square", "side": 2.0})
        );
    }

    #[test]
    fn has_unknown_union_tag_matches_upstream() {
        let known = ["circle"];
        for value in [
            json!({"type": "new"}),
            json!({"type": 5}),
            json!({"type": null}),
            json!({"type": ["circle"]}),
        ] {
            assert!(has_unknown_union_tag(&value, "type", &known), "{value}");
        }
        for value in [
            json!({"type": "circle"}),
            json!({}),
            json!(null),
            json!("circle"),
            json!(5),
        ] {
            assert!(!has_unknown_union_tag(&value, "type", &known), "{value}");
        }
        // The strict decode does not treat a non-string tag as an unknown member.
        assert!(serde_json::from_value::<ForwardCompatible<Shape>>(json!({"kind": 5})).is_err());
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct ShapesHolder {
        #[serde(with = "forward_compatible_union_array")]
        shapes: Vec<Shape>,
    }

    #[test]
    fn union_array_keeps_a_known_member_however_its_fields_are_named() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "kind")]
        enum Flagged {
            #[serde(rename = "flag")]
            Flag {
                _unknown: bool,
                tag: String,
                value: String,
            },
        }
        impl TaggedUnion for Flagged {
            const TAG: &'static str = "kind";
            fn known_tags() -> &'static [&'static str] {
                &["flag"]
            }
        }
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(with = "forward_compatible_union_array")]
            values: Vec<Flagged>,
        }
        let flag = json!({"kind": "flag", "_unknown": true, "tag": "kind", "value": "x"});
        let raw = json!({ "values": [flag] });
        let holder: Holder = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(holder.values.len(), 1);
        assert_eq!(serde_json::to_value(&holder).unwrap(), raw);
        assert_known_tags(&holder.values);
    }

    #[test]
    fn union_array_drops_unknown_members_and_keeps_the_rest() {
        let holder: ShapesHolder = serde_json::from_value(json!({"shapes": [
            {"kind": "triangle"},
            {"kind": "circle", "radius": 1, "at": "2026-10-05T00:00:00.000Z"},
            {"kind": "square", "side": 2},
        ]}))
        .unwrap();
        assert_eq!(holder.shapes, vec![circle(), Shape::Square { side: 2.0 }]);
    }

    #[test]
    fn union_array_fails_when_a_known_member_is_broken() {
        let broken = json!({"shapes": [{"kind": "square", "side": "wide"}]});
        assert!(serde_json::from_value::<ShapesHolder>(broken).is_err());
    }

    #[test]
    fn union_array_encodes_as_a_plain_array() {
        let holder = ShapesHolder {
            shapes: vec![Shape::Square { side: 2.0 }],
        };
        assert_eq!(
            serde_json::to_value(&holder).unwrap(),
            json!({"shapes": [{"kind": "square", "side": 2.0}]})
        );
    }

    #[test]
    fn optional_union_array_keeps_absent_empty_and_filtered_distinct() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Holder {
            #[serde(
                default,
                skip_serializing_if = "Option::is_none",
                with = "forward_compatible_union_array::optional"
            )]
            shapes: Option<Vec<Shape>>,
        }
        let cases = [
            (json!({}), None, json!({})),
            (json!({"shapes": []}), Some(vec![]), json!({"shapes": []})),
            (
                json!({"shapes": [{"kind": "triangle"}, {"kind": "square", "side": 2}]}),
                Some(vec![Shape::Square { side: 2.0 }]),
                json!({"shapes": [{"kind": "square", "side": 2.0}]}),
            ),
            (
                json!({"shapes": [{"kind": "triangle"}]}),
                Some(vec![]),
                json!({"shapes": []}),
            ),
        ];
        for (wire, expected, encoded) in cases {
            let decoded: Holder = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(decoded.shapes, expected, "{wire}");
            assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded, "{wire}");
        }
        for broken in [
            json!({"shapes": null}),
            json!({"shapes": {"kind": "square", "side": 2}}),
            json!({"shapes": [{"kind": "square", "side": "wide"}]}),
        ] {
            assert!(
                serde_json::from_value::<Holder>(broken.clone()).is_err(),
                "{broken}"
            );
        }
    }

    // --- ForwardCompatibleArray ----------------------------------------

    #[test]
    fn optional_array_keeps_absent_empty_and_filtered_distinct() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Holder {
            #[serde(
                default,
                skip_serializing_if = "Option::is_none",
                with = "forward_compatible_array::optional"
            )]
            modes: Option<Vec<ClientSurface>>,
        }
        let cases = [
            (json!({}), None, json!({})),
            (json!({"modes": []}), Some(vec![]), json!({"modes": []})),
            (
                json!({"modes": ["desktop", "hologram", 5, null, "cli"]}),
                Some(vec![ClientSurface::Desktop, ClientSurface::Cli]),
                json!({"modes": ["desktop", "cli"]}),
            ),
            (
                json!({"modes": ["hologram"]}),
                Some(vec![]),
                json!({"modes": []}),
            ),
        ];
        for (wire, expected, encoded) in cases {
            let decoded: Holder = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(decoded.modes, expected, "{wire}");
            assert_eq!(serde_json::to_value(&decoded).unwrap(), encoded, "{wire}");
        }
        for broken in [json!({"modes": null}), json!({"modes": "desktop"})] {
            assert!(
                serde_json::from_value::<Holder>(broken.clone()).is_err(),
                "{broken}"
            );
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct ArrayHolder<T>
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        #[serde(with = "forward_compatible_array")]
        values: Vec<T>,
    }

    #[test]
    fn array_decodes_through_the_element_codec_and_drops_unreadable_elements() {
        let decoded: ArrayHolder<Shape> = serde_json::from_value(json!({"values": [
            {"kind": "circle", "radius": 1, "at": "2026-10-05T01:00:00+01:00"},
            {"kind": "triangle"},
            {"kind": "square", "side": "wide"},
        ]}))
        .unwrap();
        assert_eq!(decoded.values, vec![circle()]);
    }

    #[test]
    fn array_applies_element_defaults_instead_of_failing() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct WithDefault {
            name: String,
            #[serde(default = "seven")]
            count: u32,
        }
        fn seven() -> u32 {
            7
        }
        let decoded: ArrayHolder<WithDefault> =
            serde_json::from_value(json!({"values": [{"name": "a"}, {"name": 5}]})).unwrap();
        assert_eq!(
            decoded.values,
            vec![WithDefault {
                name: "a".into(),
                count: 7
            }]
        );
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Named {
        name: TrimmedNonEmptyString,
    }

    #[test]
    fn array_drops_an_element_it_cannot_encode_instead_of_failing() {
        struct SometimesBad(&'static str);
        impl Serialize for SometimesBad {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                if self.0.is_empty() {
                    Err(serde::ser::Error::custom("bad"))
                } else {
                    s.serialize_str(self.0)
                }
            }
        }
        #[derive(Serialize)]
        struct Outbound {
            #[serde(with = "forward_compatible_array")]
            values: Vec<SometimesBad>,
        }
        let wire = serde_json::to_value(Outbound {
            values: vec![SometimesBad("a"), SometimesBad(""), SometimesBad("b")],
        })
        .unwrap();
        assert_eq!(wire, json!({"values": ["a", "b"]}));
    }

    #[test]
    fn array_round_trips_trimmed_values_and_drops_blank_ones() {
        let decoded: ArrayHolder<Named> = serde_json::from_value(
            json!({"values": [{"name": " a "}, {"name": " "}, {"name": "b"}]}),
        )
        .unwrap();
        let names: Vec<_> = decoded.values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        let wire = serde_json::to_value(&decoded).unwrap();
        assert_eq!(wire, json!({"values": [{"name": "a"}, {"name": "b"}]}));
        let again: ArrayHolder<Named> = serde_json::from_value(wire).unwrap();
        assert_eq!(again, decoded);
    }

    #[test]
    fn array_still_drops_the_null_holes_a_server_on_an_earlier_build_sends() {
        let decoded: ArrayHolder<Named> =
            serde_json::from_value(json!({"values": [{"name": "a"}, null, {"name": "b"}]}))
                .unwrap();
        assert_eq!(decoded.values.len(), 2);
    }

    // --- optional / nullable helpers -----------------------------------

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct DoubleHolder {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        field: Option<Option<String>>,
    }

    #[test]
    fn double_option_keeps_absent_null_and_value_distinct() {
        let cases = [
            ("{}", None),
            (r#"{"field":null}"#, Some(None)),
            (r#"{"field":"x"}"#, Some(Some("x".to_owned()))),
        ];
        for (wire, expected) in cases {
            let decoded: DoubleHolder = serde_json::from_str(wire).unwrap();
            assert_eq!(decoded.field, expected, "{wire}");
            // Exact round trip: absent stays absent, null stays null.
            assert_eq!(serde_json::to_string(&decoded).unwrap(), wire);
        }
        assert!(serde_json::from_str::<DoubleHolder>(r#"{"field":5}"#).is_err());
    }

    #[test]
    fn forward_compatible_optional_nullable_round_trips_all_states() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Holder {
            #[serde(
                default,
                skip_serializing_if = "Option::is_none",
                with = "forward_compatible_optional_nullable"
            )]
            field: Option<Option<u32>>,
        }
        let cases = [
            ("{}", None, "{}"),
            (r#"{"field":null}"#, Some(None), r#"{"field":null}"#),
            (r#"{"field":3}"#, Some(Some(3)), r#"{"field":3}"#),
            (r#"{"field":"future"}"#, None, "{}"),
            (r#"{"field":-1}"#, None, "{}"),
        ];
        for (wire, expected, encoded) in cases {
            let decoded: Holder = serde_json::from_str(wire).unwrap();
            assert_eq!(decoded.field, expected, "{wire}");
            assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded, "{wire}");
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct LenientHolder {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "forward_compatible_optional"
        )]
        optional: Option<u32>,
        #[serde(default, with = "forward_compatible_nullable")]
        nullable: Option<u32>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "omitted_when_null"
        )]
        omitted: Option<u32>,
    }

    #[test]
    fn lenient_helpers_decode_missing_unknown_and_null_as_none() {
        let absent: LenientHolder = serde_json::from_str("{}").unwrap();
        for wire in [
            r#"{"optional":"future","nullable":"future","omitted":"future"}"#,
            r#"{"optional":null,"nullable":null,"omitted":null}"#,
            r#"{"optional":-5,"nullable":1.5,"omitted":[]}"#,
        ] {
            assert_eq!(serde_json::from_str::<LenientHolder>(wire).unwrap(), absent);
        }
        let values: LenientHolder =
            serde_json::from_str(r#"{"optional":1,"nullable":2,"omitted":3}"#).unwrap();
        assert_eq!(
            (values.optional, values.nullable, values.omitted),
            (Some(1), Some(2), Some(3))
        );
    }

    #[test]
    fn lenient_helpers_have_distinct_encode_rules() {
        let none = LenientHolder {
            optional: None,
            nullable: None,
            omitted: None,
        };
        // Only the nullable helper writes `null`; the other two omit the key.
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            json!({"nullable": null})
        );
        let some = LenientHolder {
            optional: Some(1),
            nullable: Some(2),
            omitted: Some(3),
        };
        assert_eq!(
            serde_json::to_value(&some).unwrap(),
            json!({"optional": 1, "nullable": 2, "omitted": 3})
        );
    }
}
