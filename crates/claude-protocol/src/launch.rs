//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.mjs ProcessTransport.initialize.
use std::{collections::BTreeMap, path::PathBuf};
use indexmap::IndexMap;
use serde_json::Value;

pub const SDK_VERSION: &str = "0.3.276";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tools { Preset, Names(Vec<String>) }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thinking { Adaptive { display: Option<String> }, Enabled { budget_tokens: Option<u32>, display: Option<String> }, Disabled }
#[derive(Debug, Clone)]
pub enum SystemPrompt { Preset { append: String }, Custom(Vec<String>) }

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
}
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub binary: PathBuf,
    pub initialize: InitializeOptions,
    pub cwd: Option<PathBuf>,
    /// Like the SDK env option, Some replaces the inherited environment entirely.
    pub env: Option<BTreeMap<String, String>>,
    pub model: Option<String>, pub effort: Option<String>, pub thinking: Option<Thinking>,
    pub permission_mode: Option<String>, pub allow_dangerously_skip_permissions: bool,
    pub can_use_tool: bool, pub tools: Option<Tools>,
    pub allowed_tools: Vec<String>, pub disallowed_tools: Vec<String>,
    pub system_prompt: Option<SystemPrompt>,
    pub resume: Option<String>, pub continue_conversation: bool,
    pub resume_session_at: Option<String>, pub session_id: Option<String>,
    pub setting_sources: Option<Vec<String>>, pub settings: Option<Value>,
    pub mcp_servers: IndexMap<String, Value>, pub strict_mcp_config: bool,
    pub include_partial_messages: bool, pub persist_session: bool,
    pub additional_directories: Vec<PathBuf>, pub extra_args: IndexMap<String, Option<String>>,
    pub max_turns: Option<u32>, pub max_budget_usd: Option<f64>,
    pub fallback_model: Option<String>, pub enable_file_checkpointing: bool,
    pub question_preview_format: Option<String>, pub question_extended: bool,
    pub question_optional_descriptions: bool,
}
impl Default for LaunchOptions {
    fn default() -> Self { Self { binary: "claude".into(), initialize: InitializeOptions::default(), cwd: None, env: None, model: None, effort: None,
        thinking: None, permission_mode: Some("default".into()), allow_dangerously_skip_permissions: false,
        can_use_tool: false, tools: None, allowed_tools: vec![], disallowed_tools: vec![],
        system_prompt: None, resume: None, continue_conversation: false, resume_session_at: None,
        session_id: None, setting_sources: None, settings: None, mcp_servers: IndexMap::new(),
        strict_mcp_config: false, include_partial_messages: false, persist_session: true,
        additional_directories: vec![], extra_args: IndexMap::new(), max_turns: None,
        max_budget_usd: None, fallback_model: None, enable_file_checkpointing: false,
        question_preview_format: None, question_extended: false, question_optional_descriptions: false } }
}
#[derive(Debug, Clone)]
pub struct LaunchSpec { pub binary: PathBuf, pub cwd: Option<PathBuf>, pub args: Vec<String>, pub env: BTreeMap<String, String> }
impl LaunchOptions {
    pub fn spec(&self) -> anyhow::Result<LaunchSpec> {
        anyhow::ensure!(!(self.resume.is_some() && self.continue_conversation), "resume and continue are mutually exclusive");
        anyhow::ensure!(self.fallback_model.as_ref().zip(self.model.as_ref()).is_none_or(|(a,b)| a != b), "fallback model equals model");
        let mut args: Vec<String> = ["--output-format", "stream-json", "--verbose", "--input-format", "stream-json"].map(str::to_owned).to_vec();
        if let Some(thinking) = &self.thinking {
            match thinking {
                Thinking::Adaptive { display } => { pair(&mut args, "--thinking", "adaptive"); if let Some(v) = display { pair(&mut args, "--thinking-display", v); } },
                Thinking::Enabled { budget_tokens, display } => { if let Some(v) = budget_tokens { pair(&mut args, "--max-thinking-tokens", &v.to_string()); } else { pair(&mut args, "--thinking", "adaptive"); } if let Some(v) = display { pair(&mut args, "--thinking-display", v); } },
                Thinking::Disabled => pair(&mut args, "--thinking", "disabled"),
            }
        }
        if let Some(v) = &self.effort { pair(&mut args, "--effort", v); }
        if let Some(v) = self.max_turns { if v != 0 { pair(&mut args, "--max-turns", &v.to_string()); } }
        if let Some(v) = self.max_budget_usd { pair(&mut args, "--max-budget-usd", &v.to_string()); }
        if let Some(v) = &self.model { pair(&mut args, "--model", v); }
        if self.can_use_tool { pair(&mut args, "--permission-prompt-tool", "stdio"); }
        if self.continue_conversation { args.push("--continue".into()); }
        if let Some(v) = &self.resume { args.push(format!("--resume={v}")); }
        if !self.allowed_tools.is_empty() { pair(&mut args, "--allowedTools", &self.allowed_tools.join(",")); }
        if !self.disallowed_tools.is_empty() { pair(&mut args, "--disallowedTools", &self.disallowed_tools.join(",")); }
        if let Some(v) = &self.tools { pair(&mut args, "--tools", &match v { Tools::Preset => "default".into(), Tools::Names(names) => names.join(",") }); }
        if !self.mcp_servers.is_empty() { pair(&mut args, "--mcp-config", &serde_json::to_string(&serde_json::json!({"mcpServers": self.mcp_servers}))?); }
        if let Some(v) = &self.setting_sources { args.push(format!("--setting-sources={}", v.join(","))); }
        if self.strict_mcp_config { args.push("--strict-mcp-config".into()); }
        if let Some(v) = &self.permission_mode { pair(&mut args, "--permission-mode", v); }
        if self.allow_dangerously_skip_permissions { args.push("--allow-dangerously-skip-permissions".into()); }
        if let Some(v) = &self.fallback_model { pair(&mut args, "--fallback-model", v); }
        if self.include_partial_messages { args.push("--include-partial-messages".into()); }
        for v in &self.additional_directories { pair(&mut args, "--add-dir", &v.to_string_lossy()); }
        if let Some(v) = &self.resume_session_at { args.push(format!("--resume-session-at={v}")); }
        if let Some(v) = &self.session_id { args.push(format!("--session-id={v}")); }
        if !self.persist_session { args.push("--no-session-persistence".into()); }
        let mut extra = self.extra_args.clone();
        if let Some(v) = &self.settings { extra.insert("settings".into(), Some(match v { Value::String(s) => s.clone(), _ => serde_json::to_string(v)? })); }
        for (key,value) in &extra { let flag = format!("--{key}"); if let Some(v) = value { pair(&mut args, &flag, v); } else { args.push(flag); } }
        let mut env: BTreeMap<String,String> = self.env.clone().unwrap_or_else(|| std::env::vars().collect());
        env.entry("CLAUDE_CODE_ENTRYPOINT".into()).or_insert("sdk-ts".into());
        env.entry("CLAUDE_AGENT_SDK_VERSION".into()).or_insert(SDK_VERSION.into());
        if self.enable_file_checkpointing { env.insert("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".into(), "true".into()); }
        if let Some(v) = &self.question_preview_format { env.insert("CLAUDE_CODE_QUESTION_PREVIEW_FORMAT".into(), v.clone()); }
        if self.question_extended { env.insert("CLAUDE_CODE_QUESTION_EXTENDED".into(), "1".into()); }
        else if self.env.is_none() { env.retain(|key,_| !key.eq_ignore_ascii_case("CLAUDE_CODE_QUESTION_EXTENDED")); }
        if self.question_optional_descriptions { env.insert("CLAUDE_CODE_QUESTION_OPTIONAL_DESCRIPTIONS".into(), "1".into()); }
        else if self.env.is_none() { env.retain(|key,_| !key.eq_ignore_ascii_case("CLAUDE_CODE_QUESTION_OPTIONAL_DESCRIPTIONS")); }
        env.remove("NODE_OPTIONS");
        if env.get("DEBUG_CLAUDE_AGENT_SDK").is_some_and(|v| !v.is_empty() && v != "0" && v != "false") { env.insert("DEBUG".into(), "1".into()); } else { env.remove("DEBUG"); }
        Ok(LaunchSpec { binary: self.binary.clone(), cwd: self.cwd.clone(), args, env })
    }
    pub fn initialize_request(&self) -> Value {
        let mut request = serde_json::json!({"subtype":"initialize"});
        if let Some(prompt) = &self.system_prompt { match prompt {
            SystemPrompt::Preset { append } => request["appendSystemPrompt"] = append.clone().into(),
            SystemPrompt::Custom(parts) => request["systemPrompt"] = serde_json::json!(parts),
        }}
        if let Some(v) = &self.initialize.hooks { request["hooks"] = v.clone(); }
        if !self.initialize.sdk_mcp_servers.is_empty() { request["sdkMcpServers"] = serde_json::json!(self.initialize.sdk_mcp_servers); }
        if !self.initialize.sdk_mcp_server_configs.is_empty() { request["sdkMcpServerConfigs"] = serde_json::json!(self.initialize.sdk_mcp_server_configs); }
        if let Some(v) = &self.initialize.agents { request["agents"] = v.clone(); }
        if let Some(v) = &self.initialize.supported_dialog_kinds { request["supportedDialogKinds"] = serde_json::json!(v); }
        if let Some(v) = self.initialize.per_task_stop_affordance { request["perTaskStopAffordance"] = v.into(); }
        if let Some(v) = &self.initialize.json_schema { request["jsonSchema"] = v.clone(); }
        if let Some(v) = &self.initialize.tool_aliases { request["toolAliases"] = v.clone(); }
        request
    }
}

