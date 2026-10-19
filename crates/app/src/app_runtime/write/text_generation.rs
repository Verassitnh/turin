//! Text generation for thread titles (N3's `TextGeneration`) over the live
//! provider instances.
//!
//! The Claude and Cursor drivers' own instances report text generation as
//! unavailable (T10d: "stays with N3"), so every instance of those drivers is
//! served here by N3's `claude -p` and `cursor-agent acp` runners, configured
//! from that instance's settings at the time of each request. Other drivers
//! (Codex, OpenCode) carry their own text generation and fall through to the
//! registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::resolve_provider_instance_enabled;
use turin_provider_claude::claude_model_catalog::{
    ClaudeModelCatalog, bundled_claude_model_catalog, resolve_claude_catalog_api_model_id,
};
use turin_provider_core::host_process::HostProcess;
use turin_provider_core::instance_environment::merge_provider_instance_environment;
use turin_provider_core::provider_host_live::DesktopSettingsHandle;
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::text_generation::ProviderTextGeneration;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ReviewGuideGenerationInput, ReviewGuideGenerationResult, ThreadTitleGenerationInput,
    ThreadTitleGenerationResult,
};
use turin_provider_cursor::CursorSettings;
use turin_text_generation::claude_text_generation::{
    ClaudeModelResolver, ClaudeResolvedModel, ClaudeTextGenerationOptions, claude_text_generation,
};
use turin_text_generation::cursor_text_generation::{
    CursorTextGenerationOptions, cursor_text_generation,
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

/// How an instance's text generation is made, from the driver it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Maker {
    Claude,
    Cursor,
}

struct Instances {
    settings: DesktopSettingsHandle,
    registry: Arc<ProviderInstanceRegistry>,
}

impl TextGenerationInstances for Instances {
    fn text_generation(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Option<Arc<dyn ProviderTextGeneration>> {
        let driver = self
            .registry
            .get_instance(instance_id)
            .map(|instance| instance.driver_kind.as_str().to_owned())
            // The default Claude instance is served before the registry has it.
            .or_else(|| (instance_id.as_str() == CLAUDE_INSTANCE).then(|| CLAUDE_DRIVER.into()));
        match maker_for_driver(driver.as_deref()?) {
            Some(maker) => Some(Arc::new(SettingsBacked {
                maker,
                instance_id: instance_id.clone(),
                settings: self.settings.clone(),
            })),
            None => self.registry.text_generation(instance_id),
        }
    }
}

const CLAUDE_DRIVER: &str = "claudeAgent";
const CURSOR_DRIVER: &str = "cursor";

fn maker_for_driver(driver: &str) -> Option<Maker> {
    match driver {
        CLAUDE_DRIVER => Some(Maker::Claude),
        CURSOR_DRIVER => Some(Maker::Cursor),
        _ => None,
    }
}

/// The text generation of a Claude or Cursor instance, made from the
/// instance's settings as they are when the request runs: its binary, config
/// directory and environment. The drivers' own instances report text
/// generation as unavailable (T10d), so these are served here by N3's runners.
struct SettingsBacked {
    maker: Maker,
    instance_id: ProviderInstanceId,
    settings: DesktopSettingsHandle,
}

impl SettingsBacked {
    async fn resolve(&self) -> Arc<dyn ProviderTextGeneration> {
        let settings = self.settings.settings().await;
        let entry = settings.provider_instances.get(&self.instance_id);
        let config = entry
            .and_then(|instance| instance.config.clone())
            .filter(|config| !config.is_null())
            .unwrap_or_else(|| serde_json::json!({}));
        let host = HostProcess::current();
        let environment: BTreeMap<String, String> = merge_provider_instance_environment(
            entry.and_then(|instance| instance.environment.as_ref()),
            &host.environment,
            &host.home_directory,
        )
        .into_iter()
        .collect();
        let home_dir = std::env::var("HOME").unwrap_or_default();
        match self.maker {
            Maker::Claude => Arc::new(claude_text_generation(ClaudeTextGenerationOptions {
                settings: decode_or_default(config),
                environment: Some(environment),
                models: Arc::new(BundledClaudeModels(bundled_claude_model_catalog())),
                has_managed_mcp_config: Arc::new(|| false),
                home_dir,
            })),
            Maker::Cursor => {
                let cursor = CursorSettings::from_value(config).unwrap_or_default();
                let enabled = entry.map_or(true, resolve_provider_instance_enabled);
                Arc::new(cursor_text_generation(CursorTextGenerationOptions {
                    enabled,
                    binary_path: cursor.agent_program().to_owned(),
                    environment: Some(environment),
                    resolve_api_key: None,
                    home_dir,
                }))
            }
        }
    }
}

/// `serde_json::from_value`, or the type's defaults (an empty object decodes)
/// when the stored config does not decode.
fn decode_or_default<T: serde::de::DeserializeOwned>(config: serde_json::Value) -> T {
    serde_json::from_value(config)
        .unwrap_or_else(|_| serde_json::from_value(serde_json::json!({})).expect("defaults decode"))
}

#[async_trait]
impl ProviderTextGeneration for SettingsBacked {
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        self.resolve().await.generate_commit_message(input).await
    }

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        self.resolve().await.generate_pr_content(input).await
    }

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        self.resolve().await.generate_branch_name(input).await
    }

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        self.resolve().await.generate_thread_title(input).await
    }

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        self.resolve().await.generate_review_guide(input).await
    }
}

pub fn build(
    settings: DesktopSettingsHandle,
    registry: Arc<ProviderInstanceRegistry>,
) -> TextGeneration {
    TextGeneration::new(
        Arc::new(Instances { settings, registry }),
        Arc::new(NoLinks),
    )
}
