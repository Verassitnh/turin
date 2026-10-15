//! Live smoke against an installed OpenCode 1.x binary.
//!
//! Gated on `TURIN_OPENCODE_LIVE=1`. Starts `opencode serve`, checks
//! `/global/health`, opens `GET /event?directory=`, and creates a session in a
//! temp directory. Confirms that stream delivers `session.created` for that
//! session. Does not send a prompt.

use std::time::Duration;

use futures::StreamExt;
use serde_json::json;

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
    let directory = std::env::temp_dir().join(format!("t3-opencode-live-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temp dir");
    let directory = directory.to_string_lossy().into_owned();
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
            ledger: None,
        },
        default_v1_verify(),
    )
    .await
    .expect("opencode serve");
    let client =
        V1Client::new(&process.url, &directory, process.server_password.as_deref()).unwrap();
    let health = client.health().await.expect("health");
    assert!(health.healthy);
    assert_eq!(health.version, process.version);

    let mut events = client.events().await.expect("event stream");
    let session = client
        .create_session(&json!({}))
        .await
        .expect("create session");
    let session_id = session["id"].as_str().expect("session id").to_string();

    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.next().await {
            let event = event.expect("event");
            if event.event_type == "session.created"
                && event.properties["info"]["id"].as_str() == Some(session_id.as_str())
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("timed out waiting for session.created");
    assert!(
        found,
        "GET /event?directory= did not deliver session.created"
    );
    process.shutdown().await;
    let _ = std::fs::remove_dir_all(&directory);
}
