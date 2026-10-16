//! Port of upstream/apps/server/src/provider/ClaudeCapabilitiesProbe.test.ts.
//!
//! The SDK-boundary cases run against `tests/fixtures/claude_capabilities_probe.fixture.mjs`,
//! a fake `claude` that answers initialize and get_usage over stdin/stdout.
//! Skill and workspace-snapshot cases belong with the skills port.
use super::*;
use crate::claude_home::ProcessEnv;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use turin_contracts::settings::ClaudeSettings;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_capabilities_probe.fixture.mjs"
);

fn host_env() -> ProcessEnv {
    std::env::vars().collect()
}

fn settings_for(binary: &str) -> ClaudeSettings {
    serde_json::from_value(json!({"binaryPath": binary})).expect("settings decode")
}

fn command(name: &str, description: &str, hint: &str) -> ServerProviderSlashCommand {
    serde_json::from_value(json!({
        "name": name,
        "description": description,
        "input": {"hint": hint},
    }))
    .expect("slash command decodes")
}

#[test]
fn isolates_capability_probes_without_dropping_workspace_setting_sources() {
    let environment: ProcessEnv = [
        ("HOME", "/home/user"),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "true"),
        ("FORCE_CODE_TERMINAL", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let options = build_claude_capabilities_probe_launch_options(ClaudeCapabilitiesProbeOptions {
        executable_path: "/usr/bin/claude",
        environment: &environment,
        cwd: Some(Path::new("/workspace/project")),
        managed_mcp_config: false,
    });

    assert!(options.mcp_servers.is_empty());
    assert!(options.strict_mcp_config);
    assert_eq!(
        options.cwd.as_deref(),
        Some(Path::new("/workspace/project"))
    );
    assert_eq!(
        options.setting_sources,
        Some(vec!["user".into(), "project".into(), "local".into()])
    );
    assert_eq!(options.settings, Some(json!({"disableAllHooks": true})));
    assert!(options.allowed_tools.is_empty());
    assert!(!options.persist_session);
    assert_eq!(options.binary, PathBuf::from("/usr/bin/claude"));
    let env = options.env.expect("probe env is set");
    assert_eq!(env["HOME"], "/home/user");
    assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "false");
    assert!(!env.contains_key("FORCE_CODE_TERMINAL"));
    assert_eq!(env["CLAUDE_CODE_AUTO_CONNECT_IDE"], "0");
    assert_eq!(env["CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL"], "1");
}

#[test]
fn drops_strict_mcp_mode_and_only_that_next_to_an_administrators_managed_mcp_json() {
    let environment: ProcessEnv = [("HOME", "/home/user")]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let options = build_claude_capabilities_probe_launch_options(ClaudeCapabilitiesProbeOptions {
        executable_path: "/usr/bin/claude",
        environment: &environment,
        cwd: None,
        managed_mcp_config: true,
    });

    // Claude exits before initializing when handed --strict-mcp-config here.
    assert!(!options.strict_mcp_config);
    assert!(options.mcp_servers.is_empty());
    assert_eq!(
        options.env.expect("env")["ENABLE_CLAUDEAI_MCP_SERVERS"],
        "false"
    );
}

#[test]
fn parses_initialization_commands_and_merges_duplicates_case_insensitively() {
    let commands = json!([
        {"name": "start-session", "description": "Project command", "argumentHint": "[topic]"},
        {"name": "nested:review", "description": "Review changes", "argumentHint": ""},
        {"name": "NESTED:review", "description": "", "argumentHint": "[path]"},
        {"name": "  ", "description": "blank name is dropped", "argumentHint": ""},
        {"name": "compact", "description": "Provider compact", "argumentHint": ""},
    ]);
    assert_eq!(
        parse_claude_initialization_commands(Some(&commands)),
        vec![
            command("start-session", "Project command", "[topic]"),
            command("nested:review", "Review changes", "[path]"),
            serde_json::from_value(json!({"name": "compact", "description": "Provider compact"}))
                .unwrap(),
        ]
    );
}

