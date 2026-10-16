//! The GitHub pull request views: port of upstream/apps/web/src/components/pullRequest.
//!
//! `*_logic` modules are the pure ports (with their upstream tests); the view modules render
//! from the view-model seam in `view_model` and never call the pull request service directly.

#![allow(dead_code)]
// Callback-heavy view props (`Rc<dyn Fn(..)>` in tuples/options) and `RenderOnce` impls that
// return a concrete element type are the idiom of every view here.
#![allow(clippy::type_complexity, refining_impl_trait)]

pub mod action_runner;
pub mod activity_unavailable;
pub mod checks_popover;
pub mod code_tab;
pub mod comment_body;
pub mod comment_form;
pub mod copyable_code;
pub mod demo;
pub mod desktop_notification;
pub mod detail_context;
pub mod detail_layout;
pub mod detail_panel;
pub mod dropdown;
pub mod edit_button;
pub mod fixtures;
pub mod guide_excerpt_expansion;
pub mod guide_run_coordinator;
pub mod guide_run_store;
pub mod guide_tab;
pub mod guide_view;
pub mod link_dialog;
pub mod link_preview;
pub mod link_pull_request_dialog_logic;
pub mod list_close_sweep;
pub mod list_filters;
pub mod list_page;
pub mod list_row;
pub mod list_states;
pub mod live_source;
pub mod mock_source;
pub mod panel_host;
pub mod pickers;
pub mod presentation_view;
pub mod pull_request_detail_logic;
pub mod pull_request_diff_logic;
pub mod pull_request_editing_logic;
pub mod pull_request_file_order_logic;
pub mod pull_request_files_viewed_logic;
pub mod pull_request_list_lines;
pub mod pull_request_list_logic;
pub mod pull_request_list_preferences;
pub mod pull_request_markdown_logic;
pub mod pull_request_presentation;
pub mod pull_request_project_assignment_logic;
pub mod pull_request_project_filter_logic;
pub mod pull_request_reactions_logic;
pub mod pull_request_review_store;
pub mod pull_request_stack_snapshot;
pub mod pull_request_summary_scroll_logic;
pub mod pull_requests_route_search;
pub mod relative_time;
pub mod review_annotation;
pub mod review_form;
pub mod shared_summary;
pub mod speed_actions;
pub mod stack_popover;
pub mod summary_tab;
pub mod thread_links;
pub mod thread_panel;
pub mod timeline_tab;
pub mod toast;
pub mod view_model;
