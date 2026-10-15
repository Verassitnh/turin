//! Bound SQL parameters. JSON columns are stored as text, matching upstream.

use rusqlite::types::{ToSql, ToSqlOutput};

#[derive(Debug, Clone)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

impl SqlValue {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    pub fn opt_text(value: Option<String>) -> Self {
        match value {
            Some(value) => Self::Text(value),
            None => Self::Null,
        }
    }

    pub fn json(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(Self::Text(serde_json::to_string(value)?))
    }

    pub fn opt_json(value: &Option<serde_json::Value>) -> Result<Self, serde_json::Error> {
        match value {
            Some(value) => Self::json(value),
            None => Ok(Self::Null),
        }
    }
}

impl ToSql for SqlValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match self {
            Self::Null => Ok(ToSqlOutput::Owned(rusqlite::types::Value::Null)),
            Self::Integer(value) => value.to_sql(),
            Self::Real(value) => value.to_sql(),
            Self::Text(value) => Ok(ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(
                value.as_bytes(),
            ))),
        }
    }
}
