use serde_json::json;
use turin_contracts::settings::default_server_settings;

use super::*;

#[test]
fn a_token_streaming_mode_decodes_as_paragraph_at_both_levels() {
    let settings = decode_settings_json(
        r#"{"responseStreamingMode":"token","projectSettingsOverrides":{"p1":{"responseStreamingMode":"token"}}}"#,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&settings.response_streaming_mode).unwrap(),
        json!("paragraph")
    );
}

#[test]
fn invalid_json_or_a_non_object_file_fails_to_decode() {
    assert!(decode_settings_json("{").is_err());
    assert!(decode_settings_json("[]").is_err());
}

#[test]
fn a_lenient_file_with_comments_and_trailing_commas_decodes() {
    let raw = "{\n  // a comment\n  \"enableProviderUpdateChecks\": false,\n}";
    let settings = decode_settings_json(raw).unwrap();
    assert!(!settings.enable_provider_update_checks);
}

#[test]
fn legacy_keys_must_be_objects_when_present() {
    assert!(decode_legacy_file(r#"{"providers":{"codex":{}}}"#).is_ok());
    assert!(decode_legacy_file(r#"{"providers":[]}"#).is_err());
    assert!(decode_legacy_file(r#"{"bitbucket":"x"}"#).is_err());
    assert!(decode_legacy_file(r#"{"other":1}"#).is_ok());
}

#[test]
fn defaults_serialize_to_an_empty_sparse_object() {
    let text = sparse_settings_json(&default_server_settings()).unwrap();
    assert_eq!(text, "{}\n");
}

#[test]
fn only_changed_fields_are_written_sparse() {
    let mut settings = default_server_settings();
    settings.enable_provider_update_checks = false;
    let text = sparse_settings_json(&settings).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value, json!({"enableProviderUpdateChecks": false}));
}

#[test]
fn atomic_keys_compare_as_whole_values() {
    let current =
        json!({"backgroundActivity": {"profile": "custom", "overrides": {"idleClientTtl": 1}}});
    let defaults = json!({"backgroundActivity": {"profile": "balanced", "overrides": {}}});
    assert_eq!(
        strip_default(&current, &defaults).unwrap(),
        current,
        "the whole atomic value is kept"
    );
}

#[test]
fn arrays_are_compared_whole() {
    assert_eq!(strip_default(&json!({"a": [1]}), &json!({"a": [1]})), None);
    assert_eq!(
        strip_default(&json!({"a": [1]}), &json!({"a": []})),
        Some(json!({"a": [1]}))
    );
}

#[test]
fn normalization_recomputes_legacy_project_maps() {
    let mut settings = default_server_settings();
    settings.project_settings_overrides = serde_json::from_value(json!({
        "p1": { "defaultAutoPull": true }
    }))
    .unwrap();
    let normalized = normalize_server_settings(settings).unwrap();
    let value = serde_json::to_value(&normalized).unwrap();
    assert_eq!(value["projectAutoPullOverrides"], json!({"p1": true}));
}

#[test]
fn a_disabled_fallback_provider_selects_another_enabled_default() {
    let mut settings = default_server_settings();
    settings.text_generation_model_selection = serde_json::from_value(json!({
        "instanceId": "codex_gone",
        "model": "gpt-5.4-mini",
    }))
    .unwrap();
    let resolved = resolve_text_generation_provider(settings);
    assert_ne!(
        resolved
            .text_generation_model_selection
            .instance_id
            .as_str(),
        "codex_gone"
    );
}
