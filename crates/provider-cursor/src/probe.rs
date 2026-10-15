//! A free `cursor-agent acp` initialize probe. It does not open a session and
//! does not send a prompt. Dropping the client signals the process group.

use turin_acp::compat::InitializeResponse;
use turin_acp::{AcpClient, SpawnCommand, spawn};

use crate::auth::ACP_ARGS;
use crate::error::{CursorError, CursorOperation};

pub async fn probe_initialize(program: &str) -> Result<InitializeResponse, CursorError> {
    let client = spawn(SpawnCommand {
        program,
        args: ACP_ARGS,
        cwd: None,
    })
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
}
