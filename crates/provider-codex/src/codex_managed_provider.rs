//! Port of upstream/apps/server/src/provider/Drivers/CodexManagedProvider.ts.
//!
//! A managed instance signs in with ChatGPT and launches `codex app-server` from
//! the installation T3 owns. Its status reports the saved ChatGPT account, not
//! the app-server's own account, so the probe skips native usage
//! (`skipNativeUsage`). The snapshot refreshes whenever the sign-in settles.
//!
//! Turin differences, each recorded in PORTING.md Deviations: text generation is
//! the shared not-yet-ported stub (N3 owns it), and there is no per-cwd skills
//! snapshot (`snapshotForCwd`), as for native Codex.

#![allow(clippy::result_large_err)]

use std::{future::Future, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use turin_contracts::git::TextGenerationError;
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_setup::ProviderAuthPhase,
    provider_usage_limits::{
        ProviderUsageLimitsUpdate, ServerProviderExternalUsage, ServerProviderUsageLimits,
        ServerProviderUsageUnavailable, ServerProviderUsageUnavailableReason,
    },
    settings::CodexSettings,
};
use turin_provider_core::{
    auth::{AdmittedFailure, AdmittedTask, ProviderAuthController},
    driver::{
        ProviderContinuationIdentity, ProviderDriverCreateInput, ProviderInstance, SnapshotForCwd,
    },
    errors::ProviderDriverError,
    instance_environment::merge_provider_instance_environment,
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
    rt::BoxStream,
    server_provider::{
        ProviderWorkspaceSnapshot, ServerProvider, ServerProviderAuth, ServerProviderAuthStatus,
        ServerProviderRuntimePaths, ServerProviderSetup, ServerProviderSkill,
    },
    snapshot::ManagedServerProvider,
    text_generation::{
        BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
        CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
        ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
        ThreadTitleGenerationInput, ThreadTitleGenerationResult,
    },
};
use turin_text_generation::codex_text_generation::{
    AttachmentPathResolver, CodexEffectiveRuntime as TextEffectiveRuntime,
    CodexManagedRuntimeSource, CodexModel, CodexModelSource, CodexTextGenerationOptions,
    codex_text_generation,
};

use crate::{
    codex_chatgpt_auth::records::CodexChatGptCredentials,
    codex_chatgpt_models::chat_gpt_models_at,
    codex_home_layout::CodexHomeMode,
    codex_managed_runtime::{
        CodexManagedHost, CodexManagedRuntime, CodexManagedRuntimeOptions, managed_launch_config,
    },
    codex_snapshot::SnapshotCell,
    driver::{
        SKILLS_PROBE_TIMEOUT, SnapshotIdentity, StatusInput, check_codex_provider_status,
        codex_continuation_identity, pending_provider, probe_skills_for_cwd,
    },
    orchestration_adapter::CodexProviderAdapter,
    runtime::{CodexRuntimeConfig, CodexRuntimeResolver},
};

/// The scope that lets T3 hand the access token to the Codex CLI.
const TOKEN_SHARING_SCOPE: &str = "chatgpt.tokens.use.direct";
const CATALOG_URL: &str = "https://api.openai.com/v1/models";
const MSG_SIGN_IN: &str = "Sign in with ChatGPT to use Codex.";
const MSG_NOT_SET_UP: &str = "Set up Codex to get started.";
const MSG_SHARING_DISABLED: &str = "Signed in with ChatGPT, but token sharing is disabled. Sign in again and enable token sharing, or use another provider.";
const MSG_CHECK_FAILED: &str =
    "Could not check Codex right now. Retry, or reconnect in provider settings.";
const MSG_USAGE_UNSUPPORTED: &str = "ChatGPT tracks subscription usage across connected apps. Open Usage settings with the account you connected to Codex.";

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn has_token_sharing(credentials: &CodexChatGptCredentials) -> bool {
    credentials
        .scopes
        .iter()
        .any(|scope| scope == TOKEN_SHARING_SCOPE)
}

fn trimmed_email(credentials: &CodexChatGptCredentials) -> Option<TrimmedNonEmptyString> {
    credentials.email.as_deref().and_then(tne)
}

/// `usageLimits` for a signed-in account: ChatGPT owns the numbers, so link out.
fn external_usage_limits(checked_at: &str) -> ServerProviderUsageLimits {
    ServerProviderUsageLimits {
        checked_at: checked_at.to_owned(),
        windows: Vec::new(),
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: Some(ServerProviderExternalUsage {
            label: tne("ChatGPT usage").expect("label"),
            url: tne("https://chatgpt.com/#settings/Usage").expect("url"),
        }),
        unavailable: Some(ServerProviderUsageUnavailable {
            reason: ServerProviderUsageUnavailableReason::Unsupported,
            message: tne(MSG_USAGE_UNSUPPORTED),
        }),
    }
}

fn unauthenticated(
    label: Option<&str>,
    email: Option<TrimmedNonEmptyString>,
) -> ServerProviderAuth {
    let mut auth = ServerProviderAuth::with_status(ServerProviderAuthStatus::Unauthenticated);
    auth.label = label.and_then(tne);
    auth.email = email;
    auth
}

