//! Legacy settings migrations from upstream/apps/server/src/serverSettings.ts:
//! the retired `providers.<kind>` blobs, the retired `bitbucket` and `github`
//! source-control keys, and the one-time fold of per-project fields.
//!
//! These run on the settings JSON, before typed decoding, the same way upstream
//! runs them on decoded objects. Each function is a no-op on a file that does
//! not carry the retired shape.

use std::collections::HashSet;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::project::ProjectScript;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, default_instance_id_for_driver,
    is_provider_driver_kind,
};
use turin_contracts::settings::resolve_provider_instance_enabled;
use turin_shared::server_settings::derive_legacy_project_overrides;

use crate::persistence_reader::{LegacyProjectSettingsRow, ProviderHistoryRow};
use crate::settings_secrets::{
    SECRET_REDACTED, read_secret_field_values, source_control_host_secret_name,
};

type Object = Map<String, Value>;

/// Drivers that started disabled, so a session in history means the user turned
/// them on before the instance kept an explicit flag.
const HISTORY_RESTORED_DRIVERS: [&str; 3] = ["cursor", "grok", "opencode"];

/// A store key that a legacy secret must be copied from, and the key it moves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMove {
    pub from: String,
    pub to: String,
}

/// The retired `bitbucket` and `github` keys, read from the raw file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LegacySourceControl {
    pub bitbucket: Option<Object>,
    pub github: Option<Object>,
}

/// Secret store keys the retired bitbucket tokens were kept under.
const LEGACY_BITBUCKET_SECRET_NAMES: [(&str, &str); 2] = [
    ("accessToken", "bitbucket-access-token"),
    ("apiToken", "bitbucket-api-token"),
];

/// `legacyGitHubTokenSecretName`.
fn legacy_github_token_secret_name(host: &str) -> String {
    format!(
        "github-token-{}",
        URL_SAFE_NO_PAD.encode(host.trim().to_lowercase().as_bytes())
    )
}

fn object_mut(value: &mut Value) -> &mut Object {
    if !value.is_object() {
        *value = Value::Object(Object::new());
    }
    value.as_object_mut().expect("object")
}

