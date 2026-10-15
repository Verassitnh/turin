//! Port of upstream/packages/shared/src/toolActivity.ts
//!
//! Also carries `ToolLifecycleItemType` / `isToolLifecycleItemType` from
//! upstream/packages/contracts/src/providerRuntime.ts, which `turin-contracts`
//! does not have yet.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use turin_contracts::base_schemas::js_trim;

use super::turn_item::{ToolActivitySource, ToolActivitySurface};

/// `ToolLifecycleItemType` (contracts/providerRuntime.ts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolLifecycleItemType {
    CommandExecution,
    FileChange,
    McpToolCall,
    DynamicToolCall,
    CollabAgentToolCall,
    WebSearch,
    ImageView,
}

impl ToolLifecycleItemType {
    /// `TOOL_LIFECYCLE_ITEM_TYPES`, in upstream order.
    pub const ALL: [Self; 7] = [
        Self::CommandExecution,
        Self::FileChange,
        Self::McpToolCall,
        Self::DynamicToolCall,
        Self::CollabAgentToolCall,
        Self::WebSearch,
        Self::ImageView,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandExecution => "command_execution",
            Self::FileChange => "file_change",
            Self::McpToolCall => "mcp_tool_call",
            Self::DynamicToolCall => "dynamic_tool_call",
            Self::CollabAgentToolCall => "collab_agent_tool_call",
            Self::WebSearch => "web_search",
            Self::ImageView => "image_view",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

/// `isToolLifecycleItemType` (contracts/providerRuntime.ts).
pub fn is_tool_lifecycle_item_type(value: &str) -> bool {
    ToolLifecycleItemType::parse(value).is_some()
}

type Record = Map<String, Value>;

fn as_record(value: Option<&Value>) -> Option<&Record> {
    value.and_then(Value::as_object)
}

/// `record?.[key]`.
fn get<'a>(record: Option<&'a Record>, key: &str) -> Option<&'a Value> {
    record.and_then(|record| record.get(key))
}

fn as_trimmed_string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).and_then(trimmed_non_empty)
}

fn trimmed_non_empty(value: &str) -> Option<&str> {
    let trimmed = js_trim(value);
    (!trimmed.is_empty()).then_some(trimmed)
}

/// A Claude `Skill` call: the skill it loads and the arguments it passes, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaudeSkillInvocation<'a> {
    pub name: &'a str,
    pub args: Option<&'a str>,
}

/// A Claude `Skill` call: the skill it loads and the arguments it passes, if any.
///
/// `input` is upstream's `unknown`; pass `Value::Null` for `undefined` (both
/// read as "not a record").
pub fn claude_skill_invocation<'a>(
    tool_name: Option<&str>,
    input: &'a Value,
) -> Option<ClaudeSkillInvocation<'a>> {
    if tool_name != Some("Skill") {
        return None;
    }
    let record = input.as_object();
    let name = as_trimmed_string(record.and_then(|record| record.get("skill")))?;
    Some(ClaudeSkillInvocation {
        name,
        args: as_trimmed_string(record.and_then(|record| record.get("args"))),
    })
}

pub fn is_java_script_repl_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("js" | "cua_repl.js" | "node_repl.js"))
}

/// `COMPUTER_USE_TOOL_SOURCE.key`.
pub const COMPUTER_USE_TOOL_SOURCE_KEY: &str = "computer-use";
/// `COMPUTER_USE_TOOL_SOURCE.name`.
pub const COMPUTER_USE_TOOL_SOURCE_NAME: &str = "Computer Use";
/// `COMPUTER_USE_TOOL_SOURCE.kind`.
pub const COMPUTER_USE_TOOL_SOURCE_KIND: &str = "computer";

/// `COMPUTER_USE_TOOL_SOURCE` (no icon).
pub fn computer_use_tool_source() -> ToolActivitySource {
    ToolActivitySource {
        key: COMPUTER_USE_TOOL_SOURCE_KEY.to_owned(),
        name: COMPUTER_USE_TOOL_SOURCE_NAME.to_owned(),
        kind: COMPUTER_USE_TOOL_SOURCE_KIND.to_owned(),
        icon: None,
    }
}

// JS `\b` is an ASCII word boundary even with the `u` flag.
static SKY_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)sky\s*\.").expect("valid sky call regex"));
static SKY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']@oai/sky["']"#).expect("valid sky import regex"));

