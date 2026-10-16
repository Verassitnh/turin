//! Port of the auth and status behavior in upstream/apps/server/src/provider/ClaudeProvider.ts.
//! Upstream has no dedicated ClaudeProvider.test.ts; the auth cases came from
//! ClaudeCapabilitiesProbe.test.ts and the rest cover the status draft.
use super::*;
use crate::claude_model_catalog::bundled_claude_model_catalog;
use serde_json::json;
use std::path::PathBuf;
use turin_contracts::model::{CustomModelSetting, ProviderOptionDescriptor};

fn probe(value: serde_json::Value) -> ClaudeCapabilitiesProbe {
    let account = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    ClaudeCapabilitiesProbe {
        email: account("email"),
        workspace_id: None,
        subscription_type: account("subscriptionType"),
        token_source: account("tokenSource"),
        api_key_source: account("apiKeySource"),
        api_provider: account("apiProvider"),
        slash_commands: Vec::new(),
        usage: None,
    }
}

#[test]
fn only_a_none_token_source_disproves_authentication() {
    assert_eq!(
        claude_auth_status(&probe(
            json!({"tokenSource": "none", "apiKeySource": "none"})
        )),
        ServerProviderAuthStatus::Unauthenticated
    );
    assert_eq!(
        claude_auth_status(&probe(
            json!({"tokenSource": "none", "apiKeySource": "env"})
        )),
        ServerProviderAuthStatus::Authenticated
    );
    assert_eq!(
        claude_auth_status(&probe(
            json!({"apiProvider": "bedrock", "tokenSource": "none"})
        )),
        ServerProviderAuthStatus::Authenticated
    );
    assert_eq!(
        claude_auth_status(&probe(json!({}))),
        ServerProviderAuthStatus::Authenticated
    );
}

#[test]
fn auth_metadata_labels_api_keys_and_subscriptions() {
    assert_eq!(
        claude_auth_metadata(None, Some("ANTHROPIC_API_KEY")),
        Some(("apiKey".to_owned(), "Claude API Key".to_owned()))
    );
    assert_eq!(
        claude_auth_metadata(Some("pro"), Some("oauth")),
        Some(("pro".to_owned(), "Claude Pro Subscription".to_owned()))
    );
    assert_eq!(
        claude_auth_metadata(Some("claude_team_subscription"), None),
        Some((
            "claude_team_subscription".to_owned(),
            "Claude Team Subscription".to_owned()
        ))
    );
    assert_eq!(
        api_provider_auth_metadata(Some("bedrock")),
        Some(("bedrock".to_owned(), "Amazon Bedrock".to_owned()))
    );
}

fn custom(value: serde_json::Value) -> CustomModelSetting {
    serde_json::from_value(value).expect("custom model decodes")
}

#[test]
fn custom_models_rename_a_builtin_or_append_with_default_capabilities() {
    let builtin: Vec<ServerProviderModel> = bundled_claude_model_catalog()
        .models
        .iter()
        .map(|entry| entry.model.clone())
        .take(1)
        .collect();
    let base_slug = builtin[0].slug.as_str().to_owned();
    let models = provider_models_from_settings(
        builtin,
        &[
            custom(json!({"slug": base_slug, "name": "Renamed"})),
            custom(json!("claude-bare-custom")),
        ],
        default_claude_model_capabilities(),
    );
    assert_eq!(models[0].name.as_str(), "Renamed");
    assert_eq!(models[0].short_name, None);
    let appended = models.last().expect("custom model appended");
    assert_eq!(appended.slug.as_str(), "claude-bare-custom");
    assert!(appended.is_custom);
    assert_eq!(
        appended.capabilities,
        Some(ModelCapabilities {
            option_descriptors: Some(Vec::<ProviderOptionDescriptor>::new())
        })
    );
}

fn settings(value: serde_json::Value) -> ClaudeSettings {
    serde_json::from_value(value).expect("settings decode")
}

fn host_env() -> ProcessEnv {
    std::env::vars().collect()
}

/// A `claude` stand-in that prints a version and exits 0.
fn fake_claude(dir: &std::path::Path, version_line: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\necho \"{version_line}\"\n")).expect("write fake");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

