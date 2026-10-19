//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts makeClaudeQueryOptions.
use crate::claude_model_catalog::ClaudeModelCatalog;
use crate::claude_model_options::compile_claude_model_selection;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
use turin_claude_protocol::{LaunchOptions, SystemPrompt, Thinking, ThinkingDisplay, Tools};
use turin_contracts::{model_selection::ModelSelection, settings::ClaudeSettings};

pub struct ClaudeQueryOptionsInput<'a> {
    pub model_selection: &'a ModelSelection,
    pub catalog: &'a ClaudeModelCatalog,
    pub native_thread_id: &'a str,
    pub resume: bool,
    pub resume_session_at: Option<&'a str>,
    pub cwd: Option<PathBuf>,
    pub attachments_dir: Option<PathBuf>,
    pub settings: Option<&'a ClaudeSettings>,
    pub sdk_settings: Option<Value>,
    pub environment: Option<BTreeMap<String, String>>,
    pub permission_mode: Option<&'a str>,
    /// `--allow-dangerously-skip-permissions` (upstream sets it with `bypassPermissions`).
    pub allow_dangerously_skip_permissions: bool,
    pub tools: Option<Vec<String>>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// Built by the owning orchestrator; the cut orchestrator MCP instructions
    /// are deliberately absent from Turin's system prompt.
    pub runtime_instructions: &'a str,
}

