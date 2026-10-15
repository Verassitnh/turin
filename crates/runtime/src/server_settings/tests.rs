use serde_json::json;
use turin_contracts::settings::default_server_settings;

use super::*;

#[test]
fn a_token_streaming_mode_decodes_as_paragraph_at_both_levels() {
    let settings = decode_settings_json(
        r#"{"responseStreamingMode":"token","projectSettingsOverrides":{"p1":{"responseStreamingMode":"token"}}}"#,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(settings.response_streaming_mode).unwrap(),
        json!("paragraph")
    );
}

#[test]
fn invalid_json_or_a_non_object_file_fails_to_decode() {
    assert!(decode_settings_json("{").is_err());
    assert!(decode_settings_json("[]").is_err());
}

#[test]
fn a_lenient_file_with_comments_and_trailing_commas_decodes() {
    let raw = "{\n  // a comment\n  \"enableProviderUpdateChecks\": false,\n}";
    let settings = decode_settings_json(raw).unwrap();
    assert!(!settings.enable_provider_update_checks);
}

#[test]
fn legacy_keys_must_be_objects_when_present() {
    assert!(decode_legacy_file(r#"{"providers":{"codex":{}}}"#).is_ok());
    assert!(decode_legacy_file(r#"{"providers":[]}"#).is_err());
    assert!(decode_legacy_file(r#"{"bitbucket":"x"}"#).is_err());
    assert!(decode_legacy_file(r#"{"other":1}"#).is_ok());
}

#[test]
fn defaults_serialize_to_an_empty_sparse_object() {
    let text = sparse_settings_json(&default_server_settings()).unwrap();
    assert_eq!(text, "{}\n");
}

#[test]
fn only_changed_fields_are_written_sparse() {
    let mut settings = default_server_settings();
    settings.enable_provider_update_checks = false;
    let text = sparse_settings_json(&settings).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value, json!({"enableProviderUpdateChecks": false}));
}

#[test]
fn atomic_keys_compare_as_whole_values() {
    let current =
        json!({"backgroundActivity": {"profile": "custom", "overrides": {"idleClientTtl": 1}}});
    let defaults = json!({"backgroundActivity": {"profile": "balanced", "overrides": {}}});
    assert_eq!(
        strip_default(&current, &defaults).unwrap(),
        current,
        "the whole atomic value is kept"
    );
}

#[test]
fn arrays_are_compared_whole() {
    assert_eq!(strip_default(&json!({"a": [1]}), &json!({"a": [1]})), None);
    assert_eq!(
        strip_default(&json!({"a": [1]}), &json!({"a": []})),
        Some(json!({"a": [1]}))
    );
}

#[test]
fn normalization_recomputes_legacy_project_maps() {
    let mut settings = default_server_settings();
    settings.project_settings_overrides = serde_json::from_value(json!({
        "p1": { "defaultAutoPull": true }
    }))
    .unwrap();
    let normalized = normalize_server_settings(settings).unwrap();
    let value = serde_json::to_value(&normalized).unwrap();
    assert_eq!(value["projectAutoPullOverrides"], json!({"p1": true}));
}

#[test]
fn a_disabled_fallback_provider_selects_another_enabled_default() {
    let mut settings = default_server_settings();
    settings.text_generation_model_selection = serde_json::from_value(json!({
        "instanceId": "codex_gone",
        "model": "gpt-5.4-mini",
    }))
    .unwrap();
    let resolved = resolve_text_generation_provider(settings);
    assert_ne!(
        resolved
            .text_generation_model_selection
            .instance_id
            .as_str(),
        "codex_gone"
    );
}

mod service {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use turin_contracts::settings::default_server_settings;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use turin_contracts::settings::ServerSettingsPatch;
    use turin_persistence::Db;
    use turin_provider_core::credential_store::{
        FileSecretStore, MemorySecretStore, SecretStore, SecretStoreError,
    };

    use super::super::service::ServerSettingsService;
    use crate::settings_secrets::{SECRET_REDACTED, source_control_host_secret_name};

    /// A store whose writes can be switched to fail, to exercise rollback.
    struct Flaky {
        inner: MemorySecretStore,
        fail_writes: AtomicBool,
    }

    #[async_trait]
    impl SecretStore for Flaky {
        async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            self.inner.get(name).await
        }
        async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(SecretStoreError::Persist {
                    resource: format!("secret {name}"),
                    detail: "injected".into(),
                });
            }
            self.inner.set(name, value).await
        }
        async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
            self.inner.remove(name).await
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        path: std::path::PathBuf,
        secrets: Arc<FileSecretStore>,
        db: Db,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("userdata").join("settings.json");
        let secrets = Arc::new(FileSecretStore::open(dir.path().join("secrets")).unwrap());
        let db = Db::open_configured(dir.path().join("userdata").join("statev2.sqlite"))
            .await
            .unwrap();
        Fixture {
            dir,
            path,
            secrets,
            db,
        }
    }

    fn patch(value: Value) -> ServerSettingsPatch {
        serde_json::from_value(value).unwrap()
    }

    fn read_file(path: &std::path::Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_missing_file_loads_defaults_without_writing() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let settings = service.get_settings().await.unwrap();
        assert_eq!(
            settings.enable_provider_update_checks,
            default_server_settings().enable_provider_update_checks
        );
        assert!(!fx.path.exists(), "defaults are not written on load");
    }

    #[tokio::test]
    async fn an_update_writes_only_the_changed_fields() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        service
            .update_settings(&patch(json!({ "enableProviderUpdateChecks": false })))
            .await
            .unwrap();
        assert_eq!(
            read_file(&fx.path),
            json!({ "enableProviderUpdateChecks": false })
        );
    }

    #[tokio::test]
    async fn a_saved_file_reloads_in_a_new_service() {
        let fx = fixture().await;
        let first = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = first.start().await.unwrap();
        first
            .update_settings(&patch(json!({ "enableProviderUpdateChecks": false })))
            .await
            .unwrap();
        drop(_watcher);

        let second = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = second.start().await.unwrap();
        assert!(
            !second
                .get_settings()
                .await
                .unwrap()
                .enable_provider_update_checks
        );
    }

    #[tokio::test]
    async fn a_token_streaming_mode_on_disk_loads_as_paragraph_and_is_rewritten() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::fs::write(&fx.path, r#"{"responseStreamingMode":"token"}"#).unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let settings = service.get_settings().await.unwrap();
        assert_eq!(
            serde_json::to_value(settings.response_streaming_mode).unwrap(),
            json!("paragraph")
        );
    }

    #[tokio::test]
    async fn an_unreadable_file_is_left_untouched_and_resumes_nothing() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        let original = "{ not json";
        std::fs::write(&fx.path, original).unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let settings = service.get_settings().await.unwrap();
        assert!(!settings.continue_threads_after_server_update);
        assert_eq!(std::fs::read_to_string(&fx.path).unwrap(), original);
    }

    #[tokio::test]
    async fn inline_credentials_move_to_the_store_and_come_back_on_read() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::fs::write(
            &fx.path,
            r#"{"sourceControlHosts":{"github":{"hosts":{},"tokens":{"github.com":"plain-token"}}}}"#,
        )
        .unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();

        let name = source_control_host_secret_name("github", "tokens", Some("github.com"));
        assert_eq!(
            fx.secrets.get(&name).await.unwrap().as_deref(),
            Some(&b"plain-token"[..])
        );
        let on_disk = read_file(&fx.path).to_string();
        assert!(!on_disk.contains("plain-token"));
        assert!(on_disk.contains(SECRET_REDACTED));

        let settings = service.get_settings().await.unwrap();
        let value = serde_json::to_value(&settings.source_control_hosts).unwrap();
        assert_eq!(
            value["github"]["tokens"]["github.com"],
            json!("plain-token")
        );
    }

    #[tokio::test]
    async fn legacy_provider_blobs_migrate_into_the_default_instance() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::fs::write(
            &fx.path,
            r#"{"providers":{"opencode":{"binaryPath":"/opt/oc"}}}"#,
        )
        .unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let settings = service.get_settings().await.unwrap();
        assert!(settings.provider_instances.contains_key(
            &turin_contracts::provider_instance::ProviderInstanceId::new("opencode").unwrap()
        ));
        assert!(!read_file(&fx.path).to_string().contains("providers"));
    }

    #[tokio::test]
    async fn a_failing_secret_write_rolls_back_and_leaves_the_file_alone() {
        let fx = fixture().await;
        let store = Arc::new(Flaky {
            inner: MemorySecretStore::new(),
            fail_writes: AtomicBool::new(false),
        });
        let service = ServerSettingsService::new(&fx.path, store.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        service
            .update_settings(&patch(json!({
                "sourceControlHosts": { "github": { "hosts": {}, "tokens": { "github.com": "t1" } } }
            })))
            .await
            .unwrap();
        let before = std::fs::read_to_string(&fx.path).ok();

        store.fail_writes.store(true, Ordering::SeqCst);
        let result = service
            .update_settings(&patch(json!({
                "sourceControlHosts": { "github": { "hosts": {}, "tokens": { "github.com": "t2" } } }
            })))
            .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&fx.path).ok(), before);
    }

    #[tokio::test]
    async fn creating_an_existing_provider_instance_is_refused() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let id = turin_contracts::provider_instance::ProviderInstanceId::new("codex_work").unwrap();
        let instance: turin_contracts::provider_instance::ProviderInstanceConfig =
            serde_json::from_value(json!({ "driver": "codex", "config": {} })).unwrap();
        let mutation = turin_contracts::provider_instance::ProviderInstanceMutation::Create {
            instance_id: id.clone(),
            instance: instance.clone(),
        };
        service
            .update_provider_instance(&mutation, None)
            .await
            .unwrap();
        assert!(
            service
                .update_provider_instance(&mutation, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subscribers_see_saved_changes() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let mut changes = service.subscribe();
        service
            .update_settings(&patch(json!({ "enableProviderUpdateChecks": false })))
            .await
            .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a change")
            .expect("open");
        assert!(!change.enable_provider_update_checks);
    }

    #[tokio::test]
    async fn an_external_edit_is_reloaded_by_the_watcher() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let mut changes = service.subscribe();
        let _watcher = service.start().await.unwrap();
        std::fs::write(&fx.path, r#"{"enableProviderUpdateChecks":false}"#).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let change = changes.recv().await.expect("open");
                if !change.enable_provider_update_checks {
                    return;
                }
            }
        })
        .await
        .expect("a reload after the external edit");
        let _ = fx.dir.path();
    }
}
