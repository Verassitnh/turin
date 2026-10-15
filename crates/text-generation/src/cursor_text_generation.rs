//! Port of upstream/packages/provider-cursor/src/server/textGeneration.ts, rebuilt on ACP (see PORTING.md provider-cursor rows).
//!
//! Each request starts its own `cursor-agent acp` child in an empty temp directory, opens one
//! session there, sets the model and options, puts the session in plan mode, and collects the
//! streamed reply text. Dropping the ACP client kills the child's process group.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::time::Instant;
use turin_acp::AcpClient;
use turin_acp::cursor::{AskQuestionResult, CreatePlanResult};
use turin_acp::errors::{AcpError, AcpProcessExitedError};
use turin_contracts::git::TextGenerationError;
use turin_contracts::js_trim;
use turin_contracts::model::ProviderOptionSelectionValue;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_core::process_group::signal_process_group;

use crate::command::make_temp_dir_owned;
use crate::text_generation_operations::{
    Operation, Request, Runner, RunnerTextGeneration, investigation_unsupported,
};

/// Upstream `CURSOR_TIMEOUT_MS`. It covers everything from spawning the child to the reply.
const CURSOR_TIMEOUT: Duration = Duration::from_secs(180);
/// Upstream bounds the native cancel with a 5 second timeout, so a wedged pipe cannot hang the request.
const CURSOR_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const CURSOR_BINARY: &str = "cursor-agent";
const CURSOR_CLIENT_NAME: &str = "turin";
const CURSOR_PLAN_MODE: &str = "plan";
/// Upstream `cursorSdkModelSelection`: the `auto` selection is the SDK's `default` model.
const CURSOR_AUTO_MODEL: &str = "auto";
const CURSOR_DEFAULT_MODEL: &str = "default";
const CURSOR_API_KEY_ENV: &str = "CURSOR_API_KEY";

/// Upstream `resolveApiKey`, run once per request so a browser sign-in change takes effect.
/// The error text is only a cause; the user sees the generic failure detail.
pub type CursorApiKeyResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// Upstream `makeCursorTextGeneration(cursorSettings, environment, resolveApiKey, withAccess)`.
pub struct CursorTextGenerationOptions {
    /// `CursorSettings.enabled`.
    pub enabled: bool,
    /// `CursorSettings.binaryPath`. Empty means `cursor-agent`.
    pub binary_path: String,
    /// The environment the child runs with. `None` means the process environment.
    pub environment: Option<BTreeMap<String, String>>,
    /// Upstream `resolveApiKey`. When absent, `CURSOR_API_KEY` from the environment is used.
    pub resolve_api_key: Option<CursorApiKeyResolver>,
    /// The directory `~/.cursor/sandbox.json` is read from.
    pub home_dir: String,
}

/// Starts the ACP connection for one request. The default spawns `cursor-agent acp`; tests
/// substitute an in-process peer.
#[async_trait]
pub trait CursorAcpConnector: Send + Sync {
    /// `cwd` is the request's empty temp directory. `environment` is the child's whole environment.
    async fn connect(
        &self,
        cwd: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<AcpClient, String>;
}

/// The default connector: a `cursor-agent acp` child owned by the connection.
pub struct CursorAgentProcess {
    pub binary_path: String,
}

/// Owns the child for as long as the connection lives. Dropping it signals the child's process
/// group, because `cursor-agent acp` does not exit on stdin EOF after `session/new`.
struct CursorAgentChild {
    pid: u32,
    // A std mutex keeps the handle `Sync` for the shutdown `Any`. It is never contended.
    _child: Mutex<Child>,
    stderr_drain: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for CursorAgentChild {
    fn drop(&mut self) {
        if let Some(drain) = self.stderr_drain.take() {
            drain.abort();
        }
        let _ = signal_process_group(i64::from(self.pid), libc::SIGKILL);
    }
}

#[async_trait]
impl CursorAcpConnector for CursorAgentProcess {
    async fn connect(
        &self,
        cwd: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<AcpClient, String> {
        // `turin_acp::spawn` cannot set the environment, so the spawn lives here.
        let mut command = Command::new(&self.binary_path);
        command
            .arg("acp")
            .current_dir(cwd)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let pid = child.id().unwrap_or(0);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "cursor-agent stdin was not piped".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "cursor-agent stdout was not piped".to_owned())?;
        // The child must never block on a full stderr pipe. Its text is not copied into errors.
        // The handle is kept so dropping the connection aborts the drain with the child.
        let stderr_drain = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            })
        });
        let on_eof: Arc<dyn Fn() -> BoxFuture<'static, AcpError> + Send + Sync> =
            Arc::new(move || {
                Box::pin(async move {
                    AcpError::from(AcpProcessExitedError {
                        code: None,
                        pid: Some(i64::from(pid)),
                        stderr: None,
                        cause: None,
                    })
                })
            });
        let shutdown = Arc::new(CursorAgentChild {
            pid,
            _child: Mutex::new(child),
            stderr_drain,
        });
        Ok(AcpClient::connect_with(
            stdout,
            stdin,
            Some(on_eof),
            Some(shutdown),
        ))
    }
}

