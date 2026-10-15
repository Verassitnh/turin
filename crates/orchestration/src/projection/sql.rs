//! Small SQL/JSON helpers shared by the projection store modules.

use rusqlite::{Connection, Params, Row};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::projection::error::Cause;

/// A store-internal result: the cause is attached to a typed error by the
/// public method that ran the body.
pub(crate) type CauseResult<T> = Result<T, Cause>;

/// The text an enum/id serializes to (`event.payload.status`, …).
pub(crate) fn text<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// `booleanInt`.
pub(crate) fn boolean_int(value: bool) -> i64 {
    i64::from(value)
}

/// JSON array of strings, the parameter shape `json_each(?)` filters read
/// (`encodeIdList`).
pub(crate) fn id_list<I, S>(ids: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let values = ids
        .into_iter()
        .map(|id| serde_json::Value::String(id.as_ref().to_owned()))
        .collect::<Vec<_>>();
    serde_json::Value::Array(values).to_string()
}

pub(crate) fn decode<T: DeserializeOwned>(payload_json: &str) -> CauseResult<T> {
    Ok(serde_json::from_str(payload_json)?)
}

/// Run a `SELECT payload_json …` and decode every row's first column.
pub(crate) fn query_payloads<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
) -> CauseResult<Vec<T>> {
    let mut statement = conn.prepare_cached(sql)?;
    let mut rows = statement.query(params)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let payload: String = row.get(0)?;
        items.push(decode(&payload)?);
    }
    Ok(items)
}

/// Like [`query_payloads`], for statements that match at most one row.
pub(crate) fn query_payload<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
) -> CauseResult<Option<T>> {
    Ok(query_payloads(conn, sql, params)?.into_iter().next())
}

/// Run a statement and map rows.
pub(crate) fn query_map<T>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> CauseResult<Vec<T>> {
    let mut statement = conn.prepare_cached(sql)?;
    let rows = statement.query_map(params, map)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub(crate) fn execute(conn: &Connection, sql: &str, params: impl Params) -> CauseResult<usize> {
    Ok(conn.prepare_cached(sql)?.execute(params)?)
}

/// Builds a statement whose shape depends on optional filters, binding every
/// value (the `sql\`…${filter ? sql\`AND …\` : sql\`\`}\`` pattern upstream uses).
#[derive(Default)]
pub(crate) struct SqlBuilder {
    pub sql: String,
    pub params: Vec<rusqlite::types::Value>,
}

impl SqlBuilder {
    pub fn new(sql: &str) -> Self {
        Self {
            sql: sql.to_owned(),
            params: Vec::new(),
        }
    }

    /// Append SQL text.
    pub fn push(&mut self, sql: &str) -> &mut Self {
        self.sql.push_str(sql);
        self
    }

    /// Append a bound value, returning the builder for chaining text after it.
    pub fn bind(&mut self, value: impl Into<rusqlite::types::Value>) -> &mut Self {
        self.params.push(value.into());
        self.sql.push_str(&format!("?{}", self.params.len()));
        self
    }

    pub fn query_payloads<T: DeserializeOwned>(&self, conn: &Connection) -> CauseResult<Vec<T>> {
        query_payloads(
            conn,
            &self.sql,
            rusqlite::params_from_iter(self.params.iter()),
        )
    }
}

/// `Option<&str>` as a bindable value.
pub(crate) fn opt_text(value: Option<&str>) -> rusqlite::types::Value {
    value.map_or(rusqlite::types::Value::Null, |text| {
        rusqlite::types::Value::Text(text.to_owned())
    })
}
