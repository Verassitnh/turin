//! Text generation for thread titles (N3's `TextGeneration`) over the live
//! provider instances.
//!
//! The Claude driver's own instance reports text generation as unavailable
//! (T10d: "stays with N3"), so the `claudeAgent` instance is served here by
//! N3's `claude -p` runner, configured from the Claude settings the host
//! booted with. Other instances fall through to the registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::{ClaudeSettings, ServerSettings};
use turin_provider_claude::claude_model_catalog::{
    ClaudeModelCatalog, bundled_claude_model_catalog, resolve_claude_catalog_api_model_id,
};
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::text_generation::ProviderTextGeneration;
use turin_text_generation::claude_text_generation::{
    ClaudeModelResolver, ClaudeResolvedModel, ClaudeTextGenerationOptions, claude_text_generation,
};
use turin_text_generation::thread_title_links::{LinkLookup, SourceControlLinkResolver};
use turin_text_generation::{TextGeneration, TextGenerationInstances};

const CLAUDE_INSTANCE: &str = "claudeAgent";

struct BundledClaudeModels(&'static ClaudeModelCatalog);

impl ClaudeModelResolver for BundledClaudeModels {
    fn resolve(&self, selection: &ModelSelection) -> ClaudeResolvedModel {
        ClaudeResolvedModel {
            api_model_id: resolve_claude_catalog_api_model_id(self.0, selection),
            cli_effort: None,
            ultracode: false,
            thinking: None,
            fast_mode: false,
        }
    }
}

/// No source-control provider is wired to the app yet: links in a message are
/// not looked up for the title prompt.
struct NoLinks;

impl SourceControlLinkResolver for NoLinks {
    fn resolve_link(&self, _cwd: &str, _url: &url::Url) -> Option<LinkLookup> {
        None
    }
}

struct Instances {
    claude: Arc<dyn ProviderTextGeneration>,
    registry: Arc<ProviderInstanceRegistry>,
}

impl TextGenerationInstances for Instances {
    fn text_generation(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Option<Arc<dyn ProviderTextGeneration>> {
        if instance_id.as_str() == CLAUDE_INSTANCE {
            return Some(self.claude.clone());
        }
        self.registry.text_generation(instance_id)
    }
}

/// The Claude settings of the default instance in `settings`.
fn claude_settings(settings: &ServerSettings) -> ClaudeSettings {
    let config = ProviderInstanceId::new(CLAUDE_INSTANCE)
        .ok()
        .and_then(|id| settings.provider_instances.get(&id))
        .and_then(|instance| serde_json::to_value(instance).ok())
        .and_then(|value| value.get("config").cloned())
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(config)
        .unwrap_or_else(|_| serde_json::from_value(serde_json::json!({})).expect("defaults decode"))
}

pub fn build(settings: &ServerSettings, registry: Arc<ProviderInstanceRegistry>) -> TextGeneration {
    let claude = claude_text_generation(ClaudeTextGenerationOptions {
        settings: claude_settings(settings),
        environment: None::<BTreeMap<String, String>>,
        models: Arc::new(BundledClaudeModels(bundled_claude_model_catalog())),
        has_managed_mcp_config: Arc::new(|| false),
        home_dir: std::env::var("HOME").unwrap_or_default(),
    });
    TextGeneration::new(
        Arc::new(Instances {
            claude: Arc::new(claude),
            registry,
        }),
        Arc::new(NoLinks),
    )
}
