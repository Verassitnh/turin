//! Computer-use labels: a Sky REPL call keeps its action title, and a failed
//! Sky call stored as a generic `js` integration is still computer use.

use super::*;
use crate::components::chat::timeline::session_logic::derive_timeline_entries_from_visible_turn_items;
use crate::components::chat::timeline::turn_item::ToolActivitySource;

/// The streaming fixture's tool item, rekeyed to one projected tool call.
fn tool_call(
    tool_name: &str,
    status: TurnItemStatus,
    title: Option<&str>,
    input: Value,
    tool_source: Option<ToolActivitySource>,
) -> TimelineEntriesInput {
    let fixture = make_streaming_timeline_fixture("Streaming");
    let source = fixture
        .visible_turn_items
        .iter()
        .find(|row| matches!(row.item.payload, TurnItemPayload::DynamicTool { .. }))
        .expect("dynamic tool fixture")
        .clone();
    let item = TurnItem {
        status,
        title: title.map(str::to_string),
        tool_source,
        payload: dynamic_tool_payload(Some(tool_name), input, None),
        ..source.item.clone()
    };
    TimelineEntriesInput {
        visible_turn_items: vec![ProjectedTurnItem { item, ..source }],
        ..Default::default()
    }
}

fn work_entry_of(entries: &[TimelineEntry]) -> &WorkLogEntry {
    entries
        .iter()
        .find_map(|timeline_entry| match &timeline_entry.kind {
            TimelineEntryKind::Work { entry } => Some(entry),
            _ => None,
        })
        .expect("work entry")
}

#[test]
fn sky_repl_calls_show_their_action_title_in_both_views() {
    for tool_name in ["cua_repl.js", "node_repl.js", "js"] {
        let input = json!({
            "code": "await game.getAXStateAndScreenshot();",
            "title": "Inspect Saga music screen",
        });
        let timeline_input = tool_call(tool_name, TurnItemStatus::Completed, None, input, None);
        let entries = derive_timeline_entries_from_visible_turn_items(&timeline_input);
        assert_eq!(
            work_entry_display_label(work_entry_of(&entries), None),
            "Inspect Saga music screen",
            "{tool_name}"
        );
        let rows = derive(&MessagesTimelineRowsInput {
            running_run_id: Some("live-run".into()),
            active_turn_started_at: Some(streaming_fixture_time(0)),
            ..rows_input(entries, false)
        });
        match find_kind(&rows, MessagesTimelineRowKind::Work) {
            Some(Row::Work { display_label, .. }) => assert_eq!(
                display_label.as_deref(),
                Some("Inspect Saga music screen"),
                "{tool_name}"
            ),
            other => panic!("{tool_name}: expected a work row, got {other:?}"),
        }
    }
}

#[test]
fn failed_sky_call_stored_as_a_generic_js_integration_is_computer_use() {
    let integration = ToolActivitySource {
        key: "mcp:node_repl".into(),
        name: "node repl".into(),
        kind: "integration".into(),
        icon: None,
    };
    let input = json!({
        "title": "Restrict the breakpoint to Git actions",
        "code": "await sky.type_text({app:'T3 Code (Nightly)',text:'debug(ga.run)'});",
    });
    let timeline_input = tool_call(
        "node_repl.js",
        TurnItemStatus::Failed,
        Some("js"),
        input,
        Some(integration),
    );
    let entries = derive_timeline_entries_from_visible_turn_items(&timeline_input);
    let entry = work_entry_of(&entries);
    assert_eq!(
        work_entry_display_label(entry, None),
        "Restrict the breakpoint to Git actions"
    );
    assert_eq!(entry.tool_surface, Some(ToolActivitySurface::Computer));
    let source = entry.tool_source.as_ref().expect("tool source");
    assert_eq!(
        (source.key.as_str(), source.kind.as_str()),
        ("computer-use", "computer")
    );
}
