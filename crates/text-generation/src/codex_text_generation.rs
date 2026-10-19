//! Port of upstream/apps/server/src/textGeneration/CodexTextGeneration.ts
//! Also: Port of upstream/apps/server/src/codexModelOptions.ts.
//!
//! Text generation runs `codex exec` once per request: the prompt on stdin, the output
//! schema and last message in temp files, and a read-only sandbox. Each run first
//! turns off the user's MCP servers and plugins, since text generation needs only the
//! prompt.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::model::ProviderOptionSelectionValue;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::settings::CodexSettings;
use turin_provider_core::path_expansion::expand_home_path;

use crate::codex_launch_args::{codex_exec_launch_args, resolve_codex_launch_args};
use crate::command::{CommandError, make_temp_dir_owned, run_command};
use crate::text_generation_operations::{Operation, Request, Runner, RunnerTextGeneration};
use crate::text_generation_utils::normalize_cli_error;

const CODEX_TIMEOUT: Duration = Duration::from_secs(180);
const CODEX_MCP_LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// Text generation's default reasoning effort, `DEFAULT_TEXT_GENERATION_REASONING_EFFORT`.
const DEFAULT_REASONING_EFFORT: &str = "low";

// Plugins and apps bring MCP servers of their own; the user's are listed and turned off by
// name below. These follow the user's launch args, so they win over a `-c features.plugins=true`
// there, which `--disable plugins` does not.
const CODEX_NO_PLUGIN_ARGS: [&str; 4] = [
    "--config",
    "features.plugins=false",
    "--config",
    "features.apps=false",
];

/// One model the Codex CLI can run, as the live catalog lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModel {
    pub slug: String,
    pub is_custom: bool,
}

/// Upstream `getModels`: the live model list. Empty when there is none.
#[async_trait]
pub trait CodexModelSource: Send + Sync {
    async fn models(&self) -> Vec<CodexModel>;
}

/// The settings and environment a managed (ChatGPT) runtime resolves to.
#[derive(Debug, Clone)]
pub struct CodexEffectiveRuntime {
    pub settings: CodexSettings,
    pub environment: BTreeMap<String, String>,
}

/// Upstream `resolveRuntime`: a managed Codex runtime that replaces the configured one.
/// `Err` is a setup failure, reported with its detail.
#[async_trait]
pub trait CodexManagedRuntimeSource: Send + Sync {
    async fn resolve(&self) -> Result<CodexEffectiveRuntime, String>;
}

/// Maps an attachment to a file on disk, or `None` when it has none. Upstream
/// `resolveAttachmentPath`; the attachment store owns the layout.
pub type AttachmentPathResolver = Arc<dyn Fn(&ChatAttachment) -> Option<PathBuf> + Send + Sync>;

/// Upstream `makeCodexTextGeneration(codexConfig, environment, getModels, resolveRuntime)`.
pub struct CodexTextGenerationOptions {
    pub settings: CodexSettings,
    /// The environment the CLI runs with. `None` means the process environment.
    pub environment: Option<BTreeMap<String, String>>,
    pub models: Arc<dyn CodexModelSource>,
    pub managed_runtime: Option<Arc<dyn CodexManagedRuntimeSource>>,
    pub resolve_attachment_path: AttachmentPathResolver,
    /// The home directory `~` expands to in `homePath`.
    pub home_dir: String,
}

/// The `Runner` for Codex: one `codex exec` per request.
pub struct CodexRunner {
    options: CodexTextGenerationOptions,
}

/// Upstream `makeCodexTextGeneration`'s closure, named `CodexTextGeneration`.
pub fn codex_text_generation(
    options: CodexTextGenerationOptions,
) -> RunnerTextGeneration<CodexRunner> {
    RunnerTextGeneration::new("Codex", CodexRunner { options })
}

/// Upstream `codexModelFamily`: `openai.gpt-…` names the same model as `gpt-…`.
fn codex_model_family(slug: &str) -> &str {
    if slug.starts_with("openai.gpt-") {
        &slug["openai.".len()..]
    } else {
        slug
    }
}

/// `getModelSelectionStringOptionValue`.
fn string_option<'a>(selection: &'a ModelSelection, id: &str) -> Option<&'a str> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match &option.value {
            ProviderOptionSelectionValue::Str(value) => Some(value.as_str()),
            ProviderOptionSelectionValue::Bool(_) => None,
        })
}

/// `getModelSelectionBooleanOptionValue`.
fn bool_option(selection: &ModelSelection, id: &str) -> Option<bool> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match option.value {
            ProviderOptionSelectionValue::Bool(value) => Some(value),
            ProviderOptionSelectionValue::Str(_) => None,
        })
}

