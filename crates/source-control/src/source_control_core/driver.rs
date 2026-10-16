//! Port of upstream/packages/source-control-core/src/server/driver.ts
//!
//! One driver per host kind. The app lists its drivers once and both registries (repository
//! operations and pull requests) iterate that list. Upstream's `make` yields what it needs from
//! the environment through Effect's `R` channel; here `make` is an async constructor that takes
//! the dependencies explicitly (see `github::driver`).

use std::sync::Arc;

use turin_contracts::source_control::SourceControlProviderKind;

use super::discovery::SourceControlProviderDiscoverySpec;
use super::pull_request_provider::PullRequestProviderApi;
use super::source_control_provider::SourceControlProvider;

pub struct SourceControlDriverInstance {
    pub source_control: Arc<dyn SourceControlProvider>,
    pub discovery: SourceControlProviderDiscoverySpec,
    /// `None` for a host whose pull requests this build cannot read.
    pub pull_requests: Option<Arc<dyn PullRequestProviderApi>>,
}

pub struct SourceControlDriver {
    pub kind: SourceControlProviderKind,
    pub instance: SourceControlDriverInstance,
}
