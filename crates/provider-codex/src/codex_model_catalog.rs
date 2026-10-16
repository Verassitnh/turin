//! Port of upstream/apps/server/src/provider/codexModelCatalog.ts.
//!
//! Codex's view of its manifest catalog. Codex discovers models from its app
//! server, so the catalog only reclassifies them, moves the default, and names
//! models a newer Codex build would unlock.

use std::sync::Arc;

use serde_json::Value;
use turin_contracts::TrimmedNonEmptyString;
use turin_provider_core::{
    model_catalog::{CatalogMatching, apply_model_catalog},
    model_manifest::{ManifestModelStatus, ProviderCatalog},
    semver::{compare_semver_versions, is_valid_semver},
    server_provider::{
        ServerProviderDraft, ServerProviderModelBadge, ServerProviderUpdateRequiredModel,
    },
};
use turin_shared::model::codex_model_family;

/// Dated Codex builds match their base model's catalog entry.
fn codex_matching() -> CatalogMatching {
    CatalogMatching {
        family: Some(Arc::new(|slug: &str| codex_model_family(slug).to_owned())),
    }
}

/// `adapter.codex.minVersion` of a catalog entry: a trimmed, non-empty string.
fn min_version(adapter: Option<&Value>) -> Option<String> {
    let value = adapter?.get("codex")?.get("minVersion")?.as_str()?;
    TrimmedNonEmptyString::new(value)
        .ok()
        .map(|value| value.as_str().to_owned())
}

/// Codex lists only the models its own build knows, so a model released after
/// the installed CLI never shows up. A current catalog entry with
/// `adapter.codex.minVersion` names that model, letting the picker say an update
/// unlocks it instead of leaving users to wonder where it is.
fn codex_update_required_models(
    catalog: Option<&ProviderCatalog>,
    draft: &ServerProviderDraft,
) -> Vec<ServerProviderUpdateRequiredModel> {
    let version = draft.version.as_ref().map(|version| {
        version
            .as_str()
            .strip_prefix('v')
            .unwrap_or(version.as_str())
    });
    let (Some(catalog), Some(version)) = (catalog, version) else {
        return Vec::new();
    };
    if version.is_empty() || !is_valid_semver(version) {
        return Vec::new();
    }
    let discovered: Vec<&str> = draft
        .models
        .iter()
        .map(|model| codex_model_family(model.slug.as_str()))
        .collect();
    catalog
        .models
        .iter()
        .filter_map(|entry| {
            if entry.status != ManifestModelStatus::Current
                || discovered.contains(&codex_model_family(&entry.slug))
            {
                return None;
            }
            let minimum = min_version(entry.adapter.as_ref())?;
            if !is_valid_semver(&minimum) || compare_semver_versions(version, &minimum).is_ge() {
                return None;
            }
            Some(ServerProviderUpdateRequiredModel {
                slug: TrimmedNonEmptyString::new(&entry.slug).ok()?,
                name: TrimmedNonEmptyString::new(&entry.name).ok()?,
                badge: entry.badge.map(|_| ServerProviderModelBadge::New),
                min_version: TrimmedNonEmptyString::new(&minimum).ok()?,
            })
        })
        .collect()
}

