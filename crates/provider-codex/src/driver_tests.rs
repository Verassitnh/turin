//! Driver-level tests with a fake `codex` that is a POSIX shell script. It answers
//! the app-server requests the status probe and the skills probe make, and the
//! `mcp list` and `exec` calls text generation makes. Nothing here starts a real
//! Codex or reaches a network.

use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

use serde_json::{Value, json};
use turin_contracts::{
    TrimmedNonEmptyString,
    model_selection::ModelSelection,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_usage_limits::{
        ProviderUsageLimitsUpdate, ServerProviderUsageWindow, ServerProviderUsageWindowKind,
    },
};
use turin_provider_core::{
    driver::{ProviderDriver, ProviderDriverCreateInput, ProviderInstanceScope},
    model_catalog::ModelCatalog,
    model_manifest::ModelManifest,
    text_generation::ThreadTitleGenerationInput,
};

use crate::{
    driver::{CodexDriver, StatusInput, check_codex_provider_status},
    runtime::CodexRuntimeConfig,
};

/// Answers `initialize`, `account/read`, `model/list`, `skills/list` (echoing the
/// requested cwd), and rate limits (an error); `mcp list` and `exec` as text
/// generation calls them.
const FAKE_CODEX: &str = r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in mcp) is_mcp=1 ;; exec) is_exec=1 ;; esac
done
if [ "$is_mcp" = 1 ]; then printf '[]'; exit 0; fi
if [ "$is_exec" = 1 ]; then
  out=''; prev=''
  for a in "$@"; do
    if [ "$prev" = --output-last-message ]; then out="$a"; fi
    prev="$a"
  done
  cat >/dev/null
  printf '%s\n' '{"title":"Fixture title"}' > "$out"
  exit 0
fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex-cli/%s","codexHome":"/tmp/fake-codex-home","platformFamily":"unix","platformOs":"macos"}}\n' "$id" "${FAKE_VERSION:-0.156.1}" ;;
    *'"method":"account/read"'*) printf '{"id":%s,"result":{"account":{"type":"apiKey"},"requiresOpenaiAuth":false}}\n' "$id" ;;
    *'"method":"model/list"'*) printf '{"id":%s,"result":{"data":[{"id":"gpt-5.5","model":"gpt-5.5","displayName":"gpt-5.5","description":"d","hidden":false,"isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"l"}]}]}}\n' "$id" ;;
    *'"method":"account/rateLimits/read"'*) printf '{"id":%s,"error":{"code":-32601,"message":"not available"}}\n' "$id" ;;
    *'"method":"skills/list"'*)
      cwd=$(printf '%s' "$line" | sed -n 's/.*"cwds":\["\([^"]*\)"\].*/\1/p')
      printf '{"id":%s,"result":{"data":[{"cwd":"%s","errors":[],"skills":[{"name":"workspace-skill","description":"d","scope":"repo","path":"%s/SKILL.md","enabled":true}]}]}}\n' "$id" "$cwd" "$cwd" ;;
  esac
done
"#;

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn create(
    driver: &CodexDriver,
    binary: &Path,
    enabled: bool,
) -> turin_provider_core::driver::ProviderInstance {
    create_with_environment(driver, binary, enabled, Vec::new()).await
}

async fn create_with_environment(
    driver: &CodexDriver,
    binary: &Path,
    enabled: bool,
    environment: turin_contracts::provider_instance::ProviderInstanceEnvironment,
) -> turin_provider_core::driver::ProviderInstance {
    driver
        .create(ProviderDriverCreateInput {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
            display_name: None,
            accent_color: None,
            environment,
            enabled,
            config: driver
                .decode_config(&json!({"binaryPath": binary.to_string_lossy()}))
                .unwrap(),
            scope: ProviderInstanceScope::new(),
        })
        .await
        .unwrap()
}

fn window(id: &str, used: f64) -> ServerProviderUsageWindow {
    ServerProviderUsageWindow {
        id: TrimmedNonEmptyString::new(id).unwrap(),
        kind: ServerProviderUsageWindowKind::Session,
        label: TrimmedNonEmptyString::new("Session").unwrap(),
        used_percent: used,
        resets_at: None,
        window_duration_mins: Some(300),
    }
}

#[tokio::test]
async fn snapshot_for_cwd_adds_the_workspace_skills_to_the_machine_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("codex");
    write_executable(&binary, FAKE_CODEX);
    let instance = create(&CodexDriver::new(), &binary, true).await;
    let for_cwd = instance.snapshot_for_cwd.clone().expect("snapshotForCwd");

    let workspace = for_cwd("/work/project".to_owned()).await.unwrap();
    let skills: Vec<_> = workspace
        .provider
        .skills
        .iter()
        .map(|skill| (skill.name.as_str(), skill.path.as_str()))
        .collect();
    assert_eq!(skills, [("workspace-skill", "/work/project/SKILL.md")]);
    assert_eq!(workspace.slash_commands_pending, None);
    assert_eq!(workspace.provider.instance_id.as_str(), "codex");
    // The shared machine snapshot is unchanged.
    assert!(instance.snapshot.get_snapshot().await.skills.is_empty());
}

