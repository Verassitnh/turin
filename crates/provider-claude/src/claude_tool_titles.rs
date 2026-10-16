//! Port of the tool titles and presentation in upstream ClaudeAdapterV2.ts:
//! `formatReadToolLabel`, `formatSearchToolLabel`, the Skill branch of
//! `dynamicToolTitle` (packages/shared/src/toolActivity.ts), and
//! `mcpToolPresentation` (packages/provider-core/src/server/mcpToolPresentation.ts).
//! Also the `tool_use_meta` presentation map on assistant frames.
//!
//! T3's own MCP tool definitions are cut, so the `resolveT3McpToolDefinition`
//! exemption (which returns no presentation for them) is not ported.
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Upstream `mcpToolPresentation` output: the item title, icon, and source.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolPresentation {
    pub title: Option<String>,
    pub tool_icon: Option<Value>,
    pub tool_source: Option<Value>,
}

/// Upstream `normalizeMcpText`: trimmed, runs of whitespace collapsed, and
/// measured in UTF-16 units like JavaScript's `length`.
pub fn normalize_mcp_text(value: Option<&Value>, max_length: usize) -> Option<String> {
    let text = value?.as_str()?.trim();
    let collapsed: Vec<&str> = text.split_whitespace().collect();
    let text = collapsed.join(" ");
    let length = text.encode_utf16().count();
    (length > 0 && length <= max_length).then_some(text)
}

/// Upstream `normalizeMcpHttpUrl`.
pub fn normalize_mcp_http_url(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    if raw.len() > 4096 {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    let href = url.as_str();
    (matches!(url.scheme(), "http" | "https") && href.len() <= 4096).then(|| href.to_owned())
}

/// Upstream `??` over raw values: the first one that is not null or absent.
fn first_present<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    values
        .iter()
        .flatten()
        .copied()
        .find(|value| !value.is_null())
}

/// The `mcp__<server>__<tool>` split: a lazy server group, then the rest.
fn qualified_name(tool_name: &str) -> Option<(String, String)> {
    let rest = tool_name
        .get(..5)?
        .eq_ignore_ascii_case("mcp__")
        .then(|| &tool_name[5..])?;
    let split = rest.find("__")?;
    let server = &rest[..split];
    let tool = &rest[split + 2..];
    (!server.is_empty() && !tool.is_empty()).then(|| (server.to_owned(), tool.to_owned()))
}

/// Upstream `mcpToolPresentation`. Inputs are the raw values the SDK sends.
pub fn mcp_tool_presentation(
    tool_name: Option<&str>,
    server_name: Option<&Value>,
    title: Option<&Value>,
    server_display_name: Option<&Value>,
    icon_url: Option<&Value>,
    icon_url_dark: Option<&Value>,
    source: Option<&Value>,
) -> ToolPresentation {
    let source = source.filter(|value| value.is_object());
    let qualified = tool_name.and_then(qualified_name);
    let tool_name_value = tool_name.map(|name| Value::String(name.to_owned()));
    let qualified_server = qualified.as_ref().map(|(server, _)| json!(server));
    let qualified_tool = qualified.as_ref().map(|(_, tool)| json!(tool));
    let server = normalize_mcp_text(
        first_present(&[server_name, qualified_server.as_ref(), server_display_name]),
        160,
    );
    let tool = normalize_mcp_text(
        first_present(&[qualified_tool.as_ref(), tool_name_value.as_ref()]),
        160,
    );
    let title = normalize_mcp_text(title, 160).or_else(|| {
        server
            .as_ref()
            .and(tool.as_ref())
            .and_then(|tool| normalize_mcp_text(Some(&json!(collapse_separators(tool))), 160))
    });
    let Some(server) = server else {
        return ToolPresentation {
            title,
            ..Default::default()
        };
    };
    let source_field = |key: &str| source.and_then(|s| s.get(key));
    let name = normalize_mcp_text(server_display_name, 160)
        .or_else(|| normalize_mcp_text(source_field("name"), 160))
        .or_else(|| normalize_mcp_text(Some(&json!(collapse_separators(&server))), 160))
        .unwrap_or_else(|| server.clone());
    let logo_url = normalize_mcp_http_url(icon_url)
        .or_else(|| normalize_mcp_http_url(source_field("logoUrl")));
    let logo_url_dark = normalize_mcp_http_url(icon_url_dark)
        .or_else(|| normalize_mcp_http_url(source_field("logoUrlDark")));
    let icon = logo_url.map(|logo_url| {
        let mut icon = Map::new();
        icon.insert("_tag".into(), json!("themed-logo"));
        icon.insert("logoUrl".into(), json!(logo_url));
        if let Some(dark) = logo_url_dark {
            icon.insert("logoUrlDark".into(), json!(dark));
        }
        Value::Object(icon)
    });
    let mut source = Map::new();
    source.insert(
        "key".into(),
        json!(format!("mcp:{}", server.to_lowercase())),
    );
    source.insert("name".into(), json!(name));
    source.insert("kind".into(), json!("integration"));
    if let Some(icon) = &icon {
        source.insert("icon".into(), icon.clone());
    }
    ToolPresentation {
        title,
        tool_icon: icon,
        tool_source: Some(Value::Object(source)),
    }
}