pub fn make_claude_query_options(input: ClaudeQueryOptionsInput<'_>) -> LaunchOptions {
    let compiled = compile_claude_model_selection(input.model_selection, input.catalog);
    let mut extra_args =
        parse_cli_flags(input.settings.map(|s| s.launch_args.as_str()).unwrap_or(""));
    let permission_arg = extra_args.shift_remove("permission-mode").flatten();
    let skip_arg = extra_args.shift_remove("dangerously-skip-permissions");
    let thinking_display = extra_args
        .get("thinking-display")
        .and_then(|v| v.as_deref());
    let summaries = compiled
        .settings
        .get("alwaysThinkingEnabled")
        .and_then(Value::as_bool)
        != Some(false)
        && thinking_display != Some("omitted");
    if summaries && !extra_args.contains_key("thinking-display") {
        extra_args.insert("thinking-display".into(), Some("summarized".into()));
    }
    let mut sdk_settings = input.sdk_settings.unwrap_or(Value::Null);
    if let Some(selection_settings) = compiled.settings.as_object()
        && !selection_settings.is_empty()
    {
        let mut merged = sdk_settings.as_object().cloned().unwrap_or_default();
        merged.extend(selection_settings.clone());
        sdk_settings = Value::Object(merged);
    }
    if let Some(window) = input
        .settings
        .map(|s| s.auto_compact_window.as_str())
        .filter(|s| !s.is_empty())
    {
        let mut merged = sdk_settings.as_object().cloned().unwrap_or_default();
        if let Ok(window) = window.parse::<u64>() {
            merged.insert("autoCompactWindow".into(), json!(window));
        }
        sdk_settings = Value::Object(merged);
    }
    if summaries && !sdk_settings.is_string() {
        let mut merged = sdk_settings.as_object().cloned().unwrap_or_default();
        merged.insert("showThinkingSummaries".into(), json!(true));
        sdk_settings = Value::Object(merged);
    }
    let mut options = LaunchOptions {
        model: Some(compiled.api_model_id),
        effort: compiled.effort,
        tools: Some(input.tools.map(Tools::Names).unwrap_or(Tools::Preset)),
        permission_mode: Some(permission_arg.unwrap_or_else(|| {
            if skip_arg
                .as_ref()
                .is_some_and(|v| v.is_none() || v.as_deref() == Some("true"))
            {
                "bypassPermissions".into()
            } else {
                input.permission_mode.unwrap_or("default").into()
            }
        })),
        allow_dangerously_skip_permissions: input.allow_dangerously_skip_permissions,
        can_use_tool: true,
        include_partial_messages: true,
        resume: input.resume.then(|| input.native_thread_id.to_owned()),
        session_id: (!input.resume).then(|| input.native_thread_id.to_owned()),
        resume_session_at: input.resume_session_at.map(str::to_owned),
        allowed_tools: input.allowed_tools,
        disallowed_tools: input.disallowed_tools,
        thinking: summaries.then_some(Thinking::Adaptive {
            display: Some(ThinkingDisplay::Summarized),
        }),
        settings: (!sdk_settings.is_null()).then_some(sdk_settings),
        cwd: input.cwd.clone(),
        system_prompt: Some(SystemPrompt::Preset {
            append: Some(input.runtime_instructions.into()),
            exclude_dynamic_sections: None,
            snapshot: None,
        }),
        extra_args,
        ..Default::default()
    };
    options.additional_directories.extend(input.cwd);
    options.additional_directories.extend(input.attachments_dir);
    if let Some(settings) = input.settings
        && !settings.binary_path.as_str().is_empty()
    {
        options.binary = settings.binary_path.as_str().into();
    }
    if input.environment.is_some()
        || input
            .settings
            .is_some_and(|s| !s.home_path.as_str().is_empty())
    {
        let mut env = input
            .environment
            .unwrap_or_else(|| std::env::vars().collect());
        if let Some(home) = input
            .settings
            .map(|s| s.home_path.as_str())
            .filter(|s| !s.is_empty())
        {
            env.insert("CLAUDE_CONFIG_DIR".into(), home.into());
        }
        options.env = Some(env);
    }
    options
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut quoted = false;
    let chars: Vec<char> = input.trim().chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        if let Some(delimiter) = quote {
            if char == delimiter {
                quote = None;
                quoted = true;
            } else if char == '\\'
                && delimiter == '"'
                && chars
                    .get(index + 1)
                    .is_some_and(|c| matches!(c, '"' | '\\' | '$' | '`'))
            {
                current.push(chars[index + 1]);
                index += 1;
            } else {
                current.push(char);
            }
        } else if char == '\'' || char == '"' {
            quote = Some(char);
            quoted = true;
        } else if char.is_whitespace() {
            if !current.is_empty() || quoted {
                tokens.push(std::mem::take(&mut current));
                quoted = false;
            }
        } else if char == '\\' && chars.get(index + 1).is_some_and(|c| c.is_whitespace()) {
            current.push(chars[index + 1]);
            index += 1;
        } else {
            current.push(char);
        }
        index += 1;
    }
    if !current.is_empty() || quoted {
        tokens.push(current);
    }
    tokens
}
fn parse_cli_flags(input: &str) -> indexmap::IndexMap<String, Option<String>> {
    let tokens = tokenize(input);
    let mut flags = indexmap::IndexMap::new();
    let mut index = 0;
    while index < tokens.len() {
        if let Some(rest) = tokens[index].strip_prefix("--")
            && !rest.is_empty()
        {
            if let Some((name, value)) = rest.split_once('=') {
                flags.insert(name.into(), Some(value.into()));
            } else if tokens
                .get(index + 1)
                .is_some_and(|next| !next.starts_with("--"))
            {
                flags.insert(rest.into(), Some(tokens[index + 1].clone()));
                index += 1;
            } else {
                flags.insert(rest.into(), None);
            }
        }
        index += 1;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_args_strip_permission_flags_and_set_summaries() {
        let selection: ModelSelection =
            serde_json::from_value(json!({"instanceId":"claudeAgent","model":"sonnet"})).unwrap();
        let settings: ClaudeSettings = serde_json::from_value(json!({"launchArgs":"--permission-mode plan --dangerously-skip-permissions --foo 'a b'","autoCompactWindow":"200000"})).unwrap();
        let options = make_claude_query_options(ClaudeQueryOptionsInput {
            model_selection: &selection,
            catalog: &ClaudeModelCatalog::default(),
            native_thread_id: "abc",
            resume: false,
            resume_session_at: None,
            cwd: None,
            attachments_dir: None,
            settings: Some(&settings),
            sdk_settings: None,
            environment: None,
            permission_mode: None,
            allow_dangerously_skip_permissions: false,
            tools: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            runtime_instructions: "runtime",
        });
        assert_eq!(options.permission_mode.as_deref(), Some("plan"));
        assert_eq!(
            options.extra_args.get("foo").and_then(|v| v.as_deref()),
            Some("a b")
        );
        assert!(!options.extra_args.contains_key("permission-mode"));
        assert_eq!(options.settings.unwrap()["autoCompactWindow"], 200000);
        assert_eq!(options.session_id.as_deref(), Some("abc"));
    }
}
