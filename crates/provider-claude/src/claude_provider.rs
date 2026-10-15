//! Port of upstream/apps/server/src/provider/ClaudeProvider.ts: the status
//! probe (installed, version, auth, models, usage), the pending snapshot, and
//! the workspace snapshot. The SDK capability probe lives in
//! `claude_capabilities_probe`, and this module consumes it through a resolver.
//!
//! Skills come from `claude_skills`, scanned from the config dir and, for a
//! workspace snapshot, the workspace's `.claude/skills`.
use crate::{
    claude_capabilities_probe::{
        ClaudeCapabilitiesProbe, DEFAULT_PROBE_TIMEOUT, dedupe_slash_commands,
        probe_claude_capabilities,
    },
    claude_home::{ProcessEnv, make_claude_environment},
    claude_model_catalog::{
        ClaudeModelCatalog, format_claude_version_upgrade_message, read_custom_model_entries,
        resolve_claude_models_for_version, resolve_claude_update_required_models,
    },
    claude_skills::discover_claude_skills,
    claude_usage_limits::{
        SharedClaudeScopedLimitNames, claude_usage_response_to_limits, record_claude_usage_response,
    },
};
use chrono::{SecondsFormat, Utc};
use std::{path::Path, process::Stdio, sync::Arc};
use tokio::{io::AsyncReadExt, process::Command};
use turin_contracts::{
    TrimmedNonEmptyString,
    model::{CustomModelSetting, ModelCapabilities},
    provider_usage_limits::{
        ServerProviderResetCredits, ServerProviderUsageLimits, ServerProviderUsageUnavailableReason,
    },
    settings::ClaudeSettings,
};
use turin_provider_core::{
    rt::BoxFuture,
    server_provider::{
        ProviderWorkspaceSnapshot, ServerProvider, ServerProviderAuth, ServerProviderAuthStatus,
        ServerProviderDraft, ServerProviderModel, ServerProviderSlashCommand, ServerProviderState,
        ServerProviderUpdateRequiredModel,
    },
    snapshot_probe::{
        BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
        build_server_provider, parse_generic_cli_version,
    },
    usage_limits::make_unavailable_usage_limits,
};

/// `CLAUDE_PRESENTATION`.
fn presentation() -> ServerProviderPresentation {
    ServerProviderPresentation {
        display_name: "Claude".to_owned(),
        badge_label: None,
        show_interaction_mode_toggle: Some(true),
        reports_context_window: Some(true),
        supported_runtime_modes: None,
        requires_new_thread_for_model_change: None,
        supports_conversation_rollback: None,
    }
}

/// `COMPACT_SLASH_COMMAND` from snapshotProbe.ts.
pub fn compact_slash_command() -> ServerProviderSlashCommand {
    ServerProviderSlashCommand {
        name: tne("compact").expect("literal is non-empty"),
        description: tne("Summarize the conversation and reduce context usage"),
        input: None,
    }
}

/// Resolves the account capability probe. Upstream caches it per instance for
/// five minutes, and the driver supplies that cache.
pub type CapabilitiesResolver =
    Arc<dyn Fn() -> BoxFuture<'static, Option<ClaudeCapabilitiesProbe>> + Send + Sync>;

/// Resolves banked reset credits for a subscription login, given the CLI version.
pub type ResetCreditsResolver =
    Arc<dyn Fn(String) -> BoxFuture<'static, Option<ServerProviderResetCredits>> + Send + Sync>;