/// `replace(/[_-]+/gu, " ")`
fn collapse_separators(value: &str) -> String {
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

/// Presentation for a tool use the SDK did not describe in `tool_use_meta`.
pub fn fallback_presentation(tool_name: &str) -> ToolPresentation {
    let name = tool_name.replacen("mcp__claude_ai_", "mcp__", 1);
    mcp_tool_presentation(Some(&name), None, None, None, None, None, None)
}

/// Upstream `claudeToolPresentationsFromAssistantMessage`: `tool_use_meta`
/// entries keyed by tool use id. `tool_names` maps the frame's tool uses.
pub fn presentations_from_meta(
    frame: &Value,
    tool_names: &HashMap<String, String>,
) -> HashMap<String, ToolPresentation> {
    let mut presentations = HashMap::new();
    let Some(meta) = frame.get("tool_use_meta").and_then(Value::as_array) else {
        return presentations;
    };
    for entry in meta {
        let Some(id) = normalize_mcp_text(entry.get("id"), 512) else {
            continue;
        };
        let tool_name = tool_names
            .get(&id)
            .map(|name| name.replacen("mcp__claude_ai_", "mcp__", 1));
        let presentation = mcp_tool_presentation(
            tool_name.as_deref(),
            None,
            entry.get("display_name"),
            entry.get("server_display_name"),
            entry.get("icon_url"),
            None,
            None,
        );
        presentations.insert(id, presentation);
    }
    presentations
}

/// Upstream `formatReadToolLabel`: "Read <path>", with an optional count.
pub fn read_label(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "Read file".to_owned()
    } else {
        format!("Read {trimmed}")
    }
}

/// Upstream `formatSearchToolLabel` over a tool's input record.
pub fn search_label(input: &Value) -> Option<String> {
    let record = input.as_object().filter(|record| !record.is_empty())?;
    let query = first_input_string(
        record,
        &["pattern", "query", "searchTerm", "regex", "grep", "needle"],
    );
    let glob = first_input_string(
        record,
        &[
            "glob",
            "globPattern",
            "glob_pattern",
            "include",
            "filePattern",
            "file_pattern",
        ],
    );
    let target = first_input_string(
        record,
        &[
            "path",
            "target_directory",
            "targetDirectory",
            "directory",
            "cwd",
            "root",
        ],
    )
    .and_then(|path| search_target_name(&path));
    match (query, glob, target) {
        (Some(query), _, Some(target)) => Some(format!("Searched {query} in {target}")),
        (None, Some(glob), Some(target)) => Some(format!("Searched files {glob} in {target}")),
        (None, Some(glob), None) => Some(format!("Searched files {glob}")),
        (Some(query), _, None) => Some(format!("Searched {query}")),
        (None, None, Some(target)) => Some(format!("Searched in {target}")),
        (None, None, None) => None,
    }
}

fn first_input_string(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = record.get(*key)?.as_str()?.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// The last non-empty segment that is not `.`, split on either slash.
fn search_target_name(value: &str) -> Option<String> {
    value
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty() && *part != ".")
        .map(str::to_owned)
}

/// Upstream `dynamicToolTitle`'s Skill branch: "Skill: <name>".
pub fn skill_title(tool_name: &str, input: &Value) -> Option<String> {
    if tool_name != "Skill" {
        return None;
    }
    let name = input.get("skill")?.as_str()?.trim();
    (!name.is_empty()).then(|| format!("Skill: {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_titles_prefer_query_then_glob_with_the_target_leaf() {
        assert_eq!(
            search_label(&json!({"pattern": "TODO", "path": "/repo/src/"})).as_deref(),
            Some("Searched TODO in src")
        );
        assert_eq!(
            search_label(&json!({"glob": "*.ts", "path": "web"})).as_deref(),
            Some("Searched files *.ts in web")
        );
        assert_eq!(search_label(&json!({})), None);
    }

    #[test]
    fn read_and_skill_titles_match_upstream() {
        assert_eq!(read_label("  src/lib.rs "), "Read src/lib.rs");
        assert_eq!(read_label("  "), "Read file");
        assert_eq!(
            skill_title("Skill", &json!({"skill": " review "})).as_deref(),
            Some("Skill: review")
        );
        assert_eq!(skill_title("Read", &json!({"skill": "x"})), None);
    }

    #[test]
    fn mcp_presentation_names_server_and_tool() {
        let presentation = mcp_tool_presentation(
            Some("mcp__linear_app__create-issue"),
            None,
            None,
            Some(&json!("Linear")),
            Some(&json!("https://linear.app/logo.png")),
            None,
            None,
        );
        assert_eq!(presentation.title.as_deref(), Some("create issue"));
        let source = presentation.tool_source.unwrap();
        assert_eq!(source["key"], "mcp:linear_app");
        assert_eq!(source["name"], "Linear");
        assert_eq!(source["kind"], "integration");
        assert_eq!(presentation.tool_icon.unwrap()["_tag"], "themed-logo");
    }

    #[test]
    fn meta_titles_win_over_the_tool_name() {
        let frame = json!({"tool_use_meta": [
            {"id": "toolu_1", "display_name": "Create issue", "server_display_name": "Linear"},
            {"id": "  "},
        ]});
        let names = HashMap::from([(
            "toolu_1".to_owned(),
            "mcp__claude_ai_linear__create".to_owned(),
        )]);
        let presentations = presentations_from_meta(&frame, &names);
        assert_eq!(presentations.len(), 1);
        assert_eq!(
            presentations["toolu_1"].title.as_deref(),
            Some("Create issue")
        );
    }
}
