//! Port of upstream/apps/server/src/provider/ClaudeProvider.ts capability probe.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use turin_claude_protocol::{ClaudeSession, ControlHandler, ControlRequest, LaunchOptions, Tools};

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(25);
pub const USAGE_TIMEOUT: Duration = Duration::from_secs(5);

struct ProbeHandler;
impl ControlHandler for ProbeHandler {
    fn handle(
        &self,
        _request: ControlRequest,
        _cancel: CancellationToken,
    ) -> turin_claude_protocol::session::HandlerFuture {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCapabilities {
    pub account: Value,
    pub models: Vec<Value>,
    pub commands: Vec<Value>,
    pub usage: Option<Value>,
    pub authenticated: bool,
}

pub fn probe_launch_options(
    binary: PathBuf,
    cwd: Option<PathBuf>,
    managed_mcp_config: bool,
) -> LaunchOptions {
    let mut options = LaunchOptions {
        binary,
        cwd,
        persist_session: false,
        setting_sources: Some(vec!["user".into(), "project".into(), "local".into()]),
        settings: Some(json!({"disableAllHooks":true})),
        allowed_tools: vec![],
        tools: Some(Tools::Names(vec![])),
        strict_mcp_config: !managed_mcp_config,
        ..Default::default()
    };
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    env.insert("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into());
    env.remove("FORCE_CODE_TERMINAL");
    env.insert("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "0".into());
    env.insert("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into());
    options.env = Some(env);
    options
}

/// Initialize only. Never sends a `user` frame, so it cannot start a paid turn.
pub async fn probe(
    binary: PathBuf,
    cwd: Option<PathBuf>,
    managed_mcp_config: bool,
    include_usage: bool,
) -> Result<ClaudeCapabilities> {
    let options = probe_launch_options(binary, cwd, managed_mcp_config);
    let session = tokio::time::timeout(
        PROBE_TIMEOUT,
        ClaudeSession::spawn(options, Arc::new(ProbeHandler)),
    )
    .await
    .context("Claude capability probe timed out")??;
    let init = session.initialization().clone();
    let usage = if include_usage {
        tokio::time::timeout(USAGE_TIMEOUT, session.get_usage(true)).await.ok().and_then(Result::ok)
            .map(|value| json!({"rate_limits_available":value.get("rate_limits_available"),"rate_limits":value.get("rate_limits")}))
    } else {
        None
    };
    let account = init.get("account").cloned().unwrap_or(Value::Null);
    let authenticated = claude_auth_status(&account);
    let capabilities = ClaudeCapabilities {
        account,
        models: init
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        commands: init
            .get("commands")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        usage,
        authenticated,
    };
    session.shutdown().await?;
    Ok(capabilities)
}

/// `tokenSource:none` is the only definitive logged-out signal. Third-party
/// API backends and profile auth often omit token-source metadata altogether.
pub fn claude_auth_status(account: &Value) -> bool {
    if account
        .get("apiProvider")
        .and_then(Value::as_str)
        .is_some_and(|p| p != "firstParty")
    {
        return true;
    }
    if account.get("tokenSource").and_then(Value::as_str) != Some("none") {
        return true;
    }
    account
        .get("apiKeySource")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty() && s != "none")
        || account
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        || account
            .get("subscriptionType")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_is_initialize_only_and_auth_matches_upstream() {
        let options = probe_launch_options("/bin/claude".into(), None, false);
        assert!(!options.persist_session);
        assert!(options.strict_mcp_config);
        assert_eq!(options.settings.unwrap()["disableAllHooks"], true);
        assert!(!claude_auth_status(
            &json!({"tokenSource":"none","apiKeySource":"none"})
        ));
        assert!(claude_auth_status(
            &json!({"tokenSource":"none","apiKeySource":"env"})
        ));
        assert!(claude_auth_status(
            &json!({"apiProvider":"bedrock","tokenSource":"none"})
        ));
        assert!(claude_auth_status(&json!({})));
    }
}
