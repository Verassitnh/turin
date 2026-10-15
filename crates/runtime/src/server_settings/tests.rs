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

    use crate::server_settings::decode_settings_json;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use turin_contracts::provider_instance::{ProviderInstanceId, ProviderInstanceMutation};
    use turin_contracts::settings::resolve_provider_instance_enabled;
    use turin_contracts::settings::{ServerSettings, ServerSettingsError, ServerSettingsPatch};
    use turin_persistence::Db;
    use turin_provider_core::credential_store::{
        FileSecretStore, MemorySecretStore, SecretStore, SecretStoreError,
    };

    use super::super::service::ServerSettingsService;
    use crate::settings_secrets::{SECRET_REDACTED, source_control_host_secret_name};
    use crate::support::atomic_write::write_file_string_atomically;

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

    // Ports of upstream serverSettings.test.ts, lines 102-328 (batch 1).

    fn settings_value(settings: &ServerSettings) -> Value {
        serde_json::to_value(settings).unwrap()
    }

    #[tokio::test]
    async fn concurrent_model_preference_edits_serialize_without_cancelling_duplicate_adds() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let favorite = |model: &str| {
            patch(
                json!({"setModelFavorites": [{"provider": "codex_work", "model": model, "favorite": true}]}),
            )
        };
        let hidden = |model: &str, hidden: bool| {
            patch(
                json!({"setModelsHidden": [{"provider": "codex_work", "model": model, "hidden": hidden}]}),
            )
        };
        let order = patch(
            json!({"setProviderModelOrder": {"provider": "codex_work", "modelOrder": ["luna", "sol"]}}),
        );
        let fav_sol = favorite("sol");
        let fav_luna = favorite("luna");
        let hide_astra = hidden("astra", true);
        let hide_luna = hidden("luna", true);
        let (a, b, c, d, e, f) = tokio::join!(
            service.update_settings(&fav_sol),
            service.update_settings(&fav_sol),
            service.update_settings(&fav_luna),
            service.update_settings(&hide_astra),
            service.update_settings(&hide_luna),
            service.update_settings(&order),
        );
        for result in [a, b, c, d, e, f] {
            result.unwrap();
        }
        let value = settings_value(&service.get_settings().await.unwrap());
        let mut models: Vec<&str> = value["favorites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["model"].as_str().unwrap())
            .collect();
        models.sort();
        assert_eq!(models, ["luna", "sol"]);
        let prefs = &value["providerModelPreferences"]["codex_work"];
        let mut hidden_models: Vec<&str> = prefs["hiddenModels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap())
            .collect();
        hidden_models.sort();
        assert_eq!(hidden_models, ["astra", "luna"]);
        assert_eq!(prefs["modelOrder"], json!(["luna", "sol"]));
    }

    #[tokio::test]
    async fn model_preferences_persist_and_broadcast_and_a_migration_cannot_undo_a_clear() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let prefs = json!({"codex_work": {"hiddenModels": ["astra"], "modelOrder": ["sol"]}});
        let saved_patch = patch(json!({
            "favorites": [{"provider": "codex_work", "model": "sol"}],
            "providerModelPreferences": prefs,
        }));
        let mut changes = service.subscribe();
        service.update_settings(&saved_patch).await.unwrap();
        let received = settings_value(
            &tokio::time::timeout(Duration::from_secs(5), changes.recv())
                .await
                .expect("a change")
                .expect("open"),
        );
        assert_eq!(
            received["favorites"],
            json!([{"provider": "codex_work", "model": "sol"}])
        );
        assert_eq!(received["providerModelPreferences"], prefs);
        let saved = read_file(&fx.path);
        assert_eq!(
            saved["favorites"],
            json!([{"provider": "codex_work", "model": "sol"}])
        );
        assert_eq!(saved["providerModelPreferences"], prefs);

        let hid = service
            .update_settings(&patch(json!({"setModelsHidden": [{"provider": "codex_work", "model": "sol", "hidden": true}]})))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&hid)["providerModelPreferences"]["codex_work"],
            json!({"hiddenModels": ["astra", "sol"], "modelOrder": ["sol"]})
        );
        let shown = service
            .update_settings(&patch(json!({"setModelsHidden": [{"provider": "codex_work", "model": "astra", "hidden": false}]})))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&shown)["providerModelPreferences"]["codex_work"],
            json!({"hiddenModels": ["sol"], "modelOrder": ["sol"]})
        );
        assert_eq!(
            read_file(&fx.path)["providerModelPreferences"],
            settings_value(&shown)["providerModelPreferences"]
        );

        service
            .update_settings(&patch(
                json!({"favorites": [], "providerModelPreferences": {}}),
            ))
            .await
            .unwrap();
        // A delayed seed from an older device cannot undo the clear.
        let migrated = service
            .update_settings(&patch(json!({"migrateModelPreferences": {
                "favorites": [{"provider": "codex_work", "model": "sol"}],
                "providerModelPreferences": prefs,
            }})))
            .await
            .unwrap();
        assert_eq!(settings_value(&migrated)["favorites"], json!([]));
        assert_eq!(
            settings_value(&migrated)["providerModelPreferences"],
            json!({})
        );
    }

    #[tokio::test]
    async fn a_saved_token_streaming_mode_migrates_per_project_without_resetting_other_settings() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::fs::write(
            &fx.path,
            r#"{
              "responseStreamingMode": "token",
              "enableAgentBrowserAccess": false,
              "projectSettingsOverrides": {
                "legacy": { "responseStreamingMode": "token", "defaultAutoPull": true },
                "buffered": { "responseStreamingMode": "turn" },
                "inherited": { "defaultAutoPull": false }
              }
            }"#,
        )
        .unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = service.get_settings().await.unwrap();
        assert_eq!(
            settings_value(&settings)["responseStreamingMode"],
            json!("paragraph")
        );
        assert_eq!(
            settings_value(&settings)["enableAgentBrowserAccess"],
            json!(false)
        );
        let overrides = settings_value(&settings)["projectSettingsOverrides"].clone();
        assert_eq!(
            overrides,
            json!({
                "legacy": { "responseStreamingMode": "paragraph", "defaultAutoPull": true },
                "buffered": { "responseStreamingMode": "turn" },
                "inherited": { "defaultAutoPull": false },
            })
        );

        service
            .update_settings(&patch(json!({"responseStreamingMode": "turn"})))
            .await
            .unwrap();
        let persisted: ServerSettings = serde_json::from_value(read_file(&fx.path)).unwrap();
        assert_eq!(
            settings_value(&persisted)["responseStreamingMode"],
            json!("turn")
        );
        assert_eq!(
            settings_value(&persisted)["projectSettingsOverrides"],
            overrides
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_save_through_a_symlinked_settings_file_keeps_the_link() {
        let fx = fixture().await;
        let dotfiles = fx.dir.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let linked = dotfiles.join("settings.json");
        std::fs::write(&linked, r#"{ "responseStreamingMode": "turn" }"#).unwrap();
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&linked, &fx.path).unwrap();

        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        service
            .update_settings(&patch(json!({"responseStreamingMode": "paragraph"})))
            .await
            .unwrap();

        assert_eq!(std::fs::read_link(&fx.path).unwrap(), linked);
        let persisted: ServerSettings = serde_json::from_value(read_file(&linked)).unwrap();
        assert_eq!(
            settings_value(&persisted)["responseStreamingMode"],
            json!("paragraph")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_settings_file_reloads_when_its_destination_changes() {
        let fx = fixture().await;
        let dotfiles = fx.dir.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let linked = dotfiles.join("settings.json");
        std::fs::write(&linked, r#"{ "responseStreamingMode": "turn" }"#).unwrap();
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&linked, &fx.path).unwrap();

        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let mut changes = service.subscribe();
        write_file_string_atomically(&linked, r#"{ "responseStreamingMode": "paragraph" }"#)
            .unwrap();

        let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a reload")
            .expect("open");
        assert_eq!(
            settings_value(&change)["responseStreamingMode"],
            json!("paragraph")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_settings_link_that_is_repointed_is_followed() {
        let fx = fixture().await;
        let dotfiles = fx.dir.path().join("dotfiles");
        let first = dotfiles.join("first").join("settings.json");
        let second = dotfiles.join("second").join("settings.json");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(&first, r#"{ "responseStreamingMode": "turn" }"#).unwrap();
        std::fs::write(&second, r#"{ "responseStreamingMode": "paragraph" }"#).unwrap();
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&first, &fx.path).unwrap();

        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let mut repointed = service.subscribe();
        std::fs::remove_file(&fx.path).unwrap();
        std::os::unix::fs::symlink(&second, &fx.path).unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), repointed.recv())
            .await
            .expect("a reload after repoint")
            .expect("open");
        assert_eq!(
            settings_value(&change)["responseStreamingMode"],
            json!("paragraph")
        );

        let mut edited = service.subscribe();
        write_file_string_atomically(&second, r#"{ "responseStreamingMode": "turn" }"#).unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), edited.recv())
            .await
            .expect("a reload after an edit to the new target")
            .expect("open");
        assert_eq!(
            settings_value(&change)["responseStreamingMode"],
            json!("turn")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_settings_link_reloads_when_its_destination_appears() {
        let fx = fixture().await;
        let dotfiles = fx.dir.path().join("dotfiles");
        let target = dotfiles.join("not-yet").join("settings.json");
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &fx.path).unwrap();

        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let _watcher = service.start().await.unwrap();
        let mut changes = service.subscribe();
        write_file_string_atomically(&target, r#"{ "responseStreamingMode": "turn" }"#).unwrap();

        let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a reload")
            .expect("open");
        assert_eq!(
            settings_value(&change)["responseStreamingMode"],
            json!("turn")
        );
    }

    // Ports of upstream serverSettings.test.ts, lines 328-567 (batch 2).

    /// A store whose reads always fail, to surface a read-secret error.
    struct ReadFailing;

    #[async_trait]
    impl SecretStore for ReadFailing {
        async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            Err(SecretStoreError::Read {
                resource: format!("secret {name}"),
                detail: "secret backend unavailable".into(),
            })
        }
        async fn set(&self, _name: &str, _value: &[u8]) -> Result<(), SecretStoreError> {
            Ok(())
        }
        async fn remove(&self, _name: &str) -> Result<(), SecretStoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_failed_environment_secret_read_names_the_variable_and_hides_the_cause() {
        let fx = fixture().await;
        std::fs::create_dir_all(fx.path.parent().unwrap()).unwrap();
        std::fs::write(
            &fx.path,
            r#"{"providerInstances":{"codex_personal":{"driver":"codex","environment":[{"name":"OPENROUTER_API_KEY","value":"","sensitive":true,"valueRedacted":true}],"config":{}}}}"#,
        )
        .unwrap();
        let service = ServerSettingsService::new(&fx.path, Arc::new(ReadFailing), fx.db.clone());
        let error = service.get_settings().await.unwrap_err();
        let ServerSettingsError::ServerSettingsError {
            operation,
            provider_instance_id,
            environment_variable,
            ..
        } = &error;
        assert_eq!(operation.as_str(), "read-secret");
        assert_eq!(provider_instance_id.as_deref(), Some("codex_personal"));
        assert_eq!(environment_variable.as_deref(), Some("OPENROUTER_API_KEY"));
        assert!(!error.to_string().contains("secret backend unavailable"));
    }

    #[tokio::test]
    async fn a_provider_history_query_failure_is_identified_as_such() {
        let fx = fixture().await;
        fx.db
            .write(|conn| {
                conn.execute_batch("DROP TABLE projection_thread_sessions")
                    .map_err(|err| {
                        turin_persistence::PersistenceError::sql("test.dropSessions", &err)
                    })
            })
            .await
            .unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let error = service.get_settings().await.unwrap_err();
        let ServerSettingsError::ServerSettingsError {
            operation,
            settings_path,
            ..
        } = &error;
        assert_eq!(operation.as_str(), "read-provider-history");
        assert_eq!(settings_path, &fx.path.display().to_string());
    }

    #[tokio::test]
    async fn a_failed_settings_read_is_retried_rather_than_kept() {
        let fx = fixture().await;
        std::fs::create_dir_all(&fx.path).unwrap();
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let error = service.get_settings().await.unwrap_err();
        let ServerSettingsError::ServerSettingsError { operation, .. } = &error;
        assert_eq!(operation.as_str(), "read-file");

        std::fs::remove_dir_all(&fx.path).unwrap();
        std::fs::write(&fx.path, r#"{ "responseStreamingMode": "turn" }"#).unwrap();
        let settings = service.get_settings().await.unwrap();
        assert_eq!(
            settings_value(&settings)["responseStreamingMode"],
            json!("turn")
        );
    }

    #[test]
    fn a_nested_settings_patch_decodes_to_its_own_shape() {
        let raw = json!({"textGenerationModelSelection": {"options": [{"id": "fastMode", "value": false}]}});
        let decoded: ServerSettingsPatch = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), raw);
    }

    #[test]
    fn a_legacy_object_shaped_selection_decodes_to_an_option_list() {
        let settings = decode_settings_json(
            r#"{"textGenerationModelSelection":{"provider":"codex","model":"gpt-5.4-mini","options":{"reasoningEffort":"low"}}}"#,
        )
        .unwrap();
        assert_eq!(
            settings_value(&settings)["textGenerationModelSelection"],
            json!({
                "instanceId": "codex",
                "model": "gpt-5.4-mini",
                "options": [{"id": "reasoningEffort", "value": "low"}],
            })
        );
    }

    #[tokio::test]
    async fn nested_updates_merge_without_dropping_siblings() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let model = default_server_settings()
            .text_generation_model_selection
            .model
            .to_string();
        service
            .update_settings(&patch(json!({
                "observability": {"otlpTracesUrl": "http://localhost:4318/v1/traces"},
                "textGenerationModelSelection": {
                    "instanceId": "codex",
                    "model": model,
                    "options": [
                        {"id": "reasoningEffort", "value": "high"},
                        {"id": "fastMode", "value": true},
                    ],
                },
            })))
            .await
            .unwrap();
        let next = service
            .update_settings(&patch(json!({
                "observability": {"otlpMetricsUrl": "http://localhost:4318/v1/metrics"},
                "textGenerationModelSelection": {"options": [{"id": "fastMode", "value": false}]},
            })))
            .await
            .unwrap();
        let value = settings_value(&next);
        assert_eq!(
            value["observability"]["otlpTracesUrl"],
            json!("http://localhost:4318/v1/traces")
        );
        assert_eq!(
            value["observability"]["otlpMetricsUrl"],
            json!("http://localhost:4318/v1/metrics")
        );
        assert_eq!(
            value["textGenerationModelSelection"],
            json!({
                "instanceId": "codex",
                "model": model,
                "options": [
                    {"id": "reasoningEffort", "value": "high"},
                    {"id": "fastMode", "value": false},
                ],
            })
        );
    }

    #[tokio::test]
    async fn a_concurrent_provider_instance_create_is_refused_for_the_loser() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let instance_id =
            turin_contracts::provider_instance::ProviderInstanceId::new("acpRegistry_shared")
                .unwrap();
        let create = |name: &str| {
            serde_json::from_value::<ProviderInstanceMutation>(json!({
                "operation": "create",
                "instanceId": "acpRegistry_shared",
                "instance": {
                    "driver": "acpRegistry",
                    "displayName": name,
                    "config": {"agentId": "shared", "distribution": "auto"},
                },
            }))
            .unwrap()
        };
        let (first, second) = (create("First"), create("Second"));
        let (a, b) = tokio::join!(
            service.update_provider_instance(&first, None),
            service.update_provider_instance(&second, None),
        );
        assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1);
        let settings = service.get_settings().await.unwrap();
        let name = settings.provider_instances[&instance_id]
            .display_name
            .as_ref()
            .map(|value| value.to_string());
        assert!(name.as_deref() == Some("First") || name.as_deref() == Some("Second"));
    }

    #[tokio::test]
    async fn provider_instance_mutations_wait_while_a_settings_snapshot_is_in_use() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let snapshot = {
            let service = Arc::clone(&service);
            tokio::spawn(async move {
                service
                    .with_settings_snapshot(|_settings| async move {
                        let _ = entered_tx.send(());
                        let _ = release_rx.await;
                    })
                    .await
            })
        };
        entered_rx.await.unwrap();

        let mutation = serde_json::from_value::<ProviderInstanceMutation>(json!({
            "operation": "upsert",
            "instanceId": "acpRegistry_kilo",
            "instance": {
                "driver": "acpRegistry",
                "displayName": "Kilo",
                "config": {"agentId": "kilo", "distribution": "auto"},
            },
        }))
        .unwrap();
        let writer = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.update_provider_instance(&mutation, None).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!writer.is_finished(), "a mutation waits for the snapshot");

        release_tx.send(()).unwrap();
        snapshot.await.unwrap().unwrap();
        writer.await.unwrap().unwrap();
        let settings = service.get_settings().await.unwrap();
        let instance_id =
            turin_contracts::provider_instance::ProviderInstanceId::new("acpRegistry_kilo")
                .unwrap();
        let name = settings.provider_instances[&instance_id]
            .display_name
            .as_ref()
            .map(|value| value.to_string());
        assert_eq!(name.as_deref(), Some("Kilo"));
    }

    // Ports of upstream serverSettings.test.ts, lines 567-800 (batch 3).

    #[tokio::test]
    async fn a_subscriber_receives_changes_made_before_it_reads() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let mut changes = service.subscribe();
        service
            .update_settings(&patch(json!({"addProjectBaseDirectory": "~/next"})))
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(1), changes.recv())
            .await
            .expect("a buffered change")
            .expect("open");
        assert_eq!(
            settings_value(&first)["addProjectBaseDirectory"],
            json!("~/next")
        );
    }

    #[tokio::test]
    async fn custom_usage_prices_persist_and_are_removed_on_null() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let prices = json!({
            "inputCostPerMillionTokens": 2,
            "outputCostPerMillionTokens": 8,
            "cacheReadCostPerMillionTokens": 0,
        });
        service
            .update_settings(&patch(
                json!({"usagePriceOverrides": {"example-model": prices}}),
            ))
            .await
            .unwrap();
        let persisted =
            settings_value(&serde_json::from_value::<ServerSettings>(read_file(&fx.path)).unwrap());
        let saved = &persisted["usagePriceOverrides"]["example-model"];
        assert_eq!(saved["inputCostPerMillionTokens"].as_f64(), Some(2.0));
        assert_eq!(saved["outputCostPerMillionTokens"].as_f64(), Some(8.0));
        assert_eq!(saved["cacheReadCostPerMillionTokens"].as_f64(), Some(0.0));

        service
            .update_settings(&patch(
                json!({"usagePriceOverrides": {"example-model": null}}),
            ))
            .await
            .unwrap();
        let restored =
            settings_value(&serde_json::from_value::<ServerSettings>(read_file(&fx.path)).unwrap());
        assert_eq!(restored["usagePriceOverrides"], json!({}));
    }

    #[tokio::test]
    async fn thread_settlement_settings_persist_and_broadcast_including_null() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let mut changes = service.subscribe();
        let next = service
            .update_settings(&patch(json!({
                "sidebarAutoSettleAfterDays": null,
                "sidebarAutoSettleOnMerge": false,
            })))
            .await
            .unwrap();
        let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a change")
            .expect("open");
        // The raw file, before any decode can apply defaults.
        let raw: Value = read_file(&fx.path);
        for value in [settings_value(&next), settings_value(&change), raw] {
            assert!(value["sidebarAutoSettleAfterDays"].is_null());
            assert_eq!(value["sidebarAutoSettleOnMerge"], json!(false));
        }
    }

    #[tokio::test]
    async fn a_text_generation_switch_between_providers_keeps_the_new_options() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        service
            .update_settings(&patch(json!({"textGenerationModelSelection": {
                "instanceId": "claudeAgent",
                "model": "claude-sonnet-4-6",
                "options": [{"id": "effort", "value": "high"}],
            }})))
            .await
            .unwrap();
        // The stale Claude option must not make the switch lose the selected model.
        let next = service
            .update_settings(&patch(json!({"textGenerationModelSelection": {
                "instanceId": "codex",
                "model": "gpt-5.4",
                "options": [{"id": "reasoningEffort", "value": "high"}],
            }})))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            json!({
                "instanceId": "codex",
                "model": "gpt-5.4",
                "options": [{"id": "reasoningEffort", "value": "high"}],
            })
        );
    }

    #[tokio::test]
    async fn a_custom_instance_text_generation_selection_is_preserved() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {
                    "claude_openrouter": {
                        "driver": "claudeAgent",
                        "enabled": true,
                        "config": {"customModels": ["openai/gpt-5.5"]},
                    },
                },
                "textGenerationModelSelection": {"instanceId": "claude_openrouter", "model": "openai/gpt-5.5"},
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            json!({"instanceId": "claude_openrouter", "model": "openai/gpt-5.5"})
        );
    }

    #[tokio::test]
    async fn a_custom_instance_selection_survives_its_drivers_default_being_disabled() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {
                    "claudeAgent": {"driver": "claudeAgent", "enabled": false},
                    "claude_openrouter": {
                        "driver": "claudeAgent",
                        "enabled": true,
                        "config": {"customModels": ["openai/gpt-5.5"]},
                    },
                },
                "textGenerationModelSelection": {"instanceId": "claude_openrouter", "model": "openai/gpt-5.5"},
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            json!({"instanceId": "claude_openrouter", "model": "openai/gpt-5.5"})
        );
    }

    #[tokio::test]
    async fn disabling_the_writer_falls_back_to_an_enabled_driver_instance_and_restores() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let providers = json!({
            "claudeAgent": {"driver": "claudeAgent", "enabled": false},
            "codex": {"driver": "codex", "enabled": true, "config": {}},
            "writer": {"driver": "claudeAgent", "enabled": true, "config": {}},
        });
        let selection = json!({"instanceId": "writer", "model": "claude-sonnet-4-6"});
        service
            .update_settings(&patch(json!({
                "providerInstances": providers,
                "textGenerationModelSelection": selection,
            })))
            .await
            .unwrap();
        let mut changes = service.subscribe();

        let mut disabled = providers.clone();
        disabled["writer"]["enabled"] = json!(false);
        let next = service
            .update_settings(&patch(json!({"providerInstances": disabled})))
            .await
            .unwrap();
        let model = default_server_settings()
            .text_generation_model_selection
            .model
            .to_string();
        let fallback = json!({"instanceId": "codex", "model": model});
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            fallback
        );
        assert_eq!(
            settings_value(&service.get_settings().await.unwrap())["textGenerationModelSelection"],
            fallback
        );
        let change = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a change")
            .expect("open");
        assert_eq!(
            settings_value(&change)["textGenerationModelSelection"],
            fallback
        );

        // The file keeps the user's choice; only the returned view falls back.
        let persisted =
            settings_value(&serde_json::from_value::<ServerSettings>(read_file(&fx.path)).unwrap());
        assert_eq!(persisted["textGenerationModelSelection"], selection);

        let restored = service
            .update_settings(&patch(json!({"providerInstances": providers})))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&restored)["textGenerationModelSelection"],
            selection
        );
    }

    // Ports of upstream serverSettings.test.ts, lines 798-1015 (batch 4).

    /// Stands in for upstream `undefined` provider statuses: every call here passes `None`.
    struct NoProviderStatus;

    impl turin_shared::server_settings::ProviderStatus for NoProviderStatus {
        fn instance_id(&self) -> &turin_contracts::provider_instance::ProviderInstanceId {
            unreachable!("no provider statuses are passed")
        }
        fn enabled(&self) -> bool {
            unreachable!("no provider statuses are passed")
        }
        fn is_available(&self) -> bool {
            unreachable!("no provider statuses are passed")
        }
        fn is_text_generation_capable(&self) -> bool {
            unreachable!("no provider statuses are passed")
        }
    }

    #[tokio::test]
    async fn a_disabled_default_instance_is_skipped_when_choosing_a_fallback() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {"codex": {"driver": "codex", "enabled": false, "config": {}}},
            })))
            .await
            .unwrap();
        let selection = &settings_value(&next)["textGenerationModelSelection"];
        assert_eq!(selection["instanceId"], json!("claudeAgent"));
        assert_eq!(selection["model"], json!("claude-haiku-5-5"));
    }

    #[tokio::test]
    async fn a_custom_model_thread_default_is_used_for_the_fallback_but_a_built_in_one_is_not() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let providers = json!({
            "codex": {"driver": "codex", "enabled": false, "config": {}},
            "claudeAgent": {"driver": "claudeAgent", "config": {"customModels": ["z-ai/glm-5.3-flash"]}},
        });
        let custom = service
            .update_settings(&patch(json!({
                "providerInstances": providers,
                "defaultModelSelection": {
                    "instanceId": "claudeAgent",
                    "model": "z-ai/glm-5.3-flash",
                    "options": [{"id": "thinking", "value": false}],
                },
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&custom)["textGenerationModelSelection"],
            json!({
                "instanceId": "claudeAgent",
                "model": "z-ai/glm-5.3-flash",
                "options": [{"id": "thinking", "value": false}],
            })
        );

        let built_in = service
            .update_settings(&patch(json!({
                "defaultModelSelection": {"instanceId": "claudeAgent", "model": "claude-fable-5-1"},
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&built_in)["textGenerationModelSelection"]["model"],
            json!("claude-haiku-5-5")
        );
    }

    #[tokio::test]
    async fn a_non_built_in_driver_keeps_its_enabled_text_generation_selection() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {
                    "openrouter_text": {
                        "driver": "openrouter",
                        "enabled": true,
                        "config": {"customModels": ["openai/gpt-5.5"]},
                    },
                },
                "textGenerationModelSelection": {"instanceId": "openrouter_text", "model": "openai/gpt-5.5"},
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            json!({"instanceId": "openrouter_text", "model": "openai/gpt-5.5"})
        );
    }

    #[tokio::test]
    async fn the_source_control_writer_selection_survives_its_instance_being_disabled() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let selection = json!({"instanceId": "codex_writer", "model": "gpt-5.4-mini"});
        service
            .update_settings(&patch(json!({
                "providerInstances": {"codex_writer": {"driver": "codex", "enabled": true, "config": {}}},
                "sourceControlWriterModelSelection": selection,
            })))
            .await
            .unwrap();
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {"codex_writer": {"driver": "codex", "enabled": false, "config": {}}},
            })))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["sourceControlWriterModelSelection"],
            selection
        );
        assert_eq!(
            turin_shared::server_settings::resolve_source_control_writer_model_selection(
                &next,
                None::<&[NoProviderStatus]>,
                None
            ),
            next.text_generation_model_selection
        );
        assert_eq!(
            settings_value(&service.get_settings().await.unwrap())["sourceControlWriterModelSelection"],
            selection
        );
        assert_eq!(
            read_file(&fx.path)["sourceControlWriterModelSelection"],
            selection
        );

        let restored = service
            .update_settings(&patch(json!({
                "providerInstances": {"codex_writer": {"driver": "codex", "enabled": true, "config": {}}},
            })))
            .await
            .unwrap();
        assert_eq!(
            turin_shared::server_settings::resolve_source_control_writer_model_selection(
                &restored,
                None::<&[NoProviderStatus]>,
                None
            ),
            serde_json::from_value(selection).unwrap()
        );
    }

    #[tokio::test]
    async fn resetting_the_selection_drops_the_stale_options() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let model = default_server_settings()
            .text_generation_model_selection
            .model
            .to_string();
        let instance = default_server_settings()
            .text_generation_model_selection
            .instance_id
            .to_string();
        service
            .update_settings(&patch(json!({"textGenerationModelSelection": {
                "instanceId": "codex",
                "model": model,
                "options": [
                    {"id": "reasoningEffort", "value": "high"},
                    {"id": "fastMode", "value": true},
                ],
            }})))
            .await
            .unwrap();
        let next = service
            .update_settings(&patch(json!({"textGenerationModelSelection": {
                "instanceId": instance,
                "model": model,
            }})))
            .await
            .unwrap();
        assert_eq!(
            settings_value(&next)["textGenerationModelSelection"],
            json!({"instanceId": instance, "model": model})
        );
    }

    #[tokio::test]
    async fn an_instance_map_entry_replaces_its_old_fields_so_omitted_ones_clear() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        service
            .update_settings(&patch(json!({"providerInstances": {"codex": {
                "driver": "codex",
                "displayName": "Codex Work",
                "accentColor": "#7c3aed",
                "enabled": true,
                "config": {"homePath": "~/.codex"},
            }}})))
            .await
            .unwrap();
        let next = service
            .update_settings(&patch(json!({"providerInstances": {"codex": {
                "driver": "codex",
                "displayName": "Codex Work",
                "enabled": true,
                "config": {"homePath": "~/.codex"},
            }}})))
            .await
            .unwrap();
        let instance = &settings_value(&next)["providerInstances"]["codex"];
        assert_eq!(instance["displayName"], json!("Codex Work"));
        assert_eq!(instance["enabled"], json!(true));
        assert_eq!(instance["config"], json!({"homePath": "~/.codex"}));
        assert!(instance.get("accentColor").map_or(true, Value::is_null));
    }

    // Ports of upstream serverSettings.test.ts, lines 1016-1253 (batch 5).

    /// Upstream `recordProviderUsage`: a thread session row, as provider history reads it.
    async fn record_provider_usage(db: &Db, provider: &str, instance: Option<&str>) {
        let thread_id = format!("thread-{}", instance.unwrap_or(provider));
        let provider = provider.to_owned();
        let instance = instance.map(str::to_owned);
        db.write(move |tx| {
            tx.execute(
                "INSERT INTO projection_thread_sessions (thread_id, status, provider_name, provider_instance_id, updated_at)
                 VALUES (?1, 'ready', ?2, ?3, '2026-08-25T00:00:00.000Z')",
                rusqlite::params![thread_id, provider, instance],
            )
            .map_err(|err| turin_persistence::PersistenceError::sql("test.recordProviderUsage", &err))?;
            Ok(())
        })
        .await
        .unwrap();
    }

    fn write_raw_settings(path: &std::path::Path, raw: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, raw).unwrap();
    }

    #[tokio::test]
    async fn legacy_provider_blobs_move_into_default_instances_on_load_and_the_file_is_rewritten_once()
     {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            &json!({
                "addProjectBaseDirectory": "~/Development",
                "providers": {
                    "codex": {"binaryPath": "/opt/guard/bin/codex"},
                    "claudeAgent": {"enabled": false},
                    "cursor": {"enabled": false},
                    "grok": {"enabled": false},
                    "opencode": {"enabled": false},
                },
            })
            .to_string(),
        );
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        assert_eq!(
            settings["providerInstances"],
            json!({
                "codex": {"driver": "codex", "config": {"binaryPath": "/opt/guard/bin/codex"}},
                "claudeAgent": {"driver": "claudeAgent", "enabled": false, "config": {}},
            })
        );
        let persisted = read_file(&fx.path);
        assert!(persisted.get("providers").is_none());
        assert_eq!(persisted["addProjectBaseDirectory"], json!("~/Development"));
        assert_eq!(
            persisted["providerInstances"]["codex"],
            json!({"driver": "codex", "config": {"binaryPath": "/opt/guard/bin/codex"}})
        );
    }

    #[tokio::test]
    async fn an_explicit_default_instance_beats_the_legacy_blob_for_its_driver() {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            &json!({
                "providers": {"codex": {"binaryPath": "/legacy/codex"}},
                "providerInstances": {
                    "codex": {"driver": "codex", "enabled": true, "config": {"binaryPath": "/explicit/codex"}},
                },
            })
            .to_string(),
        );
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        assert_eq!(
            settings["providerInstances"]["codex"],
            json!({"driver": "codex", "enabled": true, "config": {"binaryPath": "/explicit/codex"}})
        );
    }

    #[tokio::test]
    async fn previously_used_optional_providers_are_enabled_while_migrating() {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            &json!({
                "providers": {"opencode": {"serverUrl": "http://127.0.0.1:4096"}, "cursor": {}},
                "providerInstances": {
                    "cursor_work": {"driver": "cursor", "config": {}},
                    "opencode_unused": {"driver": "opencode", "config": {}},
                },
            })
            .to_string(),
        );
        record_provider_usage(&fx.db, "opencode", None).await;
        record_provider_usage(&fx.db, "grok", None).await;
        record_provider_usage(&fx.db, "cursor", Some("cursor_work")).await;

        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = service.get_settings().await.unwrap();
        let instances = settings_value(&settings)["providerInstances"].clone();
        assert_eq!(
            instances["opencode"],
            json!({"driver": "opencode", "enabled": true, "config": {"serverUrl": "http://127.0.0.1:4096"}})
        );
        // Used without any legacy blob or instance: the slot is created enabled.
        assert_eq!(instances["grok"]["enabled"], json!(true));
        assert_eq!(instances["cursor_work"]["enabled"], json!(true));
        // Using any cursor instance counts as opting into the driver.
        assert_eq!(instances["cursor"]["enabled"], json!(true));
        let unused =
            &settings.provider_instances[&ProviderInstanceId::new("opencode_unused").unwrap()];
        assert!(!resolve_provider_instance_enabled(unused));
    }

    #[tokio::test]
    async fn an_explicit_legacy_disable_survives_provider_history_showing_use() {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            &json!({"providers": {"grok": {"enabled": false, "binaryPath": "/opt/grok"}}})
                .to_string(),
        );
        record_provider_usage(&fx.db, "grok", None).await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        assert_eq!(
            settings["providerInstances"]["grok"],
            json!({"driver": "grok", "enabled": false, "config": {"binaryPath": "/opt/grok"}})
        );
    }

    #[tokio::test]
    async fn the_legacy_map_gates_provider_migration_so_a_file_without_it_gets_no_instances() {
        let fx = fixture().await;
        write_raw_settings(&fx.path, "{}");
        record_provider_usage(&fx.db, "grok", None).await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        assert_eq!(settings["providerInstances"], json!({}));
    }

    #[tokio::test]
    async fn legacy_providers_migrate_from_an_invalid_file_without_rewriting_it() {
        let fx = fixture().await;
        let raw = r#"{"addProjectBaseDirectory":42,"continueThreadsAfterServerUpdate":"false","providers":{"codex":{"binaryPath":"/x"}}}"#;
        write_raw_settings(&fx.path, raw);
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        // The readable provider settings still apply, but the file stays for the user to repair.
        assert_eq!(
            settings["providerInstances"]["codex"],
            json!({"driver": "codex", "config": {"binaryPath": "/x"}})
        );
        // An unreadable file may hold an opt-out, so threads are not resumed on a guess.
        assert_eq!(settings["continueThreadsAfterServerUpdate"], json!(false));
        assert_eq!(std::fs::read_to_string(&fx.path).unwrap(), raw);
    }

    #[tokio::test]
    async fn a_disabled_provider_instance_is_skipped_for_the_text_generation_fallback() {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            r#"{"providerInstances":{"codex":{"driver":"codex","enabled":false,"config":{}}}}"#,
        );
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = service.get_settings().await.unwrap();
        assert_eq!(
            settings
                .text_generation_model_selection
                .instance_id
                .as_str(),
            "claudeAgent"
        );
    }

    #[tokio::test]
    async fn a_default_off_providers_new_instance_stays_disabled_and_sparse_on_disk() {
        let fx = fixture().await;
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let next = service
            .update_settings(&patch(json!({
                "providerInstances": {"grok": {"driver": "grok", "config": {}}},
            })))
            .await
            .unwrap();
        let grok = &next.provider_instances[&ProviderInstanceId::new("grok").unwrap()];
        assert!(!resolve_provider_instance_enabled(grok));
        let persisted = read_file(&fx.path);
        assert!(persisted.get("providers").is_none());
        assert!(
            persisted["providerInstances"]["grok"]
                .get("enabled")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_legacy_in_config_enabled_flag_folds_into_the_instance_on_load() {
        let fx = fixture().await;
        write_raw_settings(
            &fx.path,
            r#"{"providerInstances":{"grok":{"driver":"grok","enabled":true,"config":{"enabled":false}},"codex_work":{"driver":"codex","config":{"enabled":true,"homePath":"~/.codex"}},"cursor":{"driver":"cursor","config":{"enabled":"nope"}}}}"#,
        );
        let service = ServerSettingsService::new(&fx.path, fx.secrets.clone(), fx.db.clone());
        let settings = settings_value(&service.get_settings().await.unwrap());
        // An explicit false on either side wins, so a user's disable sticks.
        assert_eq!(
            settings["providerInstances"]["grok"],
            json!({"driver": "grok", "enabled": false, "config": {}})
        );
        // A lone in-config flag is lifted to the envelope and stripped.
        assert_eq!(
            settings["providerInstances"]["codex_work"],
            json!({"driver": "codex", "enabled": true, "config": {"homePath": "~/.codex"}})
        );
        // A malformed flag is left alone so driver schema validation can surface it.
        assert_eq!(
            settings["providerInstances"]["cursor"],
            json!({"driver": "cursor", "config": {"enabled": "nope"}})
        );
    }
}