/// Inputs to [`check_claude_provider_status`]. Upstream threads these through
/// function arguments with defaults.
pub struct ClaudeStatusInput<'a> {
    pub settings: &'a ClaudeSettings,
    /// The host environment the instance inherits.
    pub environment: &'a ProcessEnv,
    /// The host's home directory, for `~` expansion of the instance home.
    pub home: &'a str,
    pub cwd: Option<&'a Path>,
    pub catalog: &'a ClaudeModelCatalog,
    /// Shared with the adapter so turn events reuse the scoped bucket names this probe saw.
    pub scoped_limit_names: Option<&'a SharedClaudeScopedLimitNames>,
    pub resolve_capabilities: Option<CapabilitiesResolver>,
    pub resolve_reset_credits: Option<ResetCreditsResolver>,
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `providerModelsFromSettings` from snapshotProbe.ts: appends the user's
/// custom models after the built-ins. A named entry whose slug matches a
/// built-in renames it instead.
pub fn provider_models_from_settings(
    built_in: Vec<ServerProviderModel>,
    custom_models: &[CustomModelSetting],
    custom_model_capabilities: Option<ModelCapabilities>,
) -> Vec<ServerProviderModel> {
    let entries = read_custom_model_entries(custom_models);
    if entries.is_empty() {
        return built_in;
    }
    let name_overrides: std::collections::HashMap<&str, &str> = entries
        .iter()
        .filter(|entry| entry.name != entry.slug)
        .map(|entry| (entry.slug.as_str(), entry.name.as_str()))
        .collect();
    let resolved: Vec<ServerProviderModel> = built_in
        .into_iter()
        .map(|mut model| {
            if let Some(name) = name_overrides.get(model.slug.as_str()) {
                // The built-in short name would undo the override in compact UI.
                model.name = tne(name).unwrap_or(model.name);
                model.short_name = None;
            }
            model
        })
        .collect();
    let mut seen: std::collections::HashSet<String> = resolved
        .iter()
        .map(|model| model.slug.as_str().to_owned())
        .collect();
    let mut models = resolved;
    for entry in entries {
        if !seen.insert(entry.slug.clone()) {
            continue;
        }
        let (Some(slug), Some(name)) = (tne(&entry.slug), tne(&entry.name)) else {
            continue;
        };
        models.push(ServerProviderModel {
            slug,
            name,
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: true,
            is_default: None,
            is_legacy: None,
            capabilities: entry
                .capabilities
                .or_else(|| custom_model_capabilities.clone()),
        });
    }
    models
}

/// `DEFAULT_CLAUDE_MODEL_CAPABILITIES`: a custom model without its own
/// capabilities presents no options.
fn default_claude_model_capabilities() -> Option<ModelCapabilities> {
    Some(ModelCapabilities {
        option_descriptors: Some(Vec::new()),
    })
}