/// Computer-use calls may fail or still be running before result metadata is available.
pub fn dynamic_tool_surface(tool_name: Option<&str>, input: &Value) -> Option<ToolActivitySurface> {
    if tool_name == Some("cua_repl.js") {
        return Some(ToolActivitySurface::Computer);
    }
    let code = input
        .as_object()
        .and_then(|record| record.get("code"))
        .and_then(Value::as_str);
    (is_java_script_repl_tool(tool_name)
        && code.is_some_and(|code| SKY_CALL.is_match(code) || SKY_IMPORT.is_match(code)))
    .then_some(ToolActivitySurface::Computer)
}

/// Activity log heading a dynamic tool derives from its input: a REPL's `title`,
/// or the skill a Claude `Skill` call loads.
pub fn dynamic_tool_title(
    tool_name: Option<&str>,
    input: &Value,
    retained_title: Option<&str>,
) -> Option<String> {
    if is_java_script_repl_tool(tool_name) {
        let title = retained_title.and_then(trimmed_non_empty);
        let meaningful_title = title.filter(|title| {
            let lower = title.to_lowercase();
            lower != "js" && Some(lower) != tool_name.map(str::to_lowercase)
        });
        let input_title =
            as_trimmed_string(input.as_object().and_then(|record| record.get("title")));
        return Some(
            input_title
                .or(meaningful_title)
                .unwrap_or(
                    if dynamic_tool_surface(tool_name, input) == Some(ToolActivitySurface::Computer)
                    {
                        "Computer use"
                    } else {
                        "Run JavaScript"
                    },
                )
                .to_owned(),
        );
    }
    claude_skill_invocation(tool_name, input).map(|skill| format!("Skill: {}", skill.name))
}

fn record_has_keys(value: Option<&Record>) -> bool {
    value.is_some_and(|record| !record.is_empty())
}

fn normalize_command_value(value: Option<&Value>) -> Option<String> {
    if let Some(direct) = as_trimmed_string(value) {
        return Some(direct.to_owned());
    }
    let Some(Value::Array(entries)) = value else {
        return None;
    };
    let parts: Vec<&str> = entries
        .iter()
        .filter_map(|entry| as_trimmed_string(Some(entry)))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

// `[\s\S]` is `(?s:.)`; JS `\d` is ASCII-only.
static TRAILING_EXIT_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^(?P<output>.*?)(?:\s*<exited with exit code [0-9]+>)\s*$")
        .expect("valid trailing exit code regex")
});

fn strip_trailing_exit_code(value: Option<&str>) -> Option<&str> {
    let trimmed = trimmed_non_empty(value?)?;
    let output = TRAILING_EXIT_CODE
        .captures(trimmed)
        .and_then(|captures| captures.name("output"))
        .map_or(trimmed, |output| js_trim(output.as_str()));
    (!output.is_empty()).then_some(output)
}

