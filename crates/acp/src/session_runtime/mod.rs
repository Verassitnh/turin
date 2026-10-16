//! Port of upstream/packages/provider-acp/src/server/AcpSessionRuntime.ts (process control helpers).

mod auth;
mod error;
mod linux_cgroup;
mod posix_tree;
mod process_group;
mod types;

#[cfg(test)]
mod port_tests;

pub use auth::{AcpAuthMethod, select_acp_agent_auth_method};
pub use error::AcpProcessGroupTerminationError;
pub use linux_cgroup::{
    Cgroup2Mount, CgroupPopulatedState, SweepStaleLinuxCgroupSiblingsOptions, is_safe_cgroup_path,
    make_linux_cgroup_controller, parse_cgroup2_mount, parse_cgroup2_mounts,
    parse_unified_cgroup_path, resolve_linux_cgroup_target_command, signal_linux_cgroup_root_term,
    sweep_stale_linux_cgroup_siblings, terminate_linux_cgroup_lease, wrap_command_for_linux_cgroup,
};
pub use posix_tree::{
    TerminatePosixOwnedProcessTreeInput, capture_posix_ownership_ledger,
    make_posix_process_tree_controller, observe_posix_ownership_ledger,
    observe_posix_ownership_ledger_continuously, posix_process_is_zombie,
    terminate_posix_owned_process_tree,
};
pub use process_group::signal_process_group;
pub use types::{
    AcpLinuxCgroupController, AcpLinuxCgroupLease, AcpOwnedPosixProcess, AcpPosixOwnershipRoot,
    AcpPosixProcessIdentity, AcpPosixProcessTreeController,
};
