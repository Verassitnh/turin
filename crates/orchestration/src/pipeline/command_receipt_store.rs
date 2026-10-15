//! Port of upstream/apps/server/src/orchestration-v2/CommandReceiptStore.ts
//!
//! Typed thread/project access to the shared receipt table. Transactional
//! callers use the `*_in` functions; the sink composes them with event and
//! projection writes in one `Db::write` (upstream `sql.withTransaction`).

use rusqlite::Connection;
use turin_contracts::{CommandId, ProjectId, ThreadId, UtcDateTime};
use turin_persistence::{
    AggregateKind, Db, OrchestrationCommandReceipt, PersistenceDecodeError, PersistenceError,
    ReceiptStatus, get_by_command_id, insert_if_absent, upsert,
};

/// `CommandReceiptV2`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub command_type: String,
    pub accepted_at: UtcDateTime,
    pub result_sequence: u64,
    pub status: ReceiptStatus,
    pub error: Option<String>,
}

/// `ProjectCommandReceiptV2`: shares the receipt table with thread commands.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectCommandReceipt {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub command_type: String,
    pub accepted_at: UtcDateTime,
    pub result_sequence: u64,
    pub status: ReceiptStatus,
    pub error: Option<String>,
}

/// `AnyCommandReceiptV2`.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyReceipt {
    Thread(CommandReceipt),
    Project(ProjectCommandReceipt),
}

impl From<CommandReceipt> for AnyReceipt {
    fn from(receipt: CommandReceipt) -> Self {
        Self::Thread(receipt)
    }
}

impl From<ProjectCommandReceipt> for AnyReceipt {
    fn from(receipt: ProjectCommandReceipt) -> Self {
        Self::Project(receipt)
    }
}

impl AnyReceipt {
    fn to_row(&self) -> OrchestrationCommandReceipt {
        match self {
            Self::Thread(r) => OrchestrationCommandReceipt {
                command_id: r.command_id.clone(),
                aggregate_kind: AggregateKind::Thread,
                aggregate_id: r.thread_id.to_string(),
                command_type: r.command_type.clone(),
                accepted_at: r.accepted_at.to_string(),
                result_sequence: r.result_sequence as i64,
                status: r.status,
                error: r.error.clone(),
            },
            Self::Project(r) => OrchestrationCommandReceipt {
                command_id: r.command_id.clone(),
                aggregate_kind: AggregateKind::Project,
                aggregate_id: r.project_id.to_string(),
                command_type: r.command_type.clone(),
                accepted_at: r.accepted_at.to_string(),
                result_sequence: r.result_sequence as i64,
                status: r.status,
                error: r.error.clone(),
            },
        }
    }
}

fn decode_err(issue: impl Into<String>) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(
        "CommandReceiptStore.decode",
        issue,
    ))
}

fn accepted_at(row: &OrchestrationCommandReceipt) -> Result<UtcDateTime, PersistenceError> {
    row.accepted_at
        .parse()
        .map_err(|err: turin_contracts::ContractError| decode_err(err.to_string()))
}

fn result_sequence(row: &OrchestrationCommandReceipt) -> Result<u64, PersistenceError> {
    u64::try_from(row.result_sequence).map_err(|_| decode_err("negative result sequence"))
}

pub fn insert_if_absent_in(
    conn: &Connection,
    receipt: &AnyReceipt,
) -> Result<bool, PersistenceError> {
    insert_if_absent(conn, &receipt.to_row())
}

pub fn upsert_in(conn: &Connection, receipt: &AnyReceipt) -> Result<(), PersistenceError> {
    upsert(conn, &receipt.to_row())
}

/// A thread command's receipt; none when the command id belongs to a project command.
pub fn get_thread_in(
    conn: &Connection,
    command_id: &CommandId,
) -> Result<Option<CommandReceipt>, PersistenceError> {
    let Some(row) = get_by_command_id(conn, command_id)? else {
        return Ok(None);
    };
    if row.aggregate_kind != AggregateKind::Thread {
        return Ok(None);
    }
    Ok(Some(CommandReceipt {
        command_id: row.command_id.clone(),
        thread_id: ThreadId::new(&row.aggregate_id)
            .map_err(|err| decode_err(err.to_string()))?,
        command_type: row.command_type.clone(),
        accepted_at: accepted_at(&row)?,
        result_sequence: result_sequence(&row)?,
        status: row.status,
        error: row.error.clone(),
    }))
}

