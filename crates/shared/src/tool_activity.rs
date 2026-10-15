//! Port of upstream/packages/shared/src/toolActivity.ts
//!
//! Tool-call presentation: the work-log label a tool call gets, and the command, path and search
//! target it is labelled with. Inputs are the JSON values upstream reads. `Value::Null` stands for
//! JS `undefined`, which upstream accepts in the same places.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};
use turin_contracts::base_schemas::js_trim;

type Record = Map<String, Value>;

static NULL: Value = Value::Null;

/// `value[key]`, or `undefined` (`Value::Null`) when absent or when `value` is not an object.
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

/// `asTrimmedString`: the trimmed text, or `None` for non-strings and blanks.
fn trimmed_value(value: &Value) -> Option<&str> {
    value.as_str().map(js_trim).filter(|text| !text.is_empty())
}

fn trimmed_str(value: Option<&str>) -> Option<&str> {
    value.map(js_trim).filter(|text| !text.is_empty())
}

/// `recordHasKeys`: a non-empty object.
fn record_has_keys(value: &Value) -> bool {
    value.as_object().is_some_and(|record| !record.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSkillInvocation {
    pub name: String,
    pub args: Option<String>,
}

/// A Claude `Skill` call: the skill it loads and the arguments it passes, if any.
pub fn claude_skill_invocation(
    tool_name: Option<&str>,
    input: &Value,
) -> Option<ClaudeSkillInvocation> {
    if tool_name != Some("Skill") {
        return None;
    }
    let name = trimmed_value(field(input, "skill"))?;
    Some(ClaudeSkillInvocation {
        name: name.to_owned(),
        args: trimmed_value(field(input, "args")).map(str::to_owned),
    })
}

pub fn is_java_script_repl_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("js" | "cua_repl.js" | "node_repl.js"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSource {
    pub key: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
}

pub const COMPUTER_USE_TOOL_SOURCE: ToolSource = ToolSource {
    key: "computer-use",
    name: "Computer Use",
    kind: "computer",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicToolSurface {
    Computer,
}

fn sky_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?-u:\b)sky\s*\.").expect("valid regex"))
}

fn sky_import() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"["']@oai/sky["']"#).expect("valid regex"))
}

/// Computer-use calls may fail or still be running before result metadata is available.
pub fn dynamic_tool_surface(tool_name: Option<&str>, input: &Value) -> Option<DynamicToolSurface> {
    if tool_name == Some("cua_repl.js") {
        return Some(DynamicToolSurface::Computer);
    }
    let code = field(input, "code").as_str()?;
    (is_java_script_repl_tool(tool_name)
        && (sky_call().is_match(code) || sky_import().is_match(code)))
    .then_some(DynamicToolSurface::Computer)
}

/// Activity log heading a dynamic tool derives from its input: a REPL's `title`, or the skill a
/// Claude `Skill` call loads.
pub fn dynamic_tool_title(
    tool_name: Option<&str>,
    input: &Value,
    retained_title: Option<&str>,
) -> Option<String> {
    if is_java_script_repl_tool(tool_name) {
        let title = trimmed_str(retained_title);
        let tool_name_lower = tool_name.map(str::to_lowercase);
        let meaningful_title = title.filter(|title| {
            let lower = title.to_lowercase();
            lower != "js" && Some(&lower) != tool_name_lower.as_ref()
        });
        return Some(
            trimmed_value(field(input, "title"))
                .or(meaningful_title)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if dynamic_tool_surface(tool_name, input) == Some(DynamicToolSurface::Computer)
                    {
                        "Computer use".to_owned()
                    } else {
                        "Run JavaScript".to_owned()
                    }
                }),
        );
    }
    claude_skill_invocation(tool_name, input).map(|skill| format!("Skill: {}", skill.name))
}

/// `normalizeCommandValue`: a command as a string, or a string list joined with spaces.
fn normalize_command_value(value: &Value) -> Option<String> {
    if let Some(direct) = trimmed_value(value) {
        return Some(direct.to_owned());
    }
    let entries = value.as_array()?;
    let parts: Vec<&str> = entries.iter().filter_map(trimmed_value).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn exit_code_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?is)^(?P<output>.*?)(?:\s*<exited with exit code [0-9]+>)\s*$")
            .expect("valid regex")
    })
}

