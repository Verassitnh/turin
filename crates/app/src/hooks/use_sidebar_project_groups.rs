//! Port of upstream/apps/web/src/hooks/useSidebarProjectGroups.ts (pure assembly).

#![allow(dead_code, unused_imports)]

pub use crate::components::sidebar::sidebar_logic::{
    ProjectSortOrder, order_items_by_preferred_ids, sort_sidebar_v2_project_groups,
};
pub use crate::logical_project::{get_project_order_key, select_project_grouping_settings};
pub use crate::state::sidebar_project_grouping::{
    SidebarProjectSnapshot, build_sidebar_project_snapshots,
};
