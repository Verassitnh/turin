//! Port of upstream/packages/effect-acp (Agent Client Protocol client), see PORTING.md

pub mod compat;
pub mod cursor;
pub mod errors;
pub mod meta;
pub mod protocol;
pub mod schema;

pub use errors::AcpError;
