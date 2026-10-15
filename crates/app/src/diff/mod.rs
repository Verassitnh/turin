//! The diff panel (upstream `DiffPanel.tsx` and friends), its pure logic and
//! the ports of the `@pierre/diffs` pieces it renders with.

pub mod base_ref_choices;
pub mod comment_submit_shortcut;
pub mod diff_collapse;
pub mod diff_file_actions;
pub mod diff_file_tree_logic;
pub mod diff_panel_store;
pub mod diff_rendering;
pub mod mock_fixtures;
pub mod mock_source;
pub mod pierre;
pub mod review_comment_context;
pub mod rows;
pub mod theme;
pub mod view_model;
