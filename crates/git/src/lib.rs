//! turin-git (see PORTING.md for upstream mapping)

pub mod checkpointing;
pub mod detach_stack_frame;
pub mod git_manager;
pub mod git_vcs_driver;
pub mod git_vcs_driver_core;
pub mod git_workflow_service;
pub mod link_created_pull_request;
pub mod locale_compare;
pub mod process;
pub mod remote_refs;
pub mod server_source_control_host;
pub mod shared_git;
pub mod source_control_bridge;
pub mod vcs_driver;
pub mod vcs_driver_registry;
pub mod vcs_process;
pub mod vcs_project_config;
pub mod vcs_provisioning_service;
pub mod vcs_status_broadcaster;
pub mod workspace;
