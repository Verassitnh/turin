//! ACP status probe. It spawns `cursor-agent acp`, initializes, asks whether
//! the agent is signed in, and lists the model catalog. It opens no session
//! and sends no prompt. The child is killed when the probe's client drops.
//!
//! `session/list` is the sign-in check because it does not write the user's
//! Cursor config (`session/new` and `session/load` do, see [`crate::acp_config`]).

use serde_json::json;
use turin_acp::compat::InitializeResponse;
use turin_acp::cursor::AvailableModel;
use turin_acp::{AcpClient, SpawnCommand, spawn};
use turin_provider_core::server_provider::{
    ServerProviderAuth, ServerProviderAuthStatus, ServerProviderState,
};

use crate::auth::ACP_ARGS;
use crate::error::{CursorError, CursorOperation};
use crate::status::{CURSOR_PROBE_TIMEOUT, CursorProbe, auth_status_from_error};

pub async fn probe_initialize(program: &str) -> Result<InitializeResponse, CursorError> {
    let client = spawn(SpawnCommand::new(program, ACP_ARGS))
        .await
        .map_err(|error| CursorError::from_acp(CursorOperation::Spawn, &error))?;
    let response = client
        .initialize(AcpClient::cursor_initialize(
            "turin",
            env!("CARGO_PKG_VERSION"),
        ))
        .await
        .map_err(|error| CursorError::from_acp(CursorOperation::Initialize, &error))?;
    drop(client);
    Ok(response)
}

/// Runs the whole probe under [`CURSOR_PROBE_TIMEOUT`].
pub async fn probe_status(program: &str) -> CursorProbe {
    match tokio::time::timeout(CURSOR_PROBE_TIMEOUT, probe_agent(program)).await {
        Ok(probe) => probe,
        Err(_) => failed(
            true,
            None,
            format!(
                "Cursor agent probe timed out after {}ms.",
                CURSOR_PROBE_TIMEOUT.as_millis()
            ),
        ),
    }
}

async fn probe_agent(program: &str) -> CursorProbe {
    let client = match spawn(SpawnCommand::new(program, ACP_ARGS)).await {
        Ok(client) => client,
        Err(error) => {
            let error = CursorError::from_acp(CursorOperation::Spawn, &error);
            return failed(
                false,
                None,
                format!("Cursor agent could not start: {}", error.detail),
            );
        }
    };
    let initialize = match client
        .initialize(AcpClient::cursor_initialize(
            "turin",
            env!("CARGO_PKG_VERSION"),
        ))
        .await
    {
        Ok(initialize) => initialize,
        Err(error) => {
            let error = CursorError::from_acp(CursorOperation::Initialize, &error);
            return failed(
                true,
                None,
                format!("Cursor agent did not initialize: {}", error.detail),
            );
        }
    };
    let version = initialize
        .agent_info
        .as_ref()
        .and_then(|info| info.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let (auth, auth_message) = match client.list_sessions(json!({})).await {
        Ok(_) => (
            ServerProviderAuth {
                type_: turin_contracts::base_schemas::TrimmedNonEmptyString::new("browser").ok(),
                label: turin_contracts::base_schemas::TrimmedNonEmptyString::new("Cursor account")
                    .ok(),
                ..ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated)
            },
            None,
        ),
        Err(error) => {
            let error = CursorError::from_acp(CursorOperation::SessionList, &error);
            if error.not_signed_in {
                (
                    auth_status_from_error(true),
                    Some("Sign in with Cursor in provider settings.".to_owned()),
                )
            } else {
                (
                    auth_status_from_error(false),
                    Some(format!("Cursor account check failed: {}", error.detail)),
                )
            }
        }
    };
    let authenticated = auth.status == ServerProviderAuthStatus::Authenticated;
    let status = if authenticated {
        ServerProviderState::Ready
    } else {
        ServerProviderState::Error
    };
    // Without a signed-in account the catalog is not read, as upstream skips it.
    let models = if authenticated {
        match client.cursor_list_available_models().await {
            Ok(result) => result.models,
            Err(error) => {
                let error = CursorError::from_acp(CursorOperation::Models, &error);
                return CursorProbe {
                    installed: true,
                    version,
                    status,
                    auth,
                    message: join_messages(
                        None,
                        Some(format!("Cursor model discovery failed: {}", error.detail)),
                    ),
                    models: Vec::new(),
                };
            }
        }
    } else {
        Vec::<AvailableModel>::new()
    };
    CursorProbe {
        installed: true,
        version,
        status,
        auth,
        message: auth_message,
        models,
    }
}

fn failed(installed: bool, version: Option<String>, message: String) -> CursorProbe {
    CursorProbe {
        installed,
        version,
        status: ServerProviderState::Error,
        auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
        message: Some(message),
        models: Vec::new(),
    }
}

fn join_messages(first: Option<String>, second: Option<String>) -> Option<String> {
    let joined = [first, second]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "local cursor-agent initialize only; no prompt"]
    async fn live_cursor_agent_initialize() {
        let version = std::process::Command::new("cursor-agent")
            .arg("--version")
            .output();
        if version.is_err() {
            return;
        }
        let response = probe_initialize("cursor-agent").await.unwrap();
        assert!(response.protocol_version >= 1);
    }

    #[tokio::test]
    async fn a_missing_binary_is_not_installed() {
        let probe = probe_status("/nonexistent/turin-cursor-agent-for-tests").await;
        assert!(!probe.installed);
        assert_eq!(probe.status, ServerProviderState::Error);
        assert!(probe.models.is_empty());
        assert!(
            probe
                .message
                .as_deref()
                .unwrap()
                .starts_with("Cursor agent could not start")
        );
    }
}
