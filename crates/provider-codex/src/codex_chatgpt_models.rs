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
    chat_gpt_models_at(client, CATALOG_URL, access_token, native).await
}

/// `chat_gpt_models` against another catalog URL. Tests point it at a mock server.
pub async fn chat_gpt_models_at(
    client: &reqwest::Client,
    url: &str,
    access_token: &str,
    native: &[ServerProviderModel],
) -> Result<Vec<ServerProviderModel>, ChatGptModelsError> {
    let response = client
        .get(url)
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

    /// Serves `catalog` for a bearer token and the recorded requests, on loopback.
    async fn serve(
        status: u16,
        catalogs: Vec<(&'static str, serde_json::Value)>,
    ) -> (
        String,
        std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use crate::codex_chatgpt_auth::mini_http;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}/v1/models",
            listener.local_addr().unwrap().port()
        );
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let requests = std::sync::Arc::clone(&seen);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let Ok(request) = mini_http::read_request(&mut socket).await else {
                    continue;
                };
                let authorization = request
                    .headers
                    .get("authorization")
                    .cloned()
                    .unwrap_or_default();
                requests.lock().push(authorization.clone());
                let body = catalogs
                    .iter()
                    .find(|(token, _)| format!("Bearer {token}") == authorization)
                    .map(|(_, body)| body.to_string())
                    .unwrap_or_else(|| "{}".to_owned());
                let _ = mini_http::write_response(&mut socket, status, "OK", &[], body.as_bytes())
                    .await;
            }
        });
        (url, seen, task)
    }

    #[tokio::test]
    async fn uses_each_selected_profiles_token_and_the_servers_visible_catalog_order_and_names() {
        let (url, requests, task) = serve(
            200,
            vec![
                (
                    "account-a",
                    serde_json::json!({"models": [
                        {"slug": "second", "display_name": "Second from OpenAI", "visibility": "list"},
                        {"slug": "hidden", "display_name": "Hidden", "visibility": "hidden"},
                        {"slug": "first", "display_name": "First from OpenAI", "visibility": "list"},
                    ]}),
                ),
                (
                    "account-b",
                    serde_json::json!({"models": [
                        {"slug": "account-b-only", "display_name": "B", "visibility": "list"},
                    ]}),
                ),
            ],
        )
        .await;
        let client = reqwest::Client::new();
        let mut first = native_model("first");
        first.name = TrimmedNonEmptyString::new("Cached first").unwrap();
        first.is_custom = false;
        first.capabilities = Some(turin_contracts::model::ModelCapabilities {
            option_descriptors: Some(Vec::new()),
        });
        let mut other = native_model("not-entitled");
        other.name = TrimmedNonEmptyString::new("Cached other").unwrap();
        other.is_custom = false;
        let native = [first, other];

        let a = chat_gpt_models_at(&client, &url, "account-a", &native)
            .await
            .unwrap();
        assert_eq!(
            a.iter()
                .map(|m| (m.slug.as_str(), m.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("second", "Second from OpenAI"),
                ("first", "First from OpenAI")
            ]
        );
        assert_eq!(a[1].capabilities, native[0].capabilities);
        assert_eq!(a[0].capabilities, None);
        let b = chat_gpt_models_at(&client, &url, "account-b", &native)
            .await
            .unwrap();
        assert_eq!(
            b.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
            ["account-b-only"]
        );
        assert_eq!(*requests.lock(), ["Bearer account-a", "Bearer account-b"]);
        task.abort();
    }

    #[tokio::test]
    async fn does_not_present_a_cached_catalog_as_account_entitlements_when_discovery_fails() {
        let (url, _, task) = serve(503, Vec::new()).await;
        let error = chat_gpt_models_at(&reqwest::Client::new(), &url, "account-a", &[])
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ChatGptModelsError::Catalog(ChatGptCatalogError { status: 503 })
        ));
        task.abort();
    }
}