pub(crate) struct ManagedCodexSnapshot {
    kind: ProviderDriverKind,
    instance_id: ProviderInstanceId,
    enabled: bool,
    identity: SnapshotIdentity,
    runtime: Arc<CodexManagedRuntime>,
    base_config: CodexRuntimeConfig,
    http: reqwest::Client,
    catalog_url: String,
    setup: ServerProviderSetup,
    runtime_paths: ServerProviderRuntimePaths,
    maintenance: ProviderMaintenanceCapabilities,
    cell: SnapshotCell,
}

impl ManagedCodexSnapshot {
    /// The pending snapshot with this instance's setup, paths, and identity on it.
    fn base(&self) -> ServerProvider {
        let mut provider = pending_provider(&self.kind, &self.instance_id, self.enabled, &[]);
        provider.models = Vec::new();
        self.decorate(provider)
    }

    fn decorate(&self, mut provider: ServerProvider) -> ServerProvider {
        provider.setup = Some(self.setup.clone());
        provider.runtime_paths = Some(self.runtime_paths.clone());
        self.identity.stamp(&self.kind, &self.instance_id, provider)
    }

    /// `check` in upstream's `makeManagedCodexProvider`.
    async fn check(&self) -> ServerProvider {
        let base = self.base();
        if !self.enabled {
            return base;
        }
        let Ok(executable) = self.runtime.installation.resolve().await else {
            let mut provider = base;
            provider.installed = false;
            provider.models = Vec::new();
            provider.message = tne(MSG_NOT_SET_UP);
            provider.auth = unauthenticated(None, None);
            return provider;
        };
        let saved = self.runtime.auth.read().await.ok().flatten();
        let version = tne(&executable.version);
        let Some(saved) = saved else {
            let mut provider = base;
            provider.installed = true;
            provider.version = version;
            provider.models = Vec::new();
            provider.message = tne(MSG_SIGN_IN);
            provider.auth = unauthenticated(None, None);
            return provider;
        };
        if !has_token_sharing(&saved) {
            let mut provider = base;
            provider.installed = true;
            provider.version = version;
            provider.models = Vec::new();
            provider.message = tne(MSG_SHARING_DISABLED);
            provider.auth = unauthenticated(Some("ChatGPT"), trimmed_email(&saved));
            return provider;
        }
        let usage_limits = external_usage_limits(&base.checked_at);
        let mut managed_auth =
            ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated);
        managed_auth.subscription_sharing = Some(true);
        managed_auth.profile_id = tne(&saved.client_id);
        managed_auth.type_ = tne("chatgpt");
        managed_auth.label = tne("ChatGPT");
        managed_auth.email = trimmed_email(&saved);

        match self.probe_signed_in(&managed_auth).await {
            Ok(mut provider) => {
                provider.auth = managed_auth;
                if provider.version.is_none() {
                    provider.version = version;
                }
                provider.usage_limits = Some(usage_limits);
                // ChatGPT token sharing does not accept service tiers.
                for model in &mut provider.models {
                    if let Some(capabilities) = model.capabilities.as_mut() {
                        capabilities
                            .option_descriptors
                            .get_or_insert_with(Vec::new)
                            .retain(|option| descriptor_id(option) != "serviceTier");
                    }
                }
                provider
                    .slash_commands
                    .retain(|command| command.name.as_str() != "feedback");
                self.decorate(provider)
            }
            Err(()) => {
                // The sign-in may have changed since `saved` was read.
                let current = self.runtime.auth.read().await.ok().flatten();
                let sharing = current.as_ref().is_some_and(has_token_sharing);
                let mut provider = base;
                provider.installed = true;
                provider.version = version;
                provider.models = Vec::new();
                let mut auth = ServerProviderAuth::with_status(if sharing {
                    ServerProviderAuthStatus::Authenticated
                } else {
                    ServerProviderAuthStatus::Unauthenticated
                });
                auth.label = tne("ChatGPT");
                if let (true, Some(current)) = (sharing, current.as_ref()) {
                    auth.subscription_sharing = Some(true);
                    auth.profile_id = tne(&current.client_id);
                }
                auth.email = current.as_ref().and_then(trimmed_email);
                provider.auth = auth;
                if sharing {
                    provider.usage_limits = Some(usage_limits);
                }
                provider.message = tne(MSG_CHECK_FAILED);
                provider
            }
        }
    }

    /// The signed-in half of `check`: resolve the runtime under the sign-in
    /// gate, probe it, and replace the models with the account's catalog.
    async fn probe_signed_in(
        &self,
        managed_auth: &ServerProviderAuth,
    ) -> Result<ServerProvider, ()> {
        let runtime = Arc::clone(&self.runtime);
        let kind = self.kind.clone();
        let instance_id = self.instance_id.clone();
        let enabled = self.enabled;
        let base_config = self.base_config.clone();
        let http = self.http.clone();
        let catalog_url = self.catalog_url.clone();
        let managed_auth = managed_auth.clone();
        let task = AdmittedTask::new(async move {
            let resolved = runtime.resolve().await.map_err(|_| ())?;
            let launch = crate::codex_managed_runtime::managed_launch_config(
                &base_config,
                &resolved.effective,
            );
            let mut provider = check_codex_provider_status(StatusInput {
                kind: &kind,
                instance_id: &instance_id,
                enabled,
                config: &launch,
                custom_models: &[],
                managed_auth: Some(managed_auth),
            })
            .await;
            let token = resolved
                .effective
                .environment
                .get("ACCESS_TOKEN")
                .cloned()
                .ok_or(())?;
            provider.models = chat_gpt_models_at(&http, &catalog_url, &token, &provider.models)
                .await
                .map_err(|_| ())?;
            Ok::<ServerProvider, ()>(provider)
        });
        let controller = &self.runtime.auth.controller;
        let outcome = match controller.with_access.clone() {
            Some(gate) => gate(task).await,
            None => task.run().await,
        };
        match outcome {
            Ok(value) => value
                .downcast::<ServerProvider>()
                .map(|boxed| *boxed)
                .map_err(|_| ()),
            Err(AdmittedFailure::Setup(_)) | Err(AdmittedFailure::Other(_)) => Err(()),
        }
    }
}

