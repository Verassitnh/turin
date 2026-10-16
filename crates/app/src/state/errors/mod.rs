//! Ports of upstream/packages/client-runtime/src/errors.
//!
//! `errorTrace.ts` and `safeLog.ts` read `cause`, `traceId`, `_tag` and
//! `errors` off arbitrary JS values. Rust errors are typed, so the shape those
//! two modules look at is [`error_graph::ErrorGraph`]: an arena of nodes with
//! the same properties, which keeps cyclic causes expressible. The
//! `orchestration.ts` predicates downcast a real error instead.

pub mod error_graph;
pub mod error_trace;
pub mod orchestration;
pub mod safe_log;
