//! Port of upstream/apps/server/src/provider/CodexMcpElicitation.ts.
//!
//! Maps an MCP elicitation request to T3 approval options and a decision to
//! the wire response. Metadata and form schemas that fail their shape check
//! are ignored, as `Schema.is` makes upstream ignore them.
use std::collections::HashMap;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::provider_policy::{ProviderApprovalDecision, ProviderApprovalOption};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationTarget {
    app: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct McpElicitationToolParams {
    app: Option<String>,
    #[serde(rename = "app_name")]
    app_name: Option<String>,
}

/// `persist` is a string or a list of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum McpElicitationPersist {
    One(String),
    Many(Vec<String>),
}

/// `McpElicitationMetadata`. Nullable fields read as absent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationMetadata {
    app: Option<String>,
    #[serde(rename = "app_name")]
    app_name_snake: Option<String>,
    app_name: Option<String>,
    #[serde(rename = "connector_name")]
    connector_name_snake: Option<String>,
    connector_name: Option<String>,
    allow_persistent_approval: Option<bool>,
    persist: Option<McpElicitationPersist>,
    target: Option<McpElicitationTarget>,
    tool_params: Option<McpElicitationToolParams>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpElicitationOneOf {
    #[serde(rename = "const")]
    const_value: String,
    title: Option<String>,
}

/// `McpElicitationFormField`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationFormField {
    #[serde(rename = "type")]
    type_: Option<String>,
    title: Option<String>,
    description: Option<String>,
    default: Option<Value>,
    #[serde(rename = "enum")]
    enum_values: Option<Vec<String>>,
    enum_names: Option<Vec<String>>,
    one_of: Option<Vec<McpElicitationOneOf>>,
}

