use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::tool_call::AcpToolCallState;
use super::util::is_record;

/// Keys of upstream `T3_MCP_TOOLS` (duplicated from provider-core; no provider-core dep).
static T3_MCP_TOOL_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "link_pull_request",
        "unlink_pull_request",
        "list_thread_pull_requests",
        "watch_pull_request",
        "unwatch_pull_request",
        "write_pull_request_guides",
        "read_pull_request_guide",
        "orchestrator_capabilities",
        "delegate_task",
        "task_status",
        "task_cancel",
        "schedule_task",
        "list_scheduled_tasks",
        "update_scheduled_task",
        "delete_scheduled_task",
        "request_secret",
        "create_threads",
        "t3_thread_start",
        "t3_thread_list",
        "t3_thread_read",
        "t3_thread_send",
        "t3_thread_wait",
        "t3_workflow_stop",
        "t3_thread_interrupt",
        "t3_worktree_handoff",
        "t3_worktree_status",
        "preview_status",
        "preview_open",
        "preview_navigate",
        "preview_dialog",
        "preview_snapshot",
        "preview_click",
        "preview_press",
        "preview_type",
        "preview_hover",
        "preview_select",
        "preview_drag",
        "preview_upload",
        "preview_scroll",
        "preview_resize",
        "preview_evaluate",
        "preview_wait_for",
        "preview_set_appearance",
        "preview_recording_start",
        "preview_recording_stop",
        "device_list",
        "device_open",
        "device_screenshot",
        "device_close",
        "run_scheduled_task_now",
        "t3_queue_list",
        "t3_queue_read",
        "t3_queue_edit",
        "t3_queue_cancel",
        "t3_queue_reorder",
        "t3_queue_promote_to_steer",
        "t3_pending_request_list",
        "t3_pending_request_read",
        "t3_pending_request_respond",
        "t3_thread_configuration",
        "t3_thread_configure",
        "t3_thread_fork",
        "t3_thread_merge_back",
        "t3_thread_search",
        "t3_thread_transfers",
        "t3_thread_organize",
        "t3_thread_update",
        "t3_worktree_list",
        "t3_preview_list",
        "t3_preview_close",
        "t3_environment_read",
        "t3_environment_preferences_update",
        "t3_thread_launch",
        "t3_project_list",
        "t3_project_read",
        "t3_project_create",
        "t3_project_update",
        "t3_project_delete",
        "t3_project_clone",
        "t3_attachment_prepare_upload",
        "t3_attachment_discard",
        "t3_thread_send_attachments",
        "html_preview",
        "html_render",
    ]
    .into_iter()
    .collect()
});

static ACP_MCP_FALLBACK_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?u)(?:^|[\s"'=])acp-mcp-call[\s"']+([A-Za-z0-9_.-]+)(?:\s+(.+?))?\s*$"#)
        .expect("valid regex")
});
static T3_MCP_TITLE_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:mcp[-_]{1,2})?t3[-_ ]?code[-_.:/ ]{1,3}(?<tool>[A-Za-z0-9][A-Za-z0-9_.-]*)(?::.*)?$")
        .expect("valid regex")
});
static T3_MCP_TITLE_SUFFIX_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?<tool>[A-Za-z0-9][A-Za-z0-9_.-]*?)(?: \(t3[-_ ]?code MCP Server\)(?::|$)|[-_.]t3[-_ ]?code$)",
    )
    .expect("valid regex")
});
static T3_MCP_BARE_TITLE_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?<tool>[A-Za-z0-9_]+)(?::\s|$)").expect("valid regex"));
static T3_CODE_SERVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^t3[-_ ]?code$").expect("valid regex"));
static MCP_QUALIFIED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^mcp__(.+?)__(.+)$").expect("valid regex"));

#[derive(Debug, Clone, PartialEq)]
pub struct AcpMcpToolCallIdentity {
    pub server: String,
    pub tool: String,
    pub input: Option<serde_json::Map<String, Value>>,
}

fn acp_mcp_fallback_input(value: Option<&str>) -> Option<serde_json::Map<String, Value>> {
    let trimmed = value?.trim();
    let mut candidates = vec![trimmed.to_owned()];
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        candidates.push(trimmed[1..trimmed.len() - 1].to_owned());
    }
    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<Value>(&candidate) {
            let unwrapped = if let Some(inner) = parsed.as_str() {
                serde_json::from_str::<Value>(inner).unwrap_or(parsed)
            } else {
                parsed
            };
            if let Some(record) = unwrapped.as_object() {
                return Some(record.clone());
            }
        }
    }
    None
}

fn t3_tool_from_meta_tool_name(meta_tool_name: &str) -> Option<&'static str> {
    for known_tool in T3_MCP_TOOL_NAMES.iter() {
        let boundary = meta_tool_name.len().saturating_sub(known_tool.len() + 1);
        if meta_tool_name == *known_tool
            || (meta_tool_name.ends_with(known_tool)
                && boundary < meta_tool_name.len()
                && !meta_tool_name
                    .chars()
                    .nth(boundary)
                    .is_some_and(|c| c.is_ascii_alphanumeric()))
        {
            return Some(known_tool);
        }
    }
    None
}

