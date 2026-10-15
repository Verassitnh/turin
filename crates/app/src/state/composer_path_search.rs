//! Port of upstream/packages/client-runtime/src/state/composerPathSearch.ts
//!
//! Types only: the `@` path-mention search state the composer reads. The
//! search itself (an RPC atom in upstream) is the composer's job to run
//! in-process, so only the shapes that cross into the UI are kept here.

use super::contracts::EnvironmentId;

/// Whether a search hit is a file or a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComposerPathSearchKind {
    File,
    Directory,
}

/// One row of the path search results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPathSearchEntry {
    pub path: String,
    pub kind: ComposerPathSearchKind,
    /// Upstream's optional `parentPath`; `None` when the entry sits at the root.
    pub parent_path: Option<String>,
}

/// The search result the composer renders: rows plus the request status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPathSearchState {
    pub entries: Vec<ComposerPathSearchEntry>,
    pub is_pending: bool,
    /// Upstream's `string | null`; `None` means no error.
    pub error: Option<String>,
}

/// What a search runs against. A missing environment, cwd or query means no
/// search is issued (upstream's `null` fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerPathSearchTarget {
    pub environment_id: Option<EnvironmentId>,
    pub cwd: Option<String>,
    pub query: Option<String>,
}
