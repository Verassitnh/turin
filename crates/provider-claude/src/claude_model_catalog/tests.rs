//! Port of upstream/apps/server/src/provider/ClaudeModelCatalog.test.ts.
//!
//! Test policy (from upstream): adding or changing a real Claude model in
//! model-manifest.json must not add or update these tests. The synthetic
//! manifest below covers resolver behavior once.
use super::*;
use crate::claude_model_manifest::{
    has_valid_claude_manifest_adapters, resolve_claude_provider_catalog,
};
use serde_json::{Value, json};
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::model_manifest::decode_manifest;

fn manifest() -> Value {
    json!({
        "version": 1,
        "currentModels": {},
        "providers": {
            "claudeAgent": {
                "profiles": {
                    "synthetic": {
                        "capabilities": {
                            "optionDescriptors": [
                                {
                                    "id": "effort",
                                    "label": "Reasoning",
                                    "type": "select",
                                    "options": [{"id": "extreme", "label": "Extreme", "isDefault": true}]
                                },
                                {
                                    "id": "contextWindow",
                                    "label": "Context Window",
                                    "type": "select",
                                    "options": [
                                        {"id": "large", "label": "Large", "isDefault": true},
                                        {"id": "small", "label": "Small"}
                                    ]
                                }
                            ]
                        },
                        "adapter": {
                            "claudeCode": {
                                "effortMap": {"extreme": "high"},
                                "modelSuffixes": {"contextWindow": {"large": "[large]"}}
                            }
                        }
                    }
                },
                "models": [
                    {
                        "slug": "claude-synthetic-next",
                        "name": "Claude Synthetic Next",
                        "aliases": ["synthetic"],
                        "status": "current",
                        "profile": "synthetic",
                        "adapter": {"claudeCode": {"minVersion": "3.2.0"}}
                    }
                ]
            }
        }
    })
}

/// Resolves Claude's catalog the way the driver does: through its manifest catalog entry.
fn catalog_from_manifest(manifest: &Value) -> ClaudeModelCatalog {
    let typed = decode_manifest(manifest.clone());
    resolve_claude_model_catalog(
        typed
            .as_ref()
            .and_then(resolve_claude_provider_catalog)
            .as_ref(),
    )
}

fn selection(value: Value) -> ModelSelection {
    serde_json::from_value(value).expect("model selection decodes")
}

fn claude_selection(model: &str) -> ModelSelection {
    selection(json!({"instanceId": "claudeAgent", "model": model}))
}

fn with_claude(manifest: &Value, claude: Value) -> Value {
    let mut next = manifest.clone();
    next["providers"]["claudeAgent"] = claude;
    next
}

#[test]
fn resolves_capacity_from_selected_options_and_fixed_catalog_windows() {
    let source = manifest();
    let mut claude = source["providers"]["claudeAgent"].clone();
    claude["profiles"]["fixed"] = json!({
        "capabilities": {"optionDescriptors": []},
        "adapter": {"claudeCode": {"fixedContextWindowTokens": 64_000}}
    });
    claude["profiles"]["synthetic"]["adapter"] = json!({
        "claudeCode": {"contextWindowTokens": {"large": 1_000_000, "small": 32_000}}
    });
    claude["models"].as_array_mut().unwrap().push(json!({
        "slug": "fixed",
        "name": "Fixed",
        "status": "current",
        "profile": "fixed"
    }));
    let catalog = catalog_from_manifest(&with_claude(&source, claude));

    let base = claude_selection("synthetic");
    assert_eq!(
        resolve_claude_catalog_context_window_tokens(&catalog, Some(&base)),
        Some(1_000_000.0)
    );
    let small = selection(json!({
        "instanceId": "claudeAgent",
        "model": "synthetic",
        "options": [{"id": "contextWindow", "value": "small"}]
    }));
    assert_eq!(
        resolve_claude_catalog_context_window_tokens(&catalog, Some(&small)),
        Some(32_000.0)
    );
    assert_eq!(
        resolve_claude_catalog_context_window_tokens(&catalog, Some(&claude_selection("fixed"))),
        Some(64_000.0)
    );
    assert_eq!(
        resolve_claude_catalog_context_window_tokens(&catalog, Some(&claude_selection("custom"))),
        None
    );
}

