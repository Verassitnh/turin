//! Port of upstream/packages/provider-core/src/server/snapshot.ts.
//!
//! One managed snapshot per provider instance. Drivers publish probes through
//! this; the registry does not poll them itself.

use async_trait::async_trait;
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;

use crate::{
    maintenance::ProviderMaintenanceCapabilities, rt::BoxStream, server_provider::ServerProvider,
};

#[async_trait]
pub trait ManagedServerProvider: Send + Sync {
    /// Ownership-derived update capabilities. `fresh` is the click path: it
    /// must not trust a resolution older than the click.
    async fn resolve_maintenance(&self, fresh: bool) -> ProviderMaintenanceCapabilities;

    async fn get_snapshot(&self) -> ServerProvider;

    async fn refresh(&self) -> ServerProvider;

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider>;

    /// Fold a runtime rate-limit update into the published snapshot without
    /// waiting for the next status probe. Sparse: windows merge by id and an
    /// update with no usable window leaves the snapshot untouched.
    async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str);
}