/// `toTitleCaseWords`: split on whitespace, `_`, and `-`, then capitalize each word.
fn to_title_case_words(value: &str) -> String {
    value
        .split(|ch: char| ch.is_whitespace() || ch == '_' || ch == '-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let first = chars.next().map(|ch| ch.to_uppercase().collect::<String>());
            let rest = chars.as_str().to_lowercase();
            format!("{}{rest}", first.unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lowercased with whitespace, `_` and `-` removed, as the subscription label switch reads it.
fn squash(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|ch| !(ch.is_whitespace() || *ch == '_' || *ch == '-'))
        .collect()
}

/// `claudeSubscriptionLabel`.
fn claude_subscription_label(subscription_type: Option<&str>) -> Option<String> {
    let subscription_type = subscription_type?;
    let normalized = squash(subscription_type);
    if normalized.is_empty() {
        return None;
    }
    let label = match normalized.as_str() {
        "claudemaxsubscription" | "max" | "maxplan" => "Max",
        "claudemax5xsubscription" | "max5" => "Max 5x",
        "claudemax20xsubscription" | "max20" => "Max 20x",
        "claudeenterprisesubscription" | "enterprise" => "Enterprise",
        "claudeteamsubscription" | "team" => "Team",
        "claudeprosubscription" | "pro" => "Pro",
        "claudefreesubscription" | "free" => "Free",
        _ => return Some(to_title_case_words(subscription_type)),
    };
    Some(label.to_owned())
}

/// `normalizeClaudeAuthMethod`: an API key method maps to `apiKey`.
fn normalize_claude_auth_method(auth_method: Option<&str>) -> Option<&'static str> {
    let normalized = squash(auth_method?);
    matches!(
        normalized.as_str(),
        "apikey" | "anthropicapikey" | "anthropicauthtoken"
    )
    .then_some("apiKey")
}

/// `formatClaudeSubscriptionAuthLabel`.
fn format_claude_subscription_auth_label(subscription_type: &str) -> String {
    let subscription_label = claude_subscription_label(Some(subscription_type))
        .unwrap_or_else(|| to_title_case_words(subscription_type));
    let normalized = squash(&subscription_label);
    if normalized.starts_with("claude") && normalized.ends_with("subscription") {
        subscription_label
    } else if normalized.starts_with("claude") {
        format!("{subscription_label} Subscription")
    } else if normalized.ends_with("subscription") {
        format!("Claude {subscription_label}")
    } else {
        format!("Claude {subscription_label} Subscription")
    }
}

/// `claudeAuthMetadata`: the auth type and label for the snapshot.
fn claude_auth_metadata(
    subscription_type: Option<&str>,
    auth_method: Option<&str>,
) -> Option<(String, String)> {
    if normalize_claude_auth_method(auth_method) == Some("apiKey") {
        return Some(("apiKey".to_owned(), "Claude API Key".to_owned()));
    }
    let subscription_type = subscription_type.filter(|value| !value.is_empty())?;
    // The type is the raw subscription string, as upstream stores it.
    Some((
        subscription_type.to_owned(),
        format_claude_subscription_auth_label(subscription_type),
    ))
}

/// `apiProviderAuthMetadata`: Bedrock logs in through AWS credentials.
fn api_provider_auth_metadata(api_provider: Option<&str>) -> Option<(String, String)> {
    (api_provider == Some("bedrock")).then(|| ("bedrock".to_owned(), "Amazon Bedrock".to_owned()))
}

/// `claudeAuthStatus`. Only `tokenSource: "none"` disproves authentication.
/// Third-party backends and profile logins often omit token fields, so silence
/// counts as authenticated, and an API key or account field is evidence too.
pub fn claude_auth_status(capabilities: &ClaudeCapabilitiesProbe) -> ServerProviderAuthStatus {
    if capabilities
        .api_provider
        .as_deref()
        .is_some_and(|provider| provider != "firstParty")
    {
        return ServerProviderAuthStatus::Authenticated;
    }
    if capabilities.token_source.as_deref() != Some("none") {
        return ServerProviderAuthStatus::Authenticated;
    }
    let has_api_key = capabilities
        .api_key_source
        .as_deref()
        .is_some_and(|source| !source.is_empty() && source != "none");
    let has_account = capabilities
        .email
        .as_deref()
        .is_some_and(|email| !email.is_empty())
        || capabilities
            .subscription_type
            .as_deref()
            .is_some_and(|subscription| !subscription.is_empty());
    if has_api_key || has_account {
        ServerProviderAuthStatus::Authenticated
    } else {
        ServerProviderAuthStatus::Unauthenticated
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

enum CommandFailure {
    NotFound,
    Other,
}

/// `runClaudeCommand` plus the spawn-and-collect of snapshotProbe.ts. The child
/// is killed on drop, so a timeout cannot leave it running.
async fn run_claude_command(
    binary: &str,
    args: &[&str],
    environment: &ProcessEnv,
    cwd: Option<&Path>,
) -> Result<CommandOutput, CommandFailure> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => CommandFailure::NotFound,
        _ => CommandFailure::Other,
    })?;
    let mut stdout = child.stdout.take().ok_or(CommandFailure::Other)?;
    let mut stderr = child.stderr.take().ok_or(CommandFailure::Other)?;
    let (out, err, status) = tokio::join!(
        async {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await.map(|_| buf)
        },
        async {
            let mut buf = Vec::new();
            stderr.read_to_end(&mut buf).await.map(|_| buf)
        },
        child.wait(),
    );
    let status = status.map_err(|_| CommandFailure::Other)?;
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&out.map_err(|_| CommandFailure::Other)?).into_owned(),
        stderr: String::from_utf8_lossy(&err.map_err(|_| CommandFailure::Other)?).into_owned(),
        code: status.code(),
    })
}

fn probe_result(
    installed: bool,
    version: Option<String>,
    status: ServerProviderState,
    auth: ServerProviderAuth,
    message: Option<String>,
) -> ProviderProbeResult {
    ProviderProbeResult {
        installed,
        version,
        status,
        auth,
        message,
        usage_limits: None,
    }
}

fn unknown_auth() -> ServerProviderAuth {
    ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown)
}

