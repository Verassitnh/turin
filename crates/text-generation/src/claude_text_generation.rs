//! Port of upstream/apps/server/src/textGeneration/ClaudeTextGeneration.ts
//!
//! Text generation runs `claude -p` once per request with a JSON Schema, tools off,
//! skills and hooks disabled, and permissions denied rather than asked. Thread titles run
//! in a fresh temp directory so the project's own Claude configuration does not load.
//! Investigations get read-only file tools and Git's read commands, and nothing that writes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use turin_contracts::git::TextGenerationError;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::settings::ClaudeSettings;
use turin_provider_core::path_expansion::expand_home_path;

use crate::command::{CommandError, make_temp_dir_owned, run_command};
use crate::text_generation_operations::{Operation, Request, Runner, RunnerTextGeneration};
use crate::text_generation_utils::normalize_cli_error;

const CLAUDE_TIMEOUT: Duration = Duration::from_secs(180);
/// The tools an investigation can see. `--tools` takes bare names; patterns go in the allow list.
const CLAUDE_INVESTIGATION_TOOLS: &str = "Read,Grep,Glob,Bash";
/// What it may run without asking. In dontAsk mode every other call, Bash included, is refused.
const CLAUDE_INVESTIGATION_ALLOWED: &str =
    "Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*)";
/// The allowed git commands accept any trailing arguments, and `--output` makes each of them write
/// a file anywhere. Deny rules win over allow rules, so this keeps the investigation read-only.
const CLAUDE_INVESTIGATION_DENIED: &str = "Bash(git * --output*),Bash(git * -o *)";

/// What the Claude CLI should run for one selection: the upstream `resolveClaude*` results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeResolvedModel {
    /// The `--model` value.
    pub api_model_id: String,
    /// The `--effort` value, when the model takes one.
    pub cli_effort: Option<String>,
    /// `ultracode` in the settings object.
    pub ultracode: bool,
    /// `alwaysThinkingEnabled` in the settings object, when the model has a thinking switch.
    pub thinking: Option<bool>,
    /// `fastMode` in the settings object, when on.
    pub fast_mode: bool,
}

/// Seam for the Claude model catalog (`ClaudeModelCatalog.ts`), which is not on `stage` yet.
///
/// Upstream first scopes the catalog to the instance's custom models, then resolves the slug,
/// effort, thinking, and fast-mode options against each model's capabilities.
pub trait ClaudeModelResolver: Send + Sync {
    fn resolve(&self, selection: &ModelSelection) -> ClaudeResolvedModel;
}

/// Upstream `makeClaudeTextGeneration(claudeSettings, environment, modelCatalog)`.
pub struct ClaudeTextGenerationOptions {
    pub settings: ClaudeSettings,
    /// The environment the CLI runs with. `None` means the process environment.
    pub environment: Option<BTreeMap<String, String>>,
    pub models: Arc<dyn ClaudeModelResolver>,
    /// Upstream `hasClaudeManagedMcpConfig()`. When set, `--strict-mcp-config` is left off
    /// because the CLI refuses that flag next to a managed-mcp.json.
    pub has_managed_mcp_config: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The home directory `~` expands to in `homePath`.
    pub home_dir: String,
}

/// The `Runner` for Claude: one `claude -p` per request.
pub struct ClaudeRunner {
    options: ClaudeTextGenerationOptions,
}

/// Upstream `makeClaudeTextGeneration`'s closure, named `ClaudeTextGeneration`.
pub fn claude_text_generation(
    options: ClaudeTextGenerationOptions,
) -> RunnerTextGeneration<ClaudeRunner> {
    RunnerTextGeneration::new("Claude", ClaudeRunner { options })
}

/// Upstream `path.resolve`: an absolute path, normalized, relative to the server's cwd when it is
/// not already absolute. The child runs in another directory, so a relative config dir would
/// resolve differently there.
pub fn resolve_path(path: &str) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};
    let joined = if std::path::Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// Upstream `claudeErrorResult`: the last non-blank `result` of an `is_error` message on stdout.
