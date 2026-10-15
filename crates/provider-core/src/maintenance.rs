//! Port of upstream/packages/provider-core/src/server/maintenance.ts.

use std::{cmp::Ordering, sync::Arc};

use turin_contracts::provider_instance::ProviderDriverKind;

use crate::host_process::ProcessEnv;

/// Compare native release revisions when the provider does not use plain semver.
pub type CompareVersions = Arc<dyn Fn(&str, &str) -> Ordering + Send + Sync>;

#[derive(Clone)]
pub struct ProviderMaintenanceCapabilities {
    pub provider: ProviderDriverKind,
    pub package_name: Option<String>,
    pub update: Option<ProviderMaintenanceCommandAction>,
    /// Latest version reported by the installer that owns the executable.
    /// `None` means the installer has no channel of its own and the npm
    /// registry entry for `package_name` is authoritative; `Some(None)` means
    /// the installer was asked and did not know.
    pub latest_version: Option<Option<String>>,
    /// Compare native release revisions when the provider does not use plain semver.
    pub compare_versions: Option<CompareVersions>,
}

impl std::fmt::Debug for ProviderMaintenanceCapabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderMaintenanceCapabilities")
            .field("provider", &self.provider)
            .field("package_name", &self.package_name)
            .field("update", &self.update)
            .field("latest_version", &self.latest_version)
            .field("compare_versions", &self.compare_versions.is_some())
            .finish()
    }
}

impl PartialEq for ProviderMaintenanceCapabilities {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.package_name == other.package_name
            && self.update == other.update
            && self.latest_version == other.latest_version
            && match (&self.compare_versions, &other.compare_versions) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMaintenanceCommandAction {
    pub command: String,
    pub executable: String,
    pub args: Vec<String>,
    pub lock_key: String,
    /// Extra environment for the spawned updater, on top of the server's own.
    /// A native updater finds its install through the same variables the
    /// provider runs with (e.g. `CODEX_HOME`), so an instance with a custom home
    /// must update that home and not the default one.
    pub env: Option<ProcessEnv>,
}