fn match_t3_title(trimmed: &str) -> Option<String> {
    for re in [
        &*T3_MCP_TITLE_CALL,
        &*T3_MCP_TITLE_SUFFIX_CALL,
        &*T3_MCP_BARE_TITLE_CALL,
    ] {
        if let Some(caps) = re.captures(trimmed)
            && let Some(tool) = caps.name("tool").map(|m| m.as_str())
            && T3_MCP_TOOL_NAMES.contains(tool)
        {
            return Some(tool.to_owned());
        }
    }
    None
}

pub fn extract_mcp_tool_call_identity(
    tool_call: &AcpToolCallState,
    embedded_terminal_commands: Option<&[String]>,
) -> Option<AcpMcpToolCallIdentity> {
    let raw_input = tool_call.data.get("rawInput").and_then(is_record);
    let meta = tool_call.data.get("meta").and_then(is_record);
    let server = raw_input
        .and_then(|r| r.get("server"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let tool = raw_input
        .and_then(|r| r.get("tool"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if meta
        .and_then(|m| m.get("is_mcp_tool_call"))
        .and_then(Value::as_bool)
        == Some(true)
        && !server.is_empty()
        && !tool.is_empty()
    {
        return Some(AcpMcpToolCallIdentity {
            server: server.to_owned(),
            tool: tool.to_owned(),
            input: None,
        });
    }
    let claude_code = meta.and_then(|m| m.get("claudeCode")).and_then(is_record);
    let goose_tool_call = meta
        .and_then(|m| m.get("goose"))
        .and_then(is_record)
        .and_then(|g| g.get("toolCall"))
        .and_then(is_record);
    let meta_server_id = meta
        .and_then(|m| m.get("serverId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let meta_tool_name = meta
        .and_then(|m| m.get("toolName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if T3_CODE_SERVER.is_match(meta_server_id)
        && !meta_tool_name.is_empty()
        && let Some(known) = t3_tool_from_meta_tool_name(meta_tool_name)
    {
        return Some(AcpMcpToolCallIdentity {
            server: "t3-code".to_owned(),
            tool: known.to_owned(),
            input: None,
        });
    }
    let goose_extension = goose_tool_call
        .and_then(|g| g.get("extensionName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let asserts_foreign_origin = (!meta_server_id.is_empty()
        && !T3_CODE_SERVER.is_match(meta_server_id))
        || (!goose_extension.is_empty() && !T3_CODE_SERVER.is_match(goose_extension));
    if asserts_foreign_origin {
        if meta_server_id.is_empty() || meta_tool_name.is_empty() {
            return None;
        }
        let prefix = ["mcp__", "mcp::"]
            .iter()
            .find(|prefix| meta_tool_name.starts_with(*prefix))
            .map(|prefix| prefix.to_string());
        let tool = prefix
            .as_ref()
            .map(|prefix| meta_tool_name[prefix.len()..].to_owned())
            .unwrap_or_else(|| meta_tool_name.to_owned());
        return (!tool.is_empty()).then(|| AcpMcpToolCallIdentity {
            server: meta_server_id.to_owned(),
            tool,
            input: None,
        });
    }
    let candidates: Vec<&str> = [
        meta.and_then(|m| m.get("toolName")).and_then(Value::as_str),
        claude_code
            .and_then(|c| c.get("toolName"))
            .and_then(Value::as_str),
        goose_tool_call
            .and_then(|g| g.get("toolName"))
            .and_then(Value::as_str),
        tool_call.data.get("title").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect();
    for candidate in candidates {
        let trimmed = candidate.trim();
        if let Some(caps) = MCP_QUALIFIED.captures(trimmed) {
            let server = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let tool = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            if !server.is_empty() && !tool.is_empty() && !T3_CODE_SERVER.is_match(server) {
                return Some(AcpMcpToolCallIdentity {
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    input: None,
                });
            }
        }
        if let Some(known) = match_t3_title(trimmed) {
            return Some(AcpMcpToolCallIdentity {
                server: "t3-code".to_owned(),
                tool: known,
                input: None,
            });
        }
    }
    let mut commands: Vec<&str> = Vec::new();
    if let Some(command) = tool_call.command.as_deref() {
        commands.push(command);
    }
    if let Some(title) = tool_call.data.get("title").and_then(Value::as_str) {
        commands.push(title);
    }
    if let Some(embedded) = embedded_terminal_commands {
        for command in embedded {
            commands.push(command);
        }
    }
    for command in commands {
        if let Some(caps) = ACP_MCP_FALLBACK_CALL.captures(command) {
            let tool = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let input = acp_mcp_fallback_input(caps.get(2).map(|m| m.as_str()));
            return Some(AcpMcpToolCallIdentity {
                server: "t3-code".to_owned(),
                tool: tool.to_owned(),
                input,
            });
        }
    }
    None
}