/// `checkClaudeProviderStatus`: the full status draft for one instance.
pub async fn check_claude_provider_status(input: ClaudeStatusInput<'_>) -> ServerProviderDraft {
    let settings = input.settings;
    let checked_at = now_iso();
    let all_models = provider_models_from_settings(
        input
            .catalog
            .models
            .iter()
            .map(|entry| entry.model.clone())
            .collect(),
        &settings.custom_models,
        default_claude_model_capabilities(),
    );

    if !settings.enabled {
        return build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: presentation(),
            enabled: false,
            checked_at,
            models: all_models,
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: probe_result(
                false,
                None,
                ServerProviderState::Warning,
                unknown_auth(),
                Some("Claude is disabled in T3 Code settings.".to_owned()),
            ),
            maintenance: None,
        });
    }

    let environment =
        make_claude_environment(settings.home_path.as_str(), input.environment, input.home);
    let binary = settings.binary_path.as_str();
    let version_probe = tokio::time::timeout(
        DEFAULT_PROBE_TIMEOUT,
        run_claude_command(binary, &["--version"], &environment, input.cwd),
    )
    .await;

    let failure = |installed: bool, message: &str| {
        build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: presentation(),
            enabled: true,
            checked_at: checked_at.clone(),
            models: all_models.clone(),
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: probe_result(
                installed,
                None,
                ServerProviderState::Error,
                unknown_auth(),
                Some(message.to_owned()),
            ),
            maintenance: None,
        })
    };

    let version = match version_probe {
        Err(_) => {
            return failure(
                true,
                "Claude Agent CLI is installed but failed to run. Timed out while running command.",
            );
        }
        Ok(Err(CommandFailure::NotFound)) => {
            return failure(false, "Claude Agent CLI (`claude`) was not found on PATH.");
        }
        Ok(Err(CommandFailure::Other)) => {
            return failure(true, "Failed to execute Claude Agent CLI health check.");
        }
        Ok(Ok(output)) => output,
    };

    let parsed_version =
        parse_generic_cli_version(&format!("{}\n{}", version.stdout, version.stderr));
    if version.code != Some(0) {
        return build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: presentation(),
            enabled: true,
            checked_at,
            models: all_models,
            update_required_models: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            probe: probe_result(
                true,
                parsed_version,
                ServerProviderState::Error,
                unknown_auth(),
                Some("Claude Agent CLI is installed but failed to run.".to_owned()),
            ),
            maintenance: None,
        });
    }

    let version_ref = parsed_version.as_deref();
    let models = provider_models_from_settings(
        resolve_claude_models_for_version(input.catalog, version_ref),
        &settings.custom_models,
        default_claude_model_capabilities(),
    );
    let update_required_models: Vec<ServerProviderUpdateRequiredModel> =
        resolve_claude_update_required_models(input.catalog, version_ref);
    let version_upgrade_message = format_claude_version_upgrade_message(input.catalog, version_ref);

    let capabilities = match &input.resolve_capabilities {
        Some(resolve) => resolve().await,
        None => None,
    };
    // Skills come from the machine's config dir, not a workspace (`cwd` is None here).
    let skills = discover_claude_skills(settings, None, input.environment, input.home);
    let slash_commands = dedupe_slash_commands(
        std::iter::once(compact_slash_command())
            .chain(
                capabilities
                    .iter()
                    .flat_map(|caps| caps.slash_commands.clone()),
            )
            .collect(),
    );

    let Some(capabilities) = capabilities else {
        return build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: presentation(),
            enabled: true,
            checked_at,
            models,
            update_required_models,
            slash_commands,
            skills: skills.clone(),
            probe: probe_result(
                true,
                parsed_version,
                ServerProviderState::Warning,
                unknown_auth(),
                Some(
                    "Could not verify Claude authentication status from initialization result."
                        .to_owned(),
                ),
            ),
            maintenance: None,
        });
    };

    if claude_auth_status(&capabilities) == ServerProviderAuthStatus::Unauthenticated {
        return build_server_provider(BuildServerProviderInput {
            driver: None,
            presentation: presentation(),
            enabled: true,
            checked_at,
            models,
            update_required_models,
            slash_commands,
            skills: skills.clone(),
            probe: probe_result(
                true,
                parsed_version,
                ServerProviderState::Error,
                ServerProviderAuth::with_status(ServerProviderAuthStatus::Unauthenticated),
                Some(
                    "Claude Code is not authenticated. Run `claude auth login` and try again."
                        .to_owned(),
                ),
            ),
            maintenance: None,
        });
    }

    let auth_metadata = claude_auth_metadata(
        capabilities.subscription_type.as_deref(),
        capabilities.token_source.as_deref(),
    )
    .or_else(|| api_provider_auth_metadata(capabilities.api_provider.as_deref()));

    let usage_limits = match (&capabilities.usage, input.scoped_limit_names) {
        (None, _) => make_unavailable_usage_limits(
            checked_at.clone(),
            ServerProviderUsageUnavailableReason::ProbeFailed,
            None,
        ),
        (Some(usage), Some(names)) => record_claude_usage_response(names, usage, &checked_at),
        (Some(usage), None) => claude_usage_response_to_limits(usage, &checked_at).0,
    };

    let reset_credits = match (
        &input.resolve_reset_credits,
        &capabilities.subscription_type,
        &version_ref,
    ) {
        (Some(resolve), Some(_), Some(version)) if usage_limits.unavailable.is_none() => {
            resolve((*version).to_owned()).await
        }
        _ => None,
    };
    let usage_limits = match reset_credits {
        Some(reset_credits) => ServerProviderUsageLimits {
            reset_credits: Some(reset_credits),
            ..usage_limits
        },
        None => usage_limits,
    };

    let mut auth = ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated);
    auth.email = capabilities.email.as_deref().and_then(tne);
    auth.workspace_id = capabilities.workspace_id.as_deref().and_then(tne);
    if let Some((auth_type, label)) = auth_metadata {
        auth.type_ = tne(&auth_type);
        auth.label = tne(&label);
    }

    let mut probe = probe_result(
        true,
        parsed_version,
        ServerProviderState::Ready,
        auth,
        version_upgrade_message,
    );
    probe.usage_limits = Some(usage_limits);
    build_server_provider(BuildServerProviderInput {
        driver: None,
        presentation: presentation(),
        enabled: true,
        checked_at,
        models,
        update_required_models,
        slash_commands,
        skills,
        probe,
        maintenance: None,
    })
}

