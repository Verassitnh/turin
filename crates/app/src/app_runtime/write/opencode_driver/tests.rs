use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use turin_contracts::provider_instance::{
    ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
};
use turin_provider_core::driver::{ProviderDriverCreateInput, ProviderInstanceScope};
use turin_provider_core::provider_host_live::{DesktopProviderHost, ShouldRunBackgroundWork};
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::server_provider::{ServerProviderAuthStatus, ServerProviderState};

use super::*;

/// A host over a temp dir; the dir lives as long as the returned guard.
fn host() -> (Arc<ProviderHost>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("host dir");
    let should_run: ShouldRunBackgroundWork = Arc::new(|_scope| Box::pin(async { false }));
    let live = DesktopProviderHost::new(
        dir.path(),
        dir.path(),
        turin_contracts::settings::default_server_settings(),
        should_run,
    )
    .expect("desktop host opens");
    (Arc::new(live.host), dir)
}

fn driver() -> (OpenCodeDriver, tempfile::TempDir) {
    let (host, dir) = host();
    (OpenCodeDriver::new(host), dir)
}

#[test]
fn exposes_the_opencode_history_reader_for_the_usage_page() {
    let (driver, _dir) = driver();
    assert!(matches!(
        driver.usage(),
        Some(ProviderUsageReader::Scan {
            provider: turin_contracts::usage::UsageProviderKind::Opencode,
            ..
        })
    ));
}

#[test]
fn config_round_trips_with_upstream_defaults() {
    let (driver, _dir) = driver();
    let decoded = driver
        .decode_config(&json!({"enabled": true, "binaryPath": " /opt/opencode "}))
        .expect("decodes");
    assert_eq!(decoded["enabled"], true);
    assert_eq!(decoded["binaryPath"], "/opt/opencode");
    assert_eq!(decoded["serverUrl"], "");
    assert_eq!(
        driver.decode_config(&Value::Null).expect("null")["binaryPath"],
        "opencode"
    );
    assert!(driver.decode_config(&json!({"binaryPath": 3})).is_err());
    // Custom models survive the decode, and a malformed list is refused.
    let with_custom = driver
        .decode_config(&json!({"customModels": ["acme/one"]}))
        .expect("custom models");
    assert_eq!(with_custom["customModels"], json!(["acme/one"]));
    assert!(driver.decode_config(&json!({"customModels": [3]})).is_err());
}

