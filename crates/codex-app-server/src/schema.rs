//! Port of upstream/packages/effect-codex-app-server/src/schema.ts
//! Also ports upstream/packages/effect-codex-app-server/src/_internal/shared.ts.
pub use crate::generated::types::*;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};
static SCHEMA: OnceLock<Value> = OnceLock::new();
static VALIDATORS: OnceLock<Mutex<HashMap<String, Arc<jsonschema::Validator>>>> = OnceLock::new();
fn schemas() -> &'static Value {
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("generated/codex.schema.json"))
            .expect("generated Codex schema")
    })
}
fn collect(
    name: &str,
    definitions: &Map<String, Value>,
    reached: &mut HashSet<String>,
) -> Result<(), String> {
    if !reached.insert(name.into()) {
        return Ok(());
    }
    let schema = definitions
        .get(name)
        .ok_or_else(|| format!("unknown Codex schema: {name}"))?;
    fn refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "$ref" {
                        if let Some(name) = value
                            .as_str()
                            .and_then(|s| s.strip_prefix("#/definitions/"))
                        {
                            out.push(name.into());
                        }
                    } else {
                        refs(value, out);
                    }
                }
            }
            Value::Array(array) => {
                for item in array {
                    refs(item, out)
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    refs(schema, &mut found);
    for target in found {
        collect(&target, definitions, reached)?;
    }
    Ok(())
}
pub fn validate_type(name: &str, value: &Value) -> Result<(), String> {
    let cache = VALIDATORS.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = cache.lock().get(name).cloned();
    let validator = if let Some(existing) = cached {
        existing
    } else {
        let definitions = schemas()
            .get("definitions")
            .and_then(Value::as_object)
            .expect("definitions");
        let mut reached = HashSet::new();
        collect(name, definitions, &mut reached)?;
        let subset: Map<String, Value> = reached
            .into_iter()
            .map(|key| {
                let schema = definitions.get(&key).expect("reachable schema").clone();
                (key, schema)
            })
            .collect();
        let schema = serde_json::json!({"$schema":"http://json-schema.org/draft-07/schema#","$ref":format!("#/definitions/{name}"),"definitions":subset});
        let compiled = Arc::new(jsonschema::validator_for(&schema).map_err(|e| e.to_string())?);
        cache.lock().insert(name.into(), compiled.clone());
        compiled
    };
    // `masked` keeps the keyword and path but replaces the instance, so a
    // rejected secret cannot leak into a protocol error or log line.
    validator
        .validate(value)
        .map_err(|error| error.masked().to_string())
}

pub fn validate_optional(name: Option<&str>, value: Option<&Value>) -> Result<(), String> {
    match (name, value) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err("unexpected payload".into()),
        (Some(_), None) => Err("missing payload".into()),
        (Some(name), Some(value)) => validate_type(name, value),
    }
}