/// Drops a trailing `<exited with exit code N>` marker; `None` when nothing is left.
fn strip_trailing_exit_code(value: Option<&str>) -> Option<String> {
    let trimmed = trimmed_str(value)?;
    let output = match exit_code_regex().captures(trimmed) {
        Some(captures) => js_trim(captures.name("output").map_or("", |m| m.as_str())),
        None => trimmed,
    };
    (!output.is_empty()).then(|| output.to_owned())
}

fn extract_command_from_title(title: Option<&str>) -> Option<String> {
    fn backtick_regex() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"`([^`]+)`").expect("valid regex"))
    }
    let title = title?;
    let captured = backtick_regex().captures(title)?.get(1)?.as_str();
    trimmed_str(Some(captured)).map(str::to_owned)
}

fn extract_tool_command(data: &Value, title: Option<&str>) -> Option<String> {
    let item = field(data, "item");
    let item_input = field(item, "input");
    let item_result = field(item, "result");
    let raw_input = field(data, "rawInput");
    let direct = [
        normalize_command_value(field(item, "command")),
        normalize_command_value(field(item_input, "command")),
        normalize_command_value(field(item_result, "command")),
        normalize_command_value(field(data, "command")),
        normalize_command_value(field(raw_input, "command")),
    ]
    .into_iter()
    .flatten()
    .next();
    if direct.is_some() {
        return direct;
    }
    let executable = trimmed_value(field(raw_input, "executable"));
    let args = normalize_command_value(field(raw_input, "args"));
    match (executable, args) {
        (Some(executable), Some(args)) => Some(format!("{executable} {args}")),
        (Some(executable), None) => Some(executable.to_owned()),
        _ => extract_command_from_title(title),
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

const PATH_NESTED_KEYS: [&str; 7] = [
    "locations",
    "item",
    "input",
    "result",
    "rawInput",
    "data",
    "changes",
];

const MAX_PATHS: usize = 8;
const MAX_PATH_DEPTH: usize = 4;

fn collect_paths(value: &Value, paths: &mut Vec<String>, seen: &mut HashSet<String>, depth: usize) {
    if depth > MAX_PATH_DEPTH || paths.len() >= MAX_PATHS {
        return;
    }
    if let Some(entries) = value.as_array() {
        for entry in entries {
            collect_paths(entry, paths, seen, depth + 1);
            if paths.len() >= MAX_PATHS {
                return;
            }
        }
        return;
    }
    let Some(record) = value.as_object() else {
        return;
    };
    for key in PATH_KEYS {
        let Some(candidate) = trimmed_value(field(value, key)) else {
            continue;
        };
        if seen.contains(candidate) {
            continue;
        }
        seen.insert(candidate.to_owned());
        paths.push(candidate.to_owned());
        if paths.len() >= MAX_PATHS {
            return;
        }
    }
    for nested in PATH_NESTED_KEYS {
        if !record.contains_key(nested) {
            continue;
        }
        collect_paths(field(value, nested), paths, seen, depth + 1);
        if paths.len() >= MAX_PATHS {
            return;
        }
    }
}

/// Structured paths from tool input, never file-body text.
pub fn collect_tool_file_paths(data: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_paths(data, &mut paths, &mut HashSet::new(), 0);
    paths
}

fn extract_primary_path(data: &Value) -> Option<String> {
    collect_tool_file_paths(data).into_iter().next()
}

/// Later ACP updates often resend `rawInput: {}`. Keep the first object that actually carried keys
/// so a parsed path is not wiped out.
pub fn merge_tool_activity_data(previous: &Value, next: &Value) -> Option<Record> {
    let Some(next_record) = next.as_object() else {
        return previous.as_object().cloned();
    };
    let Some(previous_record) = previous.as_object() else {
        return Some(next_record.clone());
    };
    let previous_input = previous_record.get("rawInput").and_then(Value::as_object);
    let next_input = next_record.get("rawInput").and_then(Value::as_object);
    let raw_input: Option<Record> = match next_input {
        Some(next_input) if !next_input.is_empty() => {
            let mut merged = previous_input.cloned().unwrap_or_default();
            for (key, value) in next_input {
                merged.insert(key.clone(), value.clone());
            }
            Some(merged)
        }
        _ => previous_input.cloned().or_else(|| next_input.cloned()),
    };
    let mut merged = previous_record.clone();
    for (key, value) in next_record {
        merged.insert(key.clone(), value.clone());
    }
    match raw_input {
        Some(raw_input) if !raw_input.is_empty() => {
            merged.insert("rawInput".to_owned(), Value::Object(raw_input));
        }
        _ => {
            merged.remove("rawInput");
        }
    }
    Some(merged)
}

fn normalize_equivalent_value(value: Option<&str>) -> Option<String> {
    fn whitespace() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"))
    }
    fn status_suffix() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"(?i)\s+(?:complete|completed|started)\s*$").expect("valid regex")
        })
    }
    let trimmed = trimmed_str(value)?;
    let collapsed = whitespace().replace_all(trimmed, " ");
    let stripped = status_suffix().replace(&collapsed, "");
    Some(js_trim(&stripped).to_owned())
}

