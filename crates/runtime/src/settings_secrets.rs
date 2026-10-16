//! Port of upstream/apps/server/src/settingsSecrets.ts, with the parts of
//! `sourceControl/sourceControlHostSecrets.ts` it needs. Only GitHub has a
//! secret host field among the kept source-control hosts, so that table is
//! GitHub-only here (see PORTING Deviations).
//!
//! Credentials stay in the secret store. The settings file and clients see
//! only [`SECRET_REDACTED`] in their place.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};
use turin_contracts::settings::{
    CLAUDE_SECRET_SETTINGS_FIELDS, CODEX_SECRET_SETTINGS_FIELDS, ServerSettings,
    USAGE_LIMIT_SOURCE_SECRET_SETTINGS_FIELDS,
};

/// Replaces every secret value in client-facing settings.
pub const SECRET_REDACTED: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

type Object = Map<String, Value>;

fn encode(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

/// `sourceControlHostSecretName`. Stable across releases: it is the store key.
pub fn source_control_host_secret_name(
    kind: &str,
    field: &str,
    server_host: Option<&str>,
) -> String {
    match server_host {
        None => format!("source-control-{}-{}", encode(kind), encode(field)),
        Some(host) => format!(
            "source-control-{}-{}-{}",
            encode(kind),
            encode(field),
            encode(&host.trim().to_lowercase())
        ),
    }
}

/// `SOURCE_CONTROL_HOST_SECRET_FIELDS`, GitHub only.
fn source_control_host_secret_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "github" => &["tokens"],
        _ => &[],
    }
}

/// Secret fields of a provider driver's config, from the contracts constants.
/// Drivers without a constant (cursor, opencode) have none.
fn provider_secret_fields(driver: &str) -> &'static [&'static str] {
    match driver {
        "codex" => CODEX_SECRET_SETTINGS_FIELDS,
        "claudeAgent" => CLAUDE_SECRET_SETTINGS_FIELDS,
        _ => &[],
    }
}

