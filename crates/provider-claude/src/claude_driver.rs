//! Port of `ClaudeAdapterV2Driver` / `createClaudeAdapterV2` in
//! upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts,
//! on the provider-core `ProviderAdapterDriver` SPI.
//!
//! The model catalog, attachments directory, and runtime instructions come
//! from the orchestrator. Upstream reads them from services that live outside
//! this crate (the manifest, `ServerConfig`, and the runtime instruction text).
use crate::{
    claude_model_catalog::ClaudeModelCatalog, claude_provider_adapter::ClaudeProviderAdapter,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};
use turin_contracts::{provider_instance::ProviderDriverKind, settings::ClaudeSettings};
use turin_provider_core::{
    adapter_driver::{
        ProviderAdapterDriver, ProviderAdapterDriverCreateError, ProviderAdapterDriverCreateInput,
    },
    instance_environment::merge_provider_instance_environment,
    path_expansion::expand_home_path,
    provider_adapter::ProviderAdapterV2,
};

pub struct ClaudeDriver {
    kind: ProviderDriverKind,
    catalog: ClaudeModelCatalog,
    attachments_dir: Option<PathBuf>,
    runtime_instructions: String,
}

impl ClaudeDriver {
    pub fn new(
        catalog: ClaudeModelCatalog,
        attachments_dir: Option<PathBuf>,
        runtime_instructions: String,
    ) -> Self {
        Self {
            kind: driver_kind(),
            catalog,
            attachments_dir,
            runtime_instructions,
        }
    }
}

fn driver_kind() -> ProviderDriverKind {
    ProviderDriverKind::new("claudeAgent").expect("valid Claude driver")
}

fn decode(raw: &Value) -> Result<ClaudeSettings, String> {
    serde_json::from_value(raw.clone()).map_err(|error| error.to_string())
}

#[async_trait]
impl ProviderAdapterDriver for ClaudeDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn default_config(&self) -> Value {
        // `DEFAULT_CLAUDE_SETTINGS` is the schema decode of `{}`.
        serde_json::to_value(decode(&json!({})).expect("empty Claude config decodes"))
            .expect("Claude settings serialize")
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        decode(raw).and_then(|settings| {
            serde_json::to_value(settings).map_err(|error| error.to_string())
        })
    }

    async fn create(
        &self,
        input: ProviderAdapterDriverCreateInput,
    ) -> Result<Arc<dyn ProviderAdapterV2>, ProviderAdapterDriverCreateError> {
        let fail = |detail: String| ProviderAdapterDriverCreateError {
            driver: self.kind.clone(),
            instance_id: input.instance_id.clone(),
            detail: format!("Failed to create Claude Agent SDK adapter. {detail}"),
            cause: None,
        };
        let mut settings = decode(&input.config).map_err(fail)?;
        settings.enabled = input.enabled;

        let home = std::env::var("HOME").unwrap_or_default();
        let host: HashMap<String, String> = std::env::vars().collect();
        let merged = merge_provider_instance_environment(Some(&input.environment), &host, &home);
        let binary = expand_home_path(settings.binary_path.as_str(), &home);
        settings.binary_path = turin_contracts::base_schemas::TrimmedString::new(binary);

        let environment: BTreeMap<String, String> = merged.into_iter().collect();
        Ok(Arc::new(ClaudeProviderAdapter::new(
            input.instance_id,
            settings,
            self.catalog.clone(),
            Some(environment),
            self.attachments_dir.clone(),
            self.runtime_instructions.clone(),
        )))
    }
}