/// A project command's receipt; none when the command id belongs to a thread command.
pub fn get_project_in(
    conn: &Connection,
    command_id: &CommandId,
) -> Result<Option<ProjectCommandReceipt>, PersistenceError> {
    let Some(row) = get_by_command_id(conn, command_id)? else {
        return Ok(None);
    };
    if row.aggregate_kind != AggregateKind::Project {
        return Ok(None);
    }
    Ok(Some(ProjectCommandReceipt {
        command_id: row.command_id.clone(),
        project_id: ProjectId::new(&row.aggregate_id)
            .map_err(|err| decode_err(err.to_string()))?,
        command_type: row.command_type.clone(),
        accepted_at: accepted_at(&row)?,
        result_sequence: result_sequence(&row)?,
        status: row.status,
        error: row.error.clone(),
    }))
}

#[derive(Clone)]
pub struct CommandReceiptStore {
    db: Db,
}

impl CommandReceiptStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn insert_if_absent(
        &self,
        receipt: impl Into<AnyReceipt>,
    ) -> Result<bool, PersistenceError> {
        let receipt = receipt.into();
        self.db
            .write(move |tx| insert_if_absent_in(tx, &receipt))
            .await
    }

    pub async fn upsert(&self, receipt: impl Into<AnyReceipt>) -> Result<(), PersistenceError> {
        let receipt = receipt.into();
        self.db.write(move |tx| upsert_in(tx, &receipt)).await
    }

    pub async fn get_by_command_id(
        &self,
        command_id: CommandId,
    ) -> Result<Option<CommandReceipt>, PersistenceError> {
        self.db
            .read(move |conn| get_thread_in(conn, &command_id))
            .await
    }

    pub async fn get_project_by_command_id(
        &self,
        command_id: CommandId,
    ) -> Result<Option<ProjectCommandReceipt>, PersistenceError> {
        self.db
            .read(move |conn| get_project_in(conn, &command_id))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_receipt(id: &str) -> ProjectCommandReceipt {
        ProjectCommandReceipt {
            command_id: CommandId::new(id).unwrap(),
            project_id: ProjectId::new("project:test").unwrap(),
            command_type: "project.create".into(),
            accepted_at: "2026-01-01T00:00:00.000Z".parse().unwrap(),
            result_sequence: 1,
            status: ReceiptStatus::Accepted,
            error: None,
        }
    }

    #[tokio::test]
    async fn project_receipt_does_not_appear_as_thread_receipt() {
        let store = CommandReceiptStore::new(Db::open_in_memory().await.unwrap());
        let receipt = project_receipt("receipt:test");
        assert!(store.insert_if_absent(receipt.clone()).await.unwrap());
        assert!(
            store
                .get_by_command_id(receipt.command_id.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .get_project_by_command_id(receipt.command_id.clone())
                .await
                .unwrap(),
            Some(receipt)
        );
    }

    #[tokio::test]
    async fn thread_receipts_round_trip_and_insert_is_idempotent() {
        let store = CommandReceiptStore::new(Db::open_in_memory().await.unwrap());
        let receipt = CommandReceipt {
            command_id: CommandId::new("receipt:thread").unwrap(),
            thread_id: ThreadId::new("thread:test").unwrap(),
            command_type: "message.dispatch".into(),
            accepted_at: "2026-01-02T03:04:05.006Z".parse().unwrap(),
            result_sequence: 0,
            status: ReceiptStatus::Accepted,
            error: None,
        };
        assert!(store.insert_if_absent(receipt.clone()).await.unwrap());
        assert!(!store.insert_if_absent(receipt.clone()).await.unwrap());
        let updated = CommandReceipt {
            result_sequence: 7,
            ..receipt.clone()
        };
        store.upsert(updated.clone()).await.unwrap();
        assert_eq!(
            store
                .get_by_command_id(receipt.command_id.clone())
                .await
                .unwrap(),
            Some(updated)
        );
        assert!(
            store
                .get_project_by_command_id(receipt.command_id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
