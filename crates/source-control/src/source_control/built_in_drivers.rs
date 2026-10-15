//! Port of upstream/apps/server/src/sourceControl/builtInDrivers.ts (GitHub only)
//!
//! The source control drivers this build ships with. Upstream lists six hosts and provides their
//! services' layers here; Turin supports GitHub only, so the list holds one driver. Upstream's
//! `layer` is the server's Effect wiring: the GitHub transport is built once by
//! [`github::driver::build`] and handed back so every GitHub reader shares it.

use std::sync::Arc;

use crate::github::api::{GitHubApiDependencies, GitHubHttp};
use crate::github::driver;
use crate::shared::host_process::Environment;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::driver::SourceControlDriver;
use crate::source_control_core::host::SourceControlHost;

/// The drivers this build ships with, in discovery order, and the transport the GitHub driver
/// reads through.
pub struct BuiltInSourceControlDrivers {
    pub drivers: Vec<SourceControlDriver>,
    /// The credentials, quota and rate-limit pauses every GitHub reader should share.
    pub github_dependencies: GitHubApiDependencies,
}

/// Builds every built-in driver over the one host, environment, clock and HTTP client. Pass
/// `ReqwestGitHubHttp::new()` as `http` outside tests.
pub async fn build_built_in_drivers(
    host: Arc<dyn SourceControlHost>,
    environment: Arc<dyn Environment>,
    clock: Arc<dyn Clock>,
    http: Arc<dyn GitHubHttp>,
) -> BuiltInSourceControlDrivers {
    let github = driver::build(host, environment, clock, http).await;
    BuiltInSourceControlDrivers {
        drivers: vec![github.driver],
        github_dependencies: github.dependencies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::api::ReqwestGitHubHttp;
    use crate::shared::host_process::MapEnvironment;
    use crate::source_control_core::clock::ManualClock;
    use crate::source_control_core::test_host::ScriptedHost;

    #[tokio::test]
    async fn ships_github_as_the_only_built_in_driver() {
        let built = build_built_in_drivers(
            Arc::new(ScriptedHost::default()),
            MapEnvironment::new([("GH_TOKEN", "t")]),
            ManualClock::new(0),
            Arc::new(ReqwestGitHubHttp::new()),
        )
        .await;
        let kinds: Vec<&str> = built
            .drivers
            .iter()
            .map(|driver| driver.kind.as_ref())
            .collect();
        assert_eq!(kinds, ["github"]);
    }
}