fn pair(args: &mut Vec<String>, key: &str, value: &str) {
    if value.len() > 1 && value.starts_with('-') { args.push(format!("{key}={value}")); }
    else { args.push(key.into()); args.push(value.into()); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_upstream_launch_options() {
        let options = LaunchOptions { model: Some("claude-sonnet-4-6".into()), effort: Some("high".into()),
            thinking: Some(Thinking::Adaptive { display: Some("summarized".into()) }),
            tools: Some(Tools::Preset), permission_mode: Some("bypassPermissions".into()),
            allow_dangerously_skip_permissions: true, can_use_tool: true, include_partial_messages: true,
            resume: Some("abc".into()), resume_session_at: Some("head".into()),
            setting_sources: Some(vec!["user".into(),"project".into()]),
            settings: Some(serde_json::json!({"showThinkingSummaries":true})),
            env: Some(BTreeMap::from([("NODE_OPTIONS".into(),"--inspect".into()),("DEBUG".into(),"old".into())])),
            ..Default::default() };
        let spec = options.spec().unwrap();
        assert_eq!(&spec.args[..5], ["--output-format","stream-json","--verbose","--input-format","stream-json"]);
        assert!(spec.args.windows(2).any(|w| w == ["--thinking", "adaptive"]));
        assert!(spec.args.windows(2).any(|w| w == ["--effort", "high"]));
        assert!(spec.args.contains(&"--resume=abc".into()));
        assert!(spec.args.contains(&"--resume-session-at=head".into()));
        assert!(spec.args.contains(&"--setting-sources=user,project".into()));
        assert!(spec.args.windows(2).any(|w| w == ["--tools", "default"]));
        assert!(!spec.env.contains_key("NODE_OPTIONS")); assert!(!spec.env.contains_key("DEBUG"));
        assert_eq!(spec.env["CLAUDE_CODE_ENTRYPOINT"],"sdk-ts");
        assert_eq!(spec.env["CLAUDE_AGENT_SDK_VERSION"],SDK_VERSION);
        assert_eq!(options.initialize_request()["appendSystemPrompt"],Value::Null);
    }
}