/// Upstream `withoutInvestigation("Cursor", run)`: Cursor has no read-only tool mode to
/// investigate with, so an investigation fails instead of answering blind.
pub struct WithoutInvestigation<R> {
    inner: R,
}

#[async_trait]
impl<R: Runner> Runner for WithoutInvestigation<R> {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        if request.investigate.is_some() {
            return Err(investigation_unsupported(request.operation, "Cursor"));
        }
        self.inner.run(request).await
    }
}

/// The `Runner` for Cursor: one ACP session per request.
pub struct CursorRunner {
    options: CursorTextGenerationOptions,
    connector: Arc<dyn CursorAcpConnector>,
}

/// Upstream `makeCursorTextGeneration`'s closure, named `CursorTextGeneration`.
pub fn cursor_text_generation(
    options: CursorTextGenerationOptions,
) -> RunnerTextGeneration<WithoutInvestigation<CursorRunner>> {
    let binary_path = if options.binary_path.is_empty() {
        CURSOR_BINARY.to_owned()
    } else {
        options.binary_path.clone()
    };
    cursor_text_generation_with_connector(options, Arc::new(CursorAgentProcess { binary_path }))
}

/// [`cursor_text_generation`] with the connection supplied by the caller.
pub fn cursor_text_generation_with_connector(
    options: CursorTextGenerationOptions,
    connector: Arc<dyn CursorAcpConnector>,
) -> RunnerTextGeneration<WithoutInvestigation<CursorRunner>> {
    RunnerTextGeneration::new(
        "Cursor",
        WithoutInvestigation {
            inner: CursorRunner { options, connector },
        },
    )
}

#[async_trait]
impl Runner for CursorRunner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        self.run_cursor(&request).await
    }
}

impl CursorRunner {
    /// Upstream `runCursorJson`. The `cwd` of the request is ignored: the agent runs in an
    /// empty temp directory, away from the project.
    async fn run_cursor(&self, request: &Request) -> Result<String, TextGenerationError> {
        let operation = request.operation;
        let mut environment = self
            .options
            .environment
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let api_key = self.api_key(operation, &environment).await?;
        // Upstream's sandbox guard: ACP has no sandbox, so a user's sandbox.json can expand the
        // writable paths the agent sees. Refuse rather than run with it.
        let sandbox_path = Path::new(&self.options.home_dir)
            .join(".cursor")
            .join("sandbox.json");
        match tokio::fs::try_exists(&sandbox_path).await {
            Ok(false) => {}
            Ok(true) => {
                return Err(detail_error(
                    operation,
                    "Cursor text generation cannot enforce workspace isolation with a custom ~/.cursor/sandbox.json. Use another text-generation provider.",
                ));
            }
            Err(error) => return Err(failed(operation, error)),
        }
        // Upstream `makeTempDirectoryScoped`: the workspace is empty and lives for this request.
        let workspace = make_temp_dir_owned("t3-cursor-text-".to_owned())
            .await
            .map_err(|error| failed(operation, error))?;
        if self.options.resolve_api_key.is_some() {
            environment.insert(CURSOR_API_KEY_ENV.to_owned(), api_key);
        }
        let deadline = Instant::now() + CURSOR_TIMEOUT;
        self.run_session(request, workspace.path(), &environment, deadline)
            .await
    }

