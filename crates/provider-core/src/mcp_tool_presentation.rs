//! Port of upstream/packages/provider-core/src/server/mcpToolPresentation.ts.
//!
//! `resolveT3McpToolDefinition` comes from
//! upstream/packages/shared/src/t3McpToolPresentation.ts; only the name
//! resolution it needs is ported here (see PORTING.md Deviations).

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use turin_contracts::{
    TrimmedNonEmptyString, js_trim,
    provider_runtime::{ToolActivityIcon, ToolActivitySource, ToolActivitySourceKind},
};

use crate::js::{collapse_whitespace, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolPresentation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_icon: Option<ToolActivityIcon>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_source: Option<ToolActivitySource>,
}

/// `normalizeMcpText(value, maxLength = 160)`.
pub fn normalize_mcp_text(value: &Value, max_length: usize) -> Option<String> {
    let text = collapse_whitespace(js_trim(value.as_str()?));
    let length = utf16_len(&text);
    (length > 0 && length <= max_length).then_some(text)
}

pub fn normalize_mcp_http_url(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if utf16_len(raw) > 4096 {
        return None;
    }
    let url = reqwest::Url::parse(raw).ok()?;
    (matches!(url.scheme(), "https" | "http") && utf16_len(url.as_str()) <= 4096)
        .then(|| url.to_string())
}

fn field<'a>(input: &'a Value, name: &str) -> &'a Value {
    input.get(name).unwrap_or(&Value::Null)
}

/// JS `a ?? b`: falls through on `undefined` and `null` only.
fn nullish_or<'a>(value: &'a Value, fallback: impl FnOnce() -> &'a Value) -> &'a Value {
    if value.is_null() { fallback() } else { value }
}

fn text(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("normalized text is nonempty")
}

static QUALIFIED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^mcp__(.+?)__(.+)$").expect("valid regex"));

/// Derives the generic integration identity for a provider MCP tool.
pub fn mcp_tool_presentation(input: &Value) -> McpToolPresentation {
    let source = field(input, "source");
    let source = if source.is_object() || source.is_array() {
        source
    } else {
        &Value::Null
    };
    let qualified = field(input, "toolName")
        .as_str()
        .and_then(|name| QUALIFIED.captures(name))
        .map(|captures| {
            (
                Value::String(captures[1].to_owned()),
                Value::String(captures[2].to_owned()),
            )
        });
    let server = normalize_mcp_text(
        nullish_or(field(input, "serverName"), || {
            qualified
                .as_ref()
                .map_or(field(input, "serverDisplayName"), |(server, _)| server)
        }),
        160,
    );
    let tool = normalize_mcp_text(
        qualified
            .as_ref()
            .map_or(field(input, "toolName"), |(_, tool)| tool),
        160,
    );
    if let (Some(server), Some(tool)) = (&server, &tool)
        && resolve_t3_mcp_tool_name(&format!("{server}.{tool}")).is_some()
    {
        return McpToolPresentation::default();
    }
    let title = normalize_mcp_text(field(input, "title"), 160).or_else(|| {
        server.as_ref()?;
        normalize_mcp_text(&Value::String(replace_separators(tool.as_ref()?)), 160)
    });
    let Some(server) = server else {
        return McpToolPresentation {
            title,
            ..Default::default()
        };
    };
    let name = normalize_mcp_text(field(input, "serverDisplayName"), 160)
        .or_else(|| normalize_mcp_text(field(source, "name"), 160))
        .or_else(|| normalize_mcp_text(&Value::String(replace_separators(&server)), 160))
        .unwrap_or_else(|| server.clone());
    let logo_url = normalize_mcp_http_url(field(input, "iconUrl"))
        .or_else(|| normalize_mcp_http_url(field(source, "logoUrl")));
    let logo_url_dark = normalize_mcp_http_url(field(input, "iconUrlDark"))
        .or_else(|| normalize_mcp_http_url(field(source, "logoUrlDark")));
    let icon = logo_url.map(|logo_url| ToolActivityIcon::ThemedLogo {
        logo_url: text(&logo_url),
        logo_url_dark: logo_url_dark.as_deref().map(text),
    });
    McpToolPresentation {
        title,
        tool_icon: icon.clone(),
        tool_source: Some(ToolActivitySource {
            key: text(&format!("mcp:{}", server.to_lowercase())),
            name: text(&name),
            kind: ToolActivitySourceKind::Integration,
            icon,
        }),
    }
}

