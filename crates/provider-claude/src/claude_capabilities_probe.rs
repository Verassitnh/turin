//! Port of the SDK capability probe in upstream/apps/server/src/provider/ClaudeProvider.ts
//! (`buildClaudeCapabilitiesProbeQueryOptions`, `probeClaudeCapabilities`,
//! `parseClaudeInitializationCommands`, `dedupeSlashCommands`).
//!
//! Upstream reads `initializationResult()` from the SDK's `query()`. Turin
//! speaks the CLI stream-json protocol directly, so the probe spawns a
//! `ClaudeSession`, reads its initialize response, and drops it. Dropping the
//! session SIGKILLs the process group, which is what the upstream abort does.
//! The probe never sends a user frame, so it cannot start a turn.
use crate::claude_home::{ProcessEnv, has_claude_managed_mcp_config, make_claude_environment};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use turin_claude_protocol::{
    ClaudeSession, ControlHandler, ControlRequest, LaunchOptions, session::HandlerFuture,
};
use turin_contracts::settings::ClaudeSettings;
use turin_provider_core::{
    host_process::Platform,
    server_provider::{ServerProviderSlashCommand, ServerProviderSlashCommandInput},
};

/// `CAPABILITIES_PROBE_TIMEOUT_MS`. Bedrock boots slowly, so this stays generous.
pub const CAPABILITIES_PROBE_TIMEOUT: Duration = Duration::from_millis(25_000);
/// `DEFAULT_TIMEOUT_MS` from snapshotProbe: the optional usage request's budget.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(4_000);

/// `CLAUDE_CAPABILITIES_PROBE_SETTING_SOURCES`: keeps workspace command
/// discovery while the periodic health check ignores local MCP config.
pub const CLAUDE_CAPABILITIES_PROBE_SETTING_SOURCES: [&str; 3] = ["user", "project", "local"];

/// `ClaudeCapabilitiesProbe`: the account and command metadata one initialize
/// round-trip returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeCapabilitiesProbe {
    pub email: Option<String>,
    /// The organization UUID from the login's `.claude.json`. The driver fills it
    /// in from the same cached probe, so it describes the same login.
    pub workspace_id: Option<String>,
    pub subscription_type: Option<String>,
    pub token_source: Option<String>,
    /// Where the CLI found an API key, when it authenticates with one.
    pub api_key_source: Option<String>,
    /// Active API backend reported by `AccountInfo`. Anthropic OAuth login only
    /// applies when `"firstParty"`.
    pub api_provider: Option<String>,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    /// `get_usage` rate limits, or `None` when that request failed. Absent
    /// windows on a successful response mean the account has none (API key).
    pub usage: Option<ClaudeUsageResponse>,
}

/// The `rate_limits_available` and `rate_limits` keys of `get_usage`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeUsageResponse {
    pub rate_limits_available: Option<bool>,
    pub rate_limits: Option<Value>,
}

/// Probe inputs for [`build_claude_capabilities_probe_launch_options`].
pub struct ClaudeCapabilitiesProbeOptions<'a> {
    pub executable_path: &'a str,
    /// The child env, already merged with the instance's `CLAUDE_CONFIG_DIR`.
    pub environment: &'a ProcessEnv,
    pub cwd: Option<&'a Path>,
    /// An administrator's `managed-mcp.json` is installed; see `has_claude_managed_mcp_config`.
    pub managed_mcp_config: bool,
}