    /// Upstream `resolveCursorApiKey`: enabled first, then the key.
    async fn api_key(
        &self,
        operation: Operation,
        environment: &BTreeMap<String, String>,
    ) -> Result<String, TextGenerationError> {
        if !self.options.enabled {
            return Err(detail_error(
                operation,
                "Cursor is disabled in T3 Code settings.",
            ));
        }
        let api_key = match &self.options.resolve_api_key {
            Some(resolve) => resolve().await.map_err(|error| failed(operation, error))?,
            None => environment
                .get(CURSOR_API_KEY_ENV)
                .map(|key| js_trim(key).to_owned())
                .unwrap_or_default(),
        };
        if api_key.is_empty() {
            return Err(detail_error(
                operation,
                "Sign in with Cursor or add CURSOR_API_KEY in provider settings.",
            ));
        }
        Ok(api_key)
    }

    /// Connects, opens the session, configures it, and returns the reply text for a finished run.
    /// Every stage shares one deadline, as upstream's single `timeoutOption` covers them all.
    async fn run_session(
        &self,
        request: &Request,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<String, TextGenerationError> {
        let operation = request.operation;
        let client = within(
            deadline,
            operation,
            self.connector.connect(workspace, environment),
        )
        .await?;

        // Registered before the first request so no streamed chunk can arrive unheard.
        let replies = Arc::new(Mutex::new(String::new()));
        client.on_session_update({
            let replies = Arc::clone(&replies);
            move |params: Value| {
                let replies = Arc::clone(&replies);
                async move {
                    if let Some(text) = agent_text_chunk(&params) {
                        replies.lock().expect("reply buffer").push_str(text);
                    }
                    Ok(())
                }
            }
        });
        // Text generation has no tools, so every permission ask is refused.
        client.on_request_permission(|params, _context| async move {
            Ok(reject_permission_outcome(&params))
        });
        client.on_cursor_ask_question(|_params, _context| async {
            Ok(AskQuestionResult::cancelled())
        });
        client
            .on_cursor_create_plan(|_params, _context| async { Ok(CreatePlanResult::cancelled()) });
        client.acknowledge_cursor_side_requests(|_method, _params, _context| async { Ok(()) });

        within(
            deadline,
            operation,
            client.initialize(AcpClient::cursor_initialize(
                CURSOR_CLIENT_NAME,
                env!("CARGO_PKG_VERSION"),
            )),
        )
        .await?;
        let created = within(
            deadline,
            operation,
            client.create_session(json!({
                "cwd": workspace.to_string_lossy().into_owned(),
                "mcpServers": [],
            })),
        )
        .await?;
        let session_id = created
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| failed(operation, "ACP session/new returned no sessionId."))?
            .to_owned();

        for (config_id, value) in cursor_config_updates(&request.model_selection) {
            within(
                deadline,
                operation,
                client.set_session_config_option(json!({
                    "sessionId": session_id,
                    "configId": config_id,
                    "value": value,
                })),
            )
            .await?;
        }
        within(
            deadline,
            operation,
            client.set_session_mode(json!({"sessionId": session_id, "modeId": CURSOR_PLAN_MODE})),
        )
        .await?;

        let prompt = client.prompt(json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": request.prompt}],
        }));
        let result = match tokio::time::timeout_at(deadline, prompt).await {
            Ok(result) => result.map_err(|error| failed(operation, error))?,
            Err(_) => {
                // Upstream cancels the native run when the request times out.
                let _ = tokio::time::timeout(
                    CURSOR_CANCEL_TIMEOUT,
                    client.cancel(json!({"sessionId": session_id})),
                )
                .await;
                return Err(detail_error(operation, "Cursor SDK request timed out."));
            }
        };

        // Upstream checks the run status first, then whether any text came back.
        match result.get("stopReason").and_then(Value::as_str) {
            Some("end_turn") => {}
            Some("cancelled") => {
                return Err(detail_error(operation, "Cursor SDK request was cancelled."));
            }
            _ => {
                return Err(detail_error(
                    operation,
                    "Cursor SDK request finished with an error.",
                ));
            }
        }
        let raw = {
            let text = replies.lock().expect("reply buffer");
            js_trim(&text).to_owned()
        };
        if raw.is_empty() {
            return Err(detail_error(operation, "Cursor SDK returned empty output."));
        }
        Ok(raw)
    }
}

