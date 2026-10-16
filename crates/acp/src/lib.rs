//! Port of upstream/packages/effect-acp (Agent Client Protocol client), see PORTING.md

pub mod agent;
pub mod client;
pub mod client_policy;
pub mod compat;
pub mod cursor;
pub mod errors;
pub mod meta;
pub mod process;
pub mod protocol;
pub mod runtime_model;
pub mod schema;
pub mod session_config;
pub mod session_runtime;
pub mod stderr;

pub use agent::AcpAgent;
pub use client::AcpClient;
pub use compat::{
    ClientCapabilities, ImplementationInfo, InitializeRequest, InitializeResponse,
    ProtocolGeneration,
};
pub use errors::AcpError;
pub use process::{SpawnCommand, spawn};
pub use session_runtime::{
    AcpAuthMethod, AcpLinuxCgroupLease, AcpOwnedPosixProcess, AcpPosixOwnershipRoot,
    AcpPosixProcessIdentity, AcpPosixProcessTreeController, AcpProcessGroupTerminationError,
    TerminatePosixOwnedProcessTreeInput, capture_posix_ownership_ledger,
    make_posix_process_tree_controller, observe_posix_ownership_ledger, parse_unified_cgroup_path,
    select_acp_agent_auth_method, signal_process_group, terminate_posix_owned_process_tree,
    wrap_command_for_linux_cgroup,
};
