//! Port of upstream/packages/effect-acp/src/compat.ts and the normalizers in client.ts.
//!
//! The public surface is the provider-neutral shape. v2 wire objects are
//! rewritten after `initialize` decides the generation. A response that contains
//! `info` is v2; Antigravity reports protocolVersion 2 with the v1
//! `agentInfo` shape, so the field, not the number, is the discriminator.

use serde_json::{Map, Value, json};

use crate::protocol::RequestContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolGeneration {
    V1,
    V2,
}

#[derive(Debug, Clone, Default)]
pub struct ClientCapabilities {
    pub fs_read_text_file: Option<bool>,
    pub fs_write_text_file: Option<bool>,
    pub terminal: Option<bool>,
    pub auth_terminal: Option<bool>,
    pub elicitation: Option<Value>,
    /// Passed through on both `clientCapabilities._meta` and v2 `capabilities._meta`.
    pub meta: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ImplementationInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct InitializeRequest {
    pub protocol_version: i64,
    pub client_capabilities: Option<ClientCapabilities>,
    pub client_info: Option<ImplementationInfo>,
    pub meta: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct InitializeResponse {
    pub protocol_version: i64,
    pub agent_capabilities: Value,
    pub auth_methods: Vec<Value>,
    pub agent_info: Option<Value>,
    pub meta: Option<Value>,
    pub generation: ProtocolGeneration,
}

pub fn negotiating_initialize_params(request: &InitializeRequest) -> Value {
    let capabilities = request.client_capabilities.as_ref();
    let mut v2_capabilities = Map::new();
    if capabilities.and_then(|caps| caps.auth_terminal) == Some(true) {
        v2_capabilities.insert("auth".to_owned(), json!({"terminal": {}}));
    }
    if let Some(elicitation) = capabilities
        .and_then(|caps| caps.elicitation.clone())
        .filter(|elicitation| !elicitation.is_null())
    {
        v2_capabilities.insert("elicitation".to_owned(), elicitation);
    }
    if let Some(meta) = capabilities.and_then(|caps| caps.meta.clone()) {
        v2_capabilities.insert("_meta".to_owned(), meta);
    }
    let info = match &request.client_info {
        Some(info) => json!({"name": info.name, "version": info.version}),
        None => json!({"name": "t3-code", "version": "unknown"}),
    };
    let mut params = Map::new();
    params.insert("protocolVersion".to_owned(), json!(2));
    params.insert("info".to_owned(), info);
    params.insert("capabilities".to_owned(), Value::Object(v2_capabilities));
    if let Some(meta) = &request.meta {
        params.insert("_meta".to_owned(), meta.clone());
    }
    params.insert(
        "clientCapabilities".to_owned(),
        capabilities
            .map(client_capabilities_value)
            .unwrap_or_else(|| json!({})),
    );
    if let Some(info) = &request.client_info {
        params.insert(
            "clientInfo".to_owned(),
            json!({"name": info.name, "version": info.version}),
        );
    }
    Value::Object(params)
}

pub fn normalize_initialize_response(response: Value) -> InitializeResponse {
    if response.get("info").is_some() {
        let normalized = normalize_v2_initialize(&response);
        InitializeResponse {
            generation: ProtocolGeneration::V2,
            ..normalized
        }
    } else {
        InitializeResponse {
            protocol_version: response
                .get("protocolVersion")
                .and_then(Value::as_i64)
                .unwrap_or(1),
            agent_capabilities: response
                .get("agentCapabilities")
                .cloned()
                .unwrap_or(json!({})),
            auth_methods: response
                .get("authMethods")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            agent_info: response
                .get("agentInfo")
                .cloned()
                .filter(|value| !value.is_null()),
            meta: response.get("_meta").cloned(),
            generation: ProtocolGeneration::V1,
        }
    }
}

pub fn normalize_session_notification(
    mut value: Value,
    generation: Option<ProtocolGeneration>,
) -> Value {
    if generation == Some(ProtocolGeneration::V1) {
        return normalize_v1_session(value);
    }
    let update = value.get("update").cloned().unwrap_or(Value::Null);
    let tag = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let normalized_update = match tag.as_str() {
        "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" => {
            let mut update = update;
            if let Some(content) = update.get("content").cloned() {
                update["content"] = normalize_content_block(&content);
            }
            update
        }
        "user_message" | "agent_message" | "agent_thought" => {
            let mut update = update;
            if let Some(content) = update.get("content") {
                if content.is_null() {
                    update["content"] = Value::Null;
                } else if let Some(items) = content.as_array() {
                    update["content"] =
                        Value::Array(items.iter().map(normalize_content_block).collect());
                }
            }
            update
        }
        "tool_call_update" => normalize_tool_call_update(&update),
        "tool_call_content_chunk" => {
            let mut update = update;
            if let Some(content) = update.get("content").cloned() {
                update["content"] = normalize_tool_content(&content);
            }
            update
        }
        "config_option_update" => {
            let mut update = update;
            if let Some(options) = update.get("configOptions").and_then(Value::as_array) {
                update["configOptions"] = Value::Array(normalize_config_options(options));
            }
            update
        }
        "available_commands_update" => normalize_commands(update),
        "state_update"
        | "terminal_update"
        | "terminal_output_chunk"
        | "plan_update"
        | "plan_removed"
        | "session_info_update"
        | "usage_update"
        | "compaction_update"
        | "compaction_summary_chunk"
        | "plan"
        | "current_mode_update" => update,
        _ => json!({
            "sessionUpdate": "_t3_unknown",
            "originalSessionUpdate": tag,
            "raw": update,
        }),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("update".to_owned(), normalized_update);
    }
    value
}

pub fn normalize_permission_request(request: Value, context: &RequestContext) -> Value {
    if request.get("toolCall").is_some() {
        return request;
    }
    let subject = request.get("subject").cloned();
    let tool_call = subject
        .as_ref()
        .and_then(|subject| subject.get("toolCall"))
        .map(normalize_tool_call_update)
        .unwrap_or_else(|| {
            let tool_call_id = subject
                .as_ref()
                .filter(|subject| subject.get("type").and_then(Value::as_str) == Some("command"))
                .and_then(|subject| subject.get("toolCallId"))
                .and_then(Value::as_str)
                .unwrap_or(context.request_id.as_str());
            let kind = if subject
                .as_ref()
                .and_then(|subject| subject.get("type"))
                .and_then(Value::as_str)
                == Some("command")
            {
                "execute"
            } else {
                "other"
            };
            json!({
                "toolCallId": tool_call_id,
                "title": request.get("title").cloned().unwrap_or(Value::Null),
                "kind": kind,
            })
        });
    let mut out = Map::new();
    if let Some(session_id) = request.get("sessionId") {
        out.insert("sessionId".to_owned(), session_id.clone());
    }
    if let Some(title) = request.get("title") {
        out.insert("title".to_owned(), title.clone());
    }
    if let Some(description) = request.get("description") {
        out.insert("description".to_owned(), description.clone());
    }
    if let Some(subject) = subject {
        out.insert("subject".to_owned(), subject);
    }
    out.insert("toolCall".to_owned(), tool_call);
    if let Some(options) = request.get("options") {
        out.insert("options".to_owned(), options.clone());
    }
    if let Some(meta) = request.get("_meta") {
        out.insert("_meta".to_owned(), meta.clone());
    }
    Value::Object(out)
}

pub fn with_v2_mcp_servers(mut payload: Value) -> Value {
    let Some(servers) = payload.get_mut("mcpServers").and_then(Value::as_array_mut) else {
        return payload;
    };
    for server in servers {
        let Some(object) = server.as_object_mut() else {
            continue;
        };
        if !object.contains_key("type") && object.contains_key("command") {
            object.insert("type".to_owned(), json!("stdio"));
        }
    }
    payload
}

pub fn v2_config_option_params(mut payload: Value) -> Value {
    let type_name = if payload.get("type").and_then(Value::as_str) == Some("boolean") {
        "boolean"
    } else {
        "id"
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert("type".to_owned(), json!(type_name));
    }
    payload
}

pub fn normalize_v2_session_setup(response: &Value) -> Value {
    let mut out = Map::new();
    if let Some(session_id) = response.get("sessionId") {
        out.insert("sessionId".to_owned(), session_id.clone());
    }
    if let Some(options) = response.get("configOptions").and_then(Value::as_array) {
        out.insert(
            "configOptions".to_owned(),
            Value::Array(normalize_config_options(options)),
        );
    }
    if let Some(meta) = response.get("_meta") {
        out.insert("_meta".to_owned(), meta.clone());
    }
    Value::Object(out)
}

pub fn idle_prompt_response(update: &Value) -> Value {
    let stop_reason = update
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let mut out = Map::new();
    out.insert("stopReason".to_owned(), json!(stop_reason));
    if let Some(usage) = update.get("usage").filter(|usage| !usage.is_null()) {
        out.insert("usage".to_owned(), usage.clone());
    }
    if let Some(meta) = update.get("_meta") {
        out.insert("_meta".to_owned(), meta.clone());
    }
    Value::Object(out)
}

fn client_capabilities_value(caps: &ClientCapabilities) -> Value {
    let mut out = Map::new();
    if caps.fs_read_text_file.is_some() || caps.fs_write_text_file.is_some() {
        out.insert(
            "fs".to_owned(),
            json!({
                "readTextFile": caps.fs_read_text_file.unwrap_or(false),
                "writeTextFile": caps.fs_write_text_file.unwrap_or(false),
            }),
        );
    }
    if let Some(terminal) = caps.terminal {
        out.insert("terminal".to_owned(), json!(terminal));
    }
    if let Some(auth_terminal) = caps.auth_terminal {
        out.insert("auth".to_owned(), json!({"terminal": auth_terminal}));
    }
    if let Some(elicitation) = &caps.elicitation {
        out.insert("elicitation".to_owned(), elicitation.clone());
    }
    if let Some(meta) = &caps.meta {
        out.insert("_meta".to_owned(), meta.clone());
    }
    Value::Object(out)
}

fn normalize_v2_initialize(response: &Value) -> InitializeResponse {
    let session = response.pointer("/capabilities/session");
    let prompt = session.and_then(|session| session.get("prompt"));
    let mcp = session.and_then(|session| session.get("mcp"));
    let mut agent_capabilities = Map::new();
    agent_capabilities.insert(
        "loadSession".to_owned(),
        json!(session.is_some() && !session.unwrap().is_null()),
    );
    agent_capabilities.insert(
        "promptCapabilities".to_owned(),
        json!({
            "image": prompt.and_then(|prompt| prompt.get("image")).is_some_and(|value| !value.is_null()),
            "audio": prompt.and_then(|prompt| prompt.get("audio")).is_some_and(|value| !value.is_null()),
            "embeddedContext": prompt.and_then(|prompt| prompt.get("embeddedContext")).is_some_and(|value| !value.is_null()),
        }),
    );
    agent_capabilities.insert(
        "mcpCapabilities".to_owned(),
        json!({
            "stdio": mcp.and_then(|mcp| mcp.get("stdio")).is_some_and(|value| !value.is_null()),
            "http": mcp.and_then(|mcp| mcp.get("http")).is_some_and(|value| !value.is_null()),
            "acp": mcp.and_then(|mcp| mcp.get("acp")).is_some_and(|value| !value.is_null()),
        }),
    );
    if session.is_some_and(|session| !session.is_null()) {
        let mut session_capabilities = Map::new();
        session_capabilities.insert("list".to_owned(), json!({}));
        session_capabilities.insert("resume".to_owned(), json!({}));
        session_capabilities.insert("close".to_owned(), json!({}));
        if session
            .unwrap()
            .get("delete")
            .is_some_and(|value| !value.is_null())
        {
            session_capabilities.insert("delete".to_owned(), json!({}));
        }
        if session
            .unwrap()
            .get("fork")
            .is_some_and(|value| !value.is_null())
        {
            session_capabilities.insert("fork".to_owned(), json!({}));
        }
        if session
            .unwrap()
            .get("additionalDirectories")
            .is_some_and(|value| !value.is_null())
        {
            session_capabilities.insert("additionalDirectories".to_owned(), json!({}));
        }
        agent_capabilities.insert(
            "sessionCapabilities".to_owned(),
            Value::Object(session_capabilities),
        );
    }
    if response
        .pointer("/capabilities/providers")
        .is_some_and(|value| !value.is_null())
    {
        agent_capabilities.insert("providers".to_owned(), json!({}));
    }
    let raw_methods = response.get("authMethods").and_then(Value::as_array);
    if raw_methods.is_some_and(|methods| !methods.is_empty()) {
        agent_capabilities.insert("auth".to_owned(), json!({"logout": {}}));
    }
    let auth_methods = raw_methods
        .map(|methods| methods.iter().filter_map(normalize_auth_method).collect())
        .unwrap_or_default();
    InitializeResponse {
        protocol_version: response
            .get("protocolVersion")
            .and_then(Value::as_i64)
            .unwrap_or(2),
        agent_capabilities: Value::Object(agent_capabilities),
        auth_methods,
        agent_info: response.get("info").cloned(),
        meta: response.get("_meta").cloned(),
        generation: ProtocolGeneration::V2,
    }
}

fn normalize_auth_method(method: &Value) -> Option<Value> {
    let kind = method.get("type").and_then(Value::as_str)?;
    let mut base = Map::new();
    base.insert(
        "id".to_owned(),
        method.get("methodId").cloned().unwrap_or(Value::Null),
    );
    base.insert(
        "name".to_owned(),
        method.get("name").cloned().unwrap_or(Value::Null),
    );
    if let Some(description) = method.get("description") {
        base.insert("description".to_owned(), description.clone());
    }
    if let Some(meta) = method.get("_meta") {
        base.insert("_meta".to_owned(), meta.clone());
    }
    match kind {
        "env_var" => {
            let vars = method.get("vars")?.as_array()?;
            if !vars
                .iter()
                .all(|var| var.get("name").and_then(Value::as_str).is_some())
            {
                return None;
            }
            base.insert("type".to_owned(), json!("env_var"));
            base.insert(
                "vars".to_owned(),
                Value::Array(
                    vars.iter()
                        .map(|var| {
                            let mut entry = Map::new();
                            entry.insert("name".to_owned(), var["name"].clone());
                            if let Some(label) = var.get("label").filter(|label| label.is_string())
                            {
                                entry.insert("label".to_owned(), label.clone());
                            }
                            Value::Object(entry)
                        })
                        .collect(),
                ),
            );
            if let Some(link) = method.get("link").filter(|link| link.is_string()) {
                base.insert("link".to_owned(), link.clone());
            }
            Some(Value::Object(base))
        }
        "agent" => {
            base.insert("type".to_owned(), json!("agent"));
            Some(Value::Object(base))
        }
        "terminal" => {
            base.insert("type".to_owned(), json!("terminal"));
            if let Some(args) = method.get("args").and_then(Value::as_array) {
                base.insert(
                    "args".to_owned(),
                    Value::Array(args.iter().filter(|arg| arg.is_string()).cloned().collect()),
                );
            }
            if let Some(env) = method.get("env").and_then(Value::as_array) {
                let mut record = Map::new();
                for variable in env {
                    let Some(name) = variable.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(value) = variable.get("value").and_then(Value::as_str) else {
                        continue;
                    };
                    record.insert(name.to_owned(), json!(value));
                }
                base.insert("env".to_owned(), Value::Object(record));
            }
            Some(Value::Object(base))
        }
        _ => None,
    }
}

fn normalize_content_block(block: &Value) -> Value {
    match block.get("type").and_then(Value::as_str) {
        Some("text" | "image" | "audio" | "resource_link" | "resource") => block.clone(),
        other => json!({
            "type": "_t3_unknown",
            "originalType": other.unwrap_or(""),
            "raw": block,
        }),
    }
}

fn normalize_tool_content(content: &Value) -> Value {
    match content.get("type").and_then(Value::as_str) {
        Some("content") => {
            let mut content = content.clone();
            if let Some(block) = content.get("content").cloned() {
                content["content"] = normalize_content_block(&block);
            }
            content
        }
        Some("terminal" | "diff") => content.clone(),
        other => json!({
            "type": "_t3_unknown",
            "originalType": other.unwrap_or(""),
            "raw": content,
        }),
    }
}

fn normalize_tool_call_update(update: &Value) -> Value {
    let mut out = Map::new();
    out.insert("sessionUpdate".to_owned(), json!("tool_call_update"));
    for key in [
        "toolCallId",
        "name",
        "title",
        "kind",
        "status",
        "locations",
        "rawInput",
        "rawOutput",
        "_meta",
    ] {
        if let Some(value) = update.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(content) = update.get("content") {
        if content.is_null() {
            out.insert("content".to_owned(), Value::Null);
        } else if let Some(items) = content.as_array() {
            out.insert(
                "content".to_owned(),
                Value::Array(items.iter().map(normalize_tool_content).collect()),
            );
        }
    }
    Value::Object(out)
}

fn normalize_commands(update: Value) -> Value {
    let Some(commands) = update.get("availableCommands").and_then(Value::as_array) else {
        return update;
    };
    let commands = commands
        .iter()
        .map(|command| {
            let mut command = command.clone();
            let keep = command.pointer("/input/type").and_then(Value::as_str) == Some("text");
            if let Some(object) = command.as_object_mut() {
                if keep {
                    // already a v2 text input
                } else {
                    object.insert("input".to_owned(), Value::Null);
                }
            }
            command
        })
        .collect();
    let mut update = update;
    update["availableCommands"] = Value::Array(commands);
    update
}

fn normalize_config_options(options: &[Value]) -> Vec<Value> {
    options.iter().filter_map(normalize_config_option).collect()
}

fn normalize_config_option(option: &Value) -> Option<Value> {
    let kind = option.get("type").and_then(Value::as_str)?;
    if kind != "boolean" && kind != "select" {
        return None;
    }
    let mut out = Map::new();
    out.insert("type".to_owned(), json!(kind));
    out.insert(
        "id".to_owned(),
        option.get("configId").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "name".to_owned(),
        option.get("name").cloned().unwrap_or(Value::Null),
    );
    if let Some(current) = option.get("currentValue") {
        out.insert("currentValue".to_owned(), current.clone());
    }
    if kind == "select"
        && let Some(options) = option.get("options")
    {
        out.insert("options".to_owned(), options.clone());
    }
    if let Some(description) = option.get("description") {
        out.insert("description".to_owned(), description.clone());
    }
    if let Some(category) = option.get("category") {
        out.insert("category".to_owned(), category.clone());
    }
    if let Some(meta) = option.get("_meta") {
        out.insert("_meta".to_owned(), meta.clone());
    }
    Some(Value::Object(out))
}

fn normalize_v1_session(value: Value) -> Value {
    if value
        .pointer("/update/sessionUpdate")
        .and_then(Value::as_str)
        != Some("available_commands_update")
    {
        return value;
    }
    let Some(commands) = value
        .pointer("/update/availableCommands")
        .and_then(Value::as_array)
    else {
        return value;
    };
    let commands: Vec<Value> = commands
        .iter()
        .map(|command| {
            let mut command = command.clone();
            let hint = command
                .pointer("/input/hint")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(object) = command.as_object_mut() {
                match hint {
                    Some(hint) => {
                        object.insert("input".to_owned(), json!({"type": "text", "hint": hint}));
                    }
                    None => {
                        object.remove("input");
                    }
                }
            }
            command
        })
        .collect();
    let mut value = value;
    value["update"]["availableCommands"] = Value::Array(commands);
    value
}