/// Upstream `cursorSdkModelSelection`, restated as ACP `session/set_config_option` updates.
/// The model comes first. Each provider option follows under its Cursor parameter id.
pub fn cursor_config_updates(selection: &ModelSelection) -> Vec<(String, String)> {
    let model = match selection.model.as_str() {
        CURSOR_AUTO_MODEL => CURSOR_DEFAULT_MODEL,
        model => model,
    };
    let mut updates = vec![("model".to_owned(), model.to_owned())];
    for option in selection.options.iter().flatten() {
        updates.push((
            cursor_parameter_id(option.id.as_str()).to_owned(),
            option_value_text(&option.value),
        ));
    }
    updates
}

/// Upstream `PROVIDER_OPTION_TO_CURSOR_SDK_PARAMETER`. Other ids pass through unchanged.
fn cursor_parameter_id(provider_option_id: &str) -> &str {
    match provider_option_id {
        "contextWindow" => "context",
        "fastMode" => "fast",
        other => other,
    }
}

/// JavaScript `String(value)` for an option value.
fn option_value_text(value: &ProviderOptionSelectionValue) -> String {
    match value {
        ProviderOptionSelectionValue::Str(text) => text.as_str().to_owned(),
        ProviderOptionSelectionValue::Bool(flag) => flag.to_string(),
    }
}

/// The text of an `agent_message_chunk` carrying a text block. Other updates are ignored.
fn agent_text_chunk(params: &Value) -> Option<&str> {
    let update = params.get("update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    let content = update.get("content")?;
    if content.get("type")?.as_str()? != "text" {
        return None;
    }
    content.get("text")?.as_str()
}

/// Answers `session/request_permission` by rejecting: the first reject option the agent offered,
/// preferring allow-once over always. With none offered, the outcome is cancelled.
fn reject_permission_outcome(params: &Value) -> Value {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let reject = ["reject_once", "reject_always"].iter().find_map(|kind| {
        options
            .iter()
            .find(|option| option.get("kind").and_then(Value::as_str) == Some(kind))
    });
    match reject
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
    {
        Some(option_id) => json!({"outcome": {"outcome": "selected", "optionId": option_id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// Runs one ACP step under the request deadline. A step that runs past it is the timeout detail.
async fn within<T, E: std::fmt::Display>(
    deadline: Instant,
    operation: Operation,
    step: impl Future<Output = Result<T, E>>,
) -> Result<T, TextGenerationError> {
    match tokio::time::timeout_at(deadline, step).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(failed(operation, error)),
        Err(_) => Err(detail_error(operation, "Cursor SDK request timed out.")),
    }
}

fn detail_error(operation: Operation, detail: &str) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: detail.to_owned(),
        cause: None,
    }
}

/// Upstream's `mapError`: anything that is not already a `TextGenerationError` becomes the
/// generic detail, with the underlying error as the cause.
fn failed(operation: Operation, cause: impl std::fmt::Display) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: "Cursor SDK text generation failed.".to_owned(),
        cause: Some(Value::String(cause.to_string())),
    }
}

#[cfg(test)]
#[path = "cursor_text_generation_tests.rs"]
mod tests;
