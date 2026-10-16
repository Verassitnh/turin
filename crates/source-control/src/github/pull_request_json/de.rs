//! Serde helpers that reproduce Effect Schema's struct decoding for the raw GitHub payloads.
//!
//! - `Schema.optional(X)`: the key may be absent, but a present `null` is a failure
//!   ([`opt`]).
//! - `Schema.optional(Schema.NullOr(X))`: absent, `null`, or `X`: a plain `Option<X>` with
//!   `#[serde(default)]`.
//! - `Schema.NullOr(X)` as a required key: the key must be present ([`nullable`]).
//! - absent versus `null` kept apart (`pr.stack === undefined` versus `null`): [`present`].
//! - `Schema.Int`: [`Int`].
//!
//! Unknown keys are ignored (Effect's default `onExcessProperty`).

use serde::{Deserialize, Deserializer, de::Error};

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `Schema.Int`: a JSON number with no fractional part inside the safe-integer range. `5.0` is an
/// integer, exactly as it is after `JSON.parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Int(pub i64);

impl<'de> Deserialize<'de> for Int {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(d)?;
        if value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER {
            Ok(Int(value as i64))
        } else {
            Err(D::Error::custom("Expected an integer"))
        }
    }
}

impl Int {
    /// A count that cannot be negative where the neutral types are unsigned.
    pub fn non_negative(self) -> u64 {
        self.0.max(0) as u64
    }
}

/// `Schema.optional(X)`: a present `null` fails instead of reading as absent.
pub fn opt<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// `Schema.NullOr(X)` as a required key (pair with no `#[serde(default)]`).
pub fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

/// `optional(NullOr(X))` where absent and `null` must stay apart: `None` absent, `Some(None)` null.
pub fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Splits an alias key like `s12` into its index, `/^<prefix>(\d+)$/` then `Number(index)`.
pub fn alias_index(alias: &str, prefix: char) -> Option<u64> {
    let digits = alias.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Sample {
        #[serde(default, deserialize_with = "opt")]
        strict: Option<String>,
        #[serde(default)]
        lenient: Option<String>,
        #[serde(deserialize_with = "nullable")]
        required_nullable: Option<String>,
        #[serde(default, deserialize_with = "present")]
        tri: Option<Option<String>>,
        count: Int,
    }

    #[test]
    fn schema_optional_semantics() {
        let ok: Sample =
            serde_json::from_str(r#"{"requiredNullable":null,"count":5.0,"tri":null}"#).unwrap();
        assert_eq!(ok.strict, None);
        assert_eq!(ok.required_nullable, None);
        assert_eq!(ok.lenient, None);
        assert_eq!(ok.tri, Some(None));
        assert_eq!(ok.count, Int(5));
        let tri: Sample = serde_json::from_str(r#"{"requiredNullable":null,"count":1}"#).unwrap();
        assert_eq!(tri.tri, None);
        // optional(String) rejects an explicit null
        assert!(
            serde_json::from_str::<Sample>(r#"{"strict":null,"requiredNullable":null,"count":1}"#)
                .is_err()
        );
        // a required NullOr key must be present
        assert!(serde_json::from_str::<Sample>(r#"{"count":1}"#).is_err());
        // Int rejects fractions and strings
        assert!(
            serde_json::from_str::<Sample>(r#"{"requiredNullable":null,"count":1.5}"#).is_err()
        );
        assert!(
            serde_json::from_str::<Sample>(r#"{"requiredNullable":null,"count":"1"}"#).is_err()
        );
    }

    #[test]
    fn alias_indexes() {
        assert_eq!(alias_index("s0", 's'), Some(0));
        assert_eq!(alias_index("s12", 's'), Some(12));
        assert_eq!(alias_index("s", 's'), None);
        assert_eq!(alias_index("s1x", 's'), None);
        assert_eq!(alias_index("w3", 's'), None);
        assert_eq!(alias_index("rateLimit", 's'), None);
    }
}