/// `buildClaudeCapabilitiesProbeQueryOptions`.
pub fn build_claude_capabilities_probe_launch_options(
    input: ClaudeCapabilitiesProbeOptions<'_>,
) -> LaunchOptions {
    let mut env: BTreeMap<String, String> = input
        .environment
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    // Connected claude.ai MCP servers are discovered outside filesystem config.
    env.insert("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into());
    // A noninteractive health check gains nothing from IDE discovery, and
    // skipping it avoids a process tree on every periodic refresh.
    env.remove("FORCE_CODE_TERMINAL");
    env.insert("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "0".into());
    env.insert("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into());
    LaunchOptions {
        binary: PathBuf::from(input.executable_path),
        cwd: input.cwd.map(Path::to_path_buf),
        env: Some(env),
        persist_session: false,
        setting_sources: Some(
            CLAUDE_CAPABILITIES_PROBE_SETTING_SOURCES
                .iter()
                .map(|source| (*source).to_owned())
                .collect(),
        ),
        // The probe keeps filesystem setting sources for slash-command discovery
        // but must not run hooks: it fires every few minutes.
        settings: Some(json!({"disableAllHooks": true})),
        allowed_tools: Vec::new(),
        // Empty explicit MCP map plus --strict-mcp-config. Claude refuses that
        // flag next to a managed-mcp.json, so it is dropped in that case only.
        mcp_servers: IndexMap::new(),
        strict_mcp_config: !input.managed_mcp_config,
        ..Default::default()
    }
}

/// Answers nothing. The probe sends no control requests that need a host answer.
struct ProbeHandler;

impl ControlHandler for ProbeHandler {
    fn handle(&self, _request: ControlRequest, _cancel: CancellationToken) -> HandlerFuture {
        Box::pin(async { Ok(None) })
    }
}

/// `probeClaudeCapabilities`: `None` when initialization fails or times out.
/// Usage has its own deadline, so a slow optional request keeps the
/// initialization result.
pub async fn probe_claude_capabilities(
    settings: &ClaudeSettings,
    environment: &ProcessEnv,
    cwd: Option<&Path>,
    include_usage: bool,
    home: &str,
) -> Option<ClaudeCapabilitiesProbe> {
    probe_claude_capabilities_with_timeouts(
        settings,
        environment,
        cwd,
        include_usage,
        home,
        CAPABILITIES_PROBE_TIMEOUT,
        DEFAULT_PROBE_TIMEOUT,
    )
    .await
}

async fn probe_claude_capabilities_with_timeouts(
    settings: &ClaudeSettings,
    environment: &ProcessEnv,
    cwd: Option<&Path>,
    include_usage: bool,
    home: &str,
    initialization_timeout: Duration,
    usage_timeout: Duration,
) -> Option<ClaudeCapabilitiesProbe> {
    let claude_environment =
        make_claude_environment(settings.home_path.as_str(), environment, home);
    let managed_mcp_config = has_claude_managed_mcp_config(Platform::current()).await;
    let options = build_claude_capabilities_probe_launch_options(ClaudeCapabilitiesProbeOptions {
        executable_path: settings.binary_path.as_str(),
        environment: &claude_environment,
        cwd,
        managed_mcp_config,
    });
    let session = tokio::time::timeout(
        initialization_timeout,
        ClaudeSession::spawn(options, Arc::new(ProbeHandler)),
    )
    .await
    .ok()?
    .ok()?;

    let usage = if include_usage {
        tokio::time::timeout(usage_timeout, session.get_usage(true))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|response| ClaudeUsageResponse {
                rate_limits_available: response
                    .get("rate_limits_available")
                    .and_then(Value::as_bool),
                rate_limits: response.get("rate_limits").cloned(),
            })
    } else {
        None
    };
    let initialization = session.initialization().clone();
    // The abort in upstream does not wait for the child to exit. Dropping the
    // session kills the process group now.
    drop(session);

    let account = initialization.get("account");
    let account_text = |key: &str| {
        account
            .and_then(|account| account.get(key))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Some(ClaudeCapabilitiesProbe {
        email: account_text("email"),
        workspace_id: None,
        subscription_type: account_text("subscriptionType"),
        token_source: account_text("tokenSource"),
        api_key_source: account_text("apiKeySource"),
        api_provider: account_text("apiProvider"),
        slash_commands: parse_claude_initialization_commands(initialization.get("commands")),
        usage,
    })
}

/// `nonEmptyProbeString`: trimmed, or `None` when blank.
pub fn non_empty_probe_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn tne(value: &str) -> Option<turin_contracts::TrimmedNonEmptyString> {
    turin_contracts::TrimmedNonEmptyString::new(value).ok()
}

/// `parseClaudeInitializationCommands`: the initialize response's commands as
/// slash commands, with blank names dropped and duplicates merged.
pub fn parse_claude_initialization_commands(
    commands: Option<&Value>,
) -> Vec<ServerProviderSlashCommand> {
    let commands = commands
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let parsed = commands.iter().filter_map(|command| {
        let name = non_empty_probe_string(command.get("name").and_then(Value::as_str))?;
        let description =
            non_empty_probe_string(command.get("description").and_then(Value::as_str));
        let hint = non_empty_probe_string(command.get("argumentHint").and_then(Value::as_str));
        Some(ServerProviderSlashCommand {
            name: tne(&name)?,
            description: description.as_deref().and_then(tne),
            input: hint
                .as_deref()
                .and_then(tne)
                .map(|hint| ServerProviderSlashCommandInput { hint }),
        })
    });
    dedupe_slash_commands(parsed.collect())
}

/// `dedupeSlashCommands`: the first occurrence of a name (case-insensitive)
/// keeps its position, and later duplicates fill in a missing description or hint.
pub fn dedupe_slash_commands(
    commands: Vec<ServerProviderSlashCommand>,
) -> Vec<ServerProviderSlashCommand> {
    let mut by_name: IndexMap<String, ServerProviderSlashCommand> = IndexMap::new();
    for command in commands {
        let key = command.name.as_str().to_lowercase();
        match by_name.get_mut(&key) {
            None => {
                by_name.insert(key, command);
            }
            Some(existing) => {
                if existing.description.is_none() {
                    existing.description = command.description;
                }
                if existing.input.is_none() {
                    existing.input = command.input;
                }
            }
        }
    }
    by_name.into_values().collect()
}

#[cfg(test)]
mod tests;