#[tokio::test]
async fn snapshot_for_cwd_fails_when_the_skills_probe_does() {
    let instance = create(
        &CodexDriver::new(),
        Path::new("/nonexistent/turin-codex-binary"),
        true,
    )
    .await;
    let error = instance.snapshot_for_cwd.clone().unwrap()("/work".to_owned())
        .await
        .expect_err("probe fails");
    assert_eq!(error.detail, "Failed to probe Codex skills for '/work'");
}

#[tokio::test]
async fn a_disabled_instance_returns_its_snapshot_without_probing() {
    let instance = create(
        &CodexDriver::new(),
        Path::new("/nonexistent/turin-codex-binary"),
        false,
    )
    .await;
    let workspace = instance.snapshot_for_cwd.clone().unwrap()("/work".to_owned())
        .await
        .unwrap();
    assert!(!workspace.provider.enabled);
    assert!(workspace.provider.skills.is_empty());
}

#[tokio::test]
async fn usage_pushes_update_the_published_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("codex");
    write_executable(&binary, FAKE_CODEX);
    let instance = create(&CodexDriver::new(), &binary, true).await;
    assert!(
        instance
            .snapshot
            .get_snapshot()
            .await
            .usage_limits
            .is_none()
    );

    instance
        .snapshot
        .apply_usage_limits(
            ProviderUsageLimitsUpdate {
                windows: vec![window("primary", 30.0)],
            },
            "2026-01-01T00:00:00.000Z",
        )
        .await;
    instance
        .snapshot
        .apply_usage_limits(
            ProviderUsageLimitsUpdate {
                windows: vec![window("primary", 61.0)],
            },
            "2026-01-01T00:01:00.000Z",
        )
        .await;

    let limits = instance
        .snapshot
        .get_snapshot()
        .await
        .usage_limits
        .clone()
        .unwrap();
    assert_eq!(limits.windows.len(), 1);
    assert_eq!(limits.windows[0].used_percent, 61.0);
    assert_eq!(limits.checked_at, "2026-01-01T00:01:00.000Z");
}

#[tokio::test]
async fn text_generation_runs_codex_exec_through_the_instance() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("codex");
    write_executable(&binary, FAKE_CODEX);
    let instance = create(&CodexDriver::new(), &binary, true).await;
    let selection: ModelSelection =
        serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5.4-mini"})).unwrap();
    let result = instance
        .text_generation
        .generate_thread_title(ThreadTitleGenerationInput {
            linked_context: None,
            cwd: temp.path().to_string_lossy().into_owned(),
            message: "Name this thread".to_owned(),
            previous_title: None,
            attachments: Vec::new(),
            model_selection: selection,
        })
        .await
        .unwrap();
    assert_eq!(result.title, "Fixture title");
}

#[tokio::test]
async fn the_manifest_catalog_names_models_a_newer_codex_unlocks() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("codex");
    write_executable(&binary, FAKE_CODEX);
    // An old CLI that lists only a legacy model. Checks off: the bundled manifest decides.
    let manifest = Arc::new(ModelManifest::new(
        temp.path().join("state"),
        Arc::new(|| Box::pin(async { false })),
    ));
    let driver = CodexDriver::new().with_model_catalog(Arc::new(ModelCatalog::new(manifest)));
    let instance = create_with_environment(
        &driver,
        &binary,
        true,
        vec![turin_contracts::provider_instance::ProviderInstanceEnvironmentVariable {
            name: turin_contracts::provider_instance::ProviderInstanceEnvironmentVariableName::new(
                "FAKE_VERSION",
            )
            .unwrap(),
            value: "0.154.0".to_owned(),
            sensitive: false,
            value_redacted: None,
        }],
    )
    .await;
    let snapshot = instance.snapshot.refresh().await;
    assert_eq!(
        snapshot.version.as_ref().map(|v| v.as_str()),
        Some("0.154.0"),
        "{:?}",
        snapshot.message
    );
    let required: Vec<_> = snapshot
        .update_required_models
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|model| {
            (
                model.slug.as_str().to_owned(),
                model.min_version.as_str().to_owned(),
            )
        })
        .collect();
    // 0.154.0 already has what 0.153.0 unlocks; Luna needs 0.155.0.
    assert_eq!(required, [("gpt-6-luna".to_owned(), "0.155.0".to_owned())]);
    // gpt-5.5 is a legacy catalog entry, so the discovered model is flagged.
    let model = snapshot
        .models
        .iter()
        .find(|model| model.slug.as_str() == "gpt-5.5")
        .unwrap();
    assert_eq!(model.is_legacy, Some(true));
}

