//! Port of upstream/apps/server/src/persistence/OrchestrationCommandReceipts.ts
//!
//! Statements take the caller's connection so they can share a `Db::write` transaction.

use rusqlite::{Connection, OptionalExtension, params_from_iter};
use turin_contracts::CommandId;

use crate::errors::PersistenceError;
use crate::sql_value::SqlValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Project,
    Thread,
}

impl AggregateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Thread => "thread",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "project" => Some(Self::Project),
            "thread" => Some(Self::Thread),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptStatus {
    Accepted,
    Rejected,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrchestrationCommandReceipt {
    pub command_id: CommandId,
    pub aggregate_kind: AggregateKind,
    pub aggregate_id: String,
    pub command_type: String,
    pub accepted_at: String,
    pub result_sequence: i64,
    pub status: ReceiptStatus,
    pub error: Option<String>,
}

pub fn insert_if_absent(
    conn: &Connection,
    receipt: &OrchestrationCommandReceipt,
) -> Result<bool, PersistenceError> {
    let mut stmt = conn
        .prepare_cached(
            "
            INSERT INTO orchestration_command_receipts (
              command_id, aggregate_kind, aggregate_id, command_type,
              accepted_at, result_sequence, status, error
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(command_id) DO NOTHING
            RETURNING command_id
            ",
        )
        .map_err(|err| {
            PersistenceError::sql(
                "OrchestrationCommandReceiptRepository.insertIfAbsent:query",
                &err,
            )
        })?;
    let rows = stmt
        .query_map(params_from_iter(receipt_params(receipt)), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|err| {
            PersistenceError::sql(
                "OrchestrationCommandReceiptRepository.insertIfAbsent:query",
                &err,
            )
        })?;
    let mut count = 0;
    for row in rows {
        row.map_err(|err| {
            PersistenceError::sql(
                "OrchestrationCommandReceiptRepository.insertIfAbsent:query",
                &err,
            )
        })?;
        count += 1;
    }
    Ok(count == 1)
}

pub fn upsert(
    conn: &Connection,
    receipt: &OrchestrationCommandReceipt,
) -> Result<(), PersistenceError> {
    conn.execute(
        "
        INSERT INTO orchestration_command_receipts (
          command_id, aggregate_kind, aggregate_id, command_type,
          accepted_at, result_sequence, status, error
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT (command_id)
        DO UPDATE SET
          aggregate_kind = excluded.aggregate_kind,
          aggregate_id = excluded.aggregate_id,
          command_type = excluded.command_type,
          accepted_at = excluded.accepted_at,
          result_sequence = excluded.result_sequence,
          status = excluded.status,
          error = excluded.error
        ",
        params_from_iter(receipt_params(receipt)),
    )
    .map_err(|err| {
        PersistenceError::sql("OrchestrationCommandReceiptRepository.upsert:query", &err)
    })?;
    Ok(())
}

pub fn get_by_command_id(
    conn: &Connection,
    command_id: &CommandId,
) -> Result<Option<OrchestrationCommandReceipt>, PersistenceError> {
    let mut stmt = conn
        .prepare_cached(
            "
            SELECT command_id, aggregate_kind, aggregate_id, command_type,
                   accepted_at, result_sequence, status, error
            FROM orchestration_command_receipts
            WHERE command_id = ?1
            ",
        )
        .map_err(|err| {
            PersistenceError::sql(
                "OrchestrationCommandReceiptRepository.getByCommandId:query",
                &err,
            )
        })?;
    stmt.query_row([command_id.as_str()], decode_receipt)
        .optional()
        .map_err(|err| {
            PersistenceError::sql(
                "OrchestrationCommandReceiptRepository.getByCommandId:query",
                &err,
            )
        })?
        .transpose()
}

fn receipt_params(receipt: &OrchestrationCommandReceipt) -> Vec<SqlValue> {
    vec![
        SqlValue::text(&receipt.command_id),
        SqlValue::text(receipt.aggregate_kind.as_str()),
        SqlValue::text(&receipt.aggregate_id),
        SqlValue::text(&receipt.command_type),
        SqlValue::text(&receipt.accepted_at),
        SqlValue::Integer(receipt.result_sequence),
        SqlValue::text(receipt.status.as_str()),
        SqlValue::opt_text(receipt.error.clone()),
    ]
}

fn decode_receipt(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<OrchestrationCommandReceipt, PersistenceError>> {
    let aggregate: String = row.get(1)?;
    let status: String = row.get(6)?;
    let Some(aggregate_kind) = AggregateKind::parse(&aggregate) else {
        return Ok(Err(PersistenceError::Decode(
            crate::errors::PersistenceDecodeError::new(
                "OrchestrationCommandReceiptRepository.getByCommandId:decode",
                format!("unknown aggregate kind {aggregate}"),
            ),
        )));
    };
    let Some(status) = ReceiptStatus::parse(&status) else {
        return Ok(Err(PersistenceError::Decode(
            crate::errors::PersistenceDecodeError::new(
                "OrchestrationCommandReceiptRepository.getByCommandId:decode",
                format!("unknown receipt status {status}"),
            ),
        )));
    };
    let command_id_text: String = row.get(0)?;
    let command_id = match CommandId::new(&command_id_text) {
        Ok(command_id) => command_id,
        Err(err) => {
            return Ok(Err(PersistenceError::Decode(
                crate::errors::PersistenceDecodeError::new(
                    "OrchestrationCommandReceiptRepository.getByCommandId:decode",
                    err.to_string(),
                ),
            )));
        }
    };
    Ok(Ok(OrchestrationCommandReceipt {
        command_id,
        aggregate_kind,
        aggregate_id: row.get(2)?,
        command_type: row.get(3)?,
        accepted_at: row.get(4)?,
        result_sequence: row.get(5)?,
        status,
        error: row.get(7)?,
    }))
}
