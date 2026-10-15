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