#[tokio::test]
async fn serializes_strict_no_mcp_options_and_still_resolves_account_capabilities() {
    let dir = tempfile::tempdir().expect("tempdir");
    let invocation_path = dir.path().join("invocation.json");
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_path = std::fs::canonicalize(workspace.path()).expect("canonical cwd");
    let mut environment = host_env();
    environment.insert(
        "T3_PROBE_INVOCATION_PATH".into(),
        invocation_path.to_string_lossy().into_owned(),
    );
    environment.insert("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "true".into());

    let capabilities = probe_claude_capabilities(
        &settings_for(FIXTURE),
        &environment,
        Some(workspace.path()),
        true,
        "/Users/test",
    )
    .await
    .expect("initialization succeeds");

    assert_eq!(capabilities.email.as_deref(), Some("dev@example.com"));
    assert_eq!(capabilities.subscription_type.as_deref(), Some("pro"));
    assert_eq!(capabilities.token_source.as_deref(), Some("oauth"));
    assert_eq!(capabilities.api_key_source, None);
    assert_eq!(capabilities.api_provider, None);
    assert_eq!(
        capabilities.slash_commands,
        vec![command("review", "Review changes", "[path]")]
    );
    assert_eq!(
        capabilities.usage,
        Some(ClaudeUsageResponse {
            rate_limits_available: Some(true),
            rate_limits: Some(
                json!({"five_hour": {"utilization": 12, "resets_at": "2026-07-18T14:39:00Z"}})
            ),
        })
    );

    let invocation: Value =
        serde_json::from_str(&std::fs::read_to_string(&invocation_path).expect("invocation"))
            .expect("invocation JSON");
    assert_eq!(invocation["cwd"], workspace_path.to_string_lossy().as_ref());
    assert_eq!(invocation["connectorEnv"], "false");
    let args: Vec<&str> = invocation["args"]
        .as_array()
        .expect("args")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(args.contains(&"--strict-mcp-config"));
    assert!(!args.contains(&"--mcp-config"));
    assert!(invocation["mcpConfig"].is_null());
    assert!(args.contains(&"--setting-sources=user,project,local"));
    let settings_index = args
        .iter()
        .position(|arg| *arg == "--settings")
        .expect("--settings");
    let flag_settings: Value =
        serde_json::from_str(args[settings_index + 1]).expect("settings JSON");
    assert_eq!(flag_settings["disableAllHooks"], true);
}

#[tokio::test]
async fn preserves_initialized_capabilities_when_optional_usage_times_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let invocation_path = dir.path().join("invocation.json");
    let mut environment = host_env();
    environment.insert(
        "T3_PROBE_INVOCATION_PATH".into(),
        invocation_path.to_string_lossy().into_owned(),
    );
    environment.insert("T3_PROBE_USAGE_HANG".into(), "1".into());

    let capabilities = probe_claude_capabilities_with_timeouts(
        &settings_for(FIXTURE),
        &environment,
        None,
        true,
        "/Users/test",
        CAPABILITIES_PROBE_TIMEOUT,
        Duration::from_millis(50),
    )
    .await
    .expect("initialization survives a usage timeout");

    assert_eq!(capabilities.email.as_deref(), Some("dev@example.com"));
    assert_eq!(capabilities.subscription_type.as_deref(), Some("pro"));
    assert_eq!(capabilities.token_source.as_deref(), Some("oauth"));
    assert_eq!(
        capabilities.slash_commands,
        vec![command("review", "Review changes", "[path]")]
    );
    assert_eq!(capabilities.usage, None);
}

#[tokio::test]
async fn asks_for_usage_without_the_local_transcript_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let request_path = dir.path().join("usage-request.json");
    let mut environment = host_env();
    environment.insert(
        "T3_PROBE_INVOCATION_PATH".into(),
        dir.path()
            .join("invocation.json")
            .to_string_lossy()
            .into_owned(),
    );
    environment.insert(
        "T3_PROBE_USAGE_REQUEST_PATH".into(),
        request_path.to_string_lossy().into_owned(),
    );

    probe_claude_capabilities(
        &settings_for(FIXTURE),
        &environment,
        None,
        true,
        "/Users/test",
    )
    .await
    .expect("initialization succeeds");

    let request: Value =
        serde_json::from_str(&std::fs::read_to_string(&request_path).expect("usage request"))
            .expect("usage request JSON");
    assert_eq!(
        request,
        json!({"subtype": "get_usage", "skip_behaviors": true})
    );
}
