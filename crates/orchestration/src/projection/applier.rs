//! The production [`ProjectionApplier`]: folds committed events into the
//! projection tables inside the event sink's write transaction.
//!
//! Upstream runs `ProjectionStore.apply` and `TurnItemPositionStore.normalize`
//! in the same `sql.withTransaction` as the event append. Until T22a this
//! seam had only test doubles, so no write could reach the read model. This
//! type is a thin delegate: every rule stays in the store modules it calls.

use rusqlite::Transaction;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_persistence::{PersistenceError, ProjectEvent};

use crate::pipeline::event_sink::ProjectionApplier;
use crate::projection::project_store::ProjectStore;
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::store::apply;
use crate::projection::turn_item_position_store;

/// Stateless: the projection tables are the only state.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProjectionWriter;

impl ProjectionApplier for ProjectionWriter {
    fn normalize_turn_item(
        &self,
        tx: &Transaction<'_>,
        mut event: OrchestrationV2DomainEvent,
        run_ordinal: Option<u64>,
    ) -> Result<OrchestrationV2DomainEvent, PersistenceError> {
        if let OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } = &mut event {
            *payload = turn_item_position_store::normalize_in_tx(tx, payload.clone(), run_ordinal)?;
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
        ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION as i64
    }
}
