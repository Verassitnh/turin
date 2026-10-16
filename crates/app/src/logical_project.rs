//! Port of upstream/apps/web/src/logicalProject.ts (re-exports from client-runtime
//! `state/project-grouping`).

pub use crate::state::project_grouping::{
    BuildProjectGroupsInput, DeriveLogicalProjectKeyOptions, GroupableProject, ProjectGroup,
    ProjectGroupMember, ProjectGroupingMode, ProjectGroupingSettings, build_project_groups,
    derive_logical_project_key, derive_logical_project_key_from_settings,
    derive_physical_project_key, derive_physical_project_key_from_path,
    derive_project_grouping_override_key, get_project_order_key, resolve_project_grouping_mode,
    select_project_grouping_settings,
};