pub fn claude_error_result(stdout: &str) -> Option<String> {
    let output: Value = serde_json::from_str(stdout).ok()?;
    let messages = match output {
        Value::Array(messages) => messages,
        other => vec![other],
    };
    messages
        .iter()
        .filter(|message| message.get("is_error") == Some(&Value::Bool(true)))
        .filter_map(|message| message.get("result")?.as_str())
        .map(|result| turin_contracts::js_trim(result).to_owned())
        .rfind(|result| !result.is_empty())
}

/// Upstream `decodeClaudeOutput`, then the envelope pick: a single object carries
/// `structured_output`; an array of messages uses its last `result` message.
///
/// Returns the structured output as JSON text, which the operation then decodes.
fn structured_output_text(
    operation: Operation,
    stdout: &str,
) -> Result<String, TextGenerationError> {
    let unexpected = || TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: "Claude CLI returned unexpected output format.".to_owned(),
        cause: None,
    };
    let output: Value = serde_json::from_str(stdout).map_err(|_| unexpected())?;
    let structured = match &output {
        // `ClaudeOutputEnvelope`: the key must be present.
        Value::Object(envelope) => Some(
            envelope
                .get("structured_output")
                .cloned()
                .ok_or_else(unexpected)?,
        ),
        // `ClaudeOutputMessage`: every entry needs a string `type`.
        Value::Array(messages) => {
            let mut last_result = None;
            for message in messages {
                let kind = message
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(unexpected)?;
                if kind == "result" {
                    last_result = Some(message.get("structured_output").cloned());
                }
            }
            last_result.flatten()
        }
        _ => return Err(unexpected()),
    };
    let structured = structured.ok_or_else(|| TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: "Claude returned invalid structured output.".to_owned(),
        cause: None,
    })?;
    Ok(structured.to_string())
}

impl ClaudeRunner {
    fn environment(&self) -> BTreeMap<String, String> {
        let mut environment = self
            .options
            .environment
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let home_path = self.options.settings.home_path.as_str();
        if !home_path.is_empty() {
            let expanded = expand_home_path(home_path, &self.options.home_dir);
            environment.insert(
                "CLAUDE_CONFIG_DIR".to_owned(),
                resolve_path(&expanded).to_string_lossy().into_owned(),
            );
        }
        environment
    }

    /// Upstream `settings` object: hooks off, plus the switches the model supports.
    fn settings_json(&self, resolved: &ClaudeResolvedModel) -> Result<String, serde_json::Error> {
        let mut settings = Map::new();
        settings.insert("disableAllHooks".to_owned(), json!(true));
        if let Some(thinking) = resolved.thinking {
            settings.insert("alwaysThinkingEnabled".to_owned(), json!(thinking));
        }
        if resolved.fast_mode {
            settings.insert("fastMode".to_owned(), json!(true));
        }
        if resolved.ultracode {
            settings.insert("ultracode".to_owned(), json!(true));
        }
        serde_json::to_string(&Value::Object(settings))
    }

