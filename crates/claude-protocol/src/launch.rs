//! Port of `@anthropic-ai/claude-agent-sdk@0.3.276` `query` → `k0` and
//! `ProcessTransport.initialize` (`sdk.mjs`). Argument order matches the SDK.

use std::{collections::BTreeMap, path::PathBuf};

use indexmap::IndexMap;
use serde_json::{Map, Value};

pub const SDK_VERSION: &str = "0.3.276";

/// Non-array `tools` (the `{type:"preset",preset:"claude_code"}` value upstream
/// passes) becomes `--tools default`. An explicit list is comma-joined, and
/// `[]` is an empty argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tools {
    Preset,
    Names(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

impl ThinkingDisplay {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// `thinking` wins over the deprecated `maxThinkingTokens` shorthand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thinking {
    Adaptive {
        display: Option<ThinkingDisplay>,
    },
    Enabled {
        budget_tokens: Option<u32>,
        display: Option<ThinkingDisplay>,
    },
    Disabled,
}

/// `systemPrompt` as `query()` accepts it. An omitted option is not this enum:
/// `initialize` then sends `systemPrompt: [""]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPrompt {
    /// Preset `claude_code`. Only `appendSystemPrompt` is sent; a `systemPrompt`
    /// field would replace the preset.
    Preset {
        append: Option<String>,
        exclude_dynamic_sections: Option<bool>,
        snapshot: Option<bool>,
    },
    /// A string prompt is wrapped in a one-element array.
    Text(String),
    /// Already an array of prompt parts.
    Parts(Vec<String>),
    Custom {
        prompt: PromptBody,
        snapshot: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptBody {
    Text(String),
    Parts(Vec<String>),
}

/// `skills: "all"` appends `Skill` to `allowedTools`. A name list appends
/// `Skill(<name>)` for names not already present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skills {
    All,
    Names(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    pub path: String,
    pub skip_mcp_discovery: bool,
}

#[derive(Debug, Clone, Default)]
pub struct InitializeOptions {
    pub hooks: Option<Value>,
    pub sdk_mcp_servers: Vec<String>,
    pub sdk_mcp_server_configs: IndexMap<String, Value>,
    pub agents: Option<Value>,
    pub supported_dialog_kinds: Option<Vec<String>>,
    pub per_task_stop_affordance: Option<bool>,
    pub json_schema: Option<Value>,
    pub tool_aliases: Option<Value>,
    pub title: Option<String>,
    pub skills: Option<Vec<String>>,
    pub plan_mode_instructions: Option<String>,
    pub prompt_suggestions: Option<bool>,
    pub forward_subagent_text: Option<bool>,
    pub plugins: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub binary: PathBuf,
    /// Prepended to the protocol arguments, matching `executableArgs`.
    pub executable_args: Vec<String>,
    pub initialize: InitializeOptions,
    pub cwd: Option<PathBuf>,
    /// `Some` replaces the inherited environment entirely, as the SDK `env` option does.
    pub env: Option<BTreeMap<String, String>>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub thinking: Option<Thinking>,
    /// Used only when `thinking` is absent. `0` becomes `--thinking disabled`.
    pub max_thinking_tokens: Option<u32>,
    pub permission_mode: Option<String>,
    /// When set, an absent `permission_mode` is omitted instead of defaulting to `default`.
    pub resolve_permission_mode_in_cli: bool,
    pub allow_dangerously_skip_permissions: bool,
    pub can_use_tool: bool,
    pub permission_prompt_tool_name: Option<String>,
    pub tools: Option<Tools>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub skills: Option<Skills>,
    pub system_prompt: Option<SystemPrompt>,
    pub resume: Option<String>,
    pub continue_conversation: bool,
    pub resume_session_at: Option<String>,
    pub resume_drops_turn: Option<String>,
    pub session_id: Option<String>,
    pub fork_session: bool,
    pub setting_sources: Option<Vec<String>>,
    pub settings: Option<Value>,
    pub managed_settings: Option<String>,
    pub sandbox: Option<Value>,
    pub mcp_servers: IndexMap<String, Value>,
    pub strict_mcp_config: bool,
    pub include_partial_messages: bool,
    pub include_hook_events: bool,
    pub persist_session: bool,
    pub session_mirror: bool,
    pub additional_directories: Vec<PathBuf>,
    pub extra_args: IndexMap<String, Option<String>>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub task_budget: Option<u64>,
    pub fallback_model: Option<String>,
    pub agent: Option<String>,
    pub betas: Vec<String>,
    pub debug: bool,
    pub debug_file: Option<PathBuf>,
    pub channels: Vec<String>,
    pub permission_prompts: Option<String>,
    pub project_config_root: Option<PathBuf>,
    pub plugins_via_initialize: bool,
    pub plugins: Vec<Plugin>,
    pub enable_file_checkpointing: bool,
    pub question_preview_format: Option<String>,
    pub question_extended: bool,
    pub question_optional_descriptions: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            binary: "claude".into(),
            executable_args: vec![],
            initialize: InitializeOptions::default(),
            cwd: None,
            env: None,
            model: None,
            effort: None,
            thinking: None,
            max_thinking_tokens: None,
            permission_mode: Some("default".into()),
            resolve_permission_mode_in_cli: false,
            allow_dangerously_skip_permissions: false,
            can_use_tool: false,
            permission_prompt_tool_name: None,
            tools: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            skills: None,
            system_prompt: None,
            resume: None,
            continue_conversation: false,
            resume_session_at: None,
            resume_drops_turn: None,
            session_id: None,
            fork_session: false,
            setting_sources: None,
            settings: None,
            managed_settings: None,
            sandbox: None,
            mcp_servers: IndexMap::new(),
            strict_mcp_config: false,
            include_partial_messages: false,
            include_hook_events: false,
            persist_session: true,
            session_mirror: false,
            additional_directories: vec![],
            extra_args: IndexMap::new(),
            max_turns: None,
            max_budget_usd: None,
            task_budget: None,
            fallback_model: None,
            agent: None,
            betas: vec![],
            debug: false,
            debug_file: None,
            channels: vec![],
            permission_prompts: None,
            project_config_root: None,
            plugins_via_initialize: false,
            plugins: vec![],
            enable_file_checkpointing: false,
            question_preview_format: None,
            question_extended: false,
            question_optional_descriptions: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub binary: PathBuf,
    pub cwd: Option<PathBuf>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl LaunchOptions {
    pub fn spec(&self) -> anyhow::Result<LaunchSpec> {
        if self.can_use_tool && self.permission_prompt_tool_name.is_some() {
            anyhow::bail!(
                "canUseTool callback cannot be used with permissionPromptToolName. Please use one or the other."
            );
        }
        if self
            .fallback_model
            .as_ref()
            .zip(self.model.as_ref())
            .is_some_and(|(fallback, model)| fallback == model)
        {
            anyhow::bail!(
                "Fallback model cannot be the same as the main model. Please specify a different model for fallbackModel option."
            );
        }

        let mut args = self.executable_args.clone();
        args.extend(
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
            ]
            .into_iter()
            .map(str::to_owned),
        );

        let thinking = self.resolved_thinking();
        if let Some(thinking) = &thinking {
            match thinking {
                Thinking::Enabled { budget_tokens, .. } => {
                    if let Some(budget) = budget_tokens {
                        pair(&mut args, "--max-thinking-tokens", &budget.to_string());
                    } else {
                        pair(&mut args, "--thinking", "adaptive");
                    }
                }
                Thinking::Disabled => pair(&mut args, "--thinking", "disabled"),
                Thinking::Adaptive { .. } => pair(&mut args, "--thinking", "adaptive"),
            }
            if !matches!(thinking, Thinking::Disabled) {
                let display = match thinking {
                    Thinking::Adaptive { display } | Thinking::Enabled { display, .. } => display,
                    Thinking::Disabled => &None,
                };
                if let Some(display) = display {
                    pair(&mut args, "--thinking-display", display.as_str());
                }
            }
        }
        if let Some(effort) = &self.effort
            && !effort.is_empty()
        {
            pair(&mut args, "--effort", effort);
        }
        // `if (maxTurns)` — zero is omitted.
        if let Some(turns) = self.max_turns.filter(|turns| *turns != 0) {
            pair(&mut args, "--max-turns", &turns.to_string());
        }
        if let Some(budget) = self.max_budget_usd {
            pair(&mut args, "--max-budget-usd", &js_number(budget));
        }
        if let Some(total) = self.task_budget.filter(|total| *total != 0) {
            pair(&mut args, "--task-budget", &total.to_string());
        }
        if let Some(model) = &self.model {
            pair(&mut args, "--model", model);
        }
        if let Some(agent) = &self.agent {
            pair(&mut args, "--agent", agent);
        }
        if !self.betas.is_empty() {
            pair(&mut args, "--betas", &self.betas.join(","));
        }
        if let Some(schema) = &self.initialize.json_schema {
            pair(&mut args, "--json-schema", &serde_json::to_string(schema)?);
        }
        if let Some(path) = &self.debug_file {
            pair(&mut args, "--debug-file", &path.to_string_lossy());
        } else if self.debug {
            args.push("--debug".into());
        }
        if self.can_use_tool {
            pair(&mut args, "--permission-prompt-tool", "stdio");
        } else if let Some(name) = &self.permission_prompt_tool_name {
            pair(&mut args, "--permission-prompt-tool", name);
        }
        if let Some(prompts) = &self.permission_prompts {
            pair(&mut args, "--permission-prompts", prompts);
        }
        if self.continue_conversation {
            args.push("--continue".into());
        }
        if let Some(resume) = &self.resume {
            args.push(format!("--resume={resume}"));
        }
        for channel in &self.channels {
            pair(&mut args, "--channels", channel);
        }

        let mut allowed = self.allowed_tools.clone();
        if let Some(skills) = &self.skills {
            let extra = match skills {
                Skills::All => vec!["Skill".to_owned()],
                Skills::Names(names) => names.iter().map(|name| format!("Skill({name})")).collect(),
            };
            for name in extra {
                if !allowed.iter().any(|existing| existing == &name) {
                    allowed.push(name);
                }
            }
        }
        if !allowed.is_empty() {
            pair(&mut args, "--allowedTools", &allowed.join(","));
        }
        if !self.disallowed_tools.is_empty() {
            pair(
                &mut args,
                "--disallowedTools",
                &self.disallowed_tools.join(","),
            );
        }
        if let Some(tools) = &self.tools {
            let value = match tools {
                Tools::Preset => "default".to_owned(),
                Tools::Names(names) if names.is_empty() => String::new(),
                Tools::Names(names) => names.join(","),
            };
            pair(&mut args, "--tools", &value);
        }
        if !self.mcp_servers.is_empty() {
            let payload = serde_json::json!({ "mcpServers": self.mcp_servers });
            pair(&mut args, "--mcp-config", &serde_json::to_string(&payload)?);
        }
        if let Some(sources) = &self.setting_sources {
            args.push(format!("--setting-sources={}", sources.join(",")));
        }
        if self.strict_mcp_config {
            args.push("--strict-mcp-config".into());
        }
        if let Some(mode) = self.permission_mode_flag() {
            pair(&mut args, "--permission-mode", &mode);
        }
        if self.allow_dangerously_skip_permissions {
            args.push("--allow-dangerously-skip-permissions".into());
        }
        if let Some(fallback) = &self.fallback_model {
            pair(&mut args, "--fallback-model", fallback);
        }
        if self.include_hook_events {
            args.push("--include-hook-events".into());
        }
        if self.include_partial_messages {
            args.push("--include-partial-messages".into());
        }
        if self.session_mirror {
            args.push("--session-mirror".into());
        }
        if let Some(root) = &self.project_config_root {
            args.push(format!("--project-config-root={}", root.display()));
        }
        for directory in &self.additional_directories {
            pair(&mut args, "--add-dir", &directory.to_string_lossy());
        }
        if self.plugins_via_initialize && !self.plugins.is_empty() {
            args.push("--await-initialize".into());
        } else {
            for plugin in &self.plugins {
                let flag = if plugin.skip_mcp_discovery {
                    "--plugin-dir-no-mcp"
                } else {
                    "--plugin-dir"
                };
                pair(&mut args, flag, &plugin.path);
            }
        }
        if self.fork_session {
            args.push("--fork-session".into());
        }
        if let Some(at) = &self.resume_session_at {
            args.push(format!("--resume-session-at={at}"));
        }
        if let Some(drops) = &self.resume_drops_turn {
            args.push(format!("--resume-drops-turn={drops}"));
        }
        if let Some(id) = &self.session_id {
            args.push(format!("--session-id={id}"));
        }
        if !self.persist_session {
            args.push("--no-session-persistence".into());
        }
        if let Some(managed) = &self.managed_settings {
            pair(&mut args, "--managed-settings", managed);
        }

        let mut extra = self.extra_args.clone();
        if let Some(settings) = self.merged_settings()? {
            extra.insert("settings".into(), Some(settings));
        }
        for (key, value) in &extra {
            let flag = format!("--{key}");
            if let Some(value) = value {
                pair(&mut args, &flag, value);
            } else {
                args.push(flag);
            }
        }

        Ok(LaunchSpec {
            binary: self.binary.clone(),
            cwd: self.cwd.clone(),
            args,
            env: self.child_env(),
        })
    }

    /// Control-request body for `subtype: "initialize"`. Undefined SDK fields
    /// are omitted; an omitted system prompt is `[""]`.
    pub fn initialize_request(&self) -> Value {
        let mut request = Map::new();
        request.insert("subtype".into(), Value::String("initialize".into()));
        if let Some(hooks) = &self.initialize.hooks {
            request.insert("hooks".into(), hooks.clone());
        }
        if !self.initialize.sdk_mcp_servers.is_empty() {
            request.insert(
                "sdkMcpServers".into(),
                serde_json::json!(self.initialize.sdk_mcp_servers),
            );
        }
        if !self.initialize.sdk_mcp_server_configs.is_empty() {
            request.insert(
                "sdkMcpServerConfigs".into(),
                serde_json::json!(self.initialize.sdk_mcp_server_configs),
            );
        }
        if let Some(schema) = &self.initialize.json_schema {
            request.insert("jsonSchema".into(), schema.clone());
        }
        self.insert_system_prompt(&mut request);
        if let Some(instructions) = &self.initialize.plan_mode_instructions {
            request.insert(
                "planModeInstructions".into(),
                Value::String(instructions.clone()),
            );
        }
        if let Some(aliases) = &self.initialize.tool_aliases {
            request.insert("toolAliases".into(), aliases.clone());
        }
        if let Some(agents) = &self.initialize.agents {
            request.insert("agents".into(), agents.clone());
        }
        if let Some(title) = &self.initialize.title {
            request.insert("title".into(), Value::String(title.clone()));
        }
        if let Some(skills) = &self.initialize.skills {
            request.insert("skills".into(), serde_json::json!(skills));
        }
        if let Some(value) = self.initialize.prompt_suggestions {
            request.insert("promptSuggestions".into(), Value::Bool(value));
        }
        if let Some(value) = self.initialize.forward_subagent_text {
            request.insert("forwardSubagentText".into(), Value::Bool(value));
        }
        if let Some(kinds) = &self.initialize.supported_dialog_kinds {
            request.insert("supportedDialogKinds".into(), serde_json::json!(kinds));
        }
        if let Some(value) = self.initialize.per_task_stop_affordance {
            request.insert("perTaskStopAffordance".into(), Value::Bool(value));
        }
        if self.plugins_via_initialize
            && !self.plugins.is_empty()
            && let Some(plugins) = &self.initialize.plugins
        {
            request.insert("plugins".into(), plugins.clone());
        }
        Value::Object(request)
    }

    fn insert_system_prompt(&self, request: &mut Map<String, Value>) {
        match &self.system_prompt {
            None => {
                request.insert("systemPrompt".into(), serde_json::json!([""]));
            }
            Some(SystemPrompt::Preset {
                append,
                exclude_dynamic_sections,
                snapshot,
            }) => {
                if let Some(append) = append {
                    request.insert("appendSystemPrompt".into(), Value::String(append.clone()));
                }
                if let Some(snapshot) = snapshot {
                    request.insert("systemPromptSnapshot".into(), Value::Bool(*snapshot));
                }
                if let Some(exclude) = exclude_dynamic_sections {
                    request.insert("excludeDynamicSections".into(), Value::Bool(*exclude));
                }
            }
            Some(SystemPrompt::Text(text)) => {
                request.insert("systemPrompt".into(), serde_json::json!([text]));
            }
            Some(SystemPrompt::Parts(parts)) => {
                request.insert("systemPrompt".into(), serde_json::json!(parts));
            }
            Some(SystemPrompt::Custom { prompt, snapshot }) => {
                let value = match prompt {
                    PromptBody::Text(text) => serde_json::json!([text]),
                    PromptBody::Parts(parts) => serde_json::json!(parts),
                };
                request.insert("systemPrompt".into(), value);
                if let Some(snapshot) = snapshot {
                    request.insert("systemPromptSnapshot".into(), Value::Bool(*snapshot));
                }
            }
        }
    }

    fn resolved_thinking(&self) -> Option<Thinking> {
        if self.thinking.is_some() {
            return self.thinking.clone();
        }
        self.max_thinking_tokens.map(|budget| {
            if budget == 0 {
                Thinking::Disabled
            } else {
                Thinking::Enabled {
                    budget_tokens: Some(budget),
                    display: None,
                }
            }
        })
    }

    fn permission_mode_flag(&self) -> Option<String> {
        if self.resolve_permission_mode_in_cli {
            self.permission_mode.clone()
        } else {
            Some(
                self.permission_mode
                    .clone()
                    .unwrap_or_else(|| "default".into()),
            )
        }
    }

    fn merged_settings(&self) -> anyhow::Result<Option<String>> {
        let settings = self.settings.as_ref().map(|value| match value {
            Value::String(path) => path.clone(),
            other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
        });
        let Some(sandbox) = &self.sandbox else {
            return Ok(settings);
        };
        let mut sandbox = sandbox.clone();
        if sandbox.get("enabled").and_then(Value::as_bool) == Some(true)
            && sandbox.get("failIfUnavailable").is_none()
            && let Some(object) = sandbox.as_object_mut()
        {
            object.insert("failIfUnavailable".into(), Value::Bool(true));
        }
        if let Some(existing) = &settings {
            let trimmed = existing.trim();
            if !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
                anyhow::bail!(
                    "Cannot use both a settings file path and the sandbox option. Include the sandbox configuration in your settings file instead."
                );
            }
            let mut parsed: Value = serde_json::from_str(existing).unwrap_or_else(|_| Value::Null);
            if let Some(object) = parsed.as_object_mut() {
                object.insert("sandbox".into(), sandbox);
                return Ok(Some(serde_json::to_string(&parsed)?));
            }
        }
        Ok(Some(serde_json::to_string(&serde_json::json!({
            "sandbox": sandbox
        }))?))
    }

    fn child_env(&self) -> BTreeMap<String, String> {
        let mut env = self
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        env.entry("CLAUDE_CODE_ENTRYPOINT".into())
            .or_insert_with(|| "sdk-ts".into());
        env.entry("CLAUDE_AGENT_SDK_VERSION".into())
            .or_insert_with(|| SDK_VERSION.into());
        if self.enable_file_checkpointing {
            env.insert(
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".into(),
                "true".into(),
            );
        }
        if let Some(format) = &self.question_preview_format {
            env.insert("CLAUDE_CODE_QUESTION_PREVIEW_FORMAT".into(), format.clone());
        }
        apply_question_flag(
            &mut env,
            "CLAUDE_CODE_QUESTION_EXTENDED",
            self.question_extended,
            self.env.is_none(),
        );
        apply_question_flag(
            &mut env,
            "CLAUDE_CODE_QUESTION_OPTIONAL_DESCRIPTIONS",
            self.question_optional_descriptions,
            self.env.is_none(),
        );
        env.remove("NODE_OPTIONS");
        if env
            .get("DEBUG_CLAUDE_AGENT_SDK")
            .is_some_and(|value| env_flag_set(value))
        {
            env.insert("DEBUG".into(), "1".into());
        } else {
            env.remove("DEBUG");
        }
        env
    }
}

fn apply_question_flag(
    env: &mut BTreeMap<String, String>,
    name: &str,
    enabled: bool,
    inherited: bool,
) {
    if enabled {
        env.insert(name.into(), "1".into());
    } else if inherited {
        env.retain(|key, _| !key.eq_ignore_ascii_case(name));
    }
}

/// SDK `Pe`: only `1`, `true`, `yes`, and `on` (trimmed, case-insensitive) are set.
pub fn env_flag_set(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// SDK `CR`: a value longer than one character that starts with `-` is joined
/// with `=` so the CLI does not treat it as another flag.
fn pair(args: &mut Vec<String>, key: &str, value: &str) {
    if value.len() > 1 && value.starts_with('-') {
        args.push(format!("{key}={value}"));
    } else {
        args.push(key.into());
        args.push(value.into());
    }
}

fn js_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol_prefix() -> Vec<String> {
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn encodes_upstream_launch_options_in_sdk_order() {
        let options = LaunchOptions {
            model: Some("claude-sonnet-4-6".into()),
            effort: Some("high".into()),
            thinking: Some(Thinking::Adaptive {
                display: Some(ThinkingDisplay::Summarized),
            }),
            tools: Some(Tools::Preset),
            permission_mode: Some("bypassPermissions".into()),
            allow_dangerously_skip_permissions: true,
            can_use_tool: true,
            include_partial_messages: true,
            resume: Some("abc".into()),
            resume_session_at: Some("head".into()),
            setting_sources: Some(vec!["user".into(), "project".into()]),
            settings: Some(serde_json::json!({ "showThinkingSummaries": true })),
            env: Some(BTreeMap::from([
                ("NODE_OPTIONS".into(), "--inspect".into()),
                ("DEBUG".into(), "old".into()),
            ])),
            system_prompt: Some(SystemPrompt::Preset {
                append: Some("t3".into()),
                exclude_dynamic_sections: None,
                snapshot: None,
            }),
            ..Default::default()
        };
        let spec = options.spec().unwrap();
        let mut expected = protocol_prefix();
        expected.extend(
            [
                "--thinking",
                "adaptive",
                "--thinking-display",
                "summarized",
                "--effort",
                "high",
                "--model",
                "claude-sonnet-4-6",
                "--permission-prompt-tool",
                "stdio",
                "--resume=abc",
                "--tools",
                "default",
                "--setting-sources=user,project",
                "--permission-mode",
                "bypassPermissions",
                "--allow-dangerously-skip-permissions",
                "--include-partial-messages",
                "--resume-session-at=head",
                "--settings",
                "{\"showThinkingSummaries\":true}",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert_eq!(spec.args, expected);
        assert!(!spec.env.contains_key("NODE_OPTIONS"));
        assert!(!spec.env.contains_key("DEBUG"));
        assert_eq!(spec.env["CLAUDE_CODE_ENTRYPOINT"], "sdk-ts");
        assert_eq!(spec.env["CLAUDE_AGENT_SDK_VERSION"], SDK_VERSION);
        assert_eq!(
            options.initialize_request()["appendSystemPrompt"],
            Value::String("t3".into())
        );
        assert!(options.initialize_request().get("systemPrompt").is_none());
    }

    #[test]
    fn omitted_system_prompt_is_empty_string_array() {
        let request = LaunchOptions::default().initialize_request();
        assert_eq!(request["systemPrompt"], serde_json::json!([""]));
        assert!(request.get("appendSystemPrompt").is_none());
    }

    #[test]
    fn custom_string_prompt_is_wrapped() {
        let options = LaunchOptions {
            system_prompt: Some(SystemPrompt::Text("You are a release bot.".into())),
            ..Default::default()
        };
        assert_eq!(
            options.initialize_request()["systemPrompt"],
            serde_json::json!(["You are a release bot."])
        );
    }

    #[test]
    fn debug_flag_matches_sdk_pe() {
        for (raw, enabled) in [
            ("1", true),
            (" YES ", true),
            ("on", true),
            ("true", true),
            ("2", false),
            ("false", false),
            ("", false),
        ] {
            let options = LaunchOptions {
                env: Some(BTreeMap::from([
                    ("DEBUG_CLAUDE_AGENT_SDK".into(), raw.into()),
                    ("DEBUG".into(), "old".into()),
                ])),
                ..Default::default()
            };
            let spec = options.spec().unwrap();
            if enabled {
                assert_eq!(
                    spec.env.get("DEBUG").map(String::as_str),
                    Some("1"),
                    "{raw}"
                );
            } else {
                assert!(!spec.env.contains_key("DEBUG"), "{raw}");
            }
        }
    }

    #[test]
    fn empty_tools_and_dash_values_and_question_env() {
        let options = LaunchOptions {
            tools: Some(Tools::Names(vec![])),
            extra_args: IndexMap::from([("model".into(), Some("-fast".into()))]),
            env: Some(BTreeMap::from([(
                "CLAUDE_CODE_QUESTION_EXTENDED".into(),
                "1".into(),
            )])),
            question_extended: false,
            ..Default::default()
        };
        let spec = options.spec().unwrap();
        assert!(spec.args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(spec.args.iter().any(|arg| arg == "--model=-fast"));
        // An explicit env is not scrubbed when the question flag is off.
        assert_eq!(
            spec.env
                .get("CLAUDE_CODE_QUESTION_EXTENDED")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn inherited_question_env_is_scrubbed_and_defaults_include_permission_mode() {
        let mut env = BTreeMap::from([
            ("claude_code_question_extended".into(), "1".into()),
            ("PATH".into(), "/usr/bin".into()),
        ]);
        apply_question_flag(&mut env, "CLAUDE_CODE_QUESTION_EXTENDED", false, true);
        assert!(
            !env.keys()
                .any(|key| key.eq_ignore_ascii_case("CLAUDE_CODE_QUESTION_EXTENDED"))
        );
        let spec = LaunchOptions {
            env: Some(BTreeMap::from([("PATH".into(), "/usr/bin".into())])),
            permission_mode: None,
            ..Default::default()
        }
        .spec()
        .unwrap();
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--permission-mode", "default"])
        );
    }

    #[test]
    fn rejects_same_fallback_model_and_mixed_permission_tools() {
        let err = LaunchOptions {
            model: Some("claude".into()),
            fallback_model: Some("claude".into()),
            ..Default::default()
        }
        .spec()
        .unwrap_err()
        .to_string();
        assert!(err.contains("Fallback model"));
        let err = LaunchOptions {
            can_use_tool: true,
            permission_prompt_tool_name: Some("mcp".into()),
            ..Default::default()
        }
        .spec()
        .unwrap_err()
        .to_string();
        assert!(err.contains("permissionPromptToolName"));
    }
}
