//! Port of upstream/apps/server/src/provider/builtInDrivers.ts.
//!
//! The static set of `ProviderDriver`s this build ships with. The instance
//! registry resolves `providerInstances` entries against this list; anything
//! not in it surfaces as an `unavailable` shadow snapshot.
//!
//! Upstream lists Codex, Claude, Cursor, Grok, OpenCode, Antigravity, Pi, Muse
//! and the ACP registry. Grok, Antigravity, Pi, Muse and the ACP registry are
//! cut. Claude and Cursor are constructed here. Codex and OpenCode drivers are
//! not on main yet, so they plug in through [`DriverFactory`] slots: when
//! `turin-provider-codex` merges, the app fills `slots.codex` with a closure
//! that builds the Codex driver from the host. An empty slot leaves that kind
//! out, and its configured instances show as unavailable until it is filled.

use std::sync::Arc;

use turin_provider_claude::claude_driver::ClaudeDriver;
use turin_provider_core::{driver::ProviderDriver, provider_host::ProviderHost};
use turin_provider_cursor::CursorDriver;

/// Builds one driver from the shared host. Called once, at boot.
pub type DriverFactory = Box<dyn FnOnce(Arc<ProviderHost>) -> Arc<dyn ProviderDriver> + Send>;

/// Drivers whose crates are not wired into this module yet.
#[derive(Default)]
pub struct BuiltInDriverSlots {
    /// `turin-provider-codex` (task T09), driver kind `codex`.
    pub codex: Option<DriverFactory>,
    /// `turin-provider-opencode` driver (task T11d), driver kind `opencode`.
    pub opencode: Option<DriverFactory>,
}

fn build_slot(
    slot: Option<DriverFactory>,
    expected_kind: &str,
    host: &Arc<ProviderHost>,
) -> Option<Arc<dyn ProviderDriver>> {
    let driver = slot?(Arc::clone(host));
    assert_eq!(
        driver.driver_kind().as_str(),
        expected_kind,
        "the {expected_kind} driver slot built a driver of another kind",
    );
    Some(driver)
}

/// Upstream `BUILT_IN_DRIVERS`, in the same order. Order only breaks ties in
/// UI presentation; the instance registry is keyed by driver kind.
pub fn built_in_drivers(
    host: Arc<ProviderHost>,
    slots: BuiltInDriverSlots,
) -> Vec<Arc<dyn ProviderDriver>> {
    let mut drivers: Vec<Arc<dyn ProviderDriver>> = Vec::new();
    drivers.extend(build_slot(slots.codex, "codex", &host));
    drivers.push(Arc::new(ClaudeDriver::new(Arc::clone(&host))));
    drivers.push(Arc::new(CursorDriver::new()));
    drivers.extend(build_slot(slots.opencode, "opencode", &host));
    drivers
}

/// The drivers that keep usage history, in the order the usage page reads
/// them. Upstream also lists Grok, Pi, and Antigravity (cut). Aggregation keeps
/// the first copy of a duplicate record, so the order is part of the result.
/// A driver whose `usage()` is `None` has nothing to read yet.
pub const BUILT_IN_USAGE_DRIVER_ORDER: [&str; 4] = ["claudeAgent", "codex", "opencode", "cursor"];

