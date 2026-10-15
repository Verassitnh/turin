//! Port of upstream/packages/effect-acp (Agent Client Protocol client), see PORTING.md

pub mod agent;
pub mod client;
pub mod compat;
pub mod cursor;
pub mod errors;
pub mod meta;
pub mod process;
pub mod protocol;
pub mod schema;

pub use agent::AcpAgent;
pub use client::AcpClient;
pub use compat::{
    ClientCapabilities, ImplementationInfo, InitializeRequest, InitializeResponse,
    ProtocolGeneration,
};
pub use errors::AcpError;
pub use process::{SpawnCommand, spawn};