/// Stand-in for Codex 0.162's app-server catalog behavior (upstream
/// `CodexProvider.catalog.test.ts`):
/// - `model/list` answers from the shared cache while it is fresh;
/// - a startup refresh rewrites the cache after `initialize`;
/// - the backend grants Ultrafast only to entitled accounts on app originators.
const FAKE_CATALOG_APP_SERVER: &str = r#"#!/bin/sh
originator="${CODEX_INTERNAL_ORIGINATOR_OVERRIDE:-codex_cli_rs}"
ultrafast=0
if [ "$FAKE_ULTRAFAST_ENTITLED" = 1 ] && [ "$originator" != codex_cli_rs ]; then ultrafast=1; fi
rm -f "$CODEX_HOME/refreshed"
(
  sleep 0.3
  printf '{"fetched_at":"2999-01-01T00:00:00Z","models":[]}' > "$CODEX_HOME/models_cache.json"
  : > "$CODEX_HOME/refreshed"
) &
tiers() {
  if [ "$1" = 1 ]; then printf '[{"id":"priority","name":"priority","description":""},{"id":"ultrafast","name":"ultrafast","description":""}]'
  else printf '[{"id":"priority","name":"priority","description":""}]'; fi
}
model() {
  printf '{"id":"gpt-6.1-sol","model":"gpt-6.1-sol","displayName":"GPT-6.1 Sol","description":"","hidden":false,"isDefault":true,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high","description":""}],"serviceTiers":%s}' "$(tiers "$1")"
}
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex/0.162.0","codexHome":"%s","platformFamily":"unix","platformOs":"macos"}}\n' "$id" "$CODEX_HOME" ;;
    *'"method":"account/read"'*) printf '{"id":%s,"result":{"account":{"type":"chatgpt","email":"test@example.com","planType":"promax"},"requiresOpenaiAuth":false}}\n' "$id" ;;
    *'"method":"skills/list"'*) printf '{"id":%s,"result":{"data":[]}}\n' "$id" ;;
    *'"method":"account/rateLimits/read"'*) printf '{"id":%s,"error":{"code":-32601,"message":"unsupported"}}\n' "$id" ;;
    *'"method":"model/list"'*)
      if [ -f "$CODEX_HOME/refreshed" ]; then shown=$ultrafast
      elif [ "$FAKE_ULTRAFAST_ENTITLED" = 1 ]; then shown=0; else shown=1; fi
      printf '{"id":%s,"result":{"data":[%s],"nextCursor":null}}\n' "$id" "$(model "$shown")" ;;
  esac
done
"#;

/// The service-tier ids Codex offers for `gpt-6.1-sol`, with the shared cache
/// holding what another client wrote a minute ago.
async fn sol_service_tiers(ultrafast_entitled: bool) -> Vec<String> {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("codex");
    write_executable(&binary, FAKE_CATALOG_APP_SERVER);
    // A fresh cache written by another client: the plain CLI's (no Ultrafast) when
    // entitled, or a stale entitled catalog when the entitlement is gone.
    std::fs::write(
        temp.path().join("models_cache.json"),
        json!({
            "fetched_at": (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
            "models": [],
        })
        .to_string(),
    )
    .unwrap();
    let config = CodexRuntimeConfig {
        binary,
        cwd: temp.path().to_path_buf(),
        home_path: Some(temp.path().to_string_lossy().into_owned()),
        launch_args: Vec::new(),
        environment: [(
            "FAKE_ULTRAFAST_ENTITLED".to_owned(),
            if ultrafast_entitled { "1" } else { "0" }.to_owned(),
        )]
        .into(),
        remove_environment: vec!["CODEX_INTERNAL_ORIGINATOR_OVERRIDE".to_owned()],
        client_version: "1".to_owned(),
    };
    let kind = ProviderDriverKind::new("codex").unwrap();
    let instance_id = ProviderInstanceId::new("codex").unwrap();
    let provider = check_codex_provider_status(StatusInput {
        kind: &kind,
        instance_id: &instance_id,
        enabled: true,
        config: &config,
        custom_models: &[],
        managed_auth: None,
    })
    .await;
    let sol = provider
        .models
        .iter()
        .find(|model| model.slug.as_str() == "gpt-6.1-sol")
        .unwrap_or_else(|| panic!("no sol model: {:?}", provider.message));
    let descriptors = sol
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.option_descriptors.as_ref())
        .unwrap();
    let value: Value = serde_json::to_value(descriptors).unwrap();
    value
        .as_array()
        .unwrap()
        .iter()
        .find(|descriptor| descriptor["id"] == "serviceTier")
        .map(|descriptor| {
            descriptor["options"]
                .as_array()
                .unwrap()
                .iter()
                .map(|option| option["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn keeps_ultrafast_when_another_codex_client_wrote_the_shared_catalog_cache() {
    assert_eq!(
        sol_service_tiers(true).await,
        ["default", "priority", "ultrafast"]
    );
}

#[tokio::test]
async fn drops_ultrafast_once_the_account_is_no_longer_entitled_to_it() {
    assert_eq!(sol_service_tiers(false).await, ["default", "priority"]);
}