fn is_equivalent(left: Option<&str>, right: Option<&str>) -> bool {
    let normalized_left = normalize_equivalent_value(left).map(|value| value.to_lowercase());
    let normalized_right = normalize_equivalent_value(right).map(|value| value.to_lowercase());
    normalized_left.is_some() && normalized_left == normalized_right
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolActivityAction {
    Command,
    Read,
    FileChange,
    Search,
    Other,
}

/// Server-prefixed MCP names (`github.read_file`, `mcp__db__find`) are not local reads or searches.
fn tool_name_token(value: Option<&str>) -> Option<String> {
    let trimmed = trimmed_str(value)?;
    if trimmed.contains("__") || trimmed.contains('.') || trimmed.contains('/') {
        return None;
    }
    Some(
        trimmed
            .chars()
            .filter(|character| !matches!(character, '_' | '-') && !character.is_whitespace())
            .collect::<String>()
            .to_lowercase(),
    )
}

pub struct ClassifyToolActivityInput<'a> {
    pub item_type: Option<&'a str>,
    pub request_kind: Option<&'a str>,
    pub title: Option<&'a str>,
    pub data: &'a Value,
}

pub fn classify_tool_activity(input: &ClassifyToolActivityInput<'_>) -> ToolActivityAction {
    let request_kind = trimmed_str(input.request_kind).map(str::to_lowercase);
    let kind = trimmed_value(field(input.data, "kind")).map(str::to_lowercase);
    let tool_name = tool_name_token(
        trimmed_value(field(input.data, "toolName"))
            .or_else(|| trimmed_value(field(field(input.data, "item"), "tool"))),
    );
    let request_kind = request_kind.as_deref();
    let kind = kind.as_deref();
    let tool_name = tool_name.as_deref();

    match input.item_type {
        Some("command_execution") => return ToolActivityAction::Command,
        Some("image_view") => return ToolActivityAction::Read,
        Some("file_change") => return ToolActivityAction::FileChange,
        Some("web_search") => return ToolActivityAction::Search,
        _ => {}
    }
    if request_kind == Some("command") || kind == Some("execute") {
        return ToolActivityAction::Command;
    }
    if request_kind == Some("file-change")
        || matches!(kind, Some("edit" | "move" | "delete" | "write"))
    {
        return ToolActivityAction::FileChange;
    }
    if kind == Some("search") || matches!(tool_name, Some("find" | "grep" | "glob" | "rg" | "ls")) {
        return ToolActivityAction::Search;
    }
    if request_kind == Some("file-read") || kind == Some("read") {
        return ToolActivityAction::Read;
    }
    if matches!(tool_name, Some("terminal" | "bash" | "shell")) {
        return ToolActivityAction::Command;
    }
    if matches!(tool_name, Some("read" | "readfile")) {
        return ToolActivityAction::Read;
    }
    ToolActivityAction::Other
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

fn first_input_string<'a>(record: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| trimmed_value(field(record, key)))
}