fn descriptor_id(option: &turin_contracts::model::ProviderOptionDescriptor) -> &str {
    match option {
        turin_contracts::model::ProviderOptionDescriptor::Select(select) => select.id.as_str(),
        turin_contracts::model::ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str(),
    }
}

#[async_trait]
impl ManagedServerProvider for ManagedCodexSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.cell.get()
    }

    async fn refresh(&self) -> ServerProvider {
        let next = self.check().await;
        self.cell.publish(next.clone());
        next
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        self.cell.stream()
    }

    async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str) {
        self.cell.apply_usage_limits(&update, checked_at);
    }
}

impl ManagedCodexSnapshot {
    /// `snapshotForCwd`: the machine snapshot with this workspace's skills, probed
    /// with the signed-in runtime. Any failure leaves the snapshot as it is.
    async fn workspace_snapshot(&self, cwd: &str) -> ProviderWorkspaceSnapshot {
        let mut provider = self.cell.get();
        if self.enabled {
            let runtime = Arc::clone(&self.runtime);
            let base_config = self.base_config.clone();
            let cwd = cwd.to_owned();
            let task = AdmittedTask::new(async move {
                let resolved = runtime.resolve().await.map_err(|_| ())?;
                let launch = managed_launch_config(&base_config, &resolved.effective);
                tokio::time::timeout(SKILLS_PROBE_TIMEOUT, probe_skills_for_cwd(&launch, &cwd))
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())
            });
            let outcome = match self.runtime.auth.controller.with_access.clone() {
                Some(gate) => gate(task).await,
                None => task.run().await,
            };
            if let Ok(skills) = outcome
                && let Ok(skills) = skills.downcast::<Vec<ServerProviderSkill>>()
            {
                provider.skills = *skills;
            }
        }
        ProviderWorkspaceSnapshot {
            provider,
            slash_commands_pending: None,
        }
    }
}

/// `CodexManagedRuntime.resolve` as text generation's managed-runtime source.
/// The executable lease is not held across the generation (a deviation).
struct ManagedTextRuntime {
    runtime: Arc<CodexManagedRuntime>,
}

#[async_trait]
impl CodexManagedRuntimeSource for ManagedTextRuntime {
    async fn resolve(&self) -> Result<TextEffectiveRuntime, String> {
        let resolved = self
            .runtime
            .resolve()
            .await
            .map_err(|error| error.to_string())?;
        Ok(TextEffectiveRuntime {
            settings: resolved.effective.config.clone(),
            environment: resolved.effective.environment.clone().into_iter().collect(),
        })
    }
}

/// `protect`: every generation runs under the sign-in gate, so a sign-out waits
/// for it and a changing account rejects it.
struct ProtectedTextGeneration {
    inner: Arc<dyn ProviderTextGeneration>,
    controller: ProviderAuthController,
}

impl ProtectedTextGeneration {
    async fn protect<T: Send + 'static>(
        &self,
        operation: &str,
        work: impl Future<Output = Result<T, TextGenerationError>> + Send + 'static,
    ) -> Result<T, TextGenerationError> {
        let failed = |detail: &str| TextGenerationError::TextGenerationError {
            operation: operation.to_owned(),
            detail: detail.to_owned(),
            cause: None,
        };
        let task = AdmittedTask::new(work);
        let outcome = match self.controller.with_access.clone() {
            Some(gate) => gate(task).await,
            None => task.run().await,
        };
        match outcome {
            Ok(value) => value
                .downcast::<T>()
                .map(|boxed| *boxed)
                .map_err(|_| failed("Codex text generation failed.")),
            Err(AdmittedFailure::Other(error)) => Err(error
                .downcast::<TextGenerationError>()
                .map(|error| *error)
                .unwrap_or_else(|_| failed("Codex text generation failed."))),
            Err(AdmittedFailure::Setup(_)) => Err(failed("Codex text generation failed.")),
        }
    }
}

#[async_trait]
impl ProviderTextGeneration for ProtectedTextGeneration {
    async fn generate_commit_message(
        &self,
        input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        let inner = Arc::clone(&self.inner);
        self.protect("generateCommitMessage", async move {
            inner.generate_commit_message(input).await
        })
        .await
    }

    async fn generate_pr_content(
        &self,
        input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        let inner = Arc::clone(&self.inner);
        self.protect("generatePrContent", async move {
            inner.generate_pr_content(input).await
        })
        .await
    }

    async fn generate_branch_name(
        &self,
        input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        let inner = Arc::clone(&self.inner);
        self.protect("generateBranchName", async move {
            inner.generate_branch_name(input).await
        })
        .await
    }

