//! Port of upstream/packages/client-runtime/src/operations: the pure
//! command-building parts. Dispatch goes through [`commands::CommandPort`],
//! which the orchestrator implements in-process.

pub mod commands;
pub mod projects;
pub mod thread_title;
