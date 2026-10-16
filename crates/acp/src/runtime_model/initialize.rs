use serde::Deserialize;
use serde_json::{Map, Value};

use super::session_setup::AcpSessionModeStateWire;
use super::util::is_record;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelInfoWire {
    model_id: String,
    name: String,
    #[serde(default)]
    description: Option<Value>,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelStateWire {
    current_model_id: String,
    available_models: Vec<SessionModelInfoWire>,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

fn is_session_model_state(value: &Value) -> bool {
    let Ok(parsed) = serde_json::from_value::<SessionModelStateWire>(value.clone()) else {
        return false;
    };
    if parsed
        .meta
        .as_ref()
        .is_some_and(|meta| !meta.is_object() && !meta.is_null())
    {
        return false;
    }
    for model in &parsed.available_models {
        if model
            .description
            .as_ref()
            .is_some_and(|d| !d.is_string() && !d.is_null())
        {
            return false;
        }
        if model
            .meta
            .as_ref()
            .is_some_and(|meta| !meta.is_object() && !meta.is_null())
        {
            return false;
        }
    }
    true
}

fn is_session_mode_state(value: &Value) -> bool {
    serde_json::from_value::<AcpSessionModeStateWire>(value.clone()).is_ok()
}

/// Model state some agents (Grok) advertise in `initialize._meta.modelState`, before any
/// session exists. Undefined when the agent does not advertise it or the shape is unknown.
pub fn session_model_state_from_initialize(initialize_result: &Value) -> Option<Value> {
    let meta = initialize_result.get("_meta").and_then(is_record)?;
    let model_state = meta.get("modelState")?;
    is_session_model_state(model_state).then(|| model_state.clone())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticLoadSessionResponse {
    pub modes: Option<Value>,
    pub meta: Map<String, Value>,
}

pub fn synthetic_load_session_response_from_initialize(
    initialize_result: &Value,
) -> SyntheticLoadSessionResponse {
    let mode_state = initialize_result
        .get("_meta")
        .and_then(is_record)
        .and_then(|meta| meta.get("modeState"));
    let modes = mode_state
        .filter(|state| is_session_mode_state(state))
        .cloned();
    SyntheticLoadSessionResponse {
        modes,
        meta: [(
            "t3SessionLoadReady".to_owned(),
            Value::String("replay_idle".to_owned()),
        )]
        .into_iter()
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_model::{AcpSessionSetupResponse, parse_session_mode_state};
    use serde_json::json;

    #[test]
    fn ignores_malformed_initialize_mode_state_in_synthetic_load_responses() {
        let response = synthetic_load_session_response_from_initialize(&json!({
            "protocolVersion": 1,
            "_meta": {
                "modeState": {
                    "currentModeId": "code",
                    "availableModes": [{ "id": "code", "name": 12 }],
                },
            },
        }));
        assert!(response.modes.is_none());
        assert_eq!(
            response
                .meta
                .get("t3SessionLoadReady")
                .and_then(Value::as_str),
            Some("replay_idle")
        );
    }

    #[test]
    fn builds_a_synthetic_load_response_with_initialize_mode_state() {
        let response = synthetic_load_session_response_from_initialize(&json!({
            "protocolVersion": 1,
            "_meta": {
                "modeState": {
                    "currentModeId": "code",
                    "availableModes": [
                        { "id": "ask", "name": "Ask" },
                        { "id": "code", "name": "Code" },
                    ],
                },
            },
        }));
        let modes = response.modes.expect("modes");
        assert_eq!(
            modes.get("currentModeId").and_then(Value::as_str),
            Some("code")
        );
        assert_eq!(
            modes
                .get("availableModes")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn keeps_initialize_mode_state_whose_modes_have_null_descriptions() {
        let response = synthetic_load_session_response_from_initialize(&json!({
            "protocolVersion": 1,
            "_meta": {
                "modeState": {
                    "currentModeId": "code",
                    "availableModes": [
                        { "id": "ask", "name": "Ask", "description": null },
                        { "id": "code", "name": "Code", "description": "Edit files", "_meta": null },
                    ],
                    "_meta": null,
                },
            },
        }));
        let setup: AcpSessionSetupResponse = serde_json::from_value(json!({
            "modes": response.modes,
        }))
        .expect("setup");
        assert_eq!(
            parse_session_mode_state(&setup),
            Some(crate::runtime_model::AcpSessionModeState {
                current_mode_id: "code".to_owned(),
                available_modes: vec![
                    crate::runtime_model::AcpSessionMode {
                        id: "ask".to_owned(),
                        name: "Ask".to_owned(),
                        description: None,
                    },
                    crate::runtime_model::AcpSessionMode {
                        id: "code".to_owned(),
                        name: "Code".to_owned(),
                        description: Some("Edit files".to_owned()),
                    },
                ],
            })
        );
    }

    #[test]
    fn reads_initialize_model_state_and_ignores_malformed_model_state() {
        let model_state = json!({
            "currentModelId": "grok-4.7",
            "availableModels": [
                {
                    "modelId": "grok-4.7",
                    "name": "Grok 4.7",
                    "description": null,
                    "_meta": { "reasoningEfforts": [{ "id": "high", "default": true }] },
                },
                { "modelId": "grok-4.6", "name": "Grok 4.6" },
            ],
        });
        assert_eq!(
            session_model_state_from_initialize(&json!({
                "protocolVersion": 1,
                "_meta": { "modelState": model_state },
            })),
            Some(model_state)
        );
        assert!(
            session_model_state_from_initialize(&json!({
                "protocolVersion": 1,
                "_meta": {
                    "modelState": {
                        "currentModelId": "grok-4.7",
                        "availableModels": [{ "modelId": "grok-4.7" }],
                    },
                },
            }))
            .is_none()
        );
        assert!(
            session_model_state_from_initialize(&json!({
                "protocolVersion": 1,
                "_meta": {
                    "modelState": {
                        "currentModelId": "grok-4.7",
                        "availableModels": [{ "modelId": "grok-4.7", "name": "Grok 4.7" }],
                        "_meta": "not an object",
                    },
                },
            }))
            .is_none()
        );
    }
}