/// `McpElicitationForm`.
#[derive(Debug, Clone, Deserialize)]
struct McpElicitationForm {
    properties: Option<IndexMap<String, McpElicitationFormField>>,
    required: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PersistDecision {
    Session,
    Always,
}

impl PersistDecision {
    fn approval(self) -> ProviderApprovalDecision {
        match self {
            Self::Session => ProviderApprovalDecision::AcceptForSession,
            Self::Always => ProviderApprovalDecision::AcceptAlways,
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Always => "always",
        }
    }
}

/// `mcpElicitationPersistenceDecision`: a session or always-style word in a
/// label or value, checked case-insensitively; `session` wins over the rest.
fn persistence_decision(value: &str) -> Option<PersistDecision> {
    let normalized = value.to_lowercase();
    if normalized.contains("session") {
        return Some(PersistDecision::Session);
    }
    ["always", "permanent", "forever", "persistent"]
        .iter()
        .any(|word| normalized.contains(word))
        .then_some(PersistDecision::Always)
}

fn metadata(payload: &Value) -> Option<McpElicitationMetadata> {
    serde_json::from_value(payload.get("_meta")?.clone()).ok()
}

/// `mcpElicitationFormFields`: a form request whose schema has a valid shape.
fn form_fields(payload: &Value) -> Option<McpElicitationForm> {
    if payload.get("mode").and_then(Value::as_str) == Some("url") {
        return None;
    }
    serde_json::from_value(payload.get("requestedSchema")?.clone()).ok()
}

/// `mcpElicitationFieldOptions`: `(value, label)` pairs from `oneOf`, or from `enum`.
fn field_options(field: &McpElicitationFormField) -> Vec<(String, Option<String>)> {
    if let Some(one_of) = &field.one_of {
        return one_of
            .iter()
            .map(|option| (option.const_value.clone(), option.title.clone()))
            .collect();
    }
    let names = field.enum_names.as_deref().unwrap_or_default();
    field
        .enum_values
        .iter()
        .flatten()
        .enumerate()
        .map(|(index, value)| (value.clone(), names.get(index).cloned()))
        .collect()
}

fn is_persistence_field(key: &str, field: &McpElicitationFormField) -> bool {
    persistence_decision(key).is_some()
        || key.eq_ignore_ascii_case("persist")
        || persistence_decision(field.title.as_deref().unwrap_or("")).is_some()
        || persistence_decision(field.description.as_deref().unwrap_or("")).is_some()
}

/// `^Allow ChatGPT to use (.+?)\?$` (case-insensitive), without a regex engine.
fn chatgpt_app_from_message(message: &str) -> Option<&str> {
    const PREFIX: &str = "Allow ChatGPT to use ";
    let rest = message.get(..PREFIX.len()).and_then(|head| {
        head.eq_ignore_ascii_case(PREFIX)
            .then(|| &message[PREFIX.len()..])
    })?;
    let name = rest.strip_suffix('?')?;
    (!name.is_empty() && !name.contains('\n')).then_some(name)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// `describeMcpElicitation`: the app name and the approval choices offered.
pub fn describe_mcp_elicitation(
    payload: &Value,
    server_name: &str,
) -> (String, Vec<ProviderApprovalOption>) {
    let meta = metadata(payload).unwrap_or_default();
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    let app_name = meta
        .app_name_snake
        .as_deref()
        .or(meta.app_name.as_deref())
        .or(meta.app.as_deref())
        .or(meta
            .target
            .as_ref()
            .and_then(|target| target.app.as_deref()))
        .or(meta
            .target
            .as_ref()
            .and_then(|target| target.name.as_deref()))
        .or(meta
            .tool_params
            .as_ref()
            .and_then(|params| params.app_name.as_deref()))
        .or(meta
            .tool_params
            .as_ref()
            .and_then(|params| params.app.as_deref()))
        .or(chatgpt_app_from_message(message))
        .or(meta.connector_name_snake.as_deref())
        .or(meta.connector_name.as_deref())
        .unwrap_or(server_name)
        .to_owned();

    let mut persistence_labels: HashMap<PersistDecision, String> = HashMap::new();
    let persist: Vec<String> = match meta.persist {
        Some(McpElicitationPersist::One(value)) => vec![value],
        Some(McpElicitationPersist::Many(values)) => values,
        None => Vec::new(),
    };
    for value in &persist {
        if let Some(decision) = persistence_decision(value) {
            persistence_labels.insert(decision, String::new());
        }
    }
    if meta.allow_persistent_approval == Some(true) {
        persistence_labels.insert(PersistDecision::Always, String::new());
    }

    let form = form_fields(payload);
    for (key, field) in form
        .iter()
        .flat_map(|form| form.properties.iter().flatten())
    {
        for (value, label) in field_options(field) {
            if let Some(decision) = persistence_decision(&value) {
                persistence_labels.insert(decision, label.unwrap_or_default());
            }
        }
        if field.type_.as_deref() == Some("boolean") && is_persistence_field(key, field) {
            persistence_labels.insert(
                PersistDecision::Always,
                field.title.clone().unwrap_or_default(),
            );
        }
    }

    let mut options = vec![
        option(ProviderApprovalDecision::Cancel, "Cancel"),
        option(ProviderApprovalDecision::Decline, "Decline"),
    ];
    for (decision, fallback) in [
        (PersistDecision::Session, "Always allow this session"),
        (PersistDecision::Always, "Always allow"),
    ] {
        if let Some(label) = persistence_labels.get(&decision)
            && to_response(payload, decision.approval()).get("action") == Some(&json!("accept"))
        {
            let label = non_empty(Some(label)).unwrap_or(fallback);
            options.push(option(decision.approval(), label));
        }
    }
    options.push(option(ProviderApprovalDecision::Accept, "Approve"));
    (app_name, options)
}

fn option(decision: ProviderApprovalDecision, label: &str) -> ProviderApprovalOption {
    ProviderApprovalOption {
        decision,
        label: TrimmedNonEmptyString::new(label).expect("static option label"),
        warning: None,
    }
}

/// `toMcpElicitationResponse`: the wire response for a T3 approval decision.
pub fn to_mcp_elicitation_response(payload: &Value, decision: ProviderApprovalDecision) -> Value {
    to_response(payload, decision)
}

fn to_response(payload: &Value, decision: ProviderApprovalDecision) -> Value {
    match decision {
        ProviderApprovalDecision::Decline => return json!({"action": "decline"}),
        ProviderApprovalDecision::Cancel => return json!({"action": "cancel"}),
        _ => {}
    }
    if payload.get("mode").and_then(Value::as_str) == Some("url") {
        return json!({"action": "decline"});
    }

    let persist = match decision {
        ProviderApprovalDecision::AcceptForSession => Some(PersistDecision::Session),
        ProviderApprovalDecision::AcceptAlways => Some(PersistDecision::Always),
        _ => None,
    };
    let form = form_fields(payload);
    let mut content = Map::new();
    for (key, field) in form
        .iter()
        .flat_map(|form| form.properties.iter().flatten())
    {
        let options = field_options(field);
        let chosen = options.iter().find(|(value, _)| match persist {
            Some(persist) => persistence_decision(value) == Some(persist),
            None => {
                let lower = value.to_lowercase();
                ["once", "accept", "approve", "allow"]
                    .iter()
                    .any(|word| lower.contains(word))
                    && persistence_decision(value).is_none()
            }
        });
        if let Some((value, _)) = chosen {
            content.insert(key.clone(), json!(value));
        } else if field.type_.as_deref() == Some("boolean") && is_persistence_field(key, field) {
            content.insert(
                key.clone(),
                json!(decision == ProviderApprovalDecision::AcceptAlways),
            );
        } else if let Some(default) = field.default.as_ref().filter(|value| !value.is_null()) {
            content.insert(key.clone(), default.clone());
        }
    }

    if let Some(form) = &form
        && form
            .required
            .iter()
            .flatten()
            .any(|key| !content.contains_key(key))
    {
        return json!({"action": "decline"});
    }

    let mut response = Map::new();
    response.insert("action".into(), json!("accept"));
    if let Some(persist) = persist {
        response.insert("_meta".into(), json!({"persist": persist.wire()}));
    }
    if form.is_some() {
        response.insert("content".into(), Value::Object(content));
    }
    Value::Object(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(options: &[ProviderApprovalOption]) -> Vec<(&ProviderApprovalDecision, &str)> {
        options
            .iter()
            .map(|option| (&option.decision, option.label.as_str()))
            .collect()
    }

    #[test]
    fn app_name_prefers_metadata_then_message_then_server() {
        let (name, _) = describe_mcp_elicitation(
            &json!({"message": "Allow ChatGPT to use Gmail?", "serverName": "srv",
                    "_meta": {"app_name": "Calendar"}}),
            "srv",
        );
        assert_eq!(name, "Calendar");
        let (name, _) = describe_mcp_elicitation(
            &json!({"message": "allow chatgpt to use Gmail?", "serverName": "srv"}),
            "srv",
        );
        assert_eq!(name, "Gmail");
        let (name, _) = describe_mcp_elicitation(
            &json!({"message": "Something else", "serverName": "srv"}),
            "srv",
        );
        assert_eq!(name, "srv");
    }

    #[test]
    fn invalid_metadata_is_ignored_as_a_whole() {
        let (name, _) = describe_mcp_elicitation(
            &json!({"serverName": "srv", "_meta": {"app_name": 7}}),
            "srv",
        );
        assert_eq!(name, "srv");
    }

    #[test]
    fn plain_form_offers_cancel_decline_and_approve() {
        let payload = json!({"serverName": "srv", "message": "m", "mode": "form",
            "requestedSchema": {"properties": {}}});
        let (_, options) = describe_mcp_elicitation(&payload, "srv");
        assert_eq!(
            labels(&options),
            vec![
                (&ProviderApprovalDecision::Cancel, "Cancel"),
                (&ProviderApprovalDecision::Decline, "Decline"),
                (&ProviderApprovalDecision::Accept, "Approve"),
            ]
        );
    }

    #[test]
    fn persistent_options_appear_when_the_form_offers_them() {
        let payload = json!({"serverName": "srv", "message": "m", "mode": "form",
        "requestedSchema": {"properties": {"scope": {
            "type": "string",
            "oneOf": [
                {"const": "allow_once", "title": "Once"},
                {"const": "allow_session", "title": "Session pls"},
                {"const": "always_allow", "title": "Forever"}
            ]
        }}}});
        let (_, options) = describe_mcp_elicitation(&payload, "srv");
        let decisions: Vec<&ProviderApprovalDecision> =
            options.iter().map(|option| &option.decision).collect();
        assert_eq!(
            decisions,
            vec![
                &ProviderApprovalDecision::Cancel,
                &ProviderApprovalDecision::Decline,
                &ProviderApprovalDecision::AcceptForSession,
                &ProviderApprovalDecision::AcceptAlways,
                &ProviderApprovalDecision::Accept,
            ]
        );
        assert_eq!(options[2].label.as_str(), "Session pls");
        assert_eq!(options[3].label.as_str(), "Forever");
    }

    #[test]
    fn accept_chooses_the_once_style_option_and_fills_defaults() {
        let payload = json!({"serverName": "srv", "mode": "form", "message": "m",
        "requestedSchema": {"required": ["scope", "note"], "properties": {
            "scope": {"type": "string", "oneOf": [
                {"const": "allow_once"}, {"const": "always_allow"}]},
            "note": {"type": "string", "default": "ok"}
        }}});
        let response = to_mcp_elicitation_response(&payload, ProviderApprovalDecision::Accept);
        assert_eq!(
            response,
            json!({"action": "accept", "content": {"scope": "allow_once", "note": "ok"}})
        );
    }

    #[test]
    fn persistent_accept_sets_meta_and_boolean_persist_field() {
        let payload = json!({"serverName": "srv", "mode": "form", "message": "m",
            "requestedSchema": {"properties": {"remember": {
                "type": "boolean", "title": "Remember forever"}}}});
        let response =
            to_mcp_elicitation_response(&payload, ProviderApprovalDecision::AcceptAlways);
        assert_eq!(
            response,
            json!({"action": "accept", "_meta": {"persist": "always"},
                   "content": {"remember": true}})
        );
    }

    #[test]
    fn missing_required_field_declines() {
        let payload = json!({"serverName": "srv", "mode": "form", "message": "m",
            "requestedSchema": {"required": ["token"], "properties": {}}});
        assert_eq!(
            to_mcp_elicitation_response(&payload, ProviderApprovalDecision::Accept),
            json!({"action": "decline"})
        );
    }

    #[test]
    fn url_mode_declines_accepts_but_answers_cancel_and_decline() {
        let payload =
            json!({"serverName": "srv", "mode": "url", "message": "m", "url": "https://x"});
        assert_eq!(
            to_mcp_elicitation_response(&payload, ProviderApprovalDecision::Accept),
            json!({"action": "decline"})
        );
        assert_eq!(
            to_mcp_elicitation_response(&payload, ProviderApprovalDecision::Cancel),
            json!({"action": "cancel"})
        );
    }
}