/// The record a search reads its query from: the first of `rawInput`, `input`, or `item.input` that
/// has keys, else the tool data itself.
fn search_input_record(data: &Value) -> &Value {
    [
        field(data, "rawInput"),
        field(data, "input"),
        field(field(data, "item"), "input"),
    ]
    .into_iter()
    .find(|candidate| record_has_keys(candidate))
    .unwrap_or(data)
}

fn search_target_name(value: Option<&str>) -> Option<&str> {
    value?
        .split(['/', '\\'])
        .rev()
        .find(|part| !part.is_empty() && *part != ".")
}

/// Cursor-style row: "Searched files *.{ts,tsx} in t3chat-new".
pub fn format_search_tool_label(data: &Value) -> Option<String> {
    let input = search_input_record(data);
    let query = first_input_string(input, &SEARCH_QUERY_KEYS);
    let glob = first_input_string(input, &SEARCH_GLOB_KEYS);
    let target = search_target_name(first_input_string(input, &SEARCH_TARGET_KEYS));
    if let (Some(query), Some(target)) = (query, target) {
        return Some(format!("Searched {query} in {target}"));
    }
    if let (Some(glob), Some(target)) = (glob, target) {
        return Some(format!("Searched files {glob} in {target}"));
    }
    if let Some(glob) = glob {
        return Some(format!("Searched files {glob}"));
    }
    if let Some(query) = query {
        return Some(format!("Searched {query}"));
    }
    target.map(|target| format!("Searched in {target}"))
}

