//! turin-source-control: GitHub-only source control for Turin (see PORTING.md).
//!
//! Layout mirrors upstream:
//! - [`source_control_core`]: `packages/source-control-core` (driver port, host port, rate limit).
//! - [`github`]: `packages/source-control-github` (the GitHub driver).
//! - [`source_control`]: `apps/server/src/sourceControl` (registry, discovery, repository service).
//! - [`pull_request`]: `apps/server/src/pullRequest` (pull request services, caches, review guides).

// The contracts' tagged error types (SourceControlProviderError, PullRequestOperationError) are large
// by design (they carry the upstream fields); boxing them would change every signature.
#![allow(clippy::result_large_err)]

pub mod github;
pub mod pull_request;
pub mod shared;
pub mod source_control;
pub mod source_control_core;
