//! Port of upstream/apps/server/src/orchestration-v2/ProjectionStore.ts
//!
//! The thread read model. Writes go through [`apply::apply_event`] inside the
//! event sink's transaction; reads run on the database's read connection and
//! never touch the UI thread. See `apply.rs` for the seam the pipeline uses.

pub mod apply;

use turin_persistence::Db;

use crate::projection::error::ProjectionStoreError;
use crate::projection::event::event_type;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;

/// `ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION`.
pub const ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION: u64 = 2;

/// Handle on the projection tables. Cheap to clone.
#[derive(Clone)]
pub struct ProjectionStore {
    db: Db,
}

impl ProjectionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// `apply` in its own write transaction. The event sink does not use this:
    /// it calls [`apply::apply_event`] inside the transaction that appends the
    /// event, so the event and its projection commit together.
    pub async fn apply(
        &self,
        event: OrchestrationV2DomainEvent,
    ) -> Result<(), ProjectionStoreError> {
        let result = self
            .db
            .write(move |tx| Ok(apply::apply_event(tx, &event)))
            .await
            .map_err(ProjectionStoreError::Database)?;
        result
    }
}

#[allow(dead_code)]
fn _keep(event: &OrchestrationV2DomainEvent) -> &'static str {
    event_type(event)
}
