//! Live smoke against an installed OpenCode 1.x binary.
//!
//! Gated on `TURIN_OPENCODE_LIVE=1`. Starts `opencode serve`, checks
//! `/global/health`, and opens `GET /event`. Does not send a prompt.

use std::time::Duration;

use futures::StreamExt;

use crate::{StartOpenCodeServer, V1Client, default_v1_verify, start_open_code_server_process};

#[tokio::test]
async fn installed_opencode_serves_health_and_events() {
    if std::env::var("TURIN_OPENCODE_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let binary = std::env::var("OPENCODE_BIN").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/.opencode/bin/opencode")
    });
    let directory = std::env::current_dir().unwrap().display().to_string();
    let process = start_open_code_server_process(
        StartOpenCodeServer {
            binary_path: binary,
            directory: directory.clone(),
            server_password: Some("turin-live-smoke".into()),
            environment: None,
            hostname: Some("127.0.0.1".into()),
            port: None,
            timeout: Some(Duration::from_secs(30)),
            shutdown_grace: None,
        },
        default_v1_verify(),
    )
    .await
    .expect("opencode serve");
    let client = V1Client::new(
        &process.url,
        &directory,
        process.server_password.as_deref(),
    )
    .unwrap();
    let health = client.health().await.expect("health");
    assert!(health.healthy);
    assert_eq!(health.version, process.version);
    let mut events = client.events().await.expect("event stream");
    let first = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("event timeout")
        .expect("event ended")
        .expect("event error");
    assert_eq!(first.event_type, "server.connected");
    process.shutdown().await;
}
