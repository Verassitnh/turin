//! Port of upstream/apps/server/src/persistence/ProviderSessionRuntime.ts
//!
//! JSON columns are `serde_json::Value`. Rows that no longer decode are skipped
//! by `list` and fail `get_by_thread_id`, matching upstream.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};
use turin_contracts::{ThreadId, js_trim};

use crate::errors::{PersistenceDecodeError, PersistenceError, PersistenceErrorCorrelation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSessionStatus {
    Starting,
    Running,
    Stopped,
    Error,
}

impl ProviderSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSessionRuntime {
    pub thread_id: ThreadId,
    pub provider_name: String,
    pub provider_instance_id: Option<String>,
    pub adapter_key: String,
    pub runtime_mode: String,
    pub status: ProviderSessionStatus,
    pub last_seen_at: String,
    pub resume_cursor: Option<serde_json::Value>,
    pub runtime_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertConflict {
    Update,
    Ignore,
}

pub fn upsert(
    conn: &Connection,
    runtime: &ProviderSessionRuntime,
    on_conflict: UpsertConflict,
) -> Result<(), PersistenceError> {
    let sql = match on_conflict {
        UpsertConflict::Update => UPSERT_SQL,
        UpsertConflict::Ignore => INSERT_IGNORE_SQL,
    };
    let resume = json_text(runtime.resume_cursor.as_ref()).map_err(|err| {
        decode(
            "ProviderSessionRuntimeRepository.upsert:encodeRequest",
            runtime.thread_id.as_str(),
            err,
        )
    })?;
    let payload = json_text(runtime.runtime_payload.as_ref()).map_err(|err| {
        decode(
            "ProviderSessionRuntimeRepository.upsert:encodeRequest",
            runtime.thread_id.as_str(),
            err,
        )
    })?;
    conn.execute(
        sql,
        params![
            runtime.thread_id.as_str(),
            runtime.provider_name,
            runtime.provider_instance_id,
            runtime.adapter_key,
            runtime.runtime_mode,
            runtime.status.as_str(),
            runtime.last_seen_at,
            resume,
            payload,
        ],
    )
    .map_err(|err| {
        PersistenceError::Sql(
            crate::errors::sql_error("ProviderSessionRuntimeRepository.upsert:query", &err)
                .with_correlation(PersistenceErrorCorrelation::ThreadId(
                    runtime.thread_id.as_str().to_owned(),
                )),
        )
    })?;
    Ok(())
}

pub fn record_imported_transcript(
    conn: &Connection,
    thread_id: &ThreadId,
    source: &serde_json::Value,
) -> Result<(), PersistenceError> {
    let source_json = serde_json::to_string(source).map_err(|err| {
        decode(
            "ProviderSessionRuntimeRepository.recordImportedTranscript:encodeRequest",
            thread_id.as_str(),
            err.to_string(),
        )
    })?;
    conn.execute(
        RECORD_IMPORTED_SQL,
        params![thread_id.as_str(), source_json],
    )
    .map_err(|err| {
        PersistenceError::Sql(
            crate::errors::sql_error(
                "ProviderSessionRuntimeRepository.recordImportedTranscript:query",
                &err,
            )
            .with_correlation(PersistenceErrorCorrelation::ThreadId(
                thread_id.as_str().to_owned(),
            )),
        )
    })?;
    Ok(())
}

pub fn get_by_thread_id(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<Option<ProviderSessionRuntime>, PersistenceError> {
    let row = conn
        .query_row(SELECT_SQL, params![thread_id.as_str()], decode_raw)
        .optional()
        .map_err(|err| {
            PersistenceError::Sql(
                crate::errors::sql_error(
                    "ProviderSessionRuntimeRepository.getByThreadId:query",
                    &err,
                )
                .with_correlation(PersistenceErrorCorrelation::ThreadId(
                    thread_id.as_str().to_owned(),
                )),
            )
        })?;
    row.map(|row| {
        decode_runtime(row).map_err(|(_thread, issue)| {
            decode(
                "ProviderSessionRuntimeRepository.getByThreadId:decodeRow",
                thread_id.as_str(),
                issue,
            )
        })
    })
    .transpose()
}

pub fn list(
    conn: &Connection,
    exclude_stopped: bool,
) -> Result<Vec<ProviderSessionRuntime>, PersistenceError> {
    let sql = if exclude_stopped {
        format!("{SELECT_ONE} WHERE status != 'stopped' ORDER BY last_seen_at ASC, thread_id ASC")
    } else {
        format!("{SELECT_ONE} ORDER BY last_seen_at ASC, thread_id ASC")
    };
    let mut stmt = conn.prepare_cached(&sql).map_err(|err| {
        PersistenceError::sql("ProviderSessionRuntimeRepository.list:query", &err)
    })?;
    let rows = stmt.query_map([], decode_raw).map_err(|err| {
        PersistenceError::sql("ProviderSessionRuntimeRepository.list:query", &err)
    })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row.map_err(|err| {
            PersistenceError::sql("ProviderSessionRuntimeRepository.list:query", &err)
        })?;
        match decode_runtime(row) {
            Ok(runtime) => out.push(runtime),
            Err(issue) => {
                tracing::warn!(
                    thread_id = %issue.0,
                    error = %issue.1,
                    "provider.session.runtime.row-skipped"
                );
            }
        }
    }
    Ok(out)
}

pub fn delete_by_thread_id(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<(), PersistenceError> {
    conn.execute(
        "DELETE FROM provider_session_runtime WHERE thread_id = ?1",
        params![thread_id.as_str()],
    )
    .map_err(|err| {
        PersistenceError::Sql(
            crate::errors::sql_error(
                "ProviderSessionRuntimeRepository.deleteByThreadId:query",
                &err,
            )
            .with_correlation(PersistenceErrorCorrelation::ThreadId(
                thread_id.as_str().to_owned(),
            )),
        )
    })?;
    Ok(())
}

const SELECT_ONE: &str = "
SELECT thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode,
       status, last_seen_at, resume_cursor_json, runtime_payload_json
FROM provider_session_runtime
";

const SELECT_SQL: &str = "
SELECT thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode,
       status, last_seen_at, resume_cursor_json, runtime_payload_json
FROM provider_session_runtime
WHERE thread_id = ?1
";

// Runtime writes can carry stale payloads. Only recordImportedTranscript may
// change source records, so restore that field from the row being updated.
const UPSERT_SQL: &str = "
INSERT INTO provider_session_runtime (
  thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode,
  status, last_seen_at, resume_cursor_json, runtime_payload_json
)
VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
  CASE
    WHEN json_type(?9) = 'object'
    THEN json_remove(?9, '$.importedTranscripts')
    ELSE ?9
  END
)
ON CONFLICT (thread_id)
DO UPDATE SET
  provider_name = excluded.provider_name,
  provider_instance_id = excluded.provider_instance_id,
  adapter_key = excluded.adapter_key,
  runtime_mode = excluded.runtime_mode,
  status = excluded.status,
  last_seen_at = excluded.last_seen_at,
  resume_cursor_json = excluded.resume_cursor_json,
  runtime_payload_json = CASE
    WHEN json_type(
      CASE
        WHEN json_valid(provider_session_runtime.runtime_payload_json)
        THEN provider_session_runtime.runtime_payload_json
        ELSE '{}'
      END,
      '$.importedTranscripts'
    ) IS NOT NULL
    THEN json_set(
      CASE
        WHEN json_type(excluded.runtime_payload_json) = 'object'
        THEN excluded.runtime_payload_json
        ELSE '{}'
      END,
      '$.importedTranscripts',
      json_extract(provider_session_runtime.runtime_payload_json, '$.importedTranscripts')
    )
    ELSE excluded.runtime_payload_json
  END
";

const INSERT_IGNORE_SQL: &str = "
INSERT INTO provider_session_runtime (
  thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode,
  status, last_seen_at, resume_cursor_json, runtime_payload_json
)
VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
  CASE
    WHEN json_type(?9) = 'object'
    THEN json_remove(?9, '$.importedTranscripts')
    ELSE ?9
  END
)
ON CONFLICT (thread_id) DO NOTHING
";

const RECORD_IMPORTED_SQL: &str = "
WITH current_runtime AS (
  SELECT CASE
    WHEN json_valid(runtime_payload_json) THEN CASE
      WHEN json_type(runtime_payload_json) = 'object' THEN runtime_payload_json
      ELSE '{}'
    END
    ELSE '{}'
  END AS payload
  FROM provider_session_runtime
  WHERE thread_id = ?1
)
UPDATE provider_session_runtime
SET runtime_payload_json = (
  SELECT json_set(
    payload,
    '$.importedTranscripts',
    json((
      SELECT json_group_array(json(value))
      FROM (
        SELECT value
        FROM json_each(CASE
          WHEN json_type(payload, '$.importedTranscripts') = 'array'
          THEN json_extract(payload, '$.importedTranscripts')
          ELSE '[]'
        END)
        WHERE CASE
          WHEN type = 'object' THEN
            json_extract(value, '$.providerInstanceId') IS NOT json_extract(?2, '$.providerInstanceId')
            OR json_extract(value, '$.filePath') IS NOT json_extract(?2, '$.filePath')
          ELSE 0
        END
        UNION ALL
        SELECT ?2 AS value
      )
    ))
  )
  FROM current_runtime
)
WHERE thread_id = ?1
";

struct RawRuntime {
    thread_id: Value,
    provider_name: Value,
    provider_instance_id: Value,
    adapter_key: Value,
    runtime_mode: Value,
    status: Value,
    last_seen_at: Value,
    resume_cursor: Value,
    runtime_payload: Value,
}

fn decode_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRuntime> {
    Ok(RawRuntime {
        thread_id: row.get(0)?,
        provider_name: row.get(1)?,
        provider_instance_id: row.get(2)?,
        adapter_key: row.get(3)?,
        runtime_mode: row.get(4)?,
        status: row.get(5)?,
        last_seen_at: row.get(6)?,
        resume_cursor: row.get(7)?,
        runtime_payload: row.get(8)?,
    })
}

fn decode_runtime(row: RawRuntime) -> Result<ProviderSessionRuntime, (String, String)> {
    let thread_text = match &row.thread_id {
        Value::Text(text) => text.clone(),
        _ => return Err(("<unknown>".into(), "thread id is not text".into())),
    };
    let fail = |issue: String| (thread_text.clone(), issue);
    let provider_name = require_text(row.provider_name, "provider name").map_err(fail)?;
    let provider_instance_id = provider_instance_id(row.provider_instance_id).map_err(fail)?;
    let adapter_key = require_text(row.adapter_key, "adapter key").map_err(fail)?;
    let runtime_mode = runtime_mode(row.runtime_mode).map_err(fail)?;
    let status_text = require_text(row.status, "status").map_err(fail)?;
    let status = ProviderSessionStatus::parse(&status_text)
        .ok_or_else(|| fail(format!("unknown status {status_text}")))?;
    let last_seen_at = require_text(row.last_seen_at, "last seen at").map_err(fail)?;
    let resume_cursor = parse_json_value(&thread_text, row.resume_cursor)?;
    let runtime_payload = parse_json_value(&thread_text, row.runtime_payload)?;
    let thread_id =
        ThreadId::new(&thread_text).map_err(|err| fail(format!("invalid thread id: {err}")))?;
    Ok(ProviderSessionRuntime {
        thread_id,
        provider_name,
        provider_instance_id,
        adapter_key,
        runtime_mode,
        status,
        last_seen_at,
        resume_cursor,
        runtime_payload,
    })
}

fn require_text(value: Value, label: &str) -> Result<String, String> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(format!("{label} is not text")),
    }
}

