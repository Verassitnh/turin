//! Ports of upstream/apps/web/src/components/chat.

#[allow(dead_code)]
pub mod agent_elapsed;
// Needs a GPUI text selection to drive it; mounted from the demo until then.
#[allow(dead_code)]
pub mod agent_spawn_summary;
#[allow(dead_code)]
pub mod assistant_citation_chip;
#[allow(dead_code)]
pub mod assistant_citation_comment_dismissal;
#[allow(dead_code)]
pub mod assistant_citation_comment_editor;
#[allow(dead_code)]
pub mod assistant_citation_navigation;
#[allow(dead_code)]
pub mod assistant_citation_source;
#[allow(dead_code)]
pub mod assistant_citation_target;
pub mod assistant_selection_state;
pub mod assistant_selection_toolbar;
pub mod question_attachment_drafts;
pub mod question_attachment_submit;
pub mod markdown_thread_link;
pub mod thread_navigation;
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
// The plan card's export helpers; the follow-up helpers wait for the plan composer.
#[allow(dead_code)]
pub mod proposed_plan;
// Plain command text; the timeline's command rows do not use it until the highlighter lands.
#[allow(dead_code)]
pub mod shell_command_block;
// Path-chip glyphs; the timeline's chips do not draw them yet.
#[allow(dead_code)]
pub mod pierre_entry_icon;
// The provider picker's ChatGPT plan row; the picker does not host it yet.
#[allow(dead_code)]
pub mod chat_gpt_sharing_control;
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
pub mod chat_media;
pub mod composer_pending_bridge;
pub mod composer_pending_panels;
// Embedded-script scan for command and tool views; no view calls it yet.
#[allow(dead_code)]
pub mod embedded_scripts;
#[allow(dead_code)]
pub mod expanded_image;
#[allow(dead_code)]
pub mod highlighted_tokens;
// The link context menu; the markdown link views do not open it yet.
#[allow(dead_code)]
pub mod external_link_context_menu;
// Copy-as-markdown for selections over rendered blocks; nothing calls it yet.
#[allow(dead_code)]
pub mod composer_context_undo;
#[allow(dead_code)]
pub mod composer_event_scope;
#[allow(dead_code)]
pub mod composer_focus_state;
#[allow(dead_code)]
pub mod composer_menu_state;
// Route handoff around the mobile composer morph. No view calls it yet.
#[allow(dead_code)]
pub mod draft_hero_transition;
#[allow(dead_code)]
pub mod folder_drop;
pub mod live_chat;
pub mod live_lineage;
pub mod live_wiring;
pub mod timeline_page_scroll;
pub mod markdown;
#[allow(dead_code)]
pub mod markdown_clipboard;
#[allow(dead_code)]
pub mod mermaid_diagram;
pub mod message_copy_button;
pub mod messages_timeline;
pub mod open_in_picker;
pub mod panel_layout_controls;
#[allow(dead_code)]
pub mod pending_draft_work;
// Pure scroll state machine; the timeline view does not drive page keys yet.
#[allow(dead_code)]
pub mod page_scroll_controller;
// Pure answer-draft logic; the pending-user-input card still keeps its drafts
// in element state, so nothing calls these yet.
#[allow(dead_code)]
pub mod pending_user_input;
#[allow(dead_code)]
pub mod use_composer_multiline_prompt;
#[allow(dead_code)]
pub mod use_composer_trigger_state;
#[allow(dead_code)]
pub mod use_composer_typing_guard;
// Sidebar thread drag into the composer. The sidebar view does not drive it yet.
#[allow(dead_code)]
pub mod thread_context_drag;
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
#[allow(dead_code)]
pub mod workspace_file_drop;
