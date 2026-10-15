//! Port of upstream/apps/server/src/orchestration-v2/CommandReceiptStore.ts
//!
//! Typed thread/project access to the shared receipt table. Transactional
//! callers use the `*_in` methods; the sink composes those methods with event
//! and projection writes in one `Db::write`.

use rusqlite::Connection;
use turin_contracts::{CommandId, ProjectId, ThreadId};
use turin_persistence::{AggregateKind, Db, OrchestrationCommandReceipt, PersistenceDecodeError, PersistenceError, get_by_command_id, insert_if_absent, upsert};

#[derive(Debug, Clone, PartialEq)]
pub enum CommandReceipt {
    Thread { thread_id: ThreadId, row: OrchestrationCommandReceipt },
    Project { project_id: ProjectId, row: OrchestrationCommandReceipt },
}

#[derive(Clone)]
pub struct CommandReceiptStore { db: Db }
impl CommandReceiptStore {
    pub fn new(db: Db) -> Self { Self { db } }
    pub fn insert_if_absent_in(conn: &Connection, receipt: &OrchestrationCommandReceipt) -> Result<bool, PersistenceError> { insert_if_absent(conn, receipt) }
    pub fn upsert_in(conn: &Connection, receipt: &OrchestrationCommandReceipt) -> Result<(), PersistenceError> { upsert(conn, receipt) }
    pub fn get_in(conn: &Connection, id: &CommandId) -> Result<Option<CommandReceipt>, PersistenceError> {
        get_by_command_id(conn, id)?.map(|row| {
            let invalid = |err: turin_contracts::ContractError| PersistenceError::Decode(PersistenceDecodeError::new("CommandReceiptStore.decode", err.to_string()));
            match row.aggregate_kind {
                AggregateKind::Thread => Ok(CommandReceipt::Thread { thread_id: ThreadId::new(&row.aggregate_id).map_err(invalid)?, row }),
                AggregateKind::Project => Ok(CommandReceipt::Project { project_id: ProjectId::new(&row.aggregate_id).map_err(invalid)?, row }),
            }
        }).transpose()
    }
    pub async fn get(&self, id: CommandId) -> Result<Option<CommandReceipt>, PersistenceError> {
        self.db.read(move |conn| Self::get_in(conn, &id)).await
    }
    pub async fn get_thread(&self, id: CommandId) -> Result<Option<OrchestrationCommandReceipt>, PersistenceError> {
        Ok(match self.get(id).await? { Some(CommandReceipt::Thread { row, .. }) => Some(row), _ => None })
    }
    pub async fn get_project(&self, id: CommandId) -> Result<Option<OrchestrationCommandReceipt>, PersistenceError> {
        Ok(match self.get(id).await? { Some(CommandReceipt::Project { row, .. }) => Some(row), _ => None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_persistence::ReceiptStatus;
    #[tokio::test]
    async fn project_receipt_does_not_appear_as_thread_receipt() {
        let db = Db::open_in_memory().await.unwrap();
        let store = CommandReceiptStore::new(db.clone());
        let id = CommandId::new("receipt:test").unwrap();
        let row = OrchestrationCommandReceipt {
            command_id: id.clone(), aggregate_kind: AggregateKind::Project, aggregate_id: "project:test".into(),
            command_type: "project.create".into(), accepted_at: "2026-01-01T00:00:00.000Z".into(),
            result_sequence: 1, status: ReceiptStatus::Accepted, error: None,
        };
        db.write(move |conn| { assert!(CommandReceiptStore::insert_if_absent_in(conn, &row)?); Ok(()) }).await.unwrap();
        assert!(store.get_thread(id.clone()).await.unwrap().is_none());
        assert!(store.get_project(id).await.unwrap().is_some());
    }
}
