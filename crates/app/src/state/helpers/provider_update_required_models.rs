//! Port of upstream/packages/client-runtime/src/providerUpdateRequiredModels.ts

use std::cmp::Ordering;

use turin_contracts::js_trim;
use turin_contracts::model::provider_display_name;

use super::provider_types::{ServerProvider, ServerProviderUpdateRequiredModel};
use super::shared_support::compare_semver_versions;

// Name the thing the user updates; "Claude" alone reads like the app or model.
fn runtime_name(driver: &str) -> Option<&'static str> {
    match driver {
        "claudeAgent" => Some("Claude Code"),
        "codex" => Some("the Codex CLI"),
        _ => None,
    }
}

fn format_model_list(names: &[&str]) -> String {
    if names.len() <= 2 {
        return names.join(" and ");
    }
    let (last, rest) = names.split_last().expect("more than two names");
    format!("{}, and {last}", rest.join(", "))
}

/// Model picker notice for models the manifest announces but the installed
/// provider is too old to run, e.g. "Update Claude Code to v2.1.300 or newer to
/// use Claude Opus 6." With a search query, only gated models it matches are
/// named, so searching for one explains why it is missing. `None` when nothing
/// gated matches. Pass `""` for no search.
pub fn format_provider_update_required_notice(
    provider: &ServerProvider,
    search_query: &str,
) -> Option<String> {
    let query = js_trim(search_query).to_lowercase();
    let models: Vec<&ServerProviderUpdateRequiredModel> = provider
        .update_required_models
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|model| {
            query.is_empty()
                || model.name.to_lowercase().contains(&query)
                || model.slug.to_lowercase().contains(&query)
        })
        .collect();
    // The highest bar unlocks every listed model.
    let min_version = models
        .iter()
        .map(|model| model.min_version.as_str())
        .reduce(|highest, version| {
            if compare_semver_versions(version, highest) == Ordering::Greater {
                version
            } else {
                highest
            }
        })?;
    let driver = provider.driver.as_str();
    let provider_name = runtime_name(driver)
        .or_else(|| provider_display_name(driver))
        .unwrap_or(driver);
    let version = if min_version.starts_with('v') {
        min_version.to_owned()
    } else {
        format!("v{min_version}")
    };
    let names = format_model_list(
        &models
            .iter()
            .map(|model| model.name.as_str())
            .collect::<Vec<_>>(),
    );
    Some(format!(
        "Update {provider_name} to {version} or newer to use {names}."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

    fn provider(driver: &str, models: Option<&[(&str, &str, &str)]>) -> ServerProvider {
        let mut provider = ServerProvider::new(
            ProviderInstanceId::new(driver).unwrap(),
            ProviderDriverKind::new(driver).unwrap(),
        );
        provider.update_required_models = models.map(|models| {
            models
                .iter()
                .map(
                    |(slug, name, min_version)| ServerProviderUpdateRequiredModel {
                        slug: (*slug).to_owned(),
                        name: (*name).to_owned(),
                        min_version: (*min_version).to_owned(),
                    },
                )
                .collect()
        });
        provider
    }

    #[test]
    fn names_the_cli_and_the_version_that_unlocks_every_listed_model() {
        assert_eq!(
            format_provider_update_required_notice(
                &provider(
                    "claudeAgent",
                    Some(&[("a", "Model A", "2.1.9"), ("b", "Model B", "2.1.10")])
                ),
                ""
            )
            .as_deref(),
            Some("Update Claude Code to v2.1.10 or newer to use Model A and Model B.")
        );
        assert_eq!(
            format_provider_update_required_notice(&provider("codex", None), ""),
            None
        );
    }

    #[test]
    fn names_only_the_gated_models_a_search_matches() {
        let provider = provider(
            "claudeAgent",
            Some(&[
                ("claude-a", "Model A", "2.1.9"),
                ("claude-b", "Model B", "2.1.10"),
            ]),
        );
        assert_eq!(
            format_provider_update_required_notice(&provider, " model a ").as_deref(),
            Some("Update Claude Code to v2.1.9 or newer to use Model A.")
        );
        assert_eq!(
            format_provider_update_required_notice(&provider, "gpt"),
            None
        );
    }

    #[test]
    fn names_the_release_not_a_prerelease_of_it() {
        assert_eq!(
            format_provider_update_required_notice(
                &provider(
                    "codex",
                    Some(&[("a", "A", "2.1.0-beta"), ("b", "B", "2.1.0")])
                ),
                ""
            )
            .as_deref(),
            Some("Update the Codex CLI to v2.1.0 or newer to use A and B.")
        );
    }

    #[test]
    fn lists_three_or_more_names_with_an_oxford_comma() {
        assert_eq!(format_model_list(&["A", "B", "C"]), "A, B, and C");
    }
}