/// `getCodexServiceTierOptionValue`: the `serviceTier` option, or `fast` when `fastMode` is on.
fn codex_service_tier(selection: &ModelSelection) -> Option<String> {
    string_option(selection, "serviceTier")
        .map(str::to_owned)
        .or_else(|| (bool_option(selection, "fastMode") == Some(true)).then(|| "fast".to_owned()))
}

/// `codexMcpServersOff`: a `--config` value that turns off each named MCP server. `mcp_servers={}`
/// merges into the user's servers rather than replacing them, so each one is named.
fn codex_mcp_servers_off(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let entries = names
        .iter()
        .map(|name| {
            format!(
                "{}={{enabled=false}}",
                serde_json::to_string(name).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("mcp_servers={{{entries}}}"))
}

#[derive(serde::Deserialize)]
struct McpServerEntry {
    name: String,
    enabled: bool,
}

impl CodexRunner {
    fn environment(&self) -> BTreeMap<String, String> {
        self.options
            .environment
            .clone()
            .unwrap_or_else(|| std::env::vars().collect())
    }

    /// Upstream `materializeImageAttachments`: the image files that exist, in attachment order.
    async fn image_paths(&self, attachments: &[ChatAttachment]) -> Vec<String> {
        let mut paths = Vec::new();
        for attachment in attachments {
            let ChatAttachment::Image(_) = attachment else {
                continue;
            };
            let Some(path) = (self.options.resolve_attachment_path)(attachment) else {
                continue;
            };
            if !path.is_absolute() {
                continue;
            }
            match tokio::fs::metadata(&path).await {
                Ok(info) if info.is_file() => paths.push(path.to_string_lossy().into_owned()),
                _ => {}
            }
        }
        paths
    }

    /// Upstream `listEnabledMcpServers`: the names Codex would start, or none when listing fails.
    async fn list_enabled_mcp_servers(
        &self,
        binary: &str,
        cwd: &str,
        env: &BTreeMap<String, String>,
        launch_args: &[String],
    ) -> Vec<String> {
        let mut args = launch_args.to_vec();
        args.extend(CODEX_NO_PLUGIN_ARGS.iter().map(|arg| (*arg).to_owned()));
        args.extend(["mcp", "list", "--json"].map(str::to_owned));
        let Ok(output) = run_command(binary, &args, cwd, env, None, CODEX_MCP_LIST_TIMEOUT).await
        else {
            return Vec::new();
        };
        if output.exit_code != Some(0) {
            return Vec::new();
        }
        serde_json::from_str::<Vec<McpServerEntry>>(&output.stdout)
            .map(|servers| {
                servers
                    .into_iter()
                    .filter(|server| server.enabled)
                    .map(|server| server.name)
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn run_codex(
        &self,
        request: &Request,
        image_paths: &[String],
        schema_path: &str,
        output_path: &str,
    ) -> Result<(), TextGenerationError> {
        let operation = request.operation;
        let resolved = match &self.options.managed_runtime {
            Some(source) => Some(source.resolve().await.map_err(|detail| {
                TextGenerationError::TextGenerationError {
                    operation: operation.as_str().to_owned(),
                    detail,
                    cause: None,
                }
            })?),
            None => None,
        };
        let effective_settings = resolved
            .as_ref()
            .map_or(&self.options.settings, |runtime| &runtime.settings);
        let effective_environment = resolved
            .as_ref()
            .map_or_else(|| self.environment(), |runtime| runtime.environment.clone());

        let models = self.options.models.models().await;
        let requested = request.model_selection.model.as_str();
        let model = models
            .iter()
            .find(|candidate| candidate.slug == requested)
            .or_else(|| {
                models.iter().find(|candidate| {
                    !candidate.is_custom && codex_model_family(&candidate.slug) == requested
                })
            })
            .map_or(requested, |candidate| candidate.slug.as_str())
            .to_owned();

        let launch_args =
            resolve_codex_launch_args(Some(effective_settings.launch_args.as_str()), &|key| {
                effective_environment.get(key).cloned()
            });
        let reasoning_effort = string_option(&request.model_selection, "reasoningEffort")
            .unwrap_or(DEFAULT_REASONING_EFFORT)
            .to_owned();
        // A managed runtime decides its own service tier.
        let service_tier = if resolved.is_some() {
            None
        } else {
            codex_service_tier(&request.model_selection)
        };
        let binary = if effective_settings.binary_path.as_str().is_empty() {
            "codex".to_owned()
        } else {
            effective_settings.binary_path.as_str().to_owned()
        };
        let mut env = effective_environment.clone();
        if !effective_settings.home_path.as_str().is_empty() {
            env.insert(
                "CODEX_HOME".to_owned(),
                expand_home_path(
                    effective_settings.home_path.as_str(),
                    &self.options.home_dir,
                ),
            );
        }

        let exec_launch_args = codex_exec_launch_args(Some(&launch_args));
        // `codex mcp` rejects --strict-config; the overrides still apply.
        let mcp_list_args: Vec<String> = exec_launch_args
            .iter()
            .filter(|arg| arg.as_str() != "--strict-config")
            .cloned()
            .collect();
        let mcp_names = self
            .list_enabled_mcp_servers(&binary, &request.cwd, &env, &mcp_list_args)
            .await;
        let mcp_servers_off = codex_mcp_servers_off(&mcp_names);

        let mut args: Vec<String> = vec!["exec".to_owned()];
        args.extend(exec_launch_args);
        args.extend(CODEX_NO_PLUGIN_ARGS.iter().map(|arg| (*arg).to_owned()));
        if let Some(mcp) = mcp_servers_off {
            args.extend(["--config".to_owned(), mcp]);
        }
        args.extend(
            [
                "--ephemeral",
                "--skip-git-repo-check",
                "-s",
                "read-only",
                "--model",
            ]
            .map(str::to_owned),
        );
        args.push(model);
        args.extend([
            "--config".to_owned(),
            format!("model_reasoning_effort=\"{reasoning_effort}\""),
        ]);
        if let Some(tier) = service_tier {
            args.extend(["--config".to_owned(), format!("service_tier=\"{tier}\"")]);
        }
        args.extend([
            "--output-schema".to_owned(),
            schema_path.to_owned(),
            "--output-last-message".to_owned(),
            output_path.to_owned(),
        ]);
        for image in image_paths {
            args.extend(["--image".to_owned(), image.clone()]);
        }
        args.push("-".to_owned());

        let timeout = request
            .investigate
            .map_or(CODEX_TIMEOUT, |investigate| investigate.timeout);
        let output = match run_command(
            &binary,
            &args,
            &request.cwd,
            &env,
            Some(request.prompt.clone()),
            timeout,
        )
        .await
        {
            Ok(output) => output,
            Err(CommandError::Spawn(message)) => {
                return Err(normalize_cli_error(
                    "codex",
                    operation,
                    message,
                    "Failed to spawn Codex CLI process",
                ));
            }
            Err(CommandError::TimedOut) => {
                return Err(TextGenerationError::TextGenerationError {
                    operation: operation.as_str().to_owned(),
                    detail: "Codex CLI request timed out.".to_owned(),
                    cause: None,
                });
            }
            Err(CommandError::Io(message)) => {
                return Err(normalize_cli_error(
                    "codex",
                    operation,
                    message,
                    "Failed to collect process output",
                ));
            }
        };

        if output.exit_code != Some(0) {
            let stderr = turin_contracts::js_trim(&output.stderr);
            let stdout = turin_contracts::js_trim(&output.stdout);
            let detail = if stderr.is_empty() { stdout } else { stderr };
            let message = if detail.is_empty() {
                let code = output
                    .exit_code
                    .map_or_else(|| "null".to_owned(), |code| code.to_string());
                format!("Codex CLI command failed with code {code}.")
            } else {
                format!("Codex CLI command failed: {detail}")
            };
            return Err(TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: message,
                cause: None,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Runner for CodexRunner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        let operation = request.operation;
        let image_paths = self.image_paths(&request.attachments).await;
        let schema_json = serde_json::to_string(&request.output_schema).map_err(|_| {
            TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: "Failed to encode structured output schema.".to_owned(),
                cause: None,
            }
        })?;

        // Removed when `workspace` drops, on every path, so no cleanup is needed.
        let workspace = temp_workspace(operation).await?;
        let schema_path = workspace.path().join("codex-schema.json");
        let output_path = workspace.path().join("codex-output.txt");
        write_temp_file(operation, &schema_path, &schema_json).await?;
        write_temp_file(operation, &output_path, "").await?;

        self.run_codex(
            &request,
            &image_paths,
            &schema_path.to_string_lossy(),
            &output_path.to_string_lossy(),
        )
        .await?;

        tokio::fs::read_to_string(&output_path).await.map_err(|_| {
            TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: "Failed to read Codex output file.".to_owned(),
                cause: None,
            }
        })
    }
}

async fn temp_workspace(operation: Operation) -> Result<tempfile::TempDir, TextGenerationError> {
    let prefix = format!("t3code-codex-{}-", std::process::id());
    make_temp_dir_owned(prefix)
        .await
        .map_err(|_| write_failed(operation))
}

async fn write_temp_file(
    operation: Operation,
    path: &std::path::Path,
    content: &str,
) -> Result<(), TextGenerationError> {
    tokio::fs::write(path, content)
        .await
        .map_err(|_| write_failed(operation))
}

fn write_failed(operation: Operation) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: "Failed to write temp file".to_owned(),
        cause: None,
    }
}

#[cfg(test)]
#[path = "codex_text_generation_tests.rs"]
mod tests;
