//! Port of upstream/packages/provider-core/src/server/ModelCatalog.ts.
//!
//! The server owns the manifest (bundle, remote refresh, disk cache,
//! compatibility policies). A provider only sees its own catalog entry, so a
//! provider crate never depends on the manifest file or how it is fetched.
//! Reads never wait on the network.

use std::sync::Arc;

use turin_contracts::{TrimmedNonEmptyString, provider_instance::ProviderDriverKind};

use crate::{
    model_manifest::{
        ManifestModelStatus, ModelManifest, ProviderCatalog, ProviderCatalogModel,
        resolve_provider_catalog,
    },
    server_provider::{ServerProviderDraft, ServerProviderModel, ServerProviderModelBadge},
};

/// A catalog model as the snapshot model it presents.
pub fn catalog_server_model(
    catalog: &ProviderCatalog,
    entry: &ProviderCatalogModel,
) -> ServerProviderModel {
    ServerProviderModel {
        slug: slug_of(&entry.slug),
        name: slug_of(&entry.name),
        short_name: entry.short_name.as_deref().map(slug_of),
        sub_provider: entry.sub_provider.as_deref().map(slug_of),
        aliases: entry
            .aliases
            .as_ref()
            .map(|aliases| aliases.iter().map(|alias| slug_of(alias)).collect()),
        badge: entry.badge.map(|_| ServerProviderModelBadge::New),
        is_custom: false,
        is_default: (catalog.default_chat_model.as_deref() == Some(entry.slug.as_str()))
            .then_some(true),
        is_legacy: (entry.status == ManifestModelStatus::Legacy).then_some(true),
        capabilities: entry.capabilities.clone(),
    }
}

/// Maps a slug to the family the catalog names it by.
pub type FamilyFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// How a driver matches its discovered slugs to catalog entries.
#[derive(Clone, Default)]
pub struct CatalogMatching {
    /// Maps a slug to the family the catalog names it by, such as a dated Codex
    /// build to its base model. Defaults to the slug itself.
    pub family: Option<FamilyFn>,
}

fn family_of(matching: Option<&CatalogMatching>, slug: &str) -> String {
    matching
        .and_then(|matching| matching.family.as_ref())
        .map_or_else(|| slug.to_owned(), |family| family(slug))
}

/// Reclassifies built-in models as legacy or current from the catalog. Custom
/// models are user-defined and never reclassified.
pub fn classify_catalog_models(
    models: &[ServerProviderModel],
    catalog: Option<&ProviderCatalog>,
    matching: Option<&CatalogMatching>,
) -> Vec<ServerProviderModel> {
    models
        .iter()
        .map(|model| {
            if model.is_custom {
                return model.clone();
            }
            let entry = catalog.and_then(|catalog| {
                catalog
                    .models
                    .iter()
                    .find(|candidate| candidate.slug == model.slug.as_str())
                    .or_else(|| {
                        let family = family_of(matching, model.slug.as_str());
                        catalog
                            .models
                            .iter()
                            .find(|candidate| candidate.slug == family)
                    })
            });
            if entry.is_some_and(|entry| entry.status == ManifestModelStatus::Legacy) {
                let mut model = model.clone();
                model.is_legacy = Some(true);
                return model;
            }
            let mut model = model.clone();
            if model.is_legacy == Some(true) {
                model.is_legacy = None;
            }
            model
        })
        .collect()
}

/// Moves `isDefault` to the catalog's chat default when the catalog names one
/// the models include. Aliases that pointed at the old default move with the
/// flag so the shared "provider default" alias keeps resolving.
pub fn apply_catalog_default(
    models: &[ServerProviderModel],
    catalog: Option<&ProviderCatalog>,
    matching: Option<&CatalogMatching>,
) -> Vec<ServerProviderModel> {
    let Some(requested_slug) = catalog.and_then(|catalog| catalog.default_chat_model.as_deref())
    else {
        return models.to_vec();
    };
    let has_family = matching.is_some_and(|matching| matching.family.is_some());
    let slug = models
        .iter()
        .find(|model| model.slug.as_str() == requested_slug)
        .map(|model| model.slug.as_str().to_owned())
        .or_else(|| {
            if !has_family {
                return None;
            }
            let requested_family = family_of(matching, requested_slug);
            models
                .iter()
                .find(|model| {
                    !model.is_custom && family_of(matching, model.slug.as_str()) == requested_family
                })
                .map(|model| model.slug.as_str().to_owned())
        });
    let Some(slug) = slug else {
        return models.to_vec();
    };
    let Some(previous) = models
        .iter()
        .find(|model| model.is_default == Some(true) && model.slug.as_str() != slug)
    else {
        return models.to_vec();
    };
    let previous_slug = previous.slug.as_str().to_owned();
    let moved_aliases = previous.aliases.clone().unwrap_or_default();
    models
        .iter()
        .map(|model| {
            if model.slug.as_str() == previous_slug {
                let mut model = model.clone();
                model.is_default = None;
                model.aliases = None;
                return model;
            }
            if model.slug.as_str() == slug {
                let mut aliases: Vec<_> = Vec::new();
                for alias in model.aliases.iter().flatten().chain(moved_aliases.iter()) {
                    if !aliases.contains(alias) {
                        aliases.push(alias.clone());
                    }
                }
                let mut model = model.clone();
                model.is_default = Some(true);
                if !aliases.is_empty() {
                    model.aliases = Some(aliases);
                }
                return model;
            }
            model.clone()
        })
        .collect()
}