    async fn generate_thread_title(
        &self,
        input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        let inner = Arc::clone(&self.inner);
        self.protect("generateThreadTitle", async move {
            inner.generate_thread_title(input).await
        })
        .await
    }

    async fn generate_review_guide(
        &self,
        input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        let inner = Arc::clone(&self.inner);
        self.protect("generateReviewGuide", async move {
            inner.generate_review_guide(input).await
        })
        .await
    }
}

/// The live model list text generation resolves aliases against.
struct ManagedModels(Arc<ManagedCodexSnapshot>);

#[async_trait]
impl CodexModelSource for ManagedModels {
    async fn models(&self) -> Vec<CodexModel> {
        self.0
            .cell
            .get()
            .models
            .iter()
            .map(|model| CodexModel {
                slug: model.slug.as_str().to_owned(),
                is_custom: model.is_custom,
            })
            .collect()
    }
}

/// `makeManagedCodexProvider`.
pub async fn make_managed_codex_provider(
    kind: &ProviderDriverKind,
    input: ProviderDriverCreateInput,
    settings: CodexSettings,
    host: CodexManagedHost,
    attachment_path: Option<AttachmentPathResolver>,
) -> Result<ProviderInstance, ProviderDriverError> {
    let failed =
        |detail: &str| turin_provider_core::driver::driver_error(kind, &input.instance_id, detail);
    let enabled = settings.enabled && input.enabled;
    let home = std::env::var("HOME").unwrap_or_default();
    let process_env = std::env::vars().collect();
    let environment =
        merge_provider_instance_environment(Some(&input.environment), &process_env, &home);
    let runtime = CodexManagedRuntime::make(CodexManagedRuntimeOptions {
        instance_id: input.instance_id.clone(),
        enabled,
        environment,
        config: settings.clone(),
        host,
    })
    .await
    .map_err(|error| failed(&error.to_string()))?;
    let runtime = Arc::new(runtime);
    let continuation: ProviderContinuationIdentity =
        codex_continuation_identity(kind, &runtime.home_layout);
    let identity = SnapshotIdentity {
        display_name: input
            .display_name
            .as_ref()
            .map(|name| name.as_str().to_owned()),
        accent_color: input
            .accent_color
            .as_ref()
            .map(|color| color.as_str().to_owned()),
        continuation_group_key: continuation.continuation_key.clone(),
    };
    let layout = &runtime.home_layout;
    let runtime_paths = ServerProviderRuntimePaths {
        home_path: tne(&layout.shared_home_path.to_string_lossy())
            .ok_or_else(|| failed("Codex home path is empty."))?,
        shadow_home_path: (layout.mode == CodexHomeMode::AuthOverlay)
            .then(|| tne(&runtime.home_path.to_string_lossy()))
            .flatten(),
    };
    let base_config = CodexRuntimeConfig {
        binary: PathBuf::from(settings.binary_path.as_str()),
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        home_path: None,
        launch_args: Vec::new(),
        environment: Default::default(),
        remove_environment: Vec::new(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let snapshot = Arc::new(ManagedCodexSnapshot {
        kind: kind.clone(),
        instance_id: input.instance_id.clone(),
        enabled,
        identity,
        runtime: Arc::clone(&runtime),
        base_config: base_config.clone(),
        http: reqwest::Client::new(),
        catalog_url: runtime
            .catalog_url
            .clone()
            .unwrap_or_else(|| CATALOG_URL.to_owned()),
        setup: ServerProviderSetup {
            can_authenticate: true,
            can_install: true,
            documentation_url: None,
        },
        runtime_paths,
        maintenance: make_manual_only_provider_maintenance_capabilities(kind.clone()),
        cell: SnapshotCell::new(pending_provider(kind, &input.instance_id, enabled, &[])),
    });
    snapshot.cell.publish(snapshot.base());

    // A settled sign-in changes what the snapshot says. Sign-out closes the scope
    // of an in-flight probe, which fails that refresh; keep listening so the
    // sign-out's own idle still refreshes.
    let mut states = Box::into_pin((runtime.auth.controller.subscribe)(
        "managed-codex-snapshot".to_owned(),
    ));
    let watcher = Arc::clone(&snapshot);
    let scope = Arc::clone(&input.scope);
    tokio::spawn(async move {
        loop {
            let state = tokio::select! {
                _ = scope.cancelled() => break,
                state = states.next() => state,
            };
            let Some(state) = state else { break };
            if matches!(
                state.phase,
                ProviderAuthPhase::Idle
                    | ProviderAuthPhase::Succeeded
                    | ProviderAuthPhase::Failed
                    | ProviderAuthPhase::Cancelled
            ) {
                watcher.refresh().await;
            }
        }
    });

    let usage_target = Arc::clone(&snapshot);
    let adapter = CodexProviderAdapter::with_runtime_resolver(
        input.instance_id.clone(),
        base_config,
        Arc::clone(&runtime) as Arc<dyn CodexRuntimeResolver>,
    )
    .with_usage_sink(Arc::new(move |update, checked_at| {
        usage_target.cell.apply_usage_limits(&update, &checked_at);
    }));
    // Generations run under the sign-in gate, with the signed-in runtime.
    let native_text = codex_text_generation(CodexTextGenerationOptions {
        settings: settings.clone(),
        environment: None,
        models: Arc::new(ManagedModels(Arc::clone(&snapshot))),
        managed_runtime: Some(Arc::new(ManagedTextRuntime {
            runtime: Arc::clone(&runtime),
        })),
        resolve_attachment_path: attachment_path.unwrap_or_else(|| Arc::new(|_| None)),
        home_dir: home.clone(),
    });
    let text_generation: Arc<dyn ProviderTextGeneration> = Arc::new(ProtectedTextGeneration {
        inner: Arc::new(native_text),
        controller: runtime.auth.controller.clone(),
    });
    let for_cwd = Arc::clone(&snapshot);
    let snapshot_for_cwd: SnapshotForCwd = Arc::new(move |cwd: String| {
        let snapshot = Arc::clone(&for_cwd);
        Box::pin(async move { Ok(snapshot.workspace_snapshot(&cwd).await) })
    });
    let auth = Arc::new(runtime.auth.controller.clone());
    Ok(ProviderInstance {
        instance_id: input.instance_id.clone(),
        driver_kind: kind.clone(),
        continuation_identity: continuation,
        display_name: input
            .display_name
            .as_ref()
            .map(|name| name.as_str().to_owned()),
        accent_color: input
            .accent_color
            .as_ref()
            .map(|color| color.as_str().to_owned()),
        enabled: input.enabled,
        snapshot,
        snapshot_for_cwd: Some(snapshot_for_cwd),
        refresh_models: None,
        invalidate_caches: None,
        consume_reset_credit: None,
        orchestration_adapter: Arc::new(adapter),
        text_generation,
        auth: Some(auth),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU16, Ordering},
        time::Duration,
    };

    use parking_lot::Mutex;
    use serde_json::json;
    use turin_contracts::provider_setup::ProviderInstallSource;
    use turin_contracts::provider_usage_limits::{
        ServerProviderUsageWindow, ServerProviderUsageWindowKind,
    };
    use turin_provider_core::text_generation::ThreadTitleGenerationInput;
    use turin_provider_core::{
        credential_store::{MemorySecretStore, ProviderCredentials, SecretStore},
        driver::{ProviderDriver, ProviderInstanceScope},
        server_provider::ServerProviderState,
    };

    use super::*;
    use crate::{
        codex_chatgpt_auth::mini_http,
        codex_installation::{CodexExecutable, CodexInstallationError},
        codex_managed_runtime::{CodexExecutableLease, CodexExecutableSource},
        driver::CodexDriver,
    };

    /// A `codex app-server` that answers the status probe with canned replies.
    const FAKE_APP_SERVER: &str = r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in mcp) is_mcp=1 ;; exec) is_exec=1 ;; esac
done
if [ "$is_mcp" = 1 ]; then printf '[]'; exit 0; fi
if [ "$is_exec" = 1 ]; then
  out=''; prev=''
  for a in "$@"; do
    if [ "$prev" = --output-last-message ]; then out="$a"; fi
    prev="$a"
  done
  cat >/dev/null
  printf '%s\n' "$ACCESS_TOKEN" > "$(dirname "$0")/exec-token.txt"
  printf '%s\n' '{"title":"Managed title"}' > "$out"
  exit 0
fi
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$(dirname "$0")/requests.log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex-cli/0.156.1","codexHome":"/tmp/fake-codex-home","platformFamily":"unix","platformOs":"macos"}}\n' "$id" ;;
    *'"method":"account/read"'*) printf '{"id":%s,"result":{"account":{"type":"apiKey"},"requiresOpenaiAuth":false}}\n' "$id" ;;
    *'"method":"model/list"'*) printf '{"id":%s,"result":{"data":[{"id":"gpt-5.5","model":"gpt-5.5","displayName":"gpt-5.5","description":"d","hidden":false,"isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"l"},{"reasoningEffort":"high","description":"h"}],"serviceTiers":[{"id":"fast","name":"Fast","description":"f"}]}]}}\n' "$id" ;;
    *'"method":"account/rateLimits/read"'*) printf '{"id":%s,"error":{"code":-32601,"message":"not available"}}\n' "$id" ;;
    *'"method":"skills/list"'*)
      cwd=$(printf '%s' "$line" | sed -n 's/.*"cwds":\["\([^"]*\)"\].*/\1/p')
      printf '{"id":%s,"result":{"data":[{"cwd":"%s","errors":[],"skills":[{"name":"workspace-skill","description":"d","scope":"repo","path":"%s/SKILL.md","enabled":true}]}]}}\n' "$id" "$cwd" "$cwd" ;;
  esac
