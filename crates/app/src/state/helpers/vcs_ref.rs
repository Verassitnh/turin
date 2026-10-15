//! Port of upstream/packages/client-runtime/src/state/vcsRef.ts

use turin_contracts::EnvironmentId;

/// `VcsRefTarget`. `query` is `undefined | null | string` upstream, hence the nesting: the outer
/// `Option` is "not provided".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VcsRefTarget {
    pub environment_id: Option<EnvironmentId>,
    pub cwd: Option<String>,
    pub query: Option<Option<String>>,
}

/// `VcsRef` is the contract type.
pub type VcsRef = turin_contracts::git::VcsRef;