static BACKTICK_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]+)`").expect("valid backtick regex"));

fn extract_command_from_title(title: Option<&str>) -> Option<String> {
    let captures = BACKTICK_COMMAND.captures(title?)?;
    let command = js_trim(captures.get(1)?.as_str());
    (!command.is_empty()).then(|| command.to_owned())
}

fn extract_tool_command(data: Option<&Record>, title: Option<&str>) -> Option<String> {
    let item = as_record(get(data, "item"));
    let item_input = as_record(get(item, "input"));
    let item_result = as_record(get(item, "result"));
    let raw_input = as_record(get(data, "rawInput"));
    let direct = normalize_command_value(get(item, "command"))
        .or_else(|| normalize_command_value(get(item_input, "command")))
        .or_else(|| normalize_command_value(get(item_result, "command")))
        .or_else(|| normalize_command_value(get(data, "command")))
        .or_else(|| normalize_command_value(get(raw_input, "command")));
    if direct.is_some() {
        return direct;
    }
    let executable = as_trimmed_string(get(raw_input, "executable"));
    let args = normalize_command_value(get(raw_input, "args"));
    match (executable, args) {
        (Some(executable), Some(args)) => Some(format!("{executable} {args}")),
        (Some(executable), None) => Some(executable.to_owned()),
        (None, _) => extract_command_from_title(title),
    }
}

const PATH_KEYS: [&str; 8] = [
    "path",
    "filePath",
    "file_path",
    "relativePath",
    "filename",
    "fileName",
    "newPath",
    "oldPath",
];

const NESTED_PATH_KEYS: [&str; 7] = [
    "locations",
    "item",
    "input",
    "result",
    "rawInput",
    "data",
    "changes",
];

const MAX_COLLECTED_PATHS: usize = 8;

fn collect_paths<'a>(
    value: &'a Value,
    paths: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
    depth: usize,
) {
    if depth > 4 || paths.len() >= MAX_COLLECTED_PATHS {
        return;
    }
    if let Value::Array(entries) = value {
        for entry in entries {
            collect_paths(entry, paths, seen, depth + 1);
            if paths.len() >= MAX_COLLECTED_PATHS {
                return;
            }
        }
        return;
    }
    if let Value::Object(record) = value {
        collect_paths_in_record(record, paths, seen, depth);
    }
}

fn collect_paths_in_record<'a>(
    record: &'a Record,
    paths: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
    depth: usize,
) {
    if depth > 4 || paths.len() >= MAX_COLLECTED_PATHS {
        return;
    }
    for key in PATH_KEYS {
        let Some(candidate) = as_trimmed_string(record.get(key)) else {
            continue;
        };
        if !seen.insert(candidate) {
            continue;
        }
        paths.push(candidate);
        if paths.len() >= MAX_COLLECTED_PATHS {
            return;
        }
    }
    for nested_key in NESTED_PATH_KEYS {
        let Some(nested) = record.get(nested_key) else {
            continue;
        };
        collect_paths(nested, paths, seen, depth + 1);
        if paths.len() >= MAX_COLLECTED_PATHS {
            return;
        }
    }
}

/// Structured paths from tool input, never file-body text.
pub fn collect_tool_file_paths(data: Option<&Value>) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(data) = data {
        collect_paths(data, &mut paths, &mut HashSet::new(), 0);
    }
    paths.into_iter().map(str::to_owned).collect()
}

fn extract_primary_path(data: Option<&Record>) -> Option<&str> {
    let mut paths = Vec::new();
    collect_paths_in_record(data?, &mut paths, &mut HashSet::new(), 0);
    paths.first().copied()
}

/// Later ACP updates often resend `rawInput: {}`. Keep the first object that
/// actually carried keys so a parsed path is not wiped out.
pub fn merge_tool_activity_data(
    previous: Option<&Value>,
    next: Option<&Value>,
) -> Option<Map<String, Value>> {
    let previous_record = as_record(previous);
    let Some(next_record) = as_record(next) else {
        return previous_record.cloned();
    };
    let Some(previous_record) = previous_record else {
        return Some(next_record.clone());
    };
    let previous_input = as_record(previous_record.get("rawInput"));
    let next_input = as_record(next_record.get("rawInput"));
    let raw_input = if record_has_keys(next_input) {
        let mut raw_input = previous_input.cloned().unwrap_or_default();
        raw_input.extend(
            next_input
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        Some(raw_input)
    } else {
        previous_input.or(next_input).cloned()
    };
    let mut merged = previous_record.clone();
    merged.extend(next_record.iter().map(|(k, v)| (k.clone(), v.clone())));
    match raw_input {
        Some(raw_input) if !raw_input.is_empty() => {
            merged.insert("rawInput".to_owned(), Value::Object(raw_input));
        }
        _ => {
            // `delete` keeps the other keys in place.
            merged.shift_remove("rawInput");
        }
    }
    Some(merged)
}

static WHITESPACE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("valid whitespace regex"));
static EQUIVALENT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s+(?:complete|completed|started)\s*$").expect("valid equivalent suffix regex")
});

fn normalize_equivalent_value(value: Option<&str>) -> Option<String> {
    let trimmed = trimmed_non_empty(value?)?;
    let collapsed = WHITESPACE_RUN.replace_all(trimmed, " ");
    let stripped = EQUIVALENT_SUFFIX.replace(&collapsed, "");
    Some(js_trim(&stripped).to_owned())
}

fn is_equivalent(left: Option<&str>, right: Option<&str>) -> bool {
    let normalized_left = normalize_equivalent_value(left).map(|value| value.to_lowercase());
    let normalized_right = normalize_equivalent_value(right).map(|value| value.to_lowercase());
    normalized_left.is_some() && normalized_left == normalized_right
}

/// `ToolActivityAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolActivityAction {
    Command,
    Read,
    FileChange,
    Search,
    Other,
}

