//! Port of upstream/packages/provider-opencode/src/server/textGeneration.ts (OpenCode 1.x).
//!
//! Each request opens a temporary session that denies every permission, sends one
//! prompt, and returns the reply text. The server is either a configured URL, which is
//! connected and version-checked per request, or a local `opencode serve` lent by
//! `OpenCodeServerOwner`, which stays warm between requests.

use async_trait::async_trait;
use serde_json::{Value, json};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::git::TextGenerationError;
use turin_contracts::js_trim;
use turin_contracts::model::ProviderOptionSelectionValue;
use turin_contracts::model_selection::ModelSelection;
use turin_provider_opencode::{
    ConnectOpenCodeServer, OpenCodeRuntimeError, OpenCodeServerOwner, V1Client,
    connect_to_open_code_server, default_v1_verify, open_code_runtime_error_detail,
};

use crate::codex_text_generation::AttachmentPathResolver;
use crate::text_generation_operations::{
    Operation, Request, Runner, RunnerTextGeneration, investigation_unsupported,
};

/// The OpenCode settings text generation reads (upstream `OpenCodeSettings`).
#[derive(Debug, Clone, Default)]
pub struct OpenCodeTextGenerationSettings {
    pub binary_path: String,
    /// Empty means a local server is spawned and lent by the owner (upstream `serverUrl.length > 0`).
    pub server_url: String,
    pub server_password: Option<String>,
}

/// Upstream `makeOpenCodeTextGeneration`. The owner is the composition root's
/// `OpenCodeServerOwner`; it only spawns when a request needs a local server.
pub fn open_code_text_generation(
    settings: OpenCodeTextGenerationSettings,
    server_owner: OpenCodeServerOwner,
    resolve_attachment_path: AttachmentPathResolver,
) -> RunnerTextGeneration<OpenCodeRunner> {
    RunnerTextGeneration::new(
        "OpenCode",
        OpenCodeRunner {
            settings,
            server_owner,
            resolve_attachment_path,
        },
    )
}

/// The `Runner` for OpenCode 1.x: one temporary session and one prompt per request.
pub struct OpenCodeRunner {
    settings: OpenCodeTextGenerationSettings,
    server_owner: OpenCodeServerOwner,
    resolve_attachment_path: AttachmentPathResolver,
}

/// Upstream `parseOpenCodeModelSlug` result. Defined here because `provider-opencode`
/// does not expose one yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOpenCodeModelSlug {
    pub provider_id: String,
    pub model_id: String,
}

/// Upstream `parseOpenCodeModelSlug`: splits `provider/model` at the first `/`.
/// Both halves must be non-empty.
pub fn parse_open_code_model_slug(slug: &str) -> Option<ParsedOpenCodeModelSlug> {
    let trimmed = js_trim(slug);
    let separator = trimmed.find('/')?;
    // `separator === trimmed.length - 1` in upstream: nothing after the slash.
    if separator == 0 || separator == trimmed.len() - 1 {
        return None;
    }
    Some(ParsedOpenCodeModelSlug {
        provider_id: trimmed[..separator].to_owned(),
        model_id: trimmed[separator + 1..].to_owned(),
    })
}

/// Upstream `OpenCodeTextGenerationError` tags. Each variant carries the context
/// upstream puts on the tagged error; `to_text_generation_error` maps them to the
/// caller-visible detail.
#[derive(Debug)]
enum OpenCodeTextGenerationError {
    SessionRequest {
        operation: Operation,
        cwd: String,
        cause: OpenCodeRuntimeError,
    },
    SessionPayload {
        operation: Operation,
        cwd: String,
    },
    PromptRequest {
        context: PromptContext,
        cause: OpenCodeRuntimeError,
    },
    PromptResponse {
        context: PromptContext,
        provider_error_name: Option<String>,
        provider_message: String,
    },
    EmptyOutput {
        context: PromptContext,
        response_part_count: usize,
        text_part_count: usize,
    },
    /// Upstream `OpenCodeRuntimeError` from connecting to or lending a server.
    Runtime(OpenCodeRuntimeError),
}

