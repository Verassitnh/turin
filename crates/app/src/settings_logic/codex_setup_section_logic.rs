//! Port of upstream/apps/web/src/components/settings/CodexSetupSection.logic.ts.

// Ported ahead of the views that consume it.
#![allow(dead_code)]

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexSetupMode {
    Managed,
    Existing,
}

pub fn read_codex_setup_mode(config: &Value) -> CodexSetupMode {
    if config
        .as_object()
        .and_then(|object| object.get("setupMode"))
        .and_then(Value::as_str)
        == Some("managed")
    {
        CodexSetupMode::Managed
    } else {
        CodexSetupMode::Existing
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_a_managed_setup_mode_reads_as_managed() {
        assert_eq!(
            read_codex_setup_mode(&json!({ "setupMode": "managed" })),
            CodexSetupMode::Managed
        );
        for config in [
            json!({ "setupMode": "existing" }),
            json!({ "setupMode": 3 }),
            json!({}),
            json!(null),
            json!("managed"),
            json!([]),
        ] {
            assert_eq!(
                read_codex_setup_mode(&config),
                CodexSetupMode::Existing,
                "{config}"
            );
        }
    }
}
