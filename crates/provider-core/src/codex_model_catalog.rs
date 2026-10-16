//! Port of upstream/apps/server/src/provider/codexModelCatalog.ts
//!
//! Codex's view of its manifest catalog. Codex discovers models from its app
//! server, so the catalog only reclassifies them, moves the default, and names
//! models a newer Codex build would unlock.

use std::{collections::HashSet, sync::Arc};

use serde_json::Value;
use turin_contracts::{TrimmedNonEmptyString, js_trim};
use turin_shared::model::codex_model_family;

use crate::{
    model_catalog::{CatalogMatching, apply_model_catalog},
    model_manifest::{ManifestModelStatus, ProviderCatalog},
    semver::{compare_semver_versions, is_valid_semver},
    server_provider::{
        ServerProviderDraft, ServerProviderModelBadge, ServerProviderUpdateRequiredModel,
    },
};

/// Dated Codex builds match their base model's catalog entry.
fn codex_matching() -> CatalogMatching {
    CatalogMatching {
        family: Some(Arc::new(|slug: &str| codex_model_family(slug).to_owned())),
    }
}

/// `decodeCodexModelAdapter(entry.adapter ?? {})?.codex?.minVersion`: a
/// `TrimmedNonEmptyString`, so a blank or non-string value fails the decode and
/// reads as absent.
fn codex_min_version(adapter: Option<&Value>) -> Option<TrimmedNonEmptyString> {
    let codex = adapter?.as_object()?.get("codex")?.as_object()?;
    let min_version = codex.get("minVersion")?.as_str()?;
    TrimmedNonEmptyString::new(js_trim(min_version)).ok()
}

/// Codex lists only the models its own build knows, so a model released after
/// the installed CLI never shows up. A current catalog entry with
/// `adapter.codex.minVersion` names that model, letting the picker say an update
/// unlocks it instead of leaving users to wonder where it is.
pub fn codex_update_required_models(
    catalog: Option<&ProviderCatalog>,
    draft: &ServerProviderDraft,
) -> Vec<ServerProviderUpdateRequiredModel> {
    let version = draft.version.as_ref().map(|version| {
        version
            .as_str()
            .strip_prefix('v')
            .unwrap_or(version.as_str())
    });
    let (Some(catalog), Some(version)) = (catalog, version.filter(|version| !version.is_empty()))
    else {
        return Vec::new();
    };
    if !is_valid_semver(version) {
        return Vec::new();
    }
    let discovered: HashSet<&str> = draft
        .models
        .iter()
        .map(|model| codex_model_family(model.slug.as_str()))
        .collect();
    catalog
        .models
        .iter()
        .filter_map(|entry| {
            if entry.status != ManifestModelStatus::Current
                || discovered.contains(codex_model_family(&entry.slug))
            {
                return None;
            }
            let min_version = codex_min_version(entry.adapter.as_ref())?;
            if !is_valid_semver(min_version.as_str()) {
                return None;
            }
            if compare_semver_versions(version, min_version.as_str()).is_ge() {
                return None;
            }
            Some(ServerProviderUpdateRequiredModel {
                slug: TrimmedNonEmptyString::new(&entry.slug).ok()?,
                name: TrimmedNonEmptyString::new(&entry.name).ok()?,
                badge: entry.badge.map(|_| ServerProviderModelBadge::New),
                min_version,
            })
        })
        .collect()
}

