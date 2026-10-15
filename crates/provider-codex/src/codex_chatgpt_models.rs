//! Port of upstream/apps/server/src/provider/CodexChatGptModels.ts.
//!
//! For a ChatGPT account, the choices come from OpenAI's catalog. The native
//! `model/list` supplies capability metadata only.

use std::time::Duration;

use serde::Deserialize;
use turin_contracts::TrimmedNonEmptyString;
use turin_provider_core::server_provider::ServerProviderModel;

const CATALOG_URL: &str = "https://api.openai.com/v1/models";
const CATALOG_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ChatGPT model catalog request failed with status {status}")]
pub struct ChatGptCatalogError {
    pub status: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatGptModelsError {
    #[error(transparent)]
    Catalog(#[from] ChatGptCatalogError),
    #[error("ChatGPT model catalog request failed: {0}")]
    Transport(String),
    #[error("ChatGPT model catalog response was invalid: {0}")]
    Invalid(String),
}

/// One entry of the catalog. Empty `slug` or `display_name` is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogEntry {
    pub slug: String,
    pub display_name: String,
    pub visibility: String,
}

#[derive(Deserialize)]
struct Catalog {
    models: Vec<CatalogEntry>,
}

/// Parses the catalog body and applies the `NonEmptyString` rules.
pub fn parse_catalog(body: &str) -> Result<Vec<CatalogEntry>, ChatGptModelsError> {
    let catalog: Catalog = serde_json::from_str(body)
        .map_err(|error| ChatGptModelsError::Invalid(error.to_string()))?;
    for entry in &catalog.models {
        if entry.slug.is_empty() || entry.display_name.is_empty() {
            return Err(ChatGptModelsError::Invalid(
                "a model needs a non-empty slug and display_name".to_owned(),
            ));
        }
    }
    Ok(catalog.models)
}

/// The model choices for a ChatGPT account: the catalog's listed models,
/// with capabilities borrowed from the native model of the same slug.
pub fn chat_gpt_model_choices(
    catalog: &[CatalogEntry],
    native: &[ServerProviderModel],
) -> Result<Vec<ServerProviderModel>, ChatGptModelsError> {
    catalog
        .iter()
        .filter(|entry| entry.visibility == "list")
        .map(|entry| {
            let slug = TrimmedNonEmptyString::new(&entry.slug)
                .map_err(|error| ChatGptModelsError::Invalid(error.to_string()))?;
            let name = TrimmedNonEmptyString::new(&entry.display_name)
                .map_err(|error| ChatGptModelsError::Invalid(error.to_string()))?;
            let matched = native
                .iter()
                .find(|model| model.slug.as_str() == entry.slug);
            Ok(ServerProviderModel {
                slug,
                name,
                short_name: matched.and_then(|model| model.short_name.clone()),
                sub_provider: matched.and_then(|model| model.sub_provider.clone()),
                aliases: matched.and_then(|model| model.aliases.clone()),
                badge: matched.and_then(|model| model.badge),
                is_custom: false,
                is_default: matched.and_then(|model| model.is_default),
                is_legacy: matched.and_then(|model| model.is_legacy),
                capabilities: matched.and_then(|model| model.capabilities.clone()),
            })
        })
        .collect()
}

/// `chatGptModels`: fetches the account catalog with the access token, times out after 15 s.
pub async fn chat_gpt_models(
    client: &reqwest::Client,
    access_token: &str,
    native: &[ServerProviderModel],
) -> Result<Vec<ServerProviderModel>, ChatGptModelsError> {
    let response = client
        .get(CATALOG_URL)
        .bearer_auth(access_token)
        .timeout(CATALOG_TIMEOUT)
        .send()
        .await
        .map_err(|error| ChatGptModelsError::Transport(error.to_string()))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ChatGptCatalogError { status }.into());
    }
    let body = response
        .text()
        .await
        .map_err(|error| ChatGptModelsError::Transport(error.to_string()))?;
    let catalog = parse_catalog(&body)?;
    chat_gpt_model_choices(&catalog, native)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_model(slug: &str) -> ServerProviderModel {
        ServerProviderModel {
            slug: TrimmedNonEmptyString::new(slug).unwrap(),
            name: TrimmedNonEmptyString::new("Native").unwrap(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: true,
            is_default: Some(true),
            is_legacy: None,
            capabilities: None,
        }
    }

    #[test]
    fn only_listed_models_are_offered_with_the_catalog_name() {
        let catalog = parse_catalog(
            r#"{"models":[
                {"slug":"gpt-a","display_name":"GPT A","visibility":"list"},
                {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hide"}
            ]}"#,
        )
        .unwrap();
        let choices = chat_gpt_model_choices(&catalog, &[]).unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].slug.as_str(), "gpt-a");
        assert_eq!(choices[0].name.as_str(), "GPT A");
        assert!(!choices[0].is_custom);
    }

    #[test]
    fn native_metadata_is_kept_for_a_matching_slug_only() {
        let catalog = parse_catalog(
            r#"{"models":[
                {"slug":"gpt-a","display_name":"GPT A","visibility":"list"},
                {"slug":"gpt-b","display_name":"GPT B","visibility":"list"}
            ]}"#,
        )
        .unwrap();
        let choices = chat_gpt_model_choices(&catalog, &[native_model("gpt-a")]).unwrap();
        assert_eq!(
            choices[0].is_default,
            Some(true),
            "matched slug keeps native fields"
        );
        assert!(
            !choices[0].is_custom,
            "is_custom is always false for account choices"
        );
        assert_eq!(
            choices[1].is_default, None,
            "unmatched slug gets no native fields"
        );
        assert_eq!(choices[1].capabilities, None);
    }

    #[test]
    fn an_empty_slug_makes_the_whole_catalog_invalid() {
        let error =
            parse_catalog(r#"{"models":[{"slug":"","display_name":"x","visibility":"list"}]}"#)
                .unwrap_err();
        assert!(matches!(error, ChatGptModelsError::Invalid(_)));
    }

    #[test]
    fn a_body_without_models_is_invalid() {
        assert!(matches!(
            parse_catalog(r#"{"data":[]}"#),
            Err(ChatGptModelsError::Invalid(_))
        ));
    }
}