/// Work-log heading for a file read: verb plus the structured path, never the path alone.
pub fn format_read_tool_label(path: &str, extra_count: usize) -> String {
    let trimmed = path.trim();
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

pub struct ToolActivityPresentationInput<'a> {
    pub item_type: Option<&'a str>,
    pub title: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub data: &'a Value,
    pub fallback_summary: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolActivityPresentation {
    pub summary: String,
    pub detail: Option<String>,
}

pub fn derive_tool_activity_presentation(
    input: &ToolActivityPresentationInput<'_>,
) -> ToolActivityPresentation {
    let title = trimmed_str(input.title);
    let detail = strip_trailing_exit_code(trimmed_str(input.detail));
    let fallback_summary = trimmed_str(input.fallback_summary).unwrap_or("Tool");
    let command = extract_tool_command(input.data, title);
    let primary_path = extract_primary_path(input.data);
    let action = classify_tool_activity(&ClassifyToolActivityInput {
        item_type: input.item_type,
        request_kind: None,
        title,
        data: input.data,
    });

    match action {
        ToolActivityAction::Command => ToolActivityPresentation {
            summary: "Ran command".to_owned(),
            detail: command,
        },
        ToolActivityAction::Read => ToolActivityPresentation {
            summary: match &primary_path {
                Some(path) => format_read_tool_label(path, 0),
                None => "Read file".to_owned(),
            },
            detail: None,
        },
        ToolActivityAction::FileChange => ToolActivityPresentation {
            summary: "Changed files".to_owned(),
            detail: primary_path,
        },
        ToolActivityAction::Search => ToolActivityPresentation {
            summary: format_search_tool_label(input.data)
                .unwrap_or_else(|| "Searched files".to_owned()),
            detail: None,
        },
        ToolActivityAction::Other => {
            let summary = title.unwrap_or(fallback_summary).to_owned();
            match detail {
                Some(detail)
                    if !is_equivalent(Some(detail.as_str()), title)
                        && !is_equivalent(Some(detail.as_str()), Some(fallback_summary)) =>
                {
                    ToolActivityPresentation {
                        summary,
                        detail: Some(detail),
                    }
                }
                _ => ToolActivityPresentation {
                    summary,
                    detail: None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_command_tools_to_a_stable_ran_command_label() {
        let presentation = derive_tool_activity_presentation(&ToolActivityPresentationInput {
            item_type: Some("command_execution"),
            title: Some("Terminal"),
            detail: Some("Terminal"),
            data: &json!({"command": "bun run lint"}),
            fallback_summary: Some("Terminal"),
        });
        assert_eq!(
            presentation,
            ToolActivityPresentation {
                summary: "Ran command".to_owned(),
                detail: Some("bun run lint".to_owned()),
            }
        );
    }

    #[test]
    fn uses_structured_file_paths_for_read_file_tools_when_available() {
        let presentation = derive_tool_activity_presentation(&ToolActivityPresentationInput {
            item_type: Some("dynamic_tool_call"),
            title: Some("Read File"),
            detail: Some("Read File"),
            data: &json!({"kind": "read", "locations": [{"path": "/tmp/app.ts"}]}),
            fallback_summary: Some("Read File"),
        });
        assert_eq!(presentation.summary, "Read /tmp/app.ts");
        assert_eq!(presentation.detail, None);
    }

    #[test]
    fn drops_duplicated_generic_read_file_detail_when_no_path_is_available() {
        let presentation = derive_tool_activity_presentation(&ToolActivityPresentationInput {
            item_type: Some("dynamic_tool_call"),
            title: Some("Read File"),
            detail: Some("Read File"),
            data: &json!({"kind": "read", "rawInput": {}}),
            fallback_summary: Some("Read File"),
        });
        assert_eq!(presentation.summary, "Read file");
    }

    #[test]
    fn classifies_from_kind_and_tool_name_without_sniffing_titles() {
        let classify = |data: Value| {
            classify_tool_activity(&ClassifyToolActivityInput {
                item_type: None,
                request_kind: None,
                title: None,
                data: &data,
            })
        };
        assert_eq!(classify(json!({"kind": "read"})), ToolActivityAction::Read);
        assert_eq!(
            classify(json!({"toolName": "Grep"})),
            ToolActivityAction::Search
        );
        assert_eq!(
            classify(json!({"toolName": "Read"})),
            ToolActivityAction::Read
        );
        for tool_name in ["github.read_file", "mongodb.find", "mcp__github__read_file"] {
            assert_eq!(
                classify(json!({"toolName": tool_name})),
                ToolActivityAction::Other
            );
        }
        let titled = classify_tool_activity(&ClassifyToolActivityInput {
            item_type: None,
            request_kind: None,
            title: Some("Find"),
            data: &json!({}),
        });
        assert_eq!(titled, ToolActivityAction::Other);
    }

    #[test]
    fn classifies_claude_search_tools_ahead_of_their_broad_file_read_request_kind() {
        for tool_name in ["Glob", "Grep", "LS"] {
            let action = classify_tool_activity(&ClassifyToolActivityInput {
                item_type: None,
                request_kind: Some("file-read"),
                title: None,
                data: &json!({"toolName": tool_name}),
            });
            assert_eq!(action, ToolActivityAction::Search, "{tool_name}");
        }
        let read = classify_tool_activity(&ClassifyToolActivityInput {
            item_type: None,
            request_kind: Some("file-read"),
            title: None,
            data: &json!({"toolName": "Read"}),
        });
        assert_eq!(read, ToolActivityAction::Read);
    }

    #[test]
    fn formats_read_and_search_labels_from_structured_input() {
        assert_eq!(format_read_tool_label("src/env.ts", 0), "Read src/env.ts");
        assert_eq!(
            format_read_tool_label("src/env.ts", 2),
            "Read src/env.ts +2 more"
        );
        assert_eq!(format_read_tool_label("", 0), "Read file");
        assert_eq!(
            format_search_tool_label(&json!({"input": {"pattern": "TODO", "path": "apps/web"}}))
                .as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            format_search_tool_label(
                &json!({"input": {"glob": "*.ts", "path": "/tmp/t3chat-new"}})
            )
            .as_deref(),
            Some("Searched files *.ts in t3chat-new")
        );
        assert_eq!(
            format_search_tool_label(
                &json!({"rawInput": {}, "input": {"pattern": "TODO", "path": "apps/web"}})
            )
            .as_deref(),
            Some("Searched TODO in web")
        );
        assert_eq!(
            format_search_tool_label(
                &json!({"input": {"globPattern": "*.tsx", "path": "apps/web"}})
            )
            .as_deref(),
            Some("Searched files *.tsx in web")
        );
        assert_eq!(
            format_search_tool_label(
                &json!({"input": {"pattern": "TODO", "glob": "*.ts", "path": "apps/web"}})
            )
            .as_deref(),
            Some("Searched TODO in web")
        );
    }

    #[test]
    fn keeps_bare_filenames_from_explicit_path_fields() {
        assert_eq!(
            collect_tool_file_paths(&json!({"input": {"file_path": "README"}})),
            vec!["README"]
        );
    }

    #[test]
    fn keeps_the_first_non_empty_raw_input_when_a_later_update_is_empty() {
        assert_eq!(
            merge_tool_activity_data(
                &json!({"rawInput": {"path": "src/a.ts"}}),
                &json!({"rawInput": {}, "kind": "read"}),
            ),
            Some(
                json!({"rawInput": {"path": "src/a.ts"}, "kind": "read"})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert_eq!(
            merge_tool_activity_data(
                &json!({"rawInput": {"path": "src/a.ts"}}),
                &json!({"rawInput": {"startLine": 4}}),
            ),
            Some(
                json!({"rawInput": {"path": "src/a.ts", "startLine": 4}})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
    }

    #[test]
    fn uses_the_action_title_for_repl_tools() {
        for tool_name in ["cua_repl.js", "node_repl.js", "js"] {
            assert_eq!(
                dynamic_tool_title(
                    Some(tool_name),
                    &json!({"title": " Read the current T3 desktop window "}),
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
                dynamic_tool_title(Some(tool_name), &json!({"title": " "}), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(Some(tool_name), &json!({"title": 123}), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(Some(tool_name), &json!({}), None).as_deref(),
                Some(fallback)
            );
            assert_eq!(
                dynamic_tool_title(
                    Some(tool_name),
                    &json!({"code": "await sky.get_app_state(app);"}),
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
                &json!({"title": "Inspect the window"}),
                Some("Read the window")
            )
            .as_deref(),
            Some("Inspect the window")
        );
        assert_eq!(
            dynamic_tool_title(
                Some("js"),
                &json!({"code": "await sky.click(app);"}),
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
            Some(DynamicToolSurface::Computer)
        );
        for name in ["node_repl.js", "js"] {
            assert_eq!(
                dynamic_tool_surface(
                    Some(name),
                    &json!({"code": "await sky.get_app_state(app);"})
                ),
                Some(DynamicToolSurface::Computer)
            );
            assert_eq!(
                dynamic_tool_surface(
                    Some(name),
                    &json!({"code": "const sky = await import('@oai/sky');"})
                ),
                Some(DynamicToolSurface::Computer)
            );
            assert_eq!(
                dynamic_tool_surface(Some(name), &json!({"code": "await fs.readFile(path);"})),
                None
            );
        }
        assert_eq!(
            dynamic_tool_surface(Some("github.js"), &json!({"code": "await sky.click(app);"})),
            None
        );
    }

    #[test]
    fn leaves_ordinary_tool_titles_in_their_arguments() {
        assert_eq!(
            dynamic_tool_title(
                Some("github.create_issue"),
                &json!({"title": "Issue title"}),
                None
            ),
            None
        );
    }

    #[test]
    fn titles_claude_skill_calls_with_the_skill_they_load() {
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({"skill": "full-send"}), None).as_deref(),
            Some("Skill: full-send")
        );
        assert_eq!(
            claude_skill_invocation(
                Some("Skill"),
                &json!({"skill": "claude-api", "args": " pricing "})
            ),
            Some(ClaudeSkillInvocation {
                name: "claude-api".to_owned(),
                args: Some("pricing".to_owned()),
            })
        );
        assert_eq!(
            dynamic_tool_title(Some("Skill"), &json!({"skill": " "}), None),
            None
        );
        assert_eq!(
            dynamic_tool_title(Some("Read"), &json!({"skill": "full-send"}), None),
            None
        );
    }
}
