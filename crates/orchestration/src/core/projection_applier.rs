//! `ProjectionApplier` over the projection store: the write side folds each
//! committed event into the read tables inside the same transaction. Upstream
//! applies the projection in the Effect layer graph; `EventSink` takes this
//! trait instead, so the write pipeline does not depend on the read side
//! (PORTING.md, `pipeline/event_sink.rs` ProjectionApplier row).
//!
//! The applier calls `projection::store::apply::apply_event`,
//! `turn_item_position_store::normalize_in_tx` and `ProjectStore::apply_in_tx`
//! with the caller's transaction, so no write opens a second connection.

use rusqlite::Transaction;
use turin_contracts::orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2TurnItem};
use turin_persistence::{PersistenceError, ProjectEvent};

use crate::pipeline::event_sink::ProjectionApplier;
use crate::projection::project_store::ProjectStore;
use crate::projection::store::{
    ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION, ProjectionStore, apply,
};
use crate::projection::turn_item_position_store;

impl ProjectionApplier for ProjectionStore {
    fn normalize_turn_item(
        &self,
        tx: &Transaction<'_>,
        mut event: OrchestrationV2DomainEvent,
        run_ordinal: Option<u64>,
    ) -> Result<OrchestrationV2DomainEvent, PersistenceError> {
        if let OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } = &mut event {
            let item: OrchestrationV2TurnItem = payload.clone();
            *payload = turn_item_position_store::normalize_in_tx(tx, item, run_ordinal)?;
        }
        Ok(event)
    }

    fn apply_event(
        &self,
        tx: &Transaction<'_>,
        event: &OrchestrationV2DomainEvent,
    ) -> Result<(), PersistenceError> {
        apply::apply_event(tx, event).map_err(PersistenceError::from)
    }

    fn apply_project_event(
        &self,
        tx: &Transaction<'_>,
        event: &ProjectEvent,
    ) -> Result<(), PersistenceError> {
        ProjectStore::apply_in_tx(tx, event)
    }

    fn schema_version(&self) -> i64 {
        i64::try_from(ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION).unwrap_or(i64::MAX)
    }
}
