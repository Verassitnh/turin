//! Port of upstream/packages/contracts/src/usageLimitSourceId.ts
//!
//! Key of one `settings.usageLimitSources` entry. Lives in its own module so
//! both the settings and the usage-limit contracts can import it without
//! importing each other. Plain branded string: unlike entity ids it carries
//! no non-empty/trim constraint.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageLimitSourceId(Arc<str>);

impl UsageLimitSourceId {
    pub fn new(value: impl AsRef<str>) -> Self {
        Self(Arc::from(value.as_ref()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UsageLimitSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for UsageLimitSourceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Deref for UsageLimitSourceId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl Serialize for UsageLimitSourceId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for UsageLimitSourceId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(d)?))
    }
}