async fn status(
    settings: &ClaudeSettings,
    capabilities: Option<ClaudeCapabilitiesProbe>,
) -> ServerProviderDraft {
    let resolver: Option<CapabilitiesResolver> = capabilities.map(|caps| {
        Arc::new(move || {
            let caps = caps.clone();
            Box::pin(async move { Some(caps) }) as BoxFuture<'static, _>
        }) as CapabilitiesResolver
    });
    check_claude_provider_status(ClaudeStatusInput {
        settings,
        environment: &host_env(),
        home: "/Users/test",
        cwd: None,
        catalog: bundled_claude_model_catalog(),
        scoped_limit_names: None,
        resolve_capabilities: resolver,
        resolve_reset_credits: None,
    })
    .await
}

#[tokio::test]
async fn a_disabled_instance_reports_disabled_without_probing() {
    let draft = status(&settings(json!({"enabled": false})), None).await;
    assert_eq!(draft.status, ServerProviderState::Disabled);
    assert!(!draft.installed);
    assert_eq!(
        draft.message.as_ref().map(|m| m.as_str()),
        Some("Claude is disabled in T3 Code settings.")
    );
}

#[tokio::test]
async fn a_missing_binary_is_reported_as_not_installed() {
    let draft = status(
        &settings(json!({"binaryPath": "/nonexistent/claude-binary-for-test"})),
        None,
    )
    .await;
    assert!(!draft.installed);
    assert_eq!(draft.status, ServerProviderState::Error);
    assert_eq!(
        draft.message.as_ref().map(|m| m.as_str()),
        Some("Claude Agent CLI (`claude`) was not found on PATH.")
    );
}

#[tokio::test]
async fn an_installed_cli_without_initialization_is_a_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = fake_claude(dir.path(), "2.1.288 (Claude Code)");
    let draft = status(
        &settings(json!({"binaryPath": binary.to_string_lossy()})),
        None,
    )
    .await;
    assert!(draft.installed);
    assert_eq!(draft.version.as_ref().map(|v| v.as_str()), Some("2.1.288"));
    assert_eq!(draft.status, ServerProviderState::Warning);
    assert_eq!(draft.auth.status, ServerProviderAuthStatus::Unknown);
    assert_eq!(
        draft.message.as_ref().map(|m| m.as_str()),
        Some("Could not verify Claude authentication status from initialization result.")
    );
    assert_eq!(draft.slash_commands[0].name.as_str(), "compact");
}

#[tokio::test]
async fn an_initialized_login_is_ready_with_its_account_and_unavailable_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = fake_claude(dir.path(), "2.1.288 (Claude Code)");
    let capabilities = ClaudeCapabilitiesProbe {
        email: Some("dev@example.com".into()),
        subscription_type: Some("pro".into()),
        token_source: Some("oauth".into()),
        slash_commands: vec![ServerProviderSlashCommand {
            name: TrimmedNonEmptyString::new("review").unwrap(),
            description: None,
            input: None,
        }],
        usage: None,
        ..Default::default()
    };
    let draft = status(
        &settings(json!({"binaryPath": binary.to_string_lossy()})),
        Some(capabilities),
    )
    .await;

    assert_eq!(draft.status, ServerProviderState::Ready);
    assert_eq!(draft.auth.status, ServerProviderAuthStatus::Authenticated);
    assert_eq!(
        draft.auth.email.as_ref().map(|e| e.as_str()),
        Some("dev@example.com")
    );
    assert_eq!(
        draft.auth.label.as_ref().map(|l| l.as_str()),
        Some("Claude Pro Subscription")
    );
    let names: Vec<&str> = draft
        .slash_commands
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, vec!["compact", "review"]);
    // No usage response means the probe failed, not that the account has no limits.
    let usage = draft.usage_limits.expect("usage limits present");
    assert_eq!(
        usage.unavailable.map(|u| u.reason),
        Some(ServerProviderUsageUnavailableReason::ProbeFailed)
    );
}

#[test]
fn the_pending_snapshot_is_warning_until_the_first_probe() {
    let draft = make_pending_claude_provider(&settings(json!({})), bundled_claude_model_catalog());
    assert_eq!(draft.status, ServerProviderState::Warning);
    assert!(!draft.installed);
    assert_eq!(
        draft.message.as_ref().map(|m| m.as_str()),
        Some("Claude provider status has not been checked in this session yet.")
    );
}