/// Reclassifies a snapshot draft's built-in models and moves its default from
/// the catalog. Drivers that announce models a newer CLI unlocks add
/// `updateRequiredModels` themselves; this clears a stale list.
pub fn apply_model_catalog(
    mut draft: ServerProviderDraft,
    catalog: Option<&ProviderCatalog>,
    matching: Option<&CatalogMatching>,
) -> ServerProviderDraft {
    draft.update_required_models = None;
    draft.models = apply_catalog_default(
        &classify_catalog_models(&draft.models, catalog, matching),
        catalog,
        matching,
    );
    draft
}

/// The model manifest as the provider packages read it: each driver's own
/// catalog entry, never the file or the fetch.
pub struct ModelCatalog {
    manifest: Arc<ModelManifest>,
}

impl ModelCatalog {
    pub fn new(manifest: Arc<ModelManifest>) -> Self {
        Self { manifest }
    }

    /// The driver's catalog from the manifest in memory (disk cache or
    /// bundle), or `None` when the manifest has no entry for it. Never fetches.
    pub async fn current(&self, driver_kind: &ProviderDriverKind) -> Option<ProviderCatalog> {
        resolve_provider_catalog(&self.manifest.current().await, driver_kind)
    }

    /// The same catalog from the bundled manifest; for fallbacks when remote
    /// data is invalid.
    pub fn bundled(&self, driver_kind: &ProviderDriverKind) -> Option<ProviderCatalog> {
        resolve_provider_catalog(
            &crate::model_manifest::bundled_model_manifest(),
            driver_kind,
        )
    }

    /// Starts a TTL-gated remote refresh in the background. Never blocks and
    /// never fails.
    pub fn refresh_in_background(&self) {
        self.manifest.refresh_in_background();
    }
}

fn slug_of(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("catalog slugs are non-blank")
}

#[cfg(test)]
mod tests {
    //! Port of upstream/packages/provider-core/src/server/ModelCatalog.test.ts.

    use pretty_assertions::assert_eq;

    use super::*;

    /// Covers catalog semantics with synthetic models. Real model data lives in
    /// the server's model-manifest.json and is not asserted here.
    fn model(slug: &str) -> ServerProviderModel {
        ServerProviderModel {
            slug: slug_of(slug),
            name: slug_of("Model Test"),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: false,
            is_default: None,
            is_legacy: None,
            capabilities: None,
        }
    }

    fn legacy_flag(mut model: ServerProviderModel, flag: Option<bool>) -> ServerProviderModel {
        model.is_legacy = flag;
        model
    }

