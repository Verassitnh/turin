//! Port of upstream/apps/server/src/provider/builtInProviderCatalog.ts.
//!
//! The type the aggregated snapshot list is built from: one entry per provider
//! instance. Several entries may share a driver kind, so `instance_id` is the
//! routing key and `driver_kind` only names the implementation.

use std::sync::Arc;

use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

use crate::{
    maintenance::ProviderMaintenanceCapabilities, rt::BoxStream, server_provider::ServerProvider,
    snapshot::ManagedServerProvider,
};

/// One provider instance's snapshot source. Upstream's `getSnapshot`, `refresh`,
/// and `streamChanges` read through the managed snapshot it wraps.
#[derive(Clone)]
pub struct ProviderSnapshotSource {
    /// Routing key — uniquely identifies this instance in the aggregated list.
    pub instance_id: ProviderInstanceId,
    /// Driver implementation kind.
    pub driver_kind: ProviderDriverKind,
    managed: Arc<dyn ManagedServerProvider>,
}

impl ProviderSnapshotSource {
    pub fn new(
        instance_id: ProviderInstanceId,
        driver_kind: ProviderDriverKind,
        managed: Arc<dyn ManagedServerProvider>,
    ) -> Self {
        Self {
            instance_id,
            driver_kind,
            managed,
        }
    }

    pub async fn get_snapshot(&self) -> ServerProvider {
        self.managed.get_snapshot().await
    }

    pub async fn refresh(&self) -> ServerProvider {
        self.managed.refresh().await
    }

    pub async fn resolve_maintenance(&self, fresh: bool) -> ProviderMaintenanceCapabilities {
        self.managed.resolve_maintenance(fresh).await
    }

    pub fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        self.managed.stream_changes()
    }
}