done
"#;

    struct FakeInstallation {
        executable: Option<CodexExecutable>,
    }

    #[async_trait]
    impl CodexExecutableSource for FakeInstallation {
        async fn resolve(&self) -> Result<CodexExecutable, CodexInstallationError> {
            self.executable
                .clone()
                .ok_or_else(|| CodexInstallationError {
                    operation: "resolve",
                    detail: "not installed".to_owned(),
                })
        }

        async fn acquire(
            &self,
        ) -> Result<(CodexExecutable, CodexExecutableLease), CodexInstallationError> {
            Ok((self.resolve().await?, Box::new(())))
        }
    }

    struct Catalog {
        origin: String,
        status: Arc<AtomicU16>,
        authorizations: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Catalog {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn catalog() -> Catalog {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let status = Arc::new(AtomicU16::new(200));
        let authorizations = Arc::new(Mutex::new(Vec::new()));
        let (served_status, seen) = (Arc::clone(&status), Arc::clone(&authorizations));
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let Ok(request) = mini_http::read_request(&mut socket).await else {
                    continue;
                };
                if let Some(value) = request.headers.get("authorization") {
                    seen.lock().push(value.clone());
                }
                let body = json!({"models": [
                    {"slug": "gpt-5.5", "display_name": "GPT 5.5", "visibility": "list"},
                    {"slug": "hidden", "display_name": "Hidden", "visibility": "hide"},
                ]})
                .to_string();
                let _ = mini_http::write_response(
                    &mut socket,
                    served_status.load(Ordering::SeqCst),
                    "OK",
                    &[("content-type", "application/json")],
                    body.as_bytes(),
                )
                .await;
            }
        });
        Catalog {
            origin,
            status,
            authorizations,
            task,
        }
    }

    struct Setup {
        installed: bool,
        scopes: Option<Vec<&'static str>>,
        script: bool,
    }

    struct Harness {
        _temp: tempfile::TempDir,
        shared_home: PathBuf,
        instance: ProviderInstance,
        catalog: Catalog,
    }

    async fn harness(instance_id: &str, setup: Setup) -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let shared_home = temp.path().join("shared-home");
        std::fs::create_dir_all(&shared_home).unwrap();
        let binary = temp.path().join("codex");
        if setup.script {
            std::fs::write(&binary, FAKE_APP_SERVER).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        if let Some(scopes) = &setup.scopes {
            ProviderCredentials::open(Arc::clone(&secrets), "codex-chatgpt", instance_id)
                .set(
                    json!({
                        "clientId": "oaiapp_test",
                        "accessToken": "dummy-owned-access",
                        "refreshToken": "dummy-refresh",
                        "expiresAt": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64 + 3_600_000,
                        "earliestRefreshAt": null,
                        "scopes": scopes,
                        "subject": "test-user",
                        "email": " person@example.test ",
                    })
                    .to_string()
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        let catalog = catalog().await;
        let executable = setup.installed.then(|| CodexExecutable {
            executable_path: binary,
            source: ProviderInstallSource::Managed,
            version: "0.156.1".to_owned(),
            managed_version_directory: None,
        });
        let host = CodexManagedHost {
            state_dir: temp.path().join("userdata"),
            user_home: temp.path().join("home").to_string_lossy().into_owned(),
            server_url: "http://localhost:3773".to_owned(),
            secrets,
            secrets_directory: None,
            environment_id: "00000000-0000-4000-8000-000000000001".to_owned(),
            installation: Arc::new(FakeInstallation { executable }),
            discovery_url: None,
            catalog_url: Some(format!("{}/v1/models", catalog.origin)),
        };
        let driver = CodexDriver::with_managed_host(host);
        let instance = driver
            .create(ProviderDriverCreateInput {
                instance_id: ProviderInstanceId::new(instance_id).unwrap(),
                display_name: TrimmedNonEmptyString::new("Work Codex").ok(),
                accent_color: None,
                environment: Vec::new(),
                enabled: true,
                config: json!({
                    "setupMode": "managed",
                    "homePath": shared_home.to_string_lossy(),
                }),
                scope: ProviderInstanceScope::new(),
            })
            .await
            .unwrap();
        Harness {
            _temp: temp,
            shared_home,
            instance,
            catalog,
        }
    }

    #[tokio::test]
    async fn managed_instances_fail_to_create_without_a_host() {
        let Err(error) = CodexDriver::new()
            .create(ProviderDriverCreateInput {
                instance_id: ProviderInstanceId::new("codex").unwrap(),
                display_name: None,
                accent_color: None,
                environment: Vec::new(),
                enabled: true,
                config: json!({"setupMode": "managed"}),
                scope: ProviderInstanceScope::new(),
            })
            .await
        else {
            panic!("managed create without a host");
        };
        assert!(error.detail.contains("Managed Codex needs the host"));
    }

    #[tokio::test]
    async fn a_new_managed_instance_starts_pending_with_setup_and_paths() {
        let h = harness(
            "codex-work",
            Setup {
                installed: false,
                scopes: None,
                script: false,
            },
        )
        .await;
        let snapshot = h.instance.snapshot.get_snapshot().await;
        assert!(!snapshot.installed);
        assert!(snapshot.models.is_empty());
        let setup = snapshot.setup.as_ref().unwrap();
        assert!(setup.can_authenticate && setup.can_install);
        let paths = snapshot.runtime_paths.as_ref().unwrap();
        assert_eq!(
            paths.home_path.as_str(),
            h.shared_home.to_string_lossy().as_ref()
        );
        // An additional instance keeps its own auth under a shadow home.
        assert!(
            paths
                .shadow_home_path
                .as_ref()
                .unwrap()
                .as_str()
                .contains("userdata/providers/codex/codex-work/shadow")
        );
        assert_eq!(
            snapshot.display_name.as_ref().unwrap().as_str(),
            "Work Codex"
        );
        assert_eq!(
            h.instance.continuation_identity.continuation_key,
            format!("codex:home:{}", h.shared_home.display())
        );
        assert!(h.instance.auth.is_some());
    }

    #[tokio::test]
    async fn without_an_installation_it_asks_to_set_up_codex() {
        let h = harness(
            "codex",
            Setup {
                installed: false,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: false,
            },
        )
        .await;
        let snapshot = h.instance.snapshot.refresh().await;
        assert!(!snapshot.installed);
        assert_eq!(snapshot.message.as_ref().unwrap().as_str(), MSG_NOT_SET_UP);
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );
        assert!(
            snapshot
                .runtime_paths
                .as_ref()
                .unwrap()
                .shadow_home_path
                .is_none()
        );
    }

    #[tokio::test]
    async fn without_a_saved_account_it_asks_to_sign_in() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: None,
                script: false,
            },
        )
        .await;
        let snapshot = h.instance.snapshot.refresh().await;
        assert!(snapshot.installed);
        assert_eq!(snapshot.version.as_ref().unwrap().as_str(), "0.156.1");
        assert_eq!(snapshot.message.as_ref().unwrap().as_str(), MSG_SIGN_IN);
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );
        assert!(snapshot.models.is_empty());
    }

    #[tokio::test]
    async fn an_account_without_token_sharing_cannot_run_codex() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["openid"]),
                script: false,
            },
        )
        .await;
        let snapshot = h.instance.snapshot.refresh().await;
        assert_eq!(
            snapshot.message.as_ref().unwrap().as_str(),
            MSG_SHARING_DISABLED
        );
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );
        assert_eq!(snapshot.auth.label.as_ref().unwrap().as_str(), "ChatGPT");
        assert_eq!(
            snapshot.auth.email.as_ref().unwrap().as_str(),
            "person@example.test"
        );
    }

    #[tokio::test]
    async fn a_signed_in_account_reports_chatgpt_models_and_skips_native_usage() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: true,
            },
        )
        .await;
        let snapshot = h.instance.snapshot.refresh().await;
        assert_eq!(
            snapshot.status,
            ServerProviderState::Ready,
            "{:?}",
            snapshot.message
        );
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Authenticated
        );
        assert_eq!(snapshot.auth.subscription_sharing, Some(true));
        assert_eq!(
            snapshot.auth.profile_id.as_ref().unwrap().as_str(),
            "oaiapp_test"
        );
        assert_eq!(snapshot.auth.type_.as_ref().unwrap().as_str(), "chatgpt");
        assert_eq!(snapshot.auth.label.as_ref().unwrap().as_str(), "ChatGPT");
        assert_eq!(
            snapshot.auth.email.as_ref().unwrap().as_str(),
            "person@example.test"
        );
        // The catalog decides the choices; hidden models stay out.
        let slugs: Vec<_> = snapshot.models.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, ["gpt-5.5"]);
        assert_eq!(snapshot.models[0].name.as_str(), "GPT 5.5");
        // Token sharing does not accept service tiers; reasoning stays.
        let ids: Vec<_> = snapshot.models[0]
            .capabilities
            .as_ref()
            .unwrap()
            .option_descriptors
            .as_ref()
            .unwrap()
            .iter()
            .map(descriptor_id)
            .collect();
        assert_eq!(ids, ["reasoningEffort"]);
        let usage = snapshot.usage_limits.as_ref().unwrap();
        assert_eq!(
            usage.external_usage.as_ref().unwrap().url.as_str(),
            "https://chatgpt.com/#settings/Usage"
        );
        assert!(usage.windows.is_empty());
        let commands: Vec<_> = snapshot
            .slash_commands
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(commands, ["compact", "goal"]);
        // `skipNativeUsage`: the app-server's own rate limits are never read.
        let log = std::fs::read_to_string(h._temp.path().join("requests.log")).unwrap();
        assert!(log.contains(r#""method":"account/read""#));
        assert!(!log.contains("rateLimits"), "{log}");
        // The initial sign-in state also refreshes, so the catalog may be read twice.
        let authorizations = h.catalog.authorizations.lock();
        assert!(!authorizations.is_empty());
        assert!(
            authorizations
                .iter()
                .all(|value| value == "Bearer dummy-owned-access")
        );
    }

    #[tokio::test]
    async fn a_failed_catalog_keeps_the_account_and_says_to_retry() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: true,
            },
        )
        .await;
        h.catalog.status.store(500, Ordering::SeqCst);
        let snapshot = h.instance.snapshot.refresh().await;
        assert_eq!(
            snapshot.message.as_ref().unwrap().as_str(),
            MSG_CHECK_FAILED
        );
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Authenticated
        );
        assert_eq!(snapshot.auth.subscription_sharing, Some(true));
        assert_eq!(
            snapshot.auth.profile_id.as_ref().unwrap().as_str(),
            "oaiapp_test"
        );
        assert!(snapshot.models.is_empty());
        assert!(snapshot.usage_limits.is_some());
        assert!(snapshot.installed);
    }

    #[tokio::test]
    async fn signing_out_refreshes_the_snapshot() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: false,
            },
        )
        .await;
        h.catalog.status.store(500, Ordering::SeqCst);
        h.instance.snapshot.refresh().await;
        let auth = h.instance.auth.as_ref().unwrap();
        // The stored refresh token has nothing to revoke against in this test; the
        // local sign-out still settles to idle, which refreshes the snapshot.
        (auth.logout)(Box::pin(async { Ok(()) })).await.unwrap();
        let settled = async {
            loop {
                let snapshot = h.instance.snapshot.get_snapshot().await;
                if snapshot.message.as_ref().map(|m| m.as_str()) == Some(MSG_SIGN_IN) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let snapshot = tokio::time::timeout(Duration::from_secs(20), settled)
            .await
            .expect("sign-out refreshes the snapshot");
        assert_eq!(
            snapshot.auth.status,
            ServerProviderAuthStatus::Unauthenticated
        );
    }

    #[tokio::test]
    async fn only_a_managed_status_skips_the_native_usage_read() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: None,
                script: true,
            },
        )
        .await;
        let config = CodexRuntimeConfig {
            binary: h._temp.path().join("codex"),
            cwd: h._temp.path().to_path_buf(),
            home_path: None,
            launch_args: Vec::new(),
            environment: Default::default(),
            remove_environment: Vec::new(),
            client_version: "1".to_owned(),
        };
        let kind = ProviderDriverKind::new("codex").unwrap();
        let instance_id = ProviderInstanceId::new("codex").unwrap();
        let status = |managed_auth| {
            check_codex_provider_status(StatusInput {
                kind: &kind,
                instance_id: &instance_id,
                enabled: true,
                config: &config,
                custom_models: &[],
                managed_auth,
            })
        };
        let log = h._temp.path().join("requests.log");
        let managed = ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated);
        let provider = status(Some(managed.clone())).await;
        assert_eq!(provider.auth, managed);
        assert!(provider.usage_limits.is_none());
        assert!(
            !std::fs::read_to_string(&log)
                .unwrap()
                .contains("rateLimits")
        );

        let provider = status(None).await;
        assert!(
            std::fs::read_to_string(&log)
                .unwrap()
                .contains("account/rateLimits/read")
        );
        assert!(provider.usage_limits.is_some());
    }

    #[tokio::test]
    async fn snapshot_for_cwd_probes_skills_with_the_signed_in_runtime() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: true,
            },
        )
        .await;
        let for_cwd = h.instance.snapshot_for_cwd.clone().expect("snapshotForCwd");
        let workspace = for_cwd("/work/project".to_owned()).await.unwrap();
        let skills: Vec<_> = workspace
            .provider
            .skills
            .iter()
            .map(|skill| skill.path.as_str().to_owned())
            .collect();
        assert_eq!(skills, ["/work/project/SKILL.md"]);
        assert_eq!(workspace.provider.instance_id.as_str(), "codex");
    }

    #[tokio::test]
    async fn snapshot_for_cwd_keeps_the_snapshot_when_signed_out() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: None,
                script: true,
            },
        )
        .await;
        let for_cwd = h.instance.snapshot_for_cwd.clone().unwrap();
        let workspace = for_cwd("/work/project".to_owned()).await.unwrap();
        assert!(workspace.provider.skills.is_empty());
    }

    #[tokio::test]
    async fn text_generation_runs_with_the_signed_in_token() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: Some(vec!["chatgpt.tokens.use.direct"]),
                script: true,
            },
        )
        .await;
        let selection =
            serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5.4-mini"}))
                .unwrap();
        let result = h
            .instance
            .text_generation
            .generate_thread_title(ThreadTitleGenerationInput {
                linked_context: None,
                cwd: h._temp.path().to_string_lossy().into_owned(),
                message: "Name this".to_owned(),
                previous_title: None,
                attachments: Vec::new(),
                model_selection: selection,
            })
            .await
            .unwrap();
        assert_eq!(result.title, "Managed title");
        assert_eq!(
            std::fs::read_to_string(h._temp.path().join("exec-token.txt"))
                .unwrap()
                .trim(),
            "dummy-owned-access"
        );
    }

    #[tokio::test]
    async fn text_generation_fails_cleanly_when_signed_out() {
        let h = harness(
            "codex",
            Setup {
                installed: true,
                scopes: None,
                script: true,
            },
        )
        .await;
        let selection =
            serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5.4-mini"}))
                .unwrap();
        let error = h
            .instance
            .text_generation
            .generate_thread_title(ThreadTitleGenerationInput {
                linked_context: None,
                cwd: h._temp.path().to_string_lossy().into_owned(),
                message: "Name this".to_owned(),
                previous_title: None,
                attachments: Vec::new(),
                model_selection: selection,
            })
            .await
            .expect_err("no account");
        let TextGenerationError::TextGenerationError { detail, .. } = &error;
        assert!(!detail.is_empty(), "{error:?}");
        assert!(!h._temp.path().join("exec-token.txt").exists());
    }

    #[tokio::test]
    async fn usage_pushes_update_the_managed_snapshot() {
        let h = harness(
            "codex",
            Setup {
                installed: false,
                scopes: None,
                script: false,
            },
        )
        .await;
        let window = ServerProviderUsageWindow {
            id: TrimmedNonEmptyString::new("primary").unwrap(),
            kind: ServerProviderUsageWindowKind::Session,
            label: TrimmedNonEmptyString::new("Session").unwrap(),
            used_percent: 12.0,
            resets_at: None,
            window_duration_mins: Some(300),
        };
        h.instance
            .snapshot
            .apply_usage_limits(
                ProviderUsageLimitsUpdate {
                    windows: vec![window],
                },
                "2026-01-01T00:00:00.000Z",
            )
            .await;
        let limits = h
            .instance
            .snapshot
            .get_snapshot()
            .await
            .usage_limits
            .clone()
            .unwrap();
        assert_eq!(limits.windows[0].used_percent, 12.0);
    }
}