/// Reclassifies a Codex snapshot draft's models against its catalog.
pub fn apply_codex_model_catalog(
    draft: ServerProviderDraft,
    catalog: Option<&ProviderCatalog>,
) -> ServerProviderDraft {
    let update_required_models = codex_update_required_models(catalog, &draft);
    let mut draft = apply_model_catalog(draft, catalog, Some(&codex_matching()));
    if !update_required_models.is_empty() {
        draft.update_required_models = Some(update_required_models);
    }
    draft
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/codexModelCatalog.test.ts.

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{
        model_manifest::{ManifestBadge, ProviderCatalogModel},
        server_provider::ServerProviderModel,
    };

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

    fn entry(slug: &str) -> ProviderCatalogModel {
        ProviderCatalogModel {
            slug: slug.to_owned(),
            name: slug.to_owned(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            status: ManifestModelStatus::Current,
            badge: None,
            capabilities: None,
            adapter: None,
            profile_adapter: None,
        }
    }

    fn catalog() -> ProviderCatalog {
        let mut next = entry("gpt-next");
        next.name = "GPT Next".to_owned();
        next.badge = Some(ManifestBadge::New);
        next.adapter = Some(json!({ "codex": { "minVersion": "1.2.0" } }));
        let mut retired = entry("gpt-retired");
        retired.status = ManifestModelStatus::Legacy;
        retired.adapter = Some(json!({ "codex": { "minVersion": "1.2.0" } }));
        ProviderCatalog {
            default_chat_model: None,
            models: vec![next, entry("gpt-unversioned"), retired],
        }
    }

    fn draft(version: Option<&str>, models: Vec<ServerProviderModel>) -> ServerProviderDraft {
        serde_json::from_value(json!({
            "enabled": true,
            "installed": true,
            "version": version,
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-01-01T00:00:00.000Z",
            "models": models,
            "slashCommands": [],
            "skills": [],
        }))
        .unwrap()
    }

    #[test]
    fn names_current_models_that_need_a_newer_codex_and_are_missing_from_discovery() {
        let catalog = catalog();
        let named = apply_codex_model_catalog(draft(Some("1.1.9"), vec![]), Some(&catalog));
        assert_eq!(
            serde_json::to_value(named.update_required_models).unwrap(),
            json!([{ "slug": "gpt-next", "name": "GPT Next", "badge": "new", "minVersion": "1.2.0" }])
        );

        let qualified_catalog = ProviderCatalog {
            models: vec![{
                let mut qualified = catalog.models[0].clone();
                qualified.slug = "openai.gpt-next".to_owned();
                qualified
            }],
            default_chat_model: None,
        };
        for result in [
            // The CLI is new enough.
            apply_codex_model_catalog(draft(Some("1.2.0"), vec![]), Some(&catalog)),
            // The CLI already lists the model, even under a qualified slug.
            apply_codex_model_catalog(
                draft(Some("1.1.9"), vec![model("openai.gpt-next", false)]),
                Some(&catalog),
            ),
            // An unknown version cannot be compared.
            apply_codex_model_catalog(draft(None, vec![]), Some(&catalog)),
            // A qualified catalog slug still matches the discovered family.
            apply_codex_model_catalog(
                draft(Some("1.1.9"), vec![model("gpt-next", false)]),
                Some(&qualified_catalog),
            ),
        ] {
            assert_eq!(result.update_required_models, None);
        }
    }

    #[test]
    fn strips_a_leading_v_and_skips_unparsable_versions() {
        let catalog = catalog();
        let prefixed = apply_codex_model_catalog(draft(Some("v1.1.9"), vec![]), Some(&catalog));
        assert_eq!(
            prefixed.update_required_models.map(|models| models.len()),
            Some(1)
        );
        let unparsable = apply_codex_model_catalog(draft(Some("nightly"), vec![]), Some(&catalog));
        assert_eq!(unparsable.update_required_models, None);
        let without_catalog = apply_codex_model_catalog(draft(Some("1.1.9"), vec![]), None);
        assert_eq!(without_catalog.update_required_models, None);
    }

    #[test]
    fn classifies_qualified_codex_families_without_changing_their_wire_ids() {
        let catalog = catalog();
        let classified = apply_codex_model_catalog(
            draft(
                Some("1.2.0"),
                vec![
                    model("openai.gpt-test", true),
                    model("openai.gpt-retired", false),
                ],
            ),
            Some(&catalog),
        );
        let flags: Vec<(String, bool)> = classified
            .models
            .iter()
            .map(|entry| {
                (
                    entry.slug.as_str().to_owned(),
                    entry.is_legacy.unwrap_or(false),
                )
            })
            .collect();
        assert_eq!(
            flags,
            vec![
                ("openai.gpt-test".to_owned(), false),
                ("openai.gpt-retired".to_owned(), true),
            ]
        );
    }
}
