//! Local macros for the contracts crate (kept out of `base_schemas.rs` so
//! merges from main stay clean).

/// Defines a unit struct used for a constant-valued struct field, mirroring
/// upstream `Schema.Literal("value")` fields (e.g. `kind: "snap-shot"`,
/// `version: 1`): decodes only from exactly that string, encodes to it.
#[macro_export]
macro_rules! string_literal {
    ($(#[$meta:meta])* $name:ident, $value:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name;

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($value)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <::std::borrow::Cow<'de, str>>::deserialize(d)?;
                if raw == $value {
                    Ok($name)
                } else {
                    Err(::serde::de::Error::custom(concat!("expected ", $value)))
                }
            }
        }
    };
}

/// Integer-valued counterpart of [`string_literal`], for `Schema.Literal(n)`.
#[macro_export]
macro_rules! int_literal {
    ($(#[$meta:meta])* $name:ident, $value:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name;

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_u64($value)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <u64 as ::serde::Deserialize>::deserialize(d)?;
                if raw == $value {
                    Ok($name)
                } else {
                    Err(::serde::de::Error::custom(concat!("expected ", $value)))
                }
            }
        }
    };
}

/// Defines a module of serde `deserialize_with` helpers for
/// `Schema.String.check(Schema.isMaxLength(max))` (and the trimmed-string
/// variants). Length is JS `string.length`, i.e. UTF-16 code units. Works for
/// any `T: AsRef<str>`; `required` for plain fields, `optional` for
/// `optional`/`optionalKey`/`NullOr` fields (`Option<T>`, `null` → `None`).
///
/// Use as `crate::max_len!(max_255, 255);` then
/// `#[serde(deserialize_with = "max_255::required")]`.
#[macro_export]
macro_rules! max_len {
    ($name:ident, $max:expr) => {
        #[allow(dead_code)]
        mod $name {
            use ::serde::{Deserialize, Deserializer};

            fn check<T: AsRef<str>>(value: T) -> Result<T, String> {
                if $crate::base_schemas::js_len(value.as_ref()) > $max {
                    Err(format!("expected at most {} characters", $max))
                } else {
                    Ok(value)
                }
            }

            pub fn required<'de, D, T>(d: D) -> Result<T, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de> + AsRef<str>,
            {
                check(T::deserialize(d)?).map_err(::serde::de::Error::custom)
            }

            pub fn optional<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de> + AsRef<str>,
            {
                Option::<T>::deserialize(d)?
                    .map(check)
                    .transpose()
                    .map_err(::serde::de::Error::custom)
            }
        }
    };
}

/// Like [`max_len`] for arrays of strings:
/// `Schema.Array(String.check(isMaxLength(item_max))).check(isMaxLength(count_max))`.
/// Item lengths are UTF-16 code units.
#[macro_export]
macro_rules! max_len_vec {
    ($name:ident, $item_max:expr, $count_max:expr) => {
        #[allow(dead_code)]
        mod $name {
            use ::serde::{Deserialize, Deserializer};

            fn check<T: AsRef<str>>(values: Vec<T>) -> Result<Vec<T>, String> {
                if values.len() > $count_max {
                    return Err(format!("expected at most {} items", $count_max));
                }
                if values
                    .iter()
                    .any(|value| $crate::base_schemas::js_len(value.as_ref()) > $item_max)
                {
                    return Err(format!(
                        "expected items of at most {} characters",
                        $item_max
                    ));
                }
                Ok(values)
            }

            pub fn required<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de> + AsRef<str>,
            {
                check(Vec::<T>::deserialize(d)?).map_err(::serde::de::Error::custom)
            }

            pub fn optional<'de, D, T>(d: D) -> Result<Option<Vec<T>>, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de> + AsRef<str>,
            {
                Option::<Vec<T>>::deserialize(d)?
                    .map(check)
                    .transpose()
                    .map_err(::serde::de::Error::custom)
            }
        }
    };
}

/// `Schema.Array(X).check(Schema.isMaxLength(max))` for non-string elements.
#[macro_export]
macro_rules! max_items {
    ($name:ident, $max:expr) => {
        #[allow(dead_code)]
        mod $name {
            use ::serde::{Deserialize, Deserializer};

            fn check<T>(values: Vec<T>) -> Result<Vec<T>, String> {
                if values.len() > $max {
                    Err(format!("expected at most {} items", $max))
                } else {
                    Ok(values)
                }
            }

            pub fn required<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de>,
            {
                check(Vec::<T>::deserialize(d)?).map_err(::serde::de::Error::custom)
            }

            pub fn optional<'de, D, T>(d: D) -> Result<Option<Vec<T>>, D::Error>
            where
                D: Deserializer<'de>,
                T: Deserialize<'de>,
            {
                Option::<Vec<T>>::deserialize(d)?
                    .map(check)
                    .transpose()
                    .map_err(::serde::de::Error::custom)
            }
        }
    };
}