/// `makePendingClaudeProvider`: what the snapshot shows before the first probe.
pub fn make_pending_claude_provider(
    settings: &ClaudeSettings,
    catalog: &ClaudeModelCatalog,
) -> ServerProviderDraft {
    let checked_at = now_iso();
    let models = provider_models_from_settings(
        catalog
            .models
            .iter()
            .map(|entry| entry.model.clone())
            .collect(),
        &settings.custom_models,
        default_claude_model_capabilities(),
    );
    let message = if settings.enabled {
        "Claude provider status has not been checked in this session yet."
    } else {
        "Claude is disabled in T3 Code settings."
    };
    build_server_provider(BuildServerProviderInput {
        driver: None,
        presentation: presentation(),
        enabled: settings.enabled,
        checked_at,
        models,
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
        probe: probe_result(
            false,
            None,
            ServerProviderState::Warning,
            unknown_auth(),
            Some(message.to_owned()),
        ),
        maintenance: None,
    })
}

/// `probeClaudeWorkspaceSnapshot`: the machine snapshot with this workspace's
/// command list. Commands come from a fresh probe in `cwd`, not the instance
/// cache, since the cache is keyed by the instance's own directory. A disabled
/// instance returns the machine snapshot unchanged.
pub async fn probe_claude_workspace_snapshot(
    settings: &ClaudeSettings,
    machine: ServerProvider,
    cwd: &Path,
    environment: &ProcessEnv,
    home: &str,
) -> ProviderWorkspaceSnapshot {
    if !settings.enabled {
        return ProviderWorkspaceSnapshot {
            provider: machine,
            slash_commands_pending: None,
        };
    }
    let capabilities =
        probe_claude_capabilities(settings, environment, Some(cwd), false, home).await;
    let mut provider = machine;
    provider.draft.slash_commands = dedupe_slash_commands(
        std::iter::once(compact_slash_command())
            .chain(
                capabilities
                    .iter()
                    .flat_map(|caps| caps.slash_commands.clone()),
            )
            .collect(),
    );
    provider.draft.skills = discover_claude_skills(settings, Some(cwd), environment, home);
    ProviderWorkspaceSnapshot {
        provider,
        slash_commands_pending: Some(capabilities.is_none()),
    }
}

#[cfg(test)]
mod tests;