impl From<OpenCodeRuntimeError> for OpenCodeTextGenerationError {
    fn from(error: OpenCodeRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

#[derive(Debug, Clone)]
struct PromptContext {
    operation: Operation,
    cwd: String,
    session_id: String,
    provider_id: String,
    model_id: String,
}

/// Where a request is sent: a configured URL or a lent local server.
struct ServerEndpoint<'a> {
    url: &'a str,
    server_password: Option<&'a str>,
}

struct PromptFailure {
    name: Option<String>,
    message: String,
}

#[async_trait]
impl Runner for OpenCodeRunner {
    async fn run(&self, request: Request) -> Result<String, TextGenerationError> {
        // Upstream `withoutInvestigation("OpenCode", ...)`: no read-only tool mode to investigate with.
        if request.investigate.is_some() {
            return Err(investigation_unsupported(request.operation, "OpenCode"));
        }
        run_open_code_json(
            &self.settings,
            &self.server_owner,
            &self.resolve_attachment_path,
            request,
        )
        .await
    }
}

/// Upstream `runOpenCodeJson`: the raw reply text. Decoding happens in `RunnerTextGeneration`.
async fn run_open_code_json(
    settings: &OpenCodeTextGenerationSettings,
    server_owner: &OpenCodeServerOwner,
    resolve_attachment_path: &AttachmentPathResolver,
    request: Request,
) -> Result<String, TextGenerationError> {
    let operation = request.operation;
    let Some(model) = parse_open_code_model_slug(request.model_selection.model.as_str()) else {
        return Err(failure(
            operation,
            "OpenCode model selection must use the 'provider/model' format.",
            None,
        ));
    };
    let file_parts = to_open_code_file_parts(&request.attachments, resolve_attachment_path);

    let output = if settings.server_url.is_empty() {
        server_owner
            .with_server(|server| {
                let request = &request;
                let model = &model;
                let file_parts = &file_parts;
                async move {
                    run_against_server(
                        ServerEndpoint {
                            url: &server.url,
                            server_password: server.server_password.as_deref(),
                        },
                        request,
                        model,
                        file_parts,
                    )
                    .await
                }
            })
            .await
    } else {
        run_external(settings, &request, &model, &file_parts).await
    };
    output.map_err(|error| to_text_generation_error(operation, error))
}

/// Upstream `connectToOpenCodeServer(...)` branch: a configured URL, never spawned.
#[allow(clippy::result_large_err)] // The enum is only built on failure paths; boxing it would thread `Box::new` through every `?`.
async fn run_external(
    settings: &OpenCodeTextGenerationSettings,
    request: &Request,
    model: &ParsedOpenCodeModelSlug,
    file_parts: &[Value],
) -> Result<String, OpenCodeTextGenerationError> {
    let connection = connect_to_open_code_server(
        ConnectOpenCodeServer {
            binary_path: settings.binary_path.clone(),
            directory: request.cwd.clone(),
            server_url: Some(settings.server_url.clone()),
            // Upstream sends a password only when it is truthy.
            server_password: settings.server_password.clone().filter(|p| !p.is_empty()),
            environment: None,
            hostname: None,
            port: None,
            timeout: None,
            shutdown_grace: None,
            ledger: None,
        },
        default_v1_verify(),
    )
    .await?;
    run_against_server(
        ServerEndpoint {
            url: &connection.url,
            server_password: connection.server_password.as_deref(),
        },
        request,
        model,
        file_parts,
    )
    .await
}

/// Upstream `runAgainstServer`: one temporary session, one prompt, and the reply text.
#[allow(clippy::result_large_err)] // The enum is only built on failure paths; boxing it would thread `Box::new` through every `?`.
async fn run_against_server(
    endpoint: ServerEndpoint<'_>,
    request: &Request,
    model: &ParsedOpenCodeModelSlug,
    file_parts: &[Value],
) -> Result<String, OpenCodeTextGenerationError> {
    let operation = request.operation;
    let client = V1Client::new(endpoint.url, &request.cwd, endpoint.server_password)?;
    let session = client
        .create_session(&json!({
            "title": format!("T3 Code {}", operation.as_str()),
            "permission": [{ "permission": "*", "pattern": "*", "action": "deny" }],
        }))
        .await
        .map_err(|cause| OpenCodeTextGenerationError::SessionRequest {
            operation,
            cwd: request.cwd.clone(),
            cause,
        })?;
    // The v1 client returns the session body itself, so a payload without an id is upstream's empty `data`.
    let Some(session_id) = session.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return Err(OpenCodeTextGenerationError::SessionPayload {
            operation,
            cwd: request.cwd.clone(),
        });
    };