    fn command_args(
        &self,
        request: &Request,
        resolved: &ClaudeResolvedModel,
        schema_json: &str,
        settings_json: &str,
    ) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            schema_json,
            "--model",
            resolved.api_model_id.as_str(),
        ]
        .map(str::to_owned)
        .into();
        if let Some(effort) = &resolved.cli_effort {
            args.extend(["--effort".to_owned(), effort.clone()]);
        }
        args.extend(["--settings".to_owned(), settings_json.to_owned()]);
        // Metadata prompts need no executable capabilities, even when they contain a skill name.
        if request.investigate.is_some() {
            args.extend(
                [
                    "--tools",
                    CLAUDE_INVESTIGATION_TOOLS,
                    "--allowedTools",
                    CLAUDE_INVESTIGATION_ALLOWED,
                    "--disallowedTools",
                    CLAUDE_INVESTIGATION_DENIED,
                ]
                .map(str::to_owned),
            );
        } else {
            args.extend(["--tools".to_owned(), String::new()]);
        }
        args.push("--disable-slash-commands".to_owned());
        // Claude refuses this flag next to a managed-mcp.json, which already keeps the user's own
        // MCP servers out.
        if !(self.options.has_managed_mcp_config)() {
            args.push("--strict-mcp-config".to_owned());
        }
        args.extend(["--permission-mode".to_owned(), "dontAsk".to_owned()]);
        args
    }

    async fn run_claude(&self, request: &Request) -> Result<String, TextGenerationError> {
        let operation = request.operation;
        let resolved = self.options.models.resolve(&request.model_selection);
        let schema_json = serde_json::to_string(&request.output_schema).map_err(|_| {
            TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: "Failed to encode structured output schema.".to_owned(),
                cause: None,
            }
        })?;
        let settings_json = self.settings_json(&resolved).map_err(|_| {
            TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: "Failed to encode Claude CLI settings.".to_owned(),
                cause: None,
            }
        })?;
        let args = self.command_args(request, &resolved, &schema_json, &settings_json);
        let environment = self.environment();

        // Titles need only the supplied prompt, not configuration from the checkout.
        let title_directory = if operation == Operation::GenerateThreadTitle {
            Some(
                make_temp_dir_owned("t3code-claude-title-".to_owned())
                    .await
                    .map_err(|error| {
                        normalize_cli_error(
                            "claude",
                            operation,
                            error.to_string(),
                            "Failed to create title directory",
                        )
                    })?,
            )
        } else {
            None
        };
        let working_directory = title_directory.as_ref().map_or_else(
            || request.cwd.clone(),
            |dir| dir.path().to_string_lossy().into_owned(),
        );

        let binary = if self.options.settings.binary_path.as_str().is_empty() {
            "claude".to_owned()
        } else {
            self.options.settings.binary_path.as_str().to_owned()
        };
        let timeout = request
            .investigate
            .map_or(CLAUDE_TIMEOUT, |investigate| investigate.timeout);
        let output = match run_command(
            &binary,
            &args,
            &working_directory,
            &environment,
            Some(request.prompt.clone()),
            timeout,
        )
        .await
        {
            Ok(output) => output,
            Err(CommandError::Spawn(message)) => {
                return Err(normalize_cli_error(
                    "claude",
                    operation,
                    message,
                    "Failed to spawn Claude CLI process",
                ));
            }
            Err(CommandError::TimedOut) => {
                return Err(TextGenerationError::TextGenerationError {
                    operation: operation.as_str().to_owned(),
                    detail: "Claude CLI request timed out.".to_owned(),
                    cause: None,
                });
            }
            Err(CommandError::Io(message)) => {
                return Err(normalize_cli_error(
                    "claude",
                    operation,
                    message,
                    "Failed to collect process output",
                ));
            }
        };

        if output.exit_code != Some(0) {
            let stderr_detail = turin_contracts::js_trim(&output.stderr);
            let stdout_detail = turin_contracts::js_trim(&output.stdout);
            let detail = claude_error_result(stdout_detail).unwrap_or_else(|| {
                if stderr_detail.is_empty() {
                    stdout_detail.to_owned()
                } else {
                    stderr_detail.to_owned()
                }
            });
            let message = if detail.is_empty() {
                let code = output
                    .exit_code
                    .map_or_else(|| "null".to_owned(), |code| code.to_string());
                format!("Claude CLI command failed with code {code}.")
            } else {
                format!("Claude CLI command failed: {detail}")
            };
            return Err(TextGenerationError::TextGenerationError {
                operation: operation.as_str().to_owned(),
                detail: message,
                cause: None,
            });
        }

        structured_output_text(operation, &output.stdout)
    }
}

#[async_trait]
impl Runner for ClaudeRunner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        self.run_claude(&request).await
    }
}

#[cfg(test)]
#[path = "claude_text_generation_tests.rs"]
mod tests;