impl ToolActivityAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Read => "read",
            Self::FileChange => "file_change",
            Self::Search => "search",
            Self::Other => "other",
        }
    }
}

fn tool_name_token(value: Option<&str>) -> Option<String> {
    let trimmed = trimmed_non_empty(value?)?;
    // Server-prefixed MCP names (`github.read_file`, `mcp__db__find`) are not local reads or searches.
    if trimmed.contains("__") || trimmed.contains(['.', '/']) {
        return None;
    }
    Some(
        trimmed
            .chars()
            .filter(|c| !(*c == '_' || *c == '-' || c.is_whitespace()))
            .collect::<String>()
            .to_lowercase(),
    )
}

/// Input of [`classify_tool_activity`]. `item_type` is
/// `ToolLifecycleItemType | string`; pass [`ToolLifecycleItemType::as_str`]
/// for the typed case.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClassifyToolActivityInput<'a> {
    pub item_type: Option<&'a str>,
    pub request_kind: Option<&'a str>,
    pub title: Option<&'a str>,
    pub data: Option<&'a Map<String, Value>>,
}

pub fn classify_tool_activity(input: ClassifyToolActivityInput<'_>) -> ToolActivityAction {
    use ToolActivityAction as Action;

    let item_type = input.item_type;
    let request_kind = input
        .request_kind
        .and_then(trimmed_non_empty)
        .map(str::to_lowercase);
    let data_get = |key: &str| input.data.and_then(|data| data.get(key));
    let kind = as_trimmed_string(data_get("kind")).map(str::to_lowercase);
    let tool_name = tool_name_token(as_trimmed_string(data_get("toolName")).or_else(|| {
        as_trimmed_string(as_record(data_get("item")).and_then(|item| item.get("tool")))
    }));
    let request_kind = request_kind.as_deref();
    let kind = kind.as_deref();
    let tool_name = tool_name.as_deref();

    match item_type {
        Some("command_execution") => return Action::Command,
        Some("image_view") => return Action::Read,
        Some("file_change") => return Action::FileChange,
        Some("web_search") => return Action::Search,
        _ => {}
    }
    if request_kind == Some("command") || kind == Some("execute") {
        return Action::Command;
    }
    if request_kind == Some("file-change")
        || matches!(kind, Some("edit" | "move" | "delete" | "write"))
    {
        return Action::FileChange;
    }
    if kind == Some("search") || matches!(tool_name, Some("find" | "grep" | "glob" | "rg" | "ls")) {
        return Action::Search;
    }
    if request_kind == Some("file-read") || kind == Some("read") {
        return Action::Read;
    }
    if matches!(tool_name, Some("terminal" | "bash" | "shell")) {
        return Action::Command;
    }
    if matches!(tool_name, Some("read" | "readfile")) {
        return Action::Read;
    }
    Action::Other
}

const SEARCH_QUERY_KEYS: [&str; 6] = ["pattern", "query", "searchTerm", "regex", "grep", "needle"];
const SEARCH_GLOB_KEYS: [&str; 6] = [
    "glob",
    "globPattern",
    "glob_pattern",
    "include",
    "filePattern",
    "file_pattern",
];
const SEARCH_TARGET_KEYS: [&str; 6] = [
    "path",
    "target_directory",
    "targetDirectory",
    "directory",
    "cwd",
    "root",
];

fn first_input_string<'a>(record: Option<&'a Record>, keys: &[&str]) -> Option<&'a str> {
    let record = record?;
    keys.iter()
        .find_map(|key| as_trimmed_string(record.get(*key)))
}

fn search_input_record(data: Option<&Record>) -> Option<&Record> {
    [
        get(data, "rawInput"),
        get(data, "input"),
        get(as_record(get(data, "item")), "input"),
    ]
    .into_iter()
    .map(as_record)
    .find(|record| record_has_keys(*record))
    .flatten()
    .or(data)
}

fn search_target_name(value: Option<&str>) -> Option<&str> {
    value?
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty() && *part != ".")
}