/// `migrateLegacySourceControlSettings`. Folds the retired keys into
/// `sourceControlHosts` unless the host is already there, and returns the
/// secret-store copies to make.
pub fn migrate_legacy_source_control_settings(
    settings: &mut Value,
    legacy: &LegacySourceControl,
) -> Vec<SecretMove> {
    let root = object_mut(settings);
    let mut hosts = root
        .get("sourceControlHosts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut moves = Vec::new();

    if let Some(bitbucket) = &legacy.bitbucket {
        if !hosts.contains_key("bitbucket") {
            hosts.insert("bitbucket".into(), Value::Object(bitbucket.clone()));
            for (field, from) in LEGACY_BITBUCKET_SECRET_NAMES {
                if bitbucket.get(field).and_then(Value::as_str) != Some(SECRET_REDACTED) {
                    continue;
                }
                moves.push(SecretMove {
                    from: from.to_owned(),
                    to: source_control_host_secret_name("bitbucket", field, None),
                });
            }
        }
    }

    if let Some(github) = &legacy.github {
        if !hosts.contains_key("github") {
            hosts.insert("github".into(), Value::Object(github.clone()));
            let tokens = github.get("tokens").cloned().unwrap_or(Value::Null);
            for (server_host, value) in read_secret_field_values(&tokens) {
                let Some(server_host) = server_host else {
                    continue;
                };
                if value != SECRET_REDACTED {
                    continue;
                }
                moves.push(SecretMove {
                    from: legacy_github_token_secret_name(&server_host),
                    to: source_control_host_secret_name("github", "tokens", Some(&server_host)),
                });
            }
        }
    }

    root.insert("sourceControlHosts".into(), Value::Object(hosts));
    moves
}

/// `migrateLegacyProviderSettings`. Moves each customized legacy
/// `providers.<kind>` blob into its driver's default instance slot. An explicit
/// instance already in the slot wins. Blobs that match a fresh install are dropped.
///
/// A cursor/grok/opencode slot with no `enabled` flag is enabled when provider
/// history shows that driver ran, because that is how those drivers were opted
/// into before they had a flag.
pub fn migrate_legacy_provider_settings(
    settings: &mut Value,
    legacy_providers: &Object,
    history: &[ProviderHistoryRow],
) {
    let used_providers: HashSet<&str> = history
        .iter()
        .map(|row| row.provider_name.as_str())
        .collect();
    let used_instances: HashSet<String> = history
        .iter()
        .map(|row| {
            row.provider_instance_id
                .clone()
                .unwrap_or_else(|| row.provider_name.clone())
        })
        .collect();

    let root = object_mut(settings);
    let mut instances = root
        .get("providerInstances")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    // A history row restores an unset enabled flag on a restored-driver slot.
    for (instance_id, instance) in instances.iter_mut() {
        let driver = instance
            .get("driver")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if instance.get("enabled").is_none()
            && HISTORY_RESTORED_DRIVERS.contains(&driver)
            && used_instances.contains(instance_id)
        {
            instance["enabled"] = json!(true);
        }
    }

    // Retired-driver entries missing from the file are visited too, as empty blobs.
    let mut entries: Vec<(String, Value)> = legacy_providers
        .iter()
        .map(|(kind, blob)| (kind.clone(), blob.clone()))
        .collect();
    for driver in HISTORY_RESTORED_DRIVERS {
        if !legacy_providers.contains_key(driver) {
            entries.push((driver.to_owned(), json!({})));
        }
    }

    for (kind, blob) in entries {
        if !is_provider_driver_kind(&kind) {
            continue;
        }
        let Some(blob) = blob.as_object() else {
            continue;
        };
        let driver = ProviderDriverKind::new(&kind).expect("validated driver kind");
        let instance_id = default_instance_id_for_driver(&driver);
        if instances.contains_key(instance_id.as_str()) {
            continue;
        }

        let mut config = blob.clone();
        let explicit_enabled = config.remove("enabled").and_then(|value| value.as_bool());
        let enabled = explicit_enabled.or_else(|| {
            (HISTORY_RESTORED_DRIVERS.contains(&kind.as_str())
                && used_providers.contains(kind.as_str()))
            .then_some(true)
        });
        let driver_default = resolve_provider_instance_enabled(&ProviderInstanceConfig {
            driver: driver.clone(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: None,
            config: None,
        });
        if config.is_empty() && (enabled.is_none() || enabled == Some(driver_default)) {
            continue;
        }

        let mut instance = Map::new();
        instance.insert("driver".into(), json!(kind));
        if let Some(enabled) = enabled {
            instance.insert("enabled".into(), json!(enabled));
        }
        instance.insert("config".into(), Value::Object(config));
        instances.insert(instance_id.as_str().to_owned(), Value::Object(instance));
    }

    root.insert("providerInstances".into(), Value::Object(instances));
}

/// `foldProviderInstanceEnabledFlags`. Folds a legacy `enabled` inside a
/// provider's config blob into the instance's own flag, and removes it from the
/// blob. An explicit false on either side wins. A malformed (non-boolean)
/// `enabled` stays in the blob, so schema validation reports it.
pub fn fold_provider_instance_enabled_flags(settings: &mut Value) {
    let Some(instances) = settings
        .get_mut("providerInstances")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for instance in instances.values_mut() {
        let Some(instance) = instance.as_object_mut() else {
            continue;
        };
        let Some(config) = instance.get("config").and_then(Value::as_object).cloned() else {
            continue;
        };
        let Some(config_enabled) = config.get("enabled").and_then(Value::as_bool) else {
            continue;
        };
        let mut rest = config;
        rest.remove("enabled");
        let instance_enabled = instance.get("enabled").and_then(Value::as_bool);
        let resolved = if instance_enabled == Some(false) || !config_enabled {
            false
        } else {
            instance_enabled.unwrap_or(config_enabled)
        };
        instance.insert("enabled".into(), json!(resolved));
        instance.insert("config".into(), Value::Object(rest));
    }
}

/// One project row, decoded the way upstream decodes the fold inputs. Values
/// that do not decode are skipped, as upstream does.
fn decode_model_selection(text: Option<&str>) -> Option<ModelSelection> {
    serde_json::from_str::<Option<ModelSelection>>(text.unwrap_or("null"))
        .ok()
        .flatten()
}

fn decode_project_scripts(text: &str) -> Option<Vec<ProjectScript>> {
    serde_json::from_str::<Vec<ProjectScript>>(text).ok()
}

/// `foldLegacyProjectSettings`. A one-time fold of the legacy per-project
/// fields (the three `project*Overrides` maps and the project aggregate's
/// settings columns) into `projectSettingsOverrides`. Keys already present win.
/// Sets `projectSettingsFolded`, so a later reset survives restarts.
pub fn fold_legacy_project_settings(settings: &mut Value, rows: &[LegacyProjectSettingsRow]) {
    if settings
        .get("projectSettingsFolded")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return;
    }
    let map_of = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let browser_access = map_of("projectAgentBrowserAccessOverrides");
    let auto_pull = map_of("projectAutoPullOverrides");
    let scripts_overrides = map_of("projectScriptOverrides");
    // Nothing to fold yet (fresh install): leave the marker off so the file stays sparse.
    if rows.is_empty()
        && browser_access.is_empty()
        && auto_pull.is_empty()
        && scripts_overrides.is_empty()
    {
        return;
    }

    let mut entries: indexmap::IndexMap<String, Object> = map_of("projectSettingsOverrides")
        .into_iter()
        .map(|(id, entry)| (id, entry.as_object().cloned().unwrap_or_default()))
        .collect();
    // Keys present in the generic record win; `null` is kept as "present".
    let set = |entries: &mut indexmap::IndexMap<String, Object>,
               project_id: &str,
               key: &str,
               value: Value| {
        let entry = entries.entry(project_id.to_owned()).or_default();
        if entry.contains_key(key) {
            return;
        }
        entry.insert(key.to_owned(), value);
    };

    for (project_id, value) in &browser_access {
        set(
            &mut entries,
            project_id,
            "enableAgentBrowserAccess",
            value.clone(),
        );
    }
    for (project_id, value) in &auto_pull {
        set(&mut entries, project_id, "defaultAutoPull", value.clone());
    }
    // A stored null meant "reset to machine defaults". Plain inheritance now,
    // so the project's own aggregate scripts must not come back.
    let mut reset_scripts: HashSet<String> = HashSet::new();
    for (project_id, value) in &scripts_overrides {
        if value.is_null() {
            reset_scripts.insert(project_id.clone());
        } else {
            set(
                &mut entries,
                project_id,
                "defaultProjectScripts",
                value.clone(),
            );
        }
    }

    for row in rows {
        if let Some(model) = decode_model_selection(row.default_model_selection.as_deref()) {
            set(
                &mut entries,
                &row.project_id,
                "defaultModelSelection",
                serde_json::to_value(model).unwrap_or(Value::Null),
            );
        }
        if matches!(
            row.default_thread_env_mode.as_deref(),
            Some("local" | "worktree")
        ) {
            set(
                &mut entries,
                &row.project_id,
                "defaultThreadEnvMode",
                json!(row.default_thread_env_mode.clone().unwrap_or_default()),
            );
        }
        if row.auto_pull == 1 {
            set(
                &mut entries,
                &row.project_id,
                "defaultAutoPull",
                json!(true),
            );
        }
        if let Some(scripts) = decode_project_scripts(&row.scripts) {
            if !scripts.is_empty() && !reset_scripts.contains(&row.project_id) {
                set(
                    &mut entries,
                    &row.project_id,
                    "defaultProjectScripts",
                    serde_json::to_value(scripts).unwrap_or(Value::Null),
                );
            }
        }
    }

    let kept: Object = entries
        .into_iter()
        .filter(|(_, entry)| !entry.is_empty())
        .map(|(id, entry)| (id, Value::Object(entry)))
        .collect();
    let root = object_mut(settings);
    root.insert(
        "projectSettingsOverrides".into(),
        Value::Object(kept.clone()),
    );
    root.insert("projectSettingsFolded".into(), json!(true));
    for (key, value) in derive_legacy_project_overrides(&kept) {
        root.insert(key, value);
    }
}

/// Reads the retired `providers` map, if present, from a raw settings object.
pub fn legacy_providers(raw: &Object) -> Option<Object> {
    raw.get("providers").and_then(Value::as_object).cloned()
}

/// Reads the retired `bitbucket` and `github` keys from a raw settings object.
pub fn legacy_source_control(raw: &Object) -> LegacySourceControl {
    LegacySourceControl {
        bitbucket: raw.get("bitbucket").and_then(Value::as_object).cloned(),
        github: raw.get("github").and_then(Value::as_object).cloned(),
    }
}
