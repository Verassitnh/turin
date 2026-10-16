use serde_json::{Map, Value};

pub fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

pub fn bounded_content_metadata(value: Option<&str>, maximum_length: usize) -> String {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| {
            if text.len() <= maximum_length {
                text.to_owned()
            } else {
                text[..maximum_length].to_owned()
            }
        })
        .unwrap_or_default()
}

/// Load-replay gate state carried by `AcpSessionRuntime` (Effect loop not ported here).
#[derive(Debug, Clone)]
pub struct SessionLoadGate {
    pub active: bool,
    pub session_id: String,
    pub last_activity_at_millis: Option<u64>,
    pub idle_gap_ms: u64,
    pub initialize_result: Value,
    pub candidate_config_options: Option<Value>,
}
