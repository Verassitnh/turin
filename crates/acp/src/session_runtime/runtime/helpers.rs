//! Pure helpers ported from upstream `AcpSessionRuntime.ts`.

use serde_json::Value;

use crate::errors::{AcpError, AcpRequestError};

pub fn session_config_options_from_setup(response: Option<&Value>) -> Value {
    response
        .and_then(|value| value.get("configOptions"))
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]))
}

pub fn config_option_current_value_matches(
    config_option: &Value,
    value: &ConfigOptionValue,
) -> bool {
    let Some(object) = config_option.as_object() else {
        return false;
    };
    let option_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    let current = object.get("currentValue");
    match option_type {
        "boolean" => matches!(value, ConfigOptionValue::Boolean(current_bool)
            if current.and_then(Value::as_bool) == Some(*current_bool)),
        _ => matches!(value, ConfigOptionValue::String(next)
            if current
                .and_then(Value::as_str)
                .map(str::trim)
                == Some(next.trim())),
    }
}

pub fn is_startup_metadata_update(notification: &Value) -> bool {
    let Some(update) = notification.get("update") else {
        return false;
    };
    matches!(
        update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "current_mode_update" | "config_option_update" | "available_commands_update"
    )
}

pub fn is_acp_authentication_required(error: &AcpError) -> bool {
    matches!(
        error,
        AcpError::Request(request) if request.code == -32000
    )
}

#[allow(dead_code)]
pub fn format_config_option_value(value: &ConfigOptionValue) -> String {
    match value {
        ConfigOptionValue::Boolean(boolean) => boolean.to_string(),
        ConfigOptionValue::String(text) => {
            serde_json::to_string(text).unwrap_or_else(|_| text.clone())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOptionValue {
    String(String),
    Boolean(bool),
}

pub fn auth_methods_from_initialize(
    initialize: &crate::compat::InitializeResponse,
) -> Vec<super::super::auth::AcpAuthMethod> {
    initialize
        .auth_methods
        .iter()
        .filter_map(|method| {
            let id = method.get("id")?.as_str()?.to_owned();
            let method_type = method
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(super::super::auth::AcpAuthMethod { id, method_type })
        })
        .collect()
}

pub fn invalid_config_option_value(
    config_id: &str,
    value: &ConfigOptionValue,
    expected: &str,
) -> AcpError {
    AcpError::from(AcpRequestError::invalid_params(format!(
        "Invalid value {} for session config option \"{config_id}\": expected {expected}",
        format_config_option_value(value)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_config_options_from_setup_reads_config_options_or_empty_array() {
        assert_eq!(session_config_options_from_setup(None), json!([]));
        assert_eq!(
            session_config_options_from_setup(Some(&json!({
                "configOptions": [{ "id": "model", "type": "select" }],
            }))),
            json!([{ "id": "model", "type": "select" }])
        );
    }

    #[test]
    fn startup_metadata_gating_matches_upstream_session_update_kinds() {
        assert!(is_startup_metadata_update(&json!({
            "update": { "sessionUpdate": "current_mode_update" },
        })));
        assert!(is_startup_metadata_update(&json!({
            "update": { "sessionUpdate": "config_option_update" },
        })));
        assert!(!is_startup_metadata_update(&json!({
            "update": { "sessionUpdate": "agent_message_chunk" },
        })));
    }

    #[test]
    fn config_option_current_value_matches_trims_strings() {
        let option = json!({
            "type": "select",
            "currentValue": " fast ",
        });
        assert!(config_option_current_value_matches(
            &option,
            &ConfigOptionValue::String("fast".to_owned())
        ));
        assert!(!config_option_current_value_matches(
            &option,
            &ConfigOptionValue::Boolean(true)
        ));
    }
}