#[test]
fn filters_models_at_runtime_version_boundaries_and_derives_the_upgrade_message() {
    let catalog = catalog_from_manifest(&manifest());
    assert!(resolve_claude_models_for_version(&catalog, Some("3.1.9")).is_empty());
    assert_eq!(
        resolve_claude_models_for_version(&catalog, Some("3.2.0"))
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["claude-synthetic-next"]
    );

    let required = resolve_claude_update_required_models(&catalog, Some("3.1.9"));
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].slug.as_str(), "claude-synthetic-next");
    assert_eq!(required[0].name.as_str(), "Claude Synthetic Next");
    assert_eq!(required[0].min_version.as_str(), "3.2.0");
    assert!(resolve_claude_update_required_models(&catalog, Some("3.2.0")).is_empty());

    assert_eq!(
        format_claude_version_upgrade_message(&catalog, Some("3.1.9")).as_deref(),
        Some(
            "Claude Code v3.1.9 is too old for Claude Synthetic Next. Upgrade to v3.2.0 or newer to access it."
        )
    );
}

#[test]
fn resolves_aliases_and_declarative_adapter_mappings() {
    let source = manifest();
    let mut claude = source["providers"]["claudeAgent"].clone();
    let next = claude["models"][0].clone();
    claude["models"] = json!([
        {
            "slug": "claude-synthetic-collision",
            "name": "Claude Synthetic Collision",
            "aliases": ["claude-synthetic-next"],
            "status": "current"
        },
        next
    ]);
    let catalog = catalog_from_manifest(&with_claude(&source, claude));

    assert_eq!(
        resolve_claude_model_slug(&catalog, "synthetic"),
        "claude-synthetic-next"
    );
    assert_eq!(
        resolve_claude_model_slug(&catalog, "claude-synthetic-next"),
        "claude-synthetic-next"
    );
    assert_eq!(
        normalize_claude_catalog_effort(&catalog, Some("extreme"), Some("synthetic")).as_deref(),
        Some("high")
    );
    assert_eq!(
        resolve_claude_catalog_api_model_id(&catalog, &claude_selection("synthetic")),
        "claude-synthetic-next[large]"
    );
}

#[test]
fn rejects_malformed_adapter_mappings() {
    let mut claude = manifest()["providers"]["claudeAgent"].clone();
    claude["profiles"]["synthetic"]["adapter"] =
        json!({"claudeCode": {"effortMap": {"extreme": 123}}});
    assert!(!has_valid_claude_manifest_adapters(&with_claude(
        &manifest(),
        claude
    )));
}

#[test]
fn appends_custom_models_with_their_own_descriptors_and_keeps_bare_slugs_opaque() {
    let catalog = scope_claude_model_catalog(
        &catalog_from_manifest(&manifest()),
        &[
            serde_json::from_value(json!("synthetic")).unwrap(),
            serde_json::from_value(json!({
                "slug": "claude-custom-tuned",
                "name": "Tuned",
                "capabilities": {
                    "optionDescriptors": [{
                        "id": "effort",
                        "label": "Reasoning",
                        "type": "select",
                        "options": [
                            {"id": "gentle", "label": "Gentle", "isDefault": true},
                            {"id": "brutal", "label": "Brutal"}
                        ]
                    }]
                }
            }))
            .unwrap(),
        ],
    );

    // The bare custom slug shadows the built-in alias, so it no longer resolves to it.
    assert_eq!(
        resolve_claude_model_slug(&catalog, "synthetic"),
        "synthetic"
    );
    assert_eq!(
        resolve_claude_catalog_effort(&catalog, Some("synthetic"), Some("extreme")),
        None
    );

    // The entry with descriptors resolves user-defined effort ids and passes
    // them through untouched (no effortMap, no model suffix).
    assert_eq!(
        resolve_claude_catalog_effort(&catalog, Some("claude-custom-tuned"), Some("brutal"))
            .as_deref(),
        Some("brutal")
    );
    assert_eq!(
        resolve_claude_catalog_effort(&catalog, Some("claude-custom-tuned"), Some("bogus"))
            .as_deref(),
        Some("gentle")
    );
    assert_eq!(
        normalize_claude_catalog_effort(&catalog, Some("brutal"), Some("claude-custom-tuned"))
            .as_deref(),
        Some("brutal")
    );
    let custom = selection(json!({
        "instanceId": "claudeAgent",
        "model": "claude-custom-tuned",
        "options": [{"id": "effort", "value": "brutal"}]
    }));
    assert_eq!(
        resolve_claude_catalog_api_model_id(&catalog, &custom),
        "claude-custom-tuned"
    );
    assert_eq!(
        resolve_claude_models_for_version(&catalog, Some("3.2.0"))
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["claude-synthetic-next", "claude-custom-tuned"]
    );
}