    let selected_agent =
        string_option(&request.model_selection, "agent").filter(|value| !value.is_empty());
    let selected_variant =
        string_option(&request.model_selection, "variant").filter(|value| !value.is_empty());
    let context = PromptContext {
        operation,
        cwd: request.cwd.clone(),
        session_id: session_id.clone(),
        provider_id: model.provider_id.clone(),
        model_id: model.model_id.clone(),
    };

    let mut parts = vec![json!({ "type": "text", "text": request.prompt })];
    parts.extend(file_parts.iter().cloned());
    let mut body = json!({
        "model": { "providerID": model.provider_id, "modelID": model.model_id },
        "parts": parts,
    });
    if let Some(agent) = selected_agent {
        body["agent"] = json!(agent);
    }
    if let Some(variant) = selected_variant {
        body["variant"] = json!(variant);
    }

    let result = client.prompt(&session_id, &body).await.map_err(|cause| {
        OpenCodeTextGenerationError::PromptRequest {
            context: context.clone(),
            cause,
        }
    })?;
    if let Some(failure) = result
        .get("info")
        .and_then(|info| info.get("error"))
        .and_then(prompt_failure)
    {
        return Err(OpenCodeTextGenerationError::PromptResponse {
            context,
            provider_error_name: failure.name,
            provider_message: failure.message,
        });
    }
    let response_parts: &[Value] = result
        .get("parts")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let raw_text = text_response(response_parts);
    if raw_text.is_empty() {
        return Err(OpenCodeTextGenerationError::EmptyOutput {
            context,
            response_part_count: response_parts.len(),
            text_part_count: response_parts
                .iter()
                .filter(|part| text_part(part).is_some())
                .count(),
        });
    }
    Ok(raw_text)
}

/// Upstream `getOpenCodePromptFailure`: the provider's error name and message, if it sent either.
fn prompt_failure(error: &Value) -> Option<PromptFailure> {
    let object = error.as_object()?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let message = object
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("message"))
        .and_then(Value::as_str)
        .map(js_trim)
        .unwrap_or("");
    if !message.is_empty() {
        return Some(PromptFailure {
            name,
            message: message.to_owned(),
        });
    }
    name.map(|name| PromptFailure {
        message: name.clone(),
        name: Some(name),
    })
}

/// Upstream `isOpenCodeTextPart`.
fn text_part(part: &Value) -> Option<&str> {
    let object = part.as_object()?;
    if object.get("type")?.as_str()? != "text" {
        return None;
    }
    object.get("text")?.as_str()
}

/// Upstream `getOpenCodeTextResponse`: text parts joined with no separator, then trimmed.
fn text_response(parts: &[Value]) -> String {
    let joined: String = parts.iter().filter_map(text_part).collect();
    js_trim(&joined).to_owned()
}

const OPENCODE_NATIVE_IMAGE_MIMES: [&str; 4] =
    ["image/png", "image/jpeg", "image/gif", "image/webp"];
