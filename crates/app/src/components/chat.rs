//! Ports of upstream/apps/web/src/components/chat.

#[allow(dead_code)]
pub mod agent_elapsed;
// Needs a GPUI text selection to drive it; mounted from the demo until then.
#[allow(dead_code)]
pub mod agent_spawn_summary;
#[allow(dead_code)]
pub mod assistant_citation_navigation;
#[allow(dead_code)]
pub mod assistant_selection_toolbar;
pub mod changed_files_tree;
pub mod chat_canvas;
#[allow(dead_code)]
pub mod chat_canvas_context;
pub mod chat_git_host;
pub mod chat_header;
pub mod chat_overlay_banners;
#[allow(dead_code)]
pub mod context_window;
// Chat-lane geometry for the canvas.
pub mod chat_canvas_layout;
pub mod provider_status_banner;
// Replaces the composer on a provider-native subagent thread; no thread route reads it yet.
#[allow(dead_code)]
pub mod provider_subagent_bar;
pub mod subagent_tooltip_content;
pub mod thread_error_banner;
// The composer notice for a usage-limit stop; the composer source owns its action state.
#[allow(dead_code)]
pub mod thread_lineage_workflow_row;
#[allow(dead_code)]
pub mod thread_relationship_icon;
#[allow(dead_code)]
pub mod thread_relationships_control;
#[allow(dead_code)]
pub mod usage_limit_recovery_banner;
// Pure citation-selector text logic; the DOM capture half is not ported yet.
#[allow(dead_code)]
pub mod assistant_text_selection;
// Pure citation-target rules; the chat view does not drive the navigation yet.
#[allow(dead_code)]
pub mod assistant_citation_target;
pub mod composer_pending_panels;
// Embedded-script scan for command and tool views; no view calls it yet.
#[allow(dead_code)]
pub mod embedded_scripts;
// The link context menu; the markdown link views do not open it yet.
#[allow(dead_code)]
pub mod external_link_context_menu;
// Copy-as-markdown for selections over rendered blocks; nothing calls it yet.
pub mod markdown;
#[allow(dead_code)]
pub mod markdown_clipboard;
pub mod message_copy_button;
pub mod messages_timeline;
pub mod open_in_picker;
pub mod panel_layout_controls;
// Pure scroll state machine; the timeline view does not drive page keys yet.
#[allow(dead_code)]
pub mod page_scroll_controller;
// Pure answer-draft logic; the pending-user-input card still keeps its drafts
// in element state, so nothing calls these yet.
#[allow(dead_code)]
pub mod pending_user_input;
// The ported timeline read model keeps upstream's full helper surface; the
// view and T14's reducers call into it piecemeal.
pub mod thread_details_card;
pub mod thread_details_card_layout;
pub mod thread_details_control;
pub mod thread_details_demo;
pub mod thread_details_panel;
pub mod thread_details_panel_styles;
pub mod thread_details_pr_row;
pub mod thread_details_pr_rows;
pub mod thread_details_section;
pub mod thread_relationships_panel;
#[allow(dead_code)]
pub mod thread_status_line;
#[allow(dead_code)]
pub mod timeline;
pub mod timeline_message_rows;
#[allow(dead_code)]
pub mod timeline_minimap_items;
// Gesture routing for the minimap and the timeline; the views do not call it yet.
#[allow(dead_code)]
pub mod timeline_minimap_wheel;
#[allow(dead_code)]
pub mod timeline_scroll_target;
pub mod timeline_work_rows;
// Scroll anchoring state for the timeline view; the view does not read it yet.
#[allow(dead_code)]
pub mod timeline_scroll_anchoring;
// Selection-action placement; the DOM observer half is not ported yet.
#[allow(dead_code)]
pub mod selection_actions;
pub mod timestamp_format;
// Find-in-thread logic. The timeline does not drive find yet, so nothing calls these.
#[allow(dead_code)]
pub mod thread_find;
#[allow(dead_code)]
pub mod thread_find_highlights;
#[allow(dead_code)]
pub mod thread_find_navigation;
#[allow(dead_code)]
pub mod thread_find_position;
#[allow(dead_code)]
pub mod thread_find_state;
pub mod work_log;