/// Reclassifies a Codex snapshot draft's models against its catalog.
pub fn apply_codex_model_catalog(
    draft: ServerProviderDraft,
    catalog: Option<&ProviderCatalog>,
) -> ServerProviderDraft {
    let update_required = codex_update_required_models(catalog, &draft);
    let mut draft = apply_model_catalog(draft, catalog, Some(&codex_matching()));
    if !update_required.is_empty() {
        draft.update_required_models = Some(update_required);
    }
    draft
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/codexModelCatalog.test.ts.

    use serde_json::json;
    use turin_provider_core::{
        model_manifest::ManifestBadge,
        server_provider::{
            ServerProviderAuth, ServerProviderAuthStatus, ServerProviderModel, ServerProviderState,
        },
    };

    use super::*;

    /// Synthetic models only; real Codex model data lives in model-manifest.json.
    fn model(slug: &str, is_legacy: bool) -> ServerProviderModel {
        ServerProviderModel {
            slug: TrimmedNonEmptyString::new(slug).unwrap(),
            name: TrimmedNonEmptyString::new("GPT Test").unwrap(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: false,
            is_default: None,
            is_legacy: is_legacy.then_some(true),
            capabilities: None,
        }
    }

    fn entry(
        slug: &str,
        status: ManifestModelStatus,
    ) -> turin_provider_core::model_manifest::ProviderCatalogModel {
        turin_provider_core::model_manifest::ProviderCatalogModel {
            slug: slug.to_owned(),
            name: slug.to_owned(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            status,
            badge: None,
            capabilities: None,
            adapter: None,
            profile_adapter: None,
        }
    }

    fn catalog() -> ProviderCatalog {
        let mut next = entry("gpt-next", ManifestModelStatus::Current);
        next.name = "GPT Next".to_owned();
        next.badge = Some(ManifestBadge::New);
        next.adapter = Some(json!({"codex": {"minVersion": "1.2.0"}}));
        let mut retired = entry("gpt-retired", ManifestModelStatus::Legacy);
        retired.adapter = Some(json!({"codex": {"minVersion": "1.2.0"}}));
        ProviderCatalog {
            default_chat_model: None,
            models: vec![
                next,
                entry("gpt-unversioned", ManifestModelStatus::Current),
                retired,
            ],
        }
    }

    fn draft(version: Option<&str>, models: Vec<ServerProviderModel>) -> ServerProviderDraft {
        use turin_provider_core::snapshot_probe::{
            BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
            build_server_provider,
        };
        build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: ServerProviderPresentation {
                display_name: "Codex".to_owned(),
                badge_label: None,
                show_interaction_mode_toggle: None,
                reports_context_window: None,
                supported_runtime_modes: None,
                requires_new_thread_for_model_change: None,
                supports_conversation_rollback: None,
            },
            enabled: true,
            checked_at: "2026-01-01T00:00:00.000Z".to_owned(),
            models,
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: ProviderProbeResult {
                installed: true,
                version: version.map(str::to_owned),
                status: ServerProviderState::Ready,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated),
                message: None,
                usage_limits: None,
            },
            maintenance: None,
        })
    }

    #[test]
    fn names_current_models_that_need_a_newer_codex_and_are_missing_from_discovery() {
        let result = apply_codex_model_catalog(draft(Some("1.1.9"), vec![]), Some(&catalog()));
        let required: Vec<_> = result
            .update_required_models
            .unwrap()
            .into_iter()
            .map(|model| {
                (
                    model.slug.as_str().to_owned(),
                    model.name.as_str().to_owned(),
                    model.badge,
                    model.min_version.as_str().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            required,
            vec![(
                "gpt-next".to_owned(),
                "GPT Next".to_owned(),
                Some(ServerProviderModelBadge::New),
                "1.2.0".to_owned()
            )]
        );

        let mut qualified = catalog();
        qualified.models[0].slug = "openai.gpt-next".to_owned();
        for result in [
            // The CLI is new enough.
            apply_codex_model_catalog(draft(Some("1.2.0"), vec![]), Some(&catalog())),
            // The CLI already lists the model, even under a qualified slug.
            apply_codex_model_catalog(
                draft(Some("1.1.9"), vec![model("openai.gpt-next", false)]),
                Some(&catalog()),
            ),
            // An unknown version cannot be compared.
            apply_codex_model_catalog(draft(None, vec![]), Some(&catalog())),
            // A qualified catalog slug still matches the discovered family.
            apply_codex_model_catalog(
                draft(Some("1.1.9"), vec![model("gpt-next", false)]),
                Some(&qualified),
            ),
        ] {
            assert_eq!(result.update_required_models, None);
        }
    }

    #[test]
    fn classifies_qualified_codex_families_without_changing_their_wire_ids() {
        let classified = apply_codex_model_catalog(
            draft(
                Some("1.2.0"),
                vec![
                    model("openai.gpt-test", true),
                    model("openai.gpt-retired", false),
                ],
            ),
            Some(&catalog()),
        );
        assert_eq!(
            classified
                .models
                .iter()
                .map(|entry| (
                    entry.slug.as_str().to_owned(),
                    entry.is_legacy.unwrap_or(false)
                ))
                .collect::<Vec<_>>(),
            vec![
                ("openai.gpt-test".to_owned(), false),
                ("openai.gpt-retired".to_owned(), true),
            ]
        );
    }
}