const OPENCODE_NATIVE_FILE_PART_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Upstream `toOpenCodeFileParts`. Upstream passes only image attachments here, so the
/// text and PDF branches and the pasted-text skip (file attachments only) are not ported.
fn to_open_code_file_parts(
    attachments: &[ChatAttachment],
    resolve_attachment_path: &AttachmentPathResolver,
) -> Vec<Value> {
    attachments
        .iter()
        .filter_map(|attachment| {
            let ChatAttachment::Image(image) = attachment else {
                return None;
            };
            if image.size_bytes > OPENCODE_NATIVE_FILE_PART_MAX_BYTES {
                return None;
            }
            let mime = image.mime_type.as_str().trim().to_lowercase();
            if !OPENCODE_NATIVE_IMAGE_MIMES.contains(&mime.as_str()) {
                return None;
            }
            let path = resolve_attachment_path(attachment)?;
            // Upstream `NodeURL.pathToFileURL`. The `url` crate may encode exotic characters differently.
            let url = url::Url::from_file_path(&path).ok()?;
            Some(json!({
                "type": "file",
                "mime": image.mime_type.as_str(),
                "filename": image.name.as_str(),
                "url": url.as_str(),
            }))
        })
        .collect()
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

/// Upstream `openCodeRuntimeErrorDetail` for runtime errors, and the tagged errors' causes.
fn runtime_cause(error: &OpenCodeRuntimeError) -> Value {
    json!({
        "_tag": "OpenCodeRuntimeError",
        "operation": error.operation,
        "detail": open_code_runtime_error_detail(error),
    })
}

fn failure(operation: Operation, detail: &str, cause: Option<Value>) -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: operation.as_str().to_owned(),
        detail: detail.to_owned(),
        cause,
    }
}

/// Upstream `Effect.catchTags` at the bottom of `runOpenCodeJson` and `runAgainstServer`.
fn to_text_generation_error(
    operation: Operation,
    error: OpenCodeTextGenerationError,
) -> TextGenerationError {
    match error {
        OpenCodeTextGenerationError::SessionRequest {
            operation,
            cwd,
            cause,
        } => failure(
            operation,
            "OpenCode session.create request failed.",
            Some(json!({
                "_tag": "OpenCodeTextGenerationSessionRequestError",
                "operation": operation.as_str(),
                "cwd": cwd,
                "cause": runtime_cause(&cause),
            })),
        ),
        OpenCodeTextGenerationError::SessionPayload { operation, cwd } => failure(
            operation,
            "OpenCode session.create returned no session payload.",
            Some(json!({
                "_tag": "OpenCodeTextGenerationSessionPayloadError",
                "operation": operation.as_str(),
                "cwd": cwd,
            })),
        ),
        OpenCodeTextGenerationError::PromptRequest { context, cause } => failure(
            context.operation,
            "OpenCode session.prompt request failed.",
            Some(prompt_cause(
                "OpenCodeTextGenerationPromptRequestError",
                &context,
                [("cause", runtime_cause(&cause))],
            )),
        ),
        OpenCodeTextGenerationError::PromptResponse {
            context,
            provider_error_name,
            provider_message,
        } => {
            let mut extra = Vec::new();
            if let Some(name) = &provider_error_name {
                extra.push(("providerErrorName", json!(name)));
            }
            extra.push(("providerMessage", json!(provider_message)));
            failure(
                context.operation,
                &provider_message,
                Some(prompt_cause(
                    "OpenCodeTextGenerationPromptResponseError",
                    &context,
                    extra,
                )),
            )
        }
        OpenCodeTextGenerationError::EmptyOutput {
            context,
            response_part_count,
            text_part_count,
        } => failure(
            context.operation,
            "OpenCode returned empty output.",
            Some(prompt_cause(
                "OpenCodeTextGenerationEmptyOutputError",
                &context,
                [
                    ("responsePartCount", json!(response_part_count)),
                    ("textPartCount", json!(text_part_count)),
                ],
            )),
        ),
        OpenCodeTextGenerationError::Runtime(cause) => failure(
            operation,
            &open_code_runtime_error_detail(&cause),
            Some(runtime_cause(&cause)),
        ),
    }
}

/// The tagged prompt error's fields: its request context plus the variant's own fields.
fn prompt_cause(
    tag: &str,
    context: &PromptContext,
    extra: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let mut value = json!({
        "_tag": tag,
        "operation": context.operation.as_str(),
        "cwd": context.cwd,
        "sessionId": context.session_id,
        "providerId": context.provider_id,
        "modelId": context.model_id,
    });
    for (key, field) in extra {
        value[key] = field;
    }
    value
}

#[cfg(test)]
#[path = "opencode_text_generation_tests.rs"]
mod tests;