    fn catalog_model(slug: &str, status: ManifestModelStatus) -> ProviderCatalogModel {
        ProviderCatalogModel {
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

    fn catalog(models: Vec<ProviderCatalogModel>, default_chat: Option<&str>) -> ProviderCatalog {
        ProviderCatalog {
            models,
            default_chat_model: default_chat.map(str::to_owned),
        }
    }

    /// Strips a vendor prefix, the way a driver maps qualified slugs to catalog families.
    fn prefix_family() -> CatalogMatching {
        CatalogMatching {
            family: Some(Arc::new(|slug: &str| {
                slug.strip_prefix("vendor.").unwrap_or(slug).to_owned()
            })),
        }
    }

    fn entry_flags(models: &[ServerProviderModel]) -> Vec<(String, bool)> {
        models
            .iter()
            .map(|entry| {
                (
                    entry.slug.as_str().to_owned(),
                    entry.is_legacy.unwrap_or(false),
                )
            })
            .collect()
    }

    #[test]
    fn classify_flags_only_known_legacy_models_clears_stale_flags_and_skips_custom_models() {
        let catalog = catalog(
            vec![catalog_model("old-model", ManifestModelStatus::Legacy)],
            None,
        );
        let mut custom = model("my-own-model");
        custom.is_custom = true;
        custom.is_legacy = Some(true);
        let models = vec![
            model("current-a"),
            // Stale flag from a previous classification pass must be cleared.
            legacy_flag(model("current-b"), Some(true)),
            model("old-model"),
            // Custom models are user-defined and never reclassified.
            custom,
        ];
        assert_eq!(
            entry_flags(&classify_catalog_models(&models, Some(&catalog), None)),
            vec![
                ("current-a".to_owned(), false),
                ("current-b".to_owned(), false),
                ("old-model".to_owned(), true),
                ("my-own-model".to_owned(), true),
            ]
        );
    }

    #[test]
    fn classify_matches_qualified_slugs_through_the_family_without_changing_their_ids() {
        let catalog = catalog(
            vec![catalog_model("model-old", ManifestModelStatus::Legacy)],
            None,
        );
        let models = vec![
            legacy_flag(model("vendor.model-test"), Some(true)),
            model("vendor.model-old"),
        ];
        assert_eq!(
            entry_flags(&classify_catalog_models(
                &models,
                Some(&catalog),
                Some(&prefix_family())
            )),
            vec![
                ("vendor.model-test".to_owned(), false),
                ("vendor.model-old".to_owned(), true),
            ]
        );
    }

    #[test]
    fn classify_keeps_discovered_models_current_when_the_driver_has_no_catalog() {
        assert_eq!(
            classify_catalog_models(&[legacy_flag(model("new"), Some(true))], None, None),
            vec![model("new")]
        );
    }

    #[test]
    fn apply_default_moves_the_default_flag_and_its_aliases_to_the_catalog_chat_default() {
        let catalog = catalog(
            vec![catalog_model("model-new", ManifestModelStatus::Current)],
            Some("model-new"),
        );
        let mut old = model("model-old");
        old.is_default = Some(true);
        old.aliases = Some(vec![slug_of("provider-default")]);
        let models = vec![old, model("model-new")];

        let mut new = model("model-new");
        new.is_default = Some(true);
        new.aliases = Some(vec![slug_of("provider-default")]);
        assert_eq!(
            apply_catalog_default(&models, Some(&catalog), None),
            vec![model("model-old"), new]
        );
        // The account does not offer the catalog default: keep the runtime's choice.
        assert_eq!(
            apply_catalog_default(&models[..1], Some(&catalog), None),
            models[..1].to_vec()
        );
    }

    #[test]
    fn apply_default_keeps_an_empty_aliases_array_the_new_default_already_has() {
        // Upstream spreads the model and only overwrites `aliases` when the merged
        // list is non-empty, so a model's own `aliases: []` survives.
        let catalog = catalog(
            vec![catalog_model("model-new", ManifestModelStatus::Current)],
            Some("model-new"),
        );
        let mut old = model("model-old");
        old.is_default = Some(true);
        let mut new = model("model-new");
        new.aliases = Some(Vec::new());
        let defaulted = apply_catalog_default(&[old, new], Some(&catalog), None);
        assert_eq!(defaulted[1].is_default, Some(true));
        assert_eq!(defaulted[1].aliases, Some(Vec::new()));
        // The previous default loses its aliases key entirely.
        assert_eq!(defaulted[0].aliases, None);
    }

    #[test]
    fn apply_default_resolves_the_catalog_default_to_a_qualified_discovered_model() {
        let catalog = catalog(Vec::new(), Some("model-test"));
        let mut old = model("vendor.model-old");
        old.is_default = Some(true);
        let models = vec![old, model("vendor.model-test")];
        let defaulted = apply_catalog_default(&models, Some(&catalog), Some(&prefix_family()))
            .into_iter()
            .find(|entry| entry.is_default == Some(true))
            .map(|entry| entry.slug.into_inner());
        assert_eq!(defaulted.as_deref(), Some("vendor.model-test"));
    }

    #[test]
    fn catalog_server_model_presents_the_catalog_entry() {
        let catalog = catalog(
            vec![catalog_model("model-a", ManifestModelStatus::Current)],
            Some("model-a"),
        );
        let mut entry = catalog_model("model-a", ManifestModelStatus::Current);
        entry.badge = Some(crate::model_manifest::ManifestBadge::New);
        let presented = catalog_server_model(&catalog, &entry);
        assert_eq!(presented.badge, Some(ServerProviderModelBadge::New));
        assert_eq!(presented.is_default, Some(true));
        assert_eq!(presented.is_legacy, None);
        assert!(!presented.is_custom);
    }
}