/// `value.replace(/[_-]+/gu, " ")`.
fn replace_separators(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for ch in value.chars() {
        if ch == '_' || ch == '-' {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    out
}

const T3_MCP_SERVER_ALIASES: [&str; 3] = ["t3-code", "t3_code", "t3code"];

/// Keys of upstream `T3_MCP_TOOLS`.
const T3_MCP_TOOL_NAMES: &[&str] = &[
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
];

static COMPLETED_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:complete|completed)\s*$").expect("valid regex"));
static NAMESPACED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:t3-code|t3_code|t3code)(?:[.:/]|\s*·\s*)(.+)$").expect("valid regex")
});
static PREFIXED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:mcp[-_]{1,2})?t3[-_ ]?code(?:__|[-_.:/ ])(.+)$").expect("valid regex")
});
static OPENCODE_SERVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^t3-code-").expect("valid regex"));

/// `resolveT3McpToolName` gated on the inventory, i.e. whether
/// `resolveT3McpToolDefinition` finds a definition.
fn resolve_t3_mcp_tool_name(value: &str) -> Option<&'static str> {
    let stripped = COMPLETED_SUFFIX.replace(value, "");
    let label = js_trim(&stripped);
    let known = |name: &str| T3_MCP_TOOL_NAMES.iter().copied().find(|tool| *tool == name);
    if let Some(captures) = QUALIFIED.captures(label) {
        return T3_MCP_SERVER_ALIASES
            .contains(&captures[1].to_lowercase().as_str())
            .then(|| known(&captures[2]))
            .flatten();
    }
    if let Some(captures) = NAMESPACED.captures(label) {
        return known(&captures[1]);
    }
    let candidate = PREFIXED
        .captures(label)
        .map_or(label, |captures| captures.get(1).expect("group").as_str());
    if let Some(tool) = known(candidate) {
        return Some(tool);
    }
    // OpenCode 2 registers one server per thread, `t3-code-<thread>`, and joins
    // it to the tool with `_`. Thread ids can hold `_` too, so take the longest
    // known tool name that ends the label.
    if !OPENCODE_SERVER.is_match(label) {
        return None;
    }
    T3_MCP_TOOL_NAMES
        .iter()
        .copied()
        .filter(|tool| label.ends_with(&format!("_{tool}")))
        .fold(None, |longest: Option<&str>, tool| match longest {
            Some(current) if current.len() >= tool.len() => Some(current),
            _ => Some(tool),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn present(input: Value) -> Value {
        serde_json::to_value(mcp_tool_presentation(&input)).unwrap()
    }

    #[test]
    fn uses_supplied_names_and_logos_while_retaining_the_server_identity() {
        assert_eq!(
            present(json!({
                "serverName": "firecrawl-local",
                "toolName": "firecrawl_scrape",
                "title": "  Scrape\n page  ",
                "serverDisplayName": "Firecrawl",
                "iconUrl": "https://example.com/icon.png",
                "iconUrlDark": "https://example.com/dark.png",
            })),
            json!({
                "title": "Scrape page",
                "toolIcon": {
                    "_tag": "themed-logo",
                    "logoUrl": "https://example.com/icon.png",
                    "logoUrlDark": "https://example.com/dark.png",
                },
                "toolSource": {
                    "key": "mcp:firecrawl-local",
                    "name": "Firecrawl",
                    "kind": "integration",
                    "icon": {
                        "_tag": "themed-logo",
                        "logoUrl": "https://example.com/icon.png",
                        "logoUrlDark": "https://example.com/dark.png",
                    },
                },
            })
        );
    }

    #[test]
    fn uses_mcp_result_source_metadata_for_the_tool_and_its_integration() {
        let presentation = mcp_tool_presentation(&json!({
            "serverName": "firecrawl-local",
            "toolName": "firecrawl_scrape",
            "source": {
                "name": "Firecrawl",
                "logoUrl": "https://example.com/firecrawl.png",
                "logoUrlDark": "https://example.com/firecrawl-dark.png",
            },
        }));
        let source = presentation.tool_source.clone().unwrap();
        assert_eq!(source.name.as_str(), "Firecrawl");
        assert_eq!(
            serde_json::to_value(&presentation.tool_icon).unwrap(),
            json!({
                "_tag": "themed-logo",
                "logoUrl": "https://example.com/firecrawl.png",
                "logoUrlDark": "https://example.com/firecrawl-dark.png",
            })
        );
        assert_eq!(source.icon, presentation.tool_icon);
    }

    #[test]
    fn uses_readable_fallback_names_without_a_guessed_logo() {
        assert_eq!(
            present(json!({ "toolName": "mcp__my_server__get_weather" })),
            json!({
                "title": "get weather",
                "toolSource": { "key": "mcp:my_server", "name": "my server", "kind": "integration" },
            })
        );
        assert_eq!(present(json!({ "toolName": "Read" })), json!({}));
        assert_eq!(
            present(json!({ "serverName": "t3-code", "toolName": "delegate_task" })),
            json!({})
        );
    }

    #[test]
    fn ignores_an_invalid_logo() {
        for icon_url in [
            "javascript:alert(1)".to_owned(),
            "file:///tmp/logo.png".to_owned(),
            "not a URL".to_owned(),
            format!("https://example.com/{}", "x".repeat(4096)),
        ] {
            assert_eq!(
                present(json!({
                    "serverName": "weather",
                    "toolName": "get_weather",
                    "iconUrl": icon_url,
                    "title": "x".repeat(161),
                })),
                json!({
                    "title": "get weather",
                    "toolSource": { "key": "mcp:weather", "name": "weather", "kind": "integration" },
                }),
                "{icon_url}"
            );
        }
    }

    #[test]
    fn rejects_malformed_and_overlong_names() {
        assert_eq!(
            present(json!({ "serverName": {}, "toolName": 1, "title": [] })),
            json!({})
        );
        assert_eq!(
            present(json!({ "serverName": "x".repeat(161), "toolName": "get_weather" })),
            json!({})
        );
        assert_eq!(
            present(json!({ "title": "Tool title" })),
            json!({ "title": "Tool title" })
        );
    }

    #[test]
    fn ignores_source_metadata_without_a_valid_logo() {
        for source in [
            Value::Null,
            json!("logo"),
            json!({ "logoUrl": "file:///tmp/logo.png" }),
            json!({ "logoUrlDark": "https://example.com/dark.png" }),
        ] {
            assert_eq!(
                present(
                    json!({ "serverName": "weather", "toolName": "get_weather", "source": source })
                ),
                json!({
                    "title": "get weather",
                    "toolSource": { "key": "mcp:weather", "name": "weather", "kind": "integration" },
                }),
                "{source}"
            );
        }
    }

    #[test]
    fn invalid_server_name_does_not_fall_back_to_the_qualified_name() {
        assert_eq!(
            present(json!({ "serverName": {}, "toolName": "mcp__weather__get_weather" })),
            json!({})
        );
    }

    #[test]
    fn resolves_t3_tool_spellings() {
        for label in [
            "mcp__t3-code__delegate_task",
            "t3-code.html_render",
            "t3_code:task_status",
            "t3code · task_cancel",
            "mcp_t3code_t3_thread_read",
            "t3-code-thread_1_t3_thread_send",
            "t3-code.delegate_task completed",
        ] {
            assert!(resolve_t3_mcp_tool_name(label).is_some(), "{label}");
        }
        assert_eq!(
            resolve_t3_mcp_tool_name("t3-code-abc_t3_thread_send"),
            Some("t3_thread_send")
        );
        assert_eq!(resolve_t3_mcp_tool_name("mcp__other__delegate_task"), None);
        assert_eq!(resolve_t3_mcp_tool_name("t3-code.unknown"), None);
    }
}
