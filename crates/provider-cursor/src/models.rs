//! Model catalog from `cursor/list_available_models` and `session/set_config_option`.
//!
//! The model selector's config id is `"model"`. Parameterized options are
//! further `set_config_option` calls. This is not the SDK's per-send
//! `{id, params}` selection.

use serde_json::{Value, json};
use turin_acp::cursor::AvailableModel;
use turin_contracts::model::ProviderOptionSelectionValue;
use turin_contracts::model_selection::ModelSelection;

pub const MODEL_CONFIG_ID: &str = "model";

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigUpdate {
    pub config_id: String,
    pub value: Value,
    pub type_name: Option<String>,
}

impl ConfigUpdate {
    pub fn payload(&self, session_id: &str) -> Value {
        let mut payload = json!({
            "sessionId": session_id,
            "configId": self.config_id,
            "value": self.value,
        });
        if let Some(type_name) = &self.type_name {
            payload["type"] = json!(type_name);
        }
        payload
    }
}

pub fn config_updates(catalog: &[AvailableModel], selection: &ModelSelection) -> Vec<ConfigUpdate> {
    let mut updates = vec![ConfigUpdate {
        config_id: MODEL_CONFIG_ID.to_owned(),
        value: Value::String(selection.model.as_str().to_owned()),
        type_name: None,
    }];
    let model = catalog
        .iter()
        .find(|model| model.value == selection.model.as_str());
    let Some(options) = &selection.options else {
        return updates;
    };
    for option in options {
        let id = option.id.as_str();
        if id == MODEL_CONFIG_ID {
            continue;
        }
        if let Some(model) = model
            && !model
                .config_options
                .iter()
                .any(|candidate| candidate.id == id)
        {
            continue;
        }
        let catalog_type = model
            .and_then(|model| {
                model
                    .config_options
                    .iter()
                    .find(|candidate| candidate.id == id)
            })
            .map(|candidate| candidate.type_name.as_str());
        let (value, type_name) = match &option.value {
            ProviderOptionSelectionValue::Bool(flag) => {
                (Value::Bool(*flag), Some("boolean".to_owned()))
            }
            ProviderOptionSelectionValue::Str(text) if catalog_type == Some("boolean") => (
                Value::Bool(text.as_str() == "true"),
                Some("boolean".to_owned()),
            ),
            ProviderOptionSelectionValue::Str(text) => {
                (Value::String(text.as_str().to_owned()), None)
            }
        };
        updates.push(ConfigUpdate {
            config_id: id.to_owned(),
            value,
            type_name,
        });
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_acp::cursor::{AvailableModel, ListAvailableModelsResult, ModelConfigOption};
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::model::ProviderOptionSelection;
    use turin_contracts::provider_instance::ProviderInstanceId;

    #[test]
    fn model_and_known_parameters_become_config_options() {
        let catalog: ListAvailableModelsResult = serde_json::from_value(json!({
            "models": [{
                "value": "gpt-5.5",
                "name": "GPT-5.5",
                "configOptions": [{
                    "id": "reasoning",
                    "name": "Reasoning",
                    "type": "select",
                    "currentValue": "high",
                    "options": [{"value": "high", "name": "High"}]
                }]
            }]
        }))
        .unwrap();
        let selection = ModelSelection {
            instance_id: ProviderInstanceId::new("cursor").unwrap(),
            model: TrimmedNonEmptyString::new("gpt-5.5").unwrap(),
            options: Some(vec![
                ProviderOptionSelection {
                    id: TrimmedNonEmptyString::new("reasoning").unwrap(),
                    value: ProviderOptionSelectionValue::Str(
                        TrimmedNonEmptyString::new("high").unwrap(),
                    ),
                },
                ProviderOptionSelection {
                    id: TrimmedNonEmptyString::new("unknown").unwrap(),
                    value: ProviderOptionSelectionValue::Str(
                        TrimmedNonEmptyString::new("x").unwrap(),
                    ),
                },
            ]),
        };
        let updates = config_updates(&catalog.models, &selection);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].config_id, "model");
        assert_eq!(updates[0].payload("s1")["configId"], "model");
        assert_eq!(updates[1].config_id, "reasoning");
        assert_eq!(updates[1].value, "high");
        let _typed: &[AvailableModel] = &catalog.models;
        let _option: &ModelConfigOption = &catalog.models[0].config_options[0];
    }
}
