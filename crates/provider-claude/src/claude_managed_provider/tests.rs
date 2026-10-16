//! Wiring for the instance probes and shared services. The manifest and
//! reset-credit behavior is covered in `claude_driver` and
//! `claude_reset_credit_coordinator`.
use super::*;
use crate::{
    claude_model_catalog::bundled_claude_model_catalog, claude_usage_limits::ClaudeScopedLimitNames,
};
use parking_lot::Mutex as SyncMutex;
use serde_json::json;
use turin_contracts::settings::ServerSettings;
use turin_provider_core::provider_host_live::{DesktopProviderHost, ShouldRunBackgroundWork};

fn host(dir: &std::path::Path, settings: serde_json::Value) -> Arc<ProviderHost> {
    let settings: ServerSettings =
        serde_json::from_value(settings).expect("server settings decode");
    let should_run: ShouldRunBackgroundWork = Arc::new(|_scope| Box::pin(async { true }));
    Arc::new(
        DesktopProviderHost::new(dir, dir, settings, should_run)
            .expect("desktop host opens")
            .host,
    )
}

fn settings(value: serde_json::Value) -> ClaudeSettings {
    serde_json::from_value(value).expect("settings decode")
}

/// A `claude` stand-in that prints a version and exits 0. Its initialization
/// probe fails, so the snapshot is a warning with the version still reported.
fn fake_claude(dir: &std::path::Path, version_line: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\necho \"{version_line}\"\n")).expect("write fake");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[tokio::test]
async fn an_installed_cli_gets_a_version_advisory_without_a_latest_lookup_when_checks_are_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = fake_claude(dir.path(), "2.1.288 (Claude Code)");
    let host = host(dir.path(), json!({"enableProviderUpdateChecks": false}));
    let services = Arc::new(ClaudeSharedServices::new(host));
    let probes = Arc::new(ClaudeInstanceProbes::new(
        ProviderInstanceId::new("claudeAgent").expect("instance id"),
        settings(json!({"binaryPath": binary.to_string_lossy()})),
        ProcessEnv::new(),
        dir.path().to_string_lossy().into_owned(),
        Arc::new(SyncMutex::new(ClaudeScopedLimitNames::default())),
        services,
    ));
    let managed = ClaudeManagedProvider::new(
        probes,
        ProviderDriverKind::new("claudeAgent").expect("driver"),
        bundled_claude_model_catalog(),
    );

    let snapshot = managed.refresh().await;
    assert_eq!(
        snapshot.draft.version.as_ref().map(|v| v.as_str()),
        Some("2.1.288")
    );
    let advisory = snapshot.draft.version_advisory.expect("advisory attached");
    assert_eq!(
        advisory.current_version.as_ref().map(|v| v.as_str()),
        Some("2.1.288")
    );
    // Checks are off, so no npm lookup ran and no latest version is known.
    assert_eq!(advisory.latest_version, None);
}

#[tokio::test]
async fn instances_built_from_one_driver_share_the_reset_coordinator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = host(dir.path(), json!({}));
    let services = Arc::new(ClaudeSharedServices::new(host));
    let make = || {
        Arc::new(ClaudeInstanceProbes::new(
            ProviderInstanceId::new("claudeAgent").expect("instance id"),
            settings(json!({})),
            ProcessEnv::new(),
            dir.path().to_string_lossy().into_owned(),
            Arc::new(SyncMutex::new(ClaudeScopedLimitNames::default())),
            services.clone(),
        ))
    };
    let first = make();
    let second = make();
    assert!(Arc::ptr_eq(&first.services, &second.services));
    assert!(Arc::ptr_eq(
        &first.services.reset_credits,
        &second.services.reset_credits
    ));
}