fn runtime_mode(value: Value) -> Result<String, String> {
    let text = require_text(value, "runtime mode")?;
    match text.as_str() {
        "approval-required" | "auto-accept-edits" | "auto" | "full-access" => Ok(text),
        _ => Err(format!("unknown runtime mode {text}")),
    }
}

/// `ProviderInstanceId`: null, or a trimmed slug of 1..=64 chars.
fn provider_instance_id(value: Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Text(text) => {
            let trimmed = js_trim(&text);
            if trimmed.is_empty() {
                return Err("provider instance id is empty".into());
            }
            let chars = trimmed.chars().count();
            let mut iter = trimmed.chars();
            let slug = iter.next().is_some_and(|c| c.is_ascii_alphabetic())
                && iter.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && (1..=64).contains(&chars);
            if !slug {
                return Err(format!("invalid provider instance id {trimmed}"));
            }
            Ok(Some(trimmed.to_owned()))
        }
        _ => Err("provider instance id is not text".into()),
    }
}

fn parse_json_value(
    thread_id: &str,
    value: Value,
) -> Result<Option<serde_json::Value>, (String, String)> {
    let json = match value {
        Value::Null => return Ok(None),
        Value::Text(text) => text,
        _ => return Err((thread_id.to_owned(), "json column is not text".into())),
    };
    match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(serde_json::Value::Null) => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(err) => Err((thread_id.to_owned(), err.to_string())),
    }
}

fn json_text(value: Option<&serde_json::Value>) -> Result<Option<String>, String> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(|err| err.to_string())
}

fn decode(operation: &str, thread_id: &str, issue: String) -> PersistenceError {
    PersistenceError::Decode(
        PersistenceDecodeError::new(operation, issue)
            .with_correlation(PersistenceErrorCorrelation::ThreadId(thread_id.to_owned())),
    )
}
