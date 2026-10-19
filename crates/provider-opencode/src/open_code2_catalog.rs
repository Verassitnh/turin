//! Port of upstream/packages/provider-opencode/src/server/openCode2Catalog.ts.
//!
//! An OpenCode 2 server fronts several providers whose model lists overlap, so the
//! provider list is read on the same connection to name each model's provider. The
//! names are decoration: the provider read is bounded and optional, and a provider
//! that goes unnamed keeps its id.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::error::OpenCodeRuntimeError;
use crate::v2_client::V2Client;

/// How long the provider list may take before the models are listed without it.
pub const PROVIDER_NAME_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCode2Model {
    pub provider_id: String,
    pub id: String,
    pub name: String,
    pub provider_name: Option<String>,
    pub variants: Vec<String>,
}

#[derive(Deserialize)]
struct ModelEntry {
    #[serde(rename = "providerID")]
    provider_id: String,
    id: String,
    name: String,
    #[serde(default)]
    variants: Vec<VariantEntry>,
}

#[derive(Deserialize)]
struct VariantEntry {
    id: String,
}

#[derive(Deserialize)]
struct ProviderEntry {
    id: String,
    name: Option<String>,
}

/// Lists the server's models for `directory`, named by provider where the server says.
pub async fn load_open_code2_catalog(
    client: &V2Client,
    directory: &str,
) -> Result<Vec<OpenCode2Model>, OpenCodeRuntimeError> {
    let models = client.list_models(directory);
    let providers = async {
        tokio::time::timeout(PROVIDER_NAME_TIMEOUT, client.list_providers(directory))
            .await
            .ok()
            .and_then(Result::ok)
    };
    let (models, providers) = tokio::join!(models, providers);
    let names: HashMap<String, String> = providers
        .map(|located| {
            located
                .data
                .into_iter()
                .filter_map(|value| serde_json::from_value::<ProviderEntry>(value).ok())
                .filter_map(|provider| provider.name.map(|name| (provider.id, name)))
                .collect()
        })
        .unwrap_or_default();
    let models = models.map_err(catalog_error)?;
    let mut out = Vec::with_capacity(models.data.len());
    for value in models.data {
        let entry: ModelEntry = serde_json::from_value(value).map_err(catalog_error)?;
        out.push(OpenCode2Model {
            provider_name: names.get(&entry.provider_id).cloned(),
            provider_id: entry.provider_id,
            id: entry.id,
            name: entry.name,
            variants: entry
                .variants
                .into_iter()
                .map(|variant| variant.id)
                .collect(),
        });
    }
    Ok(out)
}

fn catalog_error(cause: impl std::error::Error + Send + Sync + 'static) -> OpenCodeRuntimeError {
    OpenCodeRuntimeError::new(
        "model.list",
        "The OpenCode server could not list its models.",
    )
    .with_cause(cause)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeResponse, FakeServer};

    #[tokio::test]
    async fn models_are_named_by_provider_and_keep_their_variants() {
        let server = FakeServer::spawn(|request| {
            if request.target.contains("/api/model") {
                return FakeResponse::json(
                    200,
                    r#"{"location":{},"data":[
                        {"providerID":"zen","id":"big","name":"Big","variants":[{"id":"high"},{"id":"low"}]},
                        {"providerID":"custom","id":"own","name":"Own","variants":[]}
                    ]}"#,
                );
            }
            FakeResponse::json(200, r#"{"location":{},"data":[{"id":"zen","name":"OpenCode Zen"}]}"#)
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let models = load_open_code2_catalog(&client, "/work").await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].provider_name.as_deref(), Some("OpenCode Zen"));
        assert_eq!(models[0].variants, ["high", "low"]);
        assert_eq!(
            models[1].provider_name, None,
            "an unnamed provider keeps only its id"
        );
    }

    #[tokio::test]
    async fn a_model_list_that_fails_is_the_catalog_error() {
        let server = FakeServer::spawn(|_| FakeResponse::json(500, "boom")).await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let err = load_open_code2_catalog(&client, "/work").await.unwrap_err();
        assert_eq!(err.operation, "model.list");
        assert_eq!(err.detail, "The OpenCode server could not list its models.");
    }
}