/// `readSecretFieldValues`: a plain string is one value with no host. An object
/// is one value per server host, with hosts lowercased. Other entries are skipped.
pub fn read_secret_field_values(value: &Value) -> Vec<(Option<String>, String)> {
    match value {
        Value::String(text) => vec![(None, text.clone())],
        Value::Object(hosts) => hosts
            .iter()
            .filter_map(|(host, entry)| {
                entry
                    .as_str()
                    .map(|text| (Some(host.trim().to_lowercase()), text.to_owned()))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `writeSecretFieldValues`: rebuilds a field in the shape it was read from.
pub fn write_secret_field_values(original: &Value, values: Vec<(Option<String>, String)>) -> Value {
    if original.is_string() {
        return Value::String(
            values
                .into_iter()
                .next()
                .map(|(_, value)| value)
                .unwrap_or_default(),
        );
    }
    let mut object = Object::new();
    for (host, value) in values {
        object.insert(host.unwrap_or_default(), Value::String(value));
    }
    Value::Object(object)
}

/// Which kind of stored secret a [`SettingsSecret`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSecretKind {
    Field,
    Environment,
}

/// `SettingsSecret`: one credential visited by [`map_settings_secrets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSecret {
    pub secret_name: String,
    pub kind: SettingsSecretKind,
    pub value: String,
    pub redacted: bool,
    pub sensitive: bool,
    pub provider_instance_id: Option<String>,
    pub environment_variable: Option<String>,
}

/// What the visitor returns for a secret: the value to keep in settings, and
/// whether it is redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretValue {
    pub value: String,
    pub redacted: bool,
}

/// Walks a settings field map. Each secret field is read, visited, and
/// written back with the visitor's value or the redaction marker.
fn map_fields(
    fields: &mut Object,
    secret_fields: &[&str],
    name: impl Fn(&str, Option<&str>) -> String,
    visit: &mut impl FnMut(&SettingsSecret) -> SecretValue,
) {
    for field in secret_fields {
        let Some(original) = fields.get(*field).cloned() else {
            continue;
        };
        let values = read_secret_field_values(&original)
            .into_iter()
            .map(|(server_host, value)| {
                let result = visit(&SettingsSecret {
                    secret_name: name(field, server_host.as_deref()),
                    kind: SettingsSecretKind::Field,
                    redacted: value == SECRET_REDACTED,
                    value: value.clone(),
                    sensitive: true,
                    provider_instance_id: None,
                    environment_variable: None,
                });
                let kept = if result.redacted {
                    SECRET_REDACTED.to_owned()
                } else {
                    result.value
                };
                (server_host, kept)
            })
            .collect();
        fields.insert(
            (*field).to_owned(),
            write_secret_field_values(&original, values),
        );
    }
}

/// `mapSettingsSecrets`. Visits every credential in `settings` and returns the
/// settings with each value replaced by what `map` returns. Store keys are
/// built here, so callers supply only the secret operation.
pub fn map_settings_secrets(
    settings: &ServerSettings,
    mut map: impl FnMut(&SettingsSecret) -> SecretValue,
) -> serde_json::Result<ServerSettings> {
    let mut value = serde_json::to_value(settings)?;
    let root = value
        .as_object_mut()
        .expect("settings serialize to an object");

    if let Some(Value::Object(instances)) = root.get_mut("providerInstances") {
        for (instance_id, instance) in instances.iter_mut() {
            let Some(instance_object) = instance.as_object_mut() else {
                continue;
            };
            let driver = instance_object
                .get("driver")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(Value::Object(config)) = instance_object.get_mut("config") {
                let secret_fields = provider_secret_fields(&driver);
                map_fields(
                    config,
                    secret_fields,
                    |field, server_host| {
                        format!(
                            "provider-config-{}-{}-{}{}",
                            encode(instance_id),
                            encode(&driver),
                            encode(field),
                            server_host
                                .map(|host| format!("-{}", encode(host)))
                                .unwrap_or_default()
                        )
                    },
                    &mut map,
                );
            }
            if let Some(Value::Array(variables)) = instance_object.get_mut("environment") {
                for variable in variables.iter_mut() {
                    let Some(variable_object) = variable.as_object_mut() else {
                        continue;
                    };
                    let name = variable_object
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let value_text = variable_object
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let sensitive = variable_object
                        .get("sensitive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let redacted_in = variable_object
                        .get("valueRedacted")
                        .and_then(Value::as_bool)
                        == Some(true);
                    let result = map(&SettingsSecret {
                        secret_name: format!(
                            "provider-env-{}-{}",
                            encode(instance_id),
                            encode(&name)
                        ),
                        kind: SettingsSecretKind::Environment,
                        value: value_text,
                        redacted: redacted_in,
                        sensitive,
                        provider_instance_id: Some(instance_id.clone()),
                        environment_variable: Some(name),
                    });
                    variable_object.insert("value".into(), Value::String(result.value));
                    variable_object.remove("valueRedacted");
                    if result.redacted {
                        variable_object.insert("valueRedacted".into(), Value::Bool(true));
                    }
                }
            }
        }
    }

    if let Some(Value::Object(hosts)) = root.get_mut("sourceControlHosts") {
        for (kind, fields) in hosts.iter_mut() {
            if let Some(fields) = fields.as_object_mut() {
                map_fields(
                    fields,
                    source_control_host_secret_fields(kind),
                    |field, server_host| source_control_host_secret_name(kind, field, server_host),
                    &mut map,
                );
            }
        }
    }

    if let Some(Value::Object(sources)) = root.get_mut("usageLimitSources") {
        for (id, source) in sources.iter_mut() {
            if let Some(source) = source.as_object_mut() {
                map_fields(
                    source,
                    USAGE_LIMIT_SOURCE_SECRET_SETTINGS_FIELDS,
                    |field, server_host| {
                        let tail = if field == "managementKey" && server_host.is_none() {
                            String::new()
                        } else {
                            format!(
                                "-{}{}",
                                encode(field),
                                server_host
                                    .map(|host| format!("-{}", encode(host)))
                                    .unwrap_or_default()
                            )
                        };
                        format!("usage-limit-source-{}{tail}", encode(id))
                    },
                    &mut map,
                );
            }
        }
    }

    serde_json::from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::settings::default_server_settings;

    fn keep(secret: &SettingsSecret) -> SecretValue {
        SecretValue {
            value: secret.value.clone(),
            redacted: false,
        }
    }

    #[test]
    fn store_keys_are_stable_and_host_lowercased() {
        assert_eq!(encode("github"), "Z2l0aHVi");
        assert_eq!(
            source_control_host_secret_name("github", "tokens", None),
            format!("source-control-{}-{}", encode("github"), encode("tokens"))
        );
        assert_eq!(
            source_control_host_secret_name("github", "tokens", Some(" GitHub.COM ")),
            source_control_host_secret_name("github", "tokens", Some("github.com"))
        );
    }

    #[test]
    fn redacted_marker_is_six_bullets() {
        assert_eq!(SECRET_REDACTED.chars().count(), 6);
        assert!(SECRET_REDACTED.chars().all(|ch| ch == '\u{2022}'));
    }

    #[test]
    fn per_host_and_plain_fields_read_back_in_their_shape() {
        assert_eq!(
            read_secret_field_values(&json!("abc")),
            vec![(None, "abc".to_owned())]
        );
        let hosts = json!({ " GitHub.com ": "t1", "ghe.example": 5 });
        assert_eq!(
            read_secret_field_values(&hosts),
            vec![(Some("github.com".to_owned()), "t1".to_owned())]
        );
        assert_eq!(
            write_secret_field_values(&json!("old"), vec![(None, "new".to_owned())]),
            json!("new")
        );
    }

    #[test]
    fn github_tokens_are_visited_per_host_and_redacted_values_are_kept() {
        let mut settings = default_server_settings();
        settings.source_control_hosts = serde_json::from_value(json!({
            "github": {
                "hosts": {},
                "tokens": { "github.com": "secret-1", "ghe.example": SECRET_REDACTED },
            },
        }))
        .unwrap();
        let mut seen = Vec::new();
        let mapped = map_settings_secrets(&settings, |secret| {
            seen.push((secret.secret_name.clone(), secret.redacted));
            // The store takes the real value and returns an empty placeholder.
            SecretValue {
                value: String::new(),
                redacted: secret.redacted,
            }
        })
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.contains(&(
            source_control_host_secret_name("github", "tokens", Some("github.com")),
            false,
        )));
        assert!(seen.contains(&(
            source_control_host_secret_name("github", "tokens", Some("ghe.example")),
            true,
        )));
        let value = serde_json::to_value(&mapped.source_control_hosts).unwrap();
        assert_eq!(value["github"]["tokens"]["github.com"], json!(""));
        assert_eq!(
            value["github"]["tokens"]["ghe.example"],
            json!(SECRET_REDACTED)
        );
    }

    #[test]
    fn usage_source_management_key_uses_the_bare_source_name() {
        let mut settings = default_server_settings();
        settings.usage_limit_sources = serde_json::from_value(json!({
            "hub": {
                "kind": "cliproxy",
                "url": "https://hub.example",
                "managementKey": "key-1",
            },
        }))
        .unwrap();
        let mut names = Vec::new();
        map_settings_secrets(&settings, |secret| {
            names.push(secret.secret_name.clone());
            keep(secret)
        })
        .unwrap();
        assert_eq!(names, vec![format!("usage-limit-source-{}", encode("hub"))]);
    }

    #[test]
    fn environment_variables_are_visited_and_marked_redacted() {
        let mut settings = default_server_settings();
        settings.provider_instances = serde_json::from_value(json!({
            "codex": {
                "driver": "codex",
                "environment": [
                    { "name": "OPENAI_KEY", "value": "sk", "sensitive": true },
                    { "name": "REGION", "value": "eu", "sensitive": false },
                ],
            },
        }))
        .unwrap();
        let mut visited = Vec::new();
        let mapped = map_settings_secrets(&settings, |secret| {
            visited.push((
                secret.environment_variable.clone(),
                secret.kind,
                secret.sensitive,
            ));
            if secret.sensitive {
                SecretValue {
                    value: SECRET_REDACTED.to_owned(),
                    redacted: true,
                }
            } else {
                SecretValue {
                    value: secret.value.clone(),
                    redacted: false,
                }
            }
        })
        .unwrap();
        assert_eq!(visited.len(), 2);
        assert!(
            visited
                .iter()
                .all(|(_, kind, _)| *kind == SettingsSecretKind::Environment)
        );
        let value = serde_json::to_value(&mapped.provider_instances).unwrap();
        assert_eq!(
            value["codex"]["environment"][0]["value"],
            json!(SECRET_REDACTED)
        );
        assert_eq!(
            value["codex"]["environment"][0]["valueRedacted"],
            json!(true)
        );
        assert_eq!(value["codex"]["environment"][1]["value"], json!("eu"));
        assert!(
            value["codex"]["environment"][1]
                .get("valueRedacted")
                .is_none()
        );
    }
}