/// A minimal HTTP server: `route` maps a request path (no query) to a JSON body.
struct FakeOpenCode {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeOpenCode {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeOpenCode {
    async fn spawn(route: impl Fn(&str) -> Option<Value> + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let route = Arc::new(route);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let route = Arc::clone(&route);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let target = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let path = target.split('?').next().unwrap_or("/");
                    let (status, body) = match route(path) {
                        Some(body) => ("200 OK", body.to_string()),
                        None => ("404 Not Found", "{}".to_owned()),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { url, task }
    }
}

/// A 1.x server with one connected provider, one skill and one command.
async fn v1_server() -> FakeOpenCode {
    FakeOpenCode::spawn(|path| match path {
        "/global/health" => Some(json!({"healthy": true, "version": "1.19.0"})),
        "/provider" => Some(json!({
            "all": [{
                "id": "openai",
                "name": "OpenAI",
                "models": {"gpt": {"id": "gpt", "name": "GPT"}}
            }],
            "connected": ["openai"]
        })),
        "/agent" => Some(json!([])),
        "/skill" => Some(json!([
            {"name": "lint", "description": "Lint it", "location": "/skills/lint"}
        ])),
        "/command" => Some(json!([{"name": "review", "description": "Review"}])),
        _ => None,
    })
    .await
}

async fn instance(driver: &OpenCodeDriver, config: Value, enabled: bool) -> ProviderInstance {
    driver
        .create(ProviderDriverCreateInput {
            instance_id: ProviderInstanceId::new("opencode").unwrap(),
            display_name: None,
            accent_color: None,
            environment: Vec::new(),
            enabled,
            config: driver.decode_config(&config).expect("config"),
            scope: ProviderInstanceScope::new(),
        })
        .await
        .expect("instance")
}

#[tokio::test]
async fn an_enabled_instance_leaves_checking_with_the_servers_models_and_version() {
    let server = v1_server().await;
    let (driver, _dir) = driver();
    let instance = instance(
        &driver,
        json!({"serverUrl": server.url, "customModels": ["acme/mine"]}),
        true,
    )
    .await;

    // The refresh the Providers panel's button runs.
    let refreshed = instance.snapshot.refresh().await;
    assert_eq!(refreshed.status, ServerProviderState::Ready);
    assert!(refreshed.installed);
    assert_eq!(refreshed.version.as_ref().unwrap().as_str(), "1.19.0");
    assert_eq!(
        refreshed.auth.status,
        ServerProviderAuthStatus::Authenticated
    );
    let slugs: Vec<_> = refreshed
        .models
        .iter()
        .map(|model| model.slug.as_str())
        .collect();
    assert_eq!(slugs, ["openai/gpt", "acme/mine"]);
    assert!(refreshed.models[1].is_custom);
    assert!(
        refreshed
            .slash_commands
            .iter()
            .any(|command| command.name.as_str() == "review")
    );
    assert_eq!(refreshed.skills[0].name.as_str(), "lint");
    // The status stream (what the registry and settings subscribe to) carries it too.
    assert_eq!(
        instance.snapshot.get_snapshot().await.status,
        ServerProviderState::Ready
    );
}

#[tokio::test]
async fn the_first_probe_runs_when_the_instance_is_created() {
    let server = v1_server().await;
    let (driver, _dir) = driver();
    let instance = instance(&driver, json!({"serverUrl": server.url}), true).await;
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = instance.snapshot.get_snapshot().await;
            if snapshot.status == ServerProviderState::Ready {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the background probe publishes");
    assert_eq!(ready.models.len(), 1);
}

#[tokio::test]
async fn an_unreachable_server_is_an_error_naming_the_url() {
    // Nothing listens on the port a closed listener held.
    let closed = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let (driver, _dir) = driver();
    let instance = instance(&driver, json!({"serverUrl": closed}), true).await;
    let snapshot = instance.snapshot.refresh().await;
    assert_eq!(snapshot.status, ServerProviderState::Error);
    assert!(snapshot.models.is_empty());
    let message = snapshot.message.as_ref().expect("a message").as_str();
    assert!(!message.contains("not been checked"), "{message}");
}

#[tokio::test]
async fn a_disabled_instance_is_not_probed() {
    let (driver, _dir) = driver();
    let instance = instance(
        &driver,
        json!({"serverUrl": "http://127.0.0.1:9", "customModels": ["acme/mine"]}),
        false,
    )
    .await;
    let snapshot = instance.snapshot.refresh().await;
    assert!(!snapshot.enabled);
    assert_eq!(snapshot.status, ServerProviderState::Disabled);
    assert!(
        snapshot
            .message
            .as_ref()
            .unwrap()
            .as_str()
            .contains("disabled")
    );
    // Custom models stay listed while the instance is off.
    assert_eq!(snapshot.models.len(), 1);
}

#[tokio::test]
async fn a_two_point_x_server_lists_its_catalog_and_workspace() {
    let server = FakeOpenCode::spawn(|path| match path {
        "/api/info" => Some(json!({
            "version": "2.0.3", "pid": 7, "urls": [], "paths": {"tmp": "/tmp"}
        })),
        "/api/model" => Some(json!({"location": {}, "data": [
            {"providerID": "zen", "id": "big", "name": "Big", "variants": [{"id": "high"}]}
        ]})),
        "/api/provider" => Some(json!({"location": {}, "data": [
            {"id": "zen", "name": "OpenCode Zen"}
        ]})),
        "/api/skill" => Some(json!({"location": {}, "data": [
            {"id": "lint", "name": "Lint", "path": "/skills/lint", "description": "Lint it"}
        ]})),
        "/api/command" => Some(json!({"location": {}, "data": [
            {"name": "review", "description": "Review"}
        ]})),
        _ => None,
    })
    .await;
    let (driver, _dir) = driver();
    let instance = instance(&driver, json!({"serverUrl": server.url}), true).await;

    let refreshed = instance.snapshot.refresh().await;
    assert_eq!(refreshed.status, ServerProviderState::Ready);
    assert_eq!(refreshed.version.as_ref().unwrap().as_str(), "2.0.3");
    assert_eq!(refreshed.models[0].slug.as_str(), "zen/big");
    assert_eq!(
        refreshed.models[0].sub_provider.as_ref().unwrap().as_str(),
        "OpenCode Zen"
    );

    let snapshot_for_cwd = instance
        .snapshot_for_cwd
        .as_ref()
        .expect("workspace listing");
    let scoped = snapshot_for_cwd("/work".to_owned())
        .await
        .expect("workspace");
    assert_eq!(scoped.provider.skills[0].name.as_str(), "lint");
    assert_eq!(
        scoped.provider.skills[0]
            .display_name
            .as_ref()
            .unwrap()
            .as_str(),
        "Lint"
    );
    let commands: Vec<_> = scoped
        .provider
        .slash_commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(commands, ["compact", "review"]);
}

#[tokio::test]
async fn the_workspace_listing_reads_one_directorys_skills_and_commands() {
    let server = v1_server().await;
    let (driver, _dir) = driver();
    let instance = instance(&driver, json!({"serverUrl": server.url}), true).await;
    instance.snapshot.refresh().await;
    let snapshot_for_cwd = instance
        .snapshot_for_cwd
        .as_ref()
        .expect("workspace listing");
    let scoped = snapshot_for_cwd("/work".to_owned())
        .await
        .expect("workspace");
    assert_eq!(scoped.provider.skills[0].path.as_str(), "/skills/lint");
    assert_eq!(scoped.provider.slash_commands[0].name.as_str(), "compact");
    assert_eq!(scoped.provider.slash_commands[1].name.as_str(), "review");
    // The machine snapshot (models) stays on the scoped one.
    assert_eq!(scoped.provider.models.len(), 1);
}

/// The settings fold strips `config.enabled` into the envelope; the
/// envelope flag alone must enable the instance.
#[tokio::test]
async fn envelope_enabled_flag_wins_over_folded_config() {
    let id = ProviderInstanceId::new("opencode").unwrap();
    let mut map = ProviderInstanceConfigMap::default();
    map.insert(
        id.clone(),
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("opencode").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: Some(true),
            config: Some(json!({"binaryPath": "/opt/opencode"})),
        },
    );
    let (driver, _dir) = driver();
    let registry =
        ProviderInstanceRegistry::new(vec![Arc::new(driver) as Arc<dyn ProviderDriver>], map).await;
    let instance = registry.get_instance(&id).unwrap();
    assert!(instance.enabled);
    let snapshot = instance.snapshot.get_snapshot().await;
    assert!(snapshot.draft.enabled);
}