/// Cursor-style row: "Searched files *.{ts,tsx} in t3chat-new".
/// Upstream takes `Record | undefined`; a non-object `data` reads as `undefined`.
pub fn format_search_tool_label(data: Option<&Value>) -> Option<String> {
    let input = search_input_record(as_record(data));
    let query = first_input_string(input, &SEARCH_QUERY_KEYS);
    let glob = first_input_string(input, &SEARCH_GLOB_KEYS);
    let target = search_target_name(first_input_string(input, &SEARCH_TARGET_KEYS));
    match (query, glob, target) {
        (Some(query), _, Some(target)) => Some(format!("Searched {query} in {target}")),
        (_, Some(glob), Some(target)) => Some(format!("Searched files {glob} in {target}")),
        (_, Some(glob), None) => Some(format!("Searched files {glob}")),
        (Some(query), None, None) => Some(format!("Searched {query}")),
        (None, None, Some(target)) => Some(format!("Searched in {target}")),
        (None, None, None) => None,
    }
}

/// Work-log heading for a file read: verb plus the structured path, never the path alone.
/// Upstream's `extraCount` defaults to `0`.
pub fn format_read_tool_label(path: &str, extra_count: usize) -> String {
    let trimmed = js_trim(path);
    let suffix = if extra_count > 0 {
        format!(" +{extra_count} more")
    } else {
        String::new()
    };
    if trimmed.is_empty() {
        return format!("Read file{suffix}");
    }
    format!("Read {trimmed}{suffix}")
}

/// `ToolActivityPresentationInput`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ToolActivityPresentationInput<'a> {
    pub item_type: Option<ToolLifecycleItemType>,
    pub title: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub data: Option<&'a Value>,
    pub fallback_summary: Option<&'a str>,
}

/// `ToolActivityPresentation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolActivityPresentation {
    pub summary: String,
    pub detail: Option<String>,
}