pub fn built_in_usage_drivers(drivers: &[Arc<dyn ProviderDriver>]) -> Vec<Arc<dyn ProviderDriver>> {
    BUILT_IN_USAGE_DRIVER_ORDER
        .iter()
        .filter_map(|kind| {
            drivers
                .iter()
                .find(|driver| driver.driver_kind().as_str() == *kind)
                .cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! No upstream test file; `providerCompatibility.test.ts` ("bundles a
    //! compatibility policy for every built-in harness") reads this list, which
    //! the compatibility check here covers for the kept drivers.

    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use serde_json::Value;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
    use turin_provider_core::{
        driver::{
            ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance, driver_error,
        },
        errors::ProviderDriverError,
        model_manifest::bundled_model_manifest,
        provider_host_live::{DesktopProviderHost, ShouldRunBackgroundWork},
    };

    use super::*;

    struct FakeDriver {
        kind: ProviderDriverKind,
        metadata: ProviderDriverMetadata,
    }

    impl FakeDriver {
        fn arc(kind: &str) -> Arc<dyn ProviderDriver> {
            Arc::new(Self {
                kind: ProviderDriverKind::new(kind).unwrap(),
                metadata: ProviderDriverMetadata::new(kind),
            })
        }
    }

    #[async_trait]
    impl ProviderDriver for FakeDriver {
        fn driver_kind(&self) -> &ProviderDriverKind {
            &self.kind
        }

        fn metadata(&self) -> &ProviderDriverMetadata {
            &self.metadata
        }

        fn default_config(&self) -> Value {
            Value::Object(Default::default())
        }

        fn decode_config(&self, raw: &Value) -> Result<Value, String> {
            Ok(raw.clone())
        }

        async fn create(
            &self,
            input: ProviderDriverCreateInput,
        ) -> Result<ProviderInstance, ProviderDriverError> {
            Err(driver_error(&self.kind, &input.instance_id, "fake driver"))
        }
    }

    fn host(dir: &std::path::Path) -> Arc<ProviderHost> {
        let should_run: ShouldRunBackgroundWork = Arc::new(|_scope| Box::pin(async { true }));
        let live = DesktopProviderHost::new(
            dir,
            dir,
            turin_contracts::settings::default_server_settings(),
            should_run,
        )
        .expect("desktop host opens");
        Arc::new(live.host)
    }

    fn kinds(drivers: &[Arc<dyn ProviderDriver>]) -> Vec<String> {
        drivers
            .iter()
            .map(|driver| driver.driver_kind().to_string())
            .collect()
    }

    fn factory(kind: &'static str) -> DriverFactory {
        Box::new(move |_host| FakeDriver::arc(kind))
    }

    #[tokio::test]
    async fn lists_the_kept_drivers_in_upstream_order() {
        let dir = tempfile::tempdir().unwrap();
        let drivers = built_in_drivers(
            host(dir.path()),
            BuiltInDriverSlots {
                codex: Some(factory("codex")),
                opencode: Some(factory("opencode")),
            },
        );
        assert_eq!(
            kinds(&drivers),
            ["codex", "claudeAgent", "cursor", "opencode"]
        );
    }

    #[tokio::test]
    async fn an_empty_slot_leaves_its_kind_out() {
        let dir = tempfile::tempdir().unwrap();
        let drivers = built_in_drivers(host(dir.path()), BuiltInDriverSlots::default());
        assert_eq!(kinds(&drivers), ["claudeAgent", "cursor"]);
    }

    #[tokio::test]
    #[should_panic(expected = "codex driver slot built a driver of another kind")]
    async fn a_slot_must_build_its_own_kind() {
        let dir = tempfile::tempdir().unwrap();
        built_in_drivers(
            host(dir.path()),
            BuiltInDriverSlots {
                codex: Some(factory("opencode")),
                opencode: None,
            },
        );
    }

    #[tokio::test]
    async fn usage_drivers_follow_the_usage_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let drivers = built_in_drivers(
            host(dir.path()),
            BuiltInDriverSlots {
                codex: Some(factory("codex")),
                opencode: Some(factory("opencode")),
            },
        );
        assert_eq!(
            kinds(&built_in_usage_drivers(&drivers)),
            ["claudeAgent", "codex", "opencode", "cursor"]
        );
    }

    #[tokio::test]
    async fn every_built_in_driver_has_a_default_instance_id_equal_to_its_kind() {
        // The status cache file name and `refresh(kind)` both rely on this.
        let dir = tempfile::tempdir().unwrap();
        for driver in built_in_drivers(host(dir.path()), BuiltInDriverSlots::default()) {
            let id: ProviderInstanceId =
                turin_contracts::provider_instance::default_instance_id_for_driver(
                    driver.driver_kind(),
                );
            assert_eq!(id.as_str(), driver.driver_kind().as_str());
        }
    }

    #[test]
    fn the_bundled_manifest_carries_a_compatibility_policy_for_each_kept_harness() {
        let manifest = bundled_model_manifest();
        let policies =
            turin_provider_core::provider_compatibility::decode_provider_compatibility_policies(
                manifest.compatibility.as_deref().unwrap_or_default(),
            );
        for kind in ["codex", "claudeAgent", "cursor", "opencode"] {
            assert!(
                policies.iter().any(|policy| policy.driver.as_str() == kind),
                "no bundled compatibility policy for {kind}"
            );
        }
    }
}
