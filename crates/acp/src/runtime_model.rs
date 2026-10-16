//! Port of upstream/packages/provider-acp/src/server/runtimeModel.ts
//!
//! Work in progress: session setup, mode, and config helpers are ported. The
//! tool-call, terminal, content, plan, and MCP identity parts follow in later
//! slices (see docs/handoff/R2.md).

use serde::Deserialize;
use turin_contracts::base_schemas::js_trim;

use crate::session_config::{SessionConfigOption, SessionConfigSelectEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionMode {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionModeState {
    pub current_mode_id: String,
    pub available_modes: Vec<AcpSessionMode>,
}

/// Wire form of `SessionMode` as the agent sends it, before trimming.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionModeWire {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Wire form of `SessionModeState` as the agent sends it, before trimming.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionModeStateWire {
    pub current_mode_id: String,
    pub available_modes: Vec<AcpSessionModeWire>,
}

/// The fields of `NewSessionResponse` / `LoadSessionResponse` /
/// `ResumeSessionResponse` / `ForkSessionResponse` that the runtime reads.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionSetupResponse {
    #[serde(default)]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(default)]
    pub modes: Option<AcpSessionModeStateWire>,
}

pub fn extract_model_config_id(session_response: &AcpSessionSetupResponse) -> Option<String> {
    let config_options = session_response.config_options.as_deref()?;
    config_options.iter().find_map(|option| {
        let trimmed = js_trim(option.id());
        (option.category() == Some("model") && !trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

pub fn find_session_config_option<'a>(
    config_options: Option<&'a [SessionConfigOption]>,
    config_id: &str,
) -> Option<&'a SessionConfigOption> {
    let config_options = config_options?;
    let normalized = js_trim(config_id);
    if normalized.is_empty() {
        return None;
    }
    config_options
        .iter()
        .find(|option| js_trim(option.id()) == normalized)
}

pub fn collect_session_config_option_values(configuration: &SessionConfigOption) -> Vec<String> {
    match configuration {
        SessionConfigOption::Select { options, .. } => options
            .iter()
            .flat_map(|entry| match entry {
                SessionConfigSelectEntry::Flat(option) => vec![option.value.clone()],
                SessionConfigSelectEntry::Group(group) => group
                    .options
                    .iter()
                    .map(|option| option.value.clone())
                    .collect(),
            })
            .collect(),
        SessionConfigOption::Boolean { .. } => Vec::new(),
    }
}

pub fn parse_session_mode_state(
    session_response: &AcpSessionSetupResponse,
) -> Option<AcpSessionModeState> {
    let mode_config = session_response
        .config_options
        .as_deref()
        .and_then(|options| {
            options.iter().find(|option| {
                option.category() == Some("mode")
                    && matches!(option, SessionConfigOption::Select { .. })
            })
        });
    let modes: AcpSessionModeStateWire = match (&session_response.modes, mode_config) {
        (Some(modes), _) => modes.clone(),
        (
            None,
            Some(SessionConfigOption::Select {
                current_value,
                options,
                ..
            }),
        ) => AcpSessionModeStateWire {
            current_mode_id: current_value.clone(),
            available_modes: options
                .iter()
                .flat_map(|entry| match entry {
                    SessionConfigSelectEntry::Flat(option) => vec![option],
                    SessionConfigSelectEntry::Group(group) => group.options.iter().collect(),
                })
                .map(|option| AcpSessionModeWire {
                    id: option.value.clone(),
                    name: option.name.clone(),
                    description: option.description.clone(),
                })
                .collect(),
        },
        (None, _) => return None,
    };
    let current_mode_id = js_trim(&modes.current_mode_id);
    if current_mode_id.is_empty() {
        return None;
    }
    let mut available_modes = Vec::new();
    for mode in &modes.available_modes {
        let id = js_trim(&mode.id);
        let name = js_trim(&mode.name);
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let description = mode
            .description
            .as_deref()
            .map(js_trim)
            .filter(|description| !description.is_empty())
            .map(str::to_owned);
        available_modes.push(AcpSessionMode {
            id: id.to_owned(),
            name: name.to_owned(),
            description,
        });
    }
    if available_modes.is_empty() {
        return None;
    }
    Some(AcpSessionModeState {
        current_mode_id: current_mode_id.to_owned(),
        available_modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(value: serde_json::Value) -> AcpSessionSetupResponse {
        serde_json::from_value(value).expect("session setup response")
    }

    #[test]
    fn parses_session_mode_state_from_typed_acp_session_setup_responses() {
        let mode_state = parse_session_mode_state(&response(json!({
            "sessionId": "session-1",
            "modes": {
                "currentModeId": " code ",
                "availableModes": [
                    { "id": " ask ", "name": " Ask ", "description": " Request approval " },
                    { "id": " code ", "name": " Code " },
                ],
            },
            "configOptions": [],
        })));

        assert_eq!(
            mode_state,
            Some(AcpSessionModeState {
                current_mode_id: "code".to_owned(),
                available_modes: vec![
                    AcpSessionMode {
                        id: "ask".to_owned(),
                        name: "Ask".to_owned(),
                        description: Some("Request approval".to_owned()),
                    },
                    AcpSessionMode {
                        id: "code".to_owned(),
                        name: "Code".to_owned(),
                        description: None,
                    },
                ],
            })
        );
    }

    #[test]
    fn extracts_the_model_config_id_from_typed_acp_config_options() {
        let model_config_id = extract_model_config_id(&response(json!({
            "sessionId": "session-1",
            "configOptions": [
                {
                    "id": "approval",
                    "name": "Approval Mode",
                    "category": "permission",
                    "type": "select",
                    "currentValue": "ask",
                    "options": [{ "value": "ask", "name": "Ask" }],
                },
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "default",
                    "options": [{ "value": "default", "name": "Auto" }],
                },
            ],
        })));

        assert_eq!(model_config_id.as_deref(), Some("model"));
    }
}