pub fn derive_tool_activity_presentation(
    input: ToolActivityPresentationInput<'_>,
) -> ToolActivityPresentation {
    let title = input.title.and_then(trimmed_non_empty);
    let detail = strip_trailing_exit_code(input.detail.and_then(trimmed_non_empty));
    let fallback_summary = input
        .fallback_summary
        .and_then(trimmed_non_empty)
        .unwrap_or("Tool");
    let data = as_record(input.data);
    let command = extract_tool_command(data, title);
    let primary_path = extract_primary_path(data);
    let action = classify_tool_activity(ClassifyToolActivityInput {
        item_type: input.item_type.map(ToolLifecycleItemType::as_str),
        request_kind: None,
        title,
        data,
    });

    let presentation =
        |summary: String, detail: Option<String>| ToolActivityPresentation { summary, detail };

    match action {
        ToolActivityAction::Command => presentation("Ran command".to_owned(), command),
        ToolActivityAction::Read => match primary_path {
            Some(primary_path) => presentation(format_read_tool_label(primary_path, 0), None),
            None => presentation("Read file".to_owned(), None),
        },
        ToolActivityAction::FileChange => {
            presentation("Changed files".to_owned(), primary_path.map(str::to_owned))
        }
        ToolActivityAction::Search => match format_search_tool_label(input.data) {
            Some(search_label) => presentation(search_label, None),
            None => presentation("Searched files".to_owned(), None),
        },
        ToolActivityAction::Other => {
            let summary = title.unwrap_or(fallback_summary).to_owned();
            match detail {
                Some(detail)
                    if !is_equivalent(Some(detail), title)
                        && !is_equivalent(Some(detail), Some(fallback_summary)) =>
                {
                    presentation(summary, Some(detail.to_owned()))
                }
                _ => presentation(summary, None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected an object fixture, got {other}"),
        }
    }

    fn classify_data(data: &Value) -> ToolActivityAction {
        classify_tool_activity(ClassifyToolActivityInput {
            data: data.as_object(),
            ..Default::default()
        })
    }

    #[test]
    fn normalizes_command_tools_to_a_stable_ran_command_label() {
        let data = json!({ "command": "bun run lint" });
        assert_eq!(
            derive_tool_activity_presentation(ToolActivityPresentationInput {
                item_type: Some(ToolLifecycleItemType::CommandExecution),
                title: Some("Terminal"),
                detail: Some("Terminal"),
                data: Some(&data),
                fallback_summary: Some("Terminal"),
            }),
            ToolActivityPresentation {
                summary: "Ran command".to_owned(),
                detail: Some("bun run lint".to_owned()),
            }
        );
    }

    #[test]
    fn uses_structured_file_paths_for_read_file_tools_when_available() {
        let data = json!({ "kind": "read", "locations": [{ "path": "/tmp/app.ts" }] });
        assert_eq!(
            derive_tool_activity_presentation(ToolActivityPresentationInput {
                item_type: Some(ToolLifecycleItemType::DynamicToolCall),
                title: Some("Read File"),
                detail: Some("Read File"),
                data: Some(&data),
                fallback_summary: Some("Read File"),
            }),
            ToolActivityPresentation {
                summary: "Read /tmp/app.ts".to_owned(),
                detail: None,
            }
        );
    }

    #[test]
    fn drops_duplicated_generic_read_file_detail_when_no_path_is_available() {
        let data = json!({ "kind": "read", "rawInput": {} });
        assert_eq!(
            derive_tool_activity_presentation(ToolActivityPresentationInput {
                item_type: Some(ToolLifecycleItemType::DynamicToolCall),
                title: Some("Read File"),
                detail: Some("Read File"),
                data: Some(&data),
                fallback_summary: Some("Read File"),
            }),
            ToolActivityPresentation {
                summary: "Read file".to_owned(),
                detail: None,
            }
        );
    }

    #[test]
    fn classifies_from_kind_and_tool_name_without_sniffing_titles() {
        assert_eq!(
            classify_data(&json!({ "kind": "read" })),
            ToolActivityAction::Read
        );
        assert_eq!(
            classify_data(&json!({ "toolName": "Grep" })),
            ToolActivityAction::Search
        );
        assert_eq!(
            classify_data(&json!({ "toolName": "Read" })),
            ToolActivityAction::Read
        );
        for tool_name in ["github.read_file", "mongodb.find", "mcp__github__read_file"] {
            assert_eq!(
                classify_data(&json!({ "toolName": tool_name })),
                ToolActivityAction::Other,
                "{tool_name}"
            );
        }
        let empty = Map::new();
        assert_eq!(
            classify_tool_activity(ClassifyToolActivityInput {
                title: Some("Find"),
                data: Some(&empty),
                ..Default::default()
            }),
            ToolActivityAction::Other
        );
    }

    #[test]
    fn classifies_claude_search_tools_ahead_of_their_broad_file_read_request_kind() {
        for tool_name in ["Glob", "Grep", "LS", "Read"] {
            let data = object(json!({ "toolName": tool_name }));
            let expected = if tool_name == "Read" {
                ToolActivityAction::Read
            } else {
                ToolActivityAction::Search
            };
            assert_eq!(
                classify_tool_activity(ClassifyToolActivityInput {
                    request_kind: Some("file-read"),
                    data: Some(&data),
                    ..Default::default()
                }),
                expected,
                "{tool_name}"
            );
        }
    }

    #[test]
    fn formats_read_and_search_labels_from_structured_input() {
        assert_eq!(format_read_tool_label("src/env.ts", 0), "Read src/env.ts");
        assert_eq!(
            format_read_tool_label("src/env.ts", 2),
            "Read src/env.ts +2 more"
        );
        assert_eq!(format_read_tool_label("", 0), "Read file");
        let search = |data: Value| format_search_tool_label(Some(&data));
        assert_eq!(
            search(json!({ "input": { "pattern": "TODO", "path": "apps/web" } })).as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            search(json!({ "input": { "glob": "*.ts", "path": "/tmp/t3chat-new" } })).as_deref(),
            Some("Searched files *.ts in t3chat-new")
        );
        assert_eq!(
            search(json!({ "rawInput": {}, "input": { "pattern": "TODO", "path": "apps/web" } }))
                .as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            search(json!({ "input": { "globPattern": "*.tsx", "path": "apps/web" } })).as_deref(),
            Some("Searched files *.tsx in web")
        );
        assert_eq!(
            search(json!({ "input": { "pattern": "TODO", "glob": "*.ts", "path": "apps/web" } }))
                .as_deref(),
            Some("Searched TODO in web")
        );
    }

    #[test]
    fn keeps_bare_filenames_from_explicit_path_fields() {
        assert_eq!(
            collect_tool_file_paths(Some(&json!({ "input": { "file_path": "README" } }))),
            vec!["README".to_owned()]
        );
    }

    #[test]
    fn keeps_the_first_non_empty_raw_input_when_a_later_update_is_empty() {
        assert_eq!(
            merge_tool_activity_data(
                Some(&json!({ "rawInput": { "path": "src/a.ts" } })),
                Some(&json!({ "rawInput": {}, "kind": "read" })),
            ),
            Some(object(
                json!({ "rawInput": { "path": "src/a.ts" }, "kind": "read" })
            ))
        );
        assert_eq!(
            merge_tool_activity_data(
                Some(&json!({ "rawInput": { "path": "src/a.ts" } })),
                Some(&json!({ "rawInput": { "startLine": 4 } })),
            ),
            Some(object(
                json!({ "rawInput": { "path": "src/a.ts", "startLine": 4 } })
            ))
        );
    }

    #[test]
    fn uses_the_action_title_for_repl_tools() {
        for tool_name in ["cua_repl.js", "node_repl.js", "js"] {
            let tool = Some(tool_name);
            assert_eq!(
                dynamic_tool_title(
                    tool,
                    &json!({ "title": " Read the current T3 desktop window " }),
                    None
                )
                .as_deref(),
                Some("Read the current T3 desktop window")
            );
            let fallback = if tool_name == "cua_repl.js" {
                "Computer use"
            } else {
                "Run JavaScript"
            };
            assert_eq!(
                dynamic_tool_title(tool, &json!({ "title": " " }), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(tool, &json!({ "title": 123 }), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(tool, &json!({}), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(
                    tool,
                    &json!({ "code": "await sky.get_app_state(app);" }),
                    None
                )
                .as_deref(),
                Some("Computer use")
            );
        }
    }

    #[test]
    fn keeps_a_retained_action_title_but_replaces_raw_repl_names() {
        assert_eq!(
            dynamic_tool_title(Some("js"), &json!({}), Some("Read the window")).as_deref(),
            Some("Read the window")
        );
        assert_eq!(
            dynamic_tool_title(
                Some("js"),
                &json!({ "title": "Inspect the window" }),
                Some("Read the window")
            )
            .as_deref(),
            Some("Inspect the window")
        );
        assert_eq!(
            dynamic_tool_title(
                Some("js"),
                &json!({ "code": "await sky.click(app);" }),
                Some("Js")
            )
            .as_deref(),
            Some("Computer use")
        );
        assert_eq!(
            dynamic_tool_title(Some("node_repl.js"), &json!({}), Some("Node_repl.js")).as_deref(),
            Some("Run JavaScript")
        );
    }

    #[test]
    fn recognizes_computer_use_repl_calls_before_result_metadata_exists() {
        assert_eq!(
            dynamic_tool_surface(Some("cua_repl.js"), &json!({})),
            Some(ToolActivitySurface::Computer)
        );
        for name in ["node_repl.js", "js"] {
            assert_eq!(
                dynamic_tool_surface(
                    Some(name),
                    &json!({ "code": "await sky.get_app_state(app);" })
                ),
                Some(ToolActivitySurface::Computer)
            );
            assert_eq!(
                dynamic_tool_surface(
                    Some(name),
                    &json!({ "code": "const sky = await import('@oai/sky');" })
                ),
                Some(ToolActivitySurface::Computer)
            );
            assert_eq!(
                dynamic_tool_surface(Some(name), &json!({ "code": "await fs.readFile(path);" })),
                None
            );
        }
        assert_eq!(
            dynamic_tool_surface(
                Some("github.js"),
                &json!({ "code": "await sky.click(app);" })
            ),
            None
        );
    }

    #[test]
    fn leaves_ordinary_tool_titles_in_their_arguments() {
        assert_eq!(
            dynamic_tool_title(
                Some("github.create_issue"),
                &json!({ "title": "Issue title" }),
                None
            ),
            None
        );
    }

    #[test]
    fn titles_claude_skill_calls_with_the_skill_they_load() {
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({ "skill": "full-send" }), None).as_deref(),
            Some("Skill: full-send")
        );
        let input = json!({ "skill": "claude-api", "args": " pricing " });
        assert_eq!(
            claude_skill_invocation(Some("Skill"), &input),
            Some(ClaudeSkillInvocation {
                name: "claude-api",
                args: Some("pricing"),
            })
        );
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({ "skill": " " }), None),
            None
        );
        assert_eq!(
            dynamic_tool_title(Some("Read"), &json!({ "skill": "full-send" }), None),
            None
        );
    }

    #[test]
    fn tool_lifecycle_item_types_round_trip() {
        for item_type in ToolLifecycleItemType::ALL {
            assert!(is_tool_lifecycle_item_type(item_type.as_str()));
            assert_eq!(
                ToolLifecycleItemType::parse(item_type.as_str()),
                Some(item_type)
            );
        }
        assert!(!is_tool_lifecycle_item_type("reasoning"));
    }
}
