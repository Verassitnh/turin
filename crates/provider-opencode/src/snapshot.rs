//! Port of the snapshot assembly in `upstream/packages/provider-opencode/src/server/status.ts`
//! (`makePendingOpenCodeProvider`, the disabled and failed branches, and the 1.x inventory
//! success). The catalog helpers in `adapter::logic` feed the draft, and provider-core's
//! `build_server_provider` builds it.
//!
//! The live probe and inventory fetch belong to the driver. This module takes their
//! results, so the mapping can be tested without a server.

use serde_json::Value;
use turin_contracts::{
    IsoDateTime, TrimmedNonEmptyString, provider_instance::ProviderDriverKind,
    provider_policy::RuntimeMode, provider_usage_limits::ServerProviderUsageLimits,
};
use turin_provider_core::{
    server_provider::{
        ServerProviderAuth, ServerProviderAuthStatus, ServerProviderDraft, ServerProviderModel,
        ServerProviderSkill, ServerProviderSlashCommand, ServerProviderState,
    },
    snapshot_probe::{
        BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
        build_server_provider,
    },
};

use crate::adapter::{
    commands_from_list, format_probe_error, models_from_provider_list,
    open_code2_model_capabilities, opencode_driver, pending_status, skills_from_list,
};
use crate::open_code2_catalog::{OpenCode2Model, load_open_code2_catalog};
use crate::v2_client::V2Client;
use crate::version_probe::MINIMUM_OPENCODE_VERSION;

/// `OPENCODE_PRESENTATION` in `status.ts`.
fn presentation() -> ServerProviderPresentation {
    ServerProviderPresentation {
        display_name: "OpenCode".to_owned(),
        badge_label: None,
        show_interaction_mode_toggle: Some(false),
        reports_context_window: None,
        supported_runtime_modes: None,
        requires_new_thread_for_model_change: None,
        supports_conversation_rollback: None,
    }
}

/// `OPENCODE_2_PRESENTATION`: the 2.x server supports the interaction toggle and every runtime mode.
fn presentation_v2() -> ServerProviderPresentation {
    ServerProviderPresentation {
        show_interaction_mode_toggle: Some(true),
        supported_runtime_modes: Some(vec![
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ]),
        ..presentation()
    }
}

/// Inputs the driver collects. `inventory` is `None` until a probe and inventory
/// load have both succeeded.
pub struct OpenCodeSnapshotInput<'a> {
    pub enabled: bool,
    pub server_url: &'a str,
    pub checked_at: IsoDateTime,
    pub outcome: ProbeOutcome<'a>,
    /// Go usage windows from `read_open_code_go_usage_limits`, if read.
    pub usage_limits: Option<ServerProviderUsageLimits>,
}

pub enum ProbeOutcome<'a> {
    /// No probe has run yet.
    Pending,
    /// The probe or inventory failed. `detail` is the error's detail text.
    Failed { detail: &'a str, phase: &'a str },
    /// A 1.x server answered. `version` is what it reported.
    Ready(V1Inventory<'a>),
    /// A 2.x server answered. `models` is `None` when its catalog could not be read.
    Ready2 {
        version: &'a str,
        models: Option<&'a [OpenCode2Model]>,
    },
}

/// The 1.x inventory: the `provider.list` payload, its agents, commands and skills.
pub struct V1Inventory<'a> {
    pub version: &'a str,
    pub provider_list: &'a Value,
    pub agents: &'a [Value],
    pub commands: &'a [Value],
    pub skills: &'a [Value],
}

/// The 2.x snapshot read from a live server: loads its catalog on `client` for `directory`,
/// then builds the draft. A failed catalog read is the snapshot's own error state.
pub async fn build_open_code2_snapshot(
    client: &V2Client,
    directory: &str,
    enabled: bool,
    checked_at: IsoDateTime,
    version: &str,
) -> ServerProviderDraft {
    let models = load_open_code2_catalog(client, directory).await.ok();
    build_open_code_snapshot(OpenCodeSnapshotInput {
        enabled,
        server_url: "",
        checked_at,
        outcome: ProbeOutcome::Ready2 {
            version,
            models: models.as_deref(),
        },
        usage_limits: None,
    })
}

/// Builds the snapshot draft for one OpenCode instance.
pub fn build_open_code_snapshot(input: OpenCodeSnapshotInput<'_>) -> ServerProviderDraft {
    let usage_limits = input.usage_limits.clone();
    let mut draft = build_draft(input);
    draft.usage_limits = usage_limits;
    draft
}

fn build_draft(input: OpenCodeSnapshotInput<'_>) -> ServerProviderDraft {
    if let ProbeOutcome::Ready2 { version, models } = input.outcome {
        return v2_draft(&input.checked_at, input.enabled, version, models);
    }
    let external = !input.server_url.trim().is_empty();
    if !input.enabled {
        let (_, _, message) = pending_status(false, input.server_url);
        return draft(
            &input.checked_at,
            false,
            ProviderProbeResult {
                installed: false,
                version: None,
                status: ServerProviderState::Warning,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
                message: Some(message),
                usage_limits: None,
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
    }
    match input.outcome {
        ProbeOutcome::Pending => {
            let (status, auth, message) = pending_status(true, input.server_url);
            draft(
                &input.checked_at,
                true,
                ProviderProbeResult {
                    installed: false,
                    version: None,
                    status,
                    auth,
                    message: Some(message),
                    usage_limits: None,
                },
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        }
        ProbeOutcome::Failed { detail, phase } => {
            let (installed, message) =
                format_probe_error(Some(detail), external, phase, input.server_url);
            draft(
                &input.checked_at,
                true,
                ProviderProbeResult {
                    installed,
                    version: None,
                    status: ServerProviderState::Error,
                    auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
                    message: Some(message),
                    usage_limits: None,
                },
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        }
        ProbeOutcome::Ready2 { .. } => unreachable!("handled above"),
        ProbeOutcome::Ready(inventory) => {
            if is_too_old(inventory.version) {
                return draft(
                    &input.checked_at,
                    true,
                    ProviderProbeResult {
                        installed: true,
                        version: Some(inventory.version.to_owned()),
                        status: ServerProviderState::Error,
                        auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
                        message: Some(format!(
                            "OpenCode v{} is too old. Upgrade to v{MINIMUM_OPENCODE_VERSION} or newer.",
                            inventory.version
                        )),
                        usage_limits: None,
                    },
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
            }
            let models = models_from_provider_list(inventory.provider_list, inventory.agents);
            let connected = connected_count(inventory.provider_list);
            let (status, auth_status, message) = if connected > 0 {
                (
                    ServerProviderState::Ready,
                    ServerProviderAuthStatus::Authenticated,
                    format!(
                        "{connected} upstream provider{} connected through {}.",
                        if connected == 1 { "" } else { "s" },
                        if external {
                            "the configured OpenCode server"
                        } else {
                            "OpenCode"
                        }
                    ),
                )
            } else if external {
                (
                    ServerProviderState::Warning,
                    ServerProviderAuthStatus::Unknown,
                    "Connected to the configured OpenCode server, but it did not report any connected upstream providers.".to_owned(),
                )
            } else {
                (
                    ServerProviderState::Warning,
                    ServerProviderAuthStatus::Unknown,
                    "OpenCode is available, but it did not report any connected upstream providers.".to_owned(),
                )
            };
            let auth = ServerProviderAuth {
                type_: TrimmedNonEmptyString::new("opencode").ok(),
                ..ServerProviderAuth::with_status(auth_status)
            };
            let probe = ProviderProbeResult {
                installed: true,
                version: Some(inventory.version.to_owned()),
                status,
                auth,
                message: Some(message),
                usage_limits: None,
            };
            draft(
                &input.checked_at,
                true,
                probe,
                models,
                commands_from_list(inventory.commands),
                skills_from_list(inventory.skills),
            )
        }
    }
}

fn draft(
    checked_at: &IsoDateTime,
    enabled: bool,
    probe: ProviderProbeResult,
    models: Vec<ServerProviderModel>,
    slash_commands: Vec<ServerProviderSlashCommand>,
    skills: Vec<ServerProviderSkill>,
) -> ServerProviderDraft {
    draft_with(
        presentation(),
        checked_at,
        enabled,
        probe,
        models,
        slash_commands,
        skills,
    )
}

fn draft_with(
    presentation: ServerProviderPresentation,
    checked_at: &IsoDateTime,
    enabled: bool,
    probe: ProviderProbeResult,
    models: Vec<ServerProviderModel>,
    slash_commands: Vec<ServerProviderSlashCommand>,
    skills: Vec<ServerProviderSkill>,
) -> ServerProviderDraft {
    let driver: ProviderDriverKind = opencode_driver();
    build_server_provider(BuildServerProviderInput {
        driver: Some(&driver),
        presentation,
        enabled,
        checked_at: checked_at.clone(),
        models,
        update_required_models: Vec::new(),
        slash_commands,
        skills,
        probe,
        maintenance: None,
    })
}

/// `checkOpenCode2`: a 2.x server is ready when its catalog lists models, and the models
/// are sorted by name. Compaction is every session's command.
fn v2_draft(
    checked_at: &IsoDateTime,
    enabled: bool,
    version: &str,
    models: Option<&[OpenCode2Model]>,
) -> ServerProviderDraft {
    let probe = |status, auth_status, message: String| ProviderProbeResult {
        installed: true,
        version: Some(version.to_owned()),
        status,
        auth: ServerProviderAuth::with_status(auth_status),
        message: Some(message),
        usage_limits: None,
    };
    let compact = commands_from_list(&[]);
    let Some(models) = models else {
        return draft_with(
            presentation_v2(),
            checked_at,
            enabled,
            probe(
                ServerProviderState::Error,
                ServerProviderAuthStatus::Unknown,
                "OpenCode could not load its model list.".to_owned(),
            ),
            Vec::new(),
            compact,
            Vec::new(),
        );
    };
    let mut listed: Vec<ServerProviderModel> = models.iter().map(v2_model).collect();
    listed.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    let count = listed.len();
    let probe = if count > 0 {
        probe(
            ServerProviderState::Ready,
            ServerProviderAuthStatus::Authenticated,
            format!(
                "OpenCode {version} lists {count} model{}.",
                if count == 1 { "" } else { "s" }
            ),
        )
    } else {
        probe(
            ServerProviderState::Warning,
            ServerProviderAuthStatus::Unknown,
            "OpenCode 2 is running, but it did not list any models yet.".to_owned(),
        )
    };
    draft_with(
        presentation_v2(),
        checked_at,
        enabled,
        probe,
        listed,
        compact,
        Vec::new(),
    )
}

/// One 2.x model. Its provider name is the sub-provider label, unless that is OpenCode itself.
fn v2_model(model: &OpenCode2Model) -> ServerProviderModel {
    let label = model
        .provider_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&model.provider_id);
    let sub_provider = if label.eq_ignore_ascii_case("opencode") {
        None
    } else {
        TrimmedNonEmptyString::new(label).ok()
    };
    ServerProviderModel {
        slug: TrimmedNonEmptyString::new(format!("{}/{}", model.provider_id, model.id))
            .unwrap_or_else(|_| TrimmedNonEmptyString::new("model").expect("literal")),
        name: TrimmedNonEmptyString::new(model.name.trim())
            .or_else(|_| TrimmedNonEmptyString::new(model.id.as_str()))
            .expect("model id is not blank"),
        short_name: None,
        sub_provider,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: None,
        is_legacy: None,
        capabilities: Some(open_code2_model_capabilities(
            &model.provider_id,
            &model.variants,
        )),
    }
}

/// `providerList.connected.length`, read from either
fn connected_count(payload: &Value) -> usize {
    let body = payload.get("data").unwrap_or(payload);
    body.get("connected")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn is_too_old(version: &str) -> bool {
    crate::semver::compare_semver_versions(version, MINIMUM_OPENCODE_VERSION) < 0
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use turin_contracts::IsoDateTime;

    use super::*;

    fn checked_at() -> IsoDateTime {
        "2026-10-15T12:00:00.000Z".to_owned()
    }

    fn input(enabled: bool, outcome: ProbeOutcome<'_>) -> OpenCodeSnapshotInput<'_> {
        OpenCodeSnapshotInput {
            enabled,
            server_url: "",
            checked_at: checked_at(),
            outcome,
            usage_limits: None,
        }
    }

    fn message(draft: &ServerProviderDraft) -> &str {
        draft
            .message
            .as_ref()
            .map(|text| text.as_str())
            .unwrap_or("")
    }

    #[test]
    fn a_disabled_instance_is_not_installed_and_says_so() {
        let draft = build_open_code_snapshot(input(false, ProbeOutcome::Pending));
        assert_eq!(draft.status, ServerProviderState::Disabled);
        assert!(!draft.installed);
        assert_eq!(message(&draft), "OpenCode is disabled in T3 Code settings.");
    }

    #[test]
    fn an_unprobed_instance_is_a_warning_not_an_error() {
        let draft = build_open_code_snapshot(input(true, ProbeOutcome::Pending));
        assert_eq!(draft.status, ServerProviderState::Warning);
        assert!(!draft.installed);
    }

    #[test]
    fn a_missing_binary_is_reported_as_not_installed() {
        let draft = build_open_code_snapshot(input(
            true,
            ProbeOutcome::Failed {
                detail: "Failed to spawn OpenCode server process: spawn opencode ENOENT (No such file or directory)",
                phase: "version",
            },
        ));
        assert_eq!(draft.status, ServerProviderState::Error);
        assert!(!draft.installed);
        assert_eq!(
            message(&draft),
            "OpenCode CLI (`opencode`) is not installed or not on PATH."
        );
    }

    #[test]
    fn connected_providers_fill_models_commands_and_skills() {
        let provider_list = json!({
            "all": [{
                "id": "openai",
                "name": "OpenAI",
                "models": { "gpt": { "id": "gpt", "name": "GPT" } }
            }],
            "connected": ["openai"]
        });
        let commands = [json!({"name": "review", "description": "Review"})];
        let skills = [json!({"name": "lint", "description": "Lint", "location": "/skills/lint"})];
        let draft = build_open_code_snapshot(input(
            true,
            ProbeOutcome::Ready(V1Inventory {
                version: "1.19.0",
                provider_list: &provider_list,
                agents: &[],
                commands: &commands,
                skills: &skills,
            }),
        ));
        assert_eq!(draft.status, ServerProviderState::Ready);
        assert_eq!(draft.auth.status, ServerProviderAuthStatus::Authenticated);
        assert_eq!(draft.version.as_ref().map(|v| v.as_str()), Some("1.19.0"));
        assert_eq!(draft.models.len(), 1);
        assert_eq!(draft.models[0].slug.as_str(), "openai/gpt");
        assert_eq!(draft.slash_commands[0].name.as_str(), "compact");
        assert_eq!(draft.slash_commands[1].name.as_str(), "review");
        assert_eq!(draft.skills.len(), 1);
        assert_eq!(
            message(&draft),
            "1 upstream provider connected through OpenCode."
        );
    }

    #[test]
    fn an_old_server_is_an_error_with_no_models() {
        let provider_list = json!({"all": [], "connected": []});
        let draft = build_open_code_snapshot(input(
            true,
            ProbeOutcome::Ready(V1Inventory {
                version: "1.0.0",
                provider_list: &provider_list,
                agents: &[],
                commands: &[],
                skills: &[],
            }),
        ));
        assert_eq!(draft.status, ServerProviderState::Error);
        assert!(draft.models.is_empty());
        assert!(message(&draft).contains("too old"), "{}", message(&draft));
    }

    fn v2_model_fixture(provider: Option<&str>, name: &str, variants: &[&str]) -> OpenCode2Model {
        OpenCode2Model {
            provider_id: "zen".to_owned(),
            id: name.to_lowercase(),
            name: name.to_owned(),
            provider_name: provider.map(str::to_owned),
            variants: variants.iter().map(|v| (*v).to_owned()).collect(),
        }
    }

    #[test]
    fn a_2x_server_with_models_is_ready_and_lists_them_by_name() {
        let models = [
            v2_model_fixture(Some("OpenCode Zen"), "Zed", &["high", "low"]),
            v2_model_fixture(Some("OpenCode"), "Alpha", &[]),
        ];
        let draft = build_open_code_snapshot(OpenCodeSnapshotInput {
            enabled: true,
            server_url: "",
            checked_at: checked_at(),
            outcome: ProbeOutcome::Ready2 {
                version: "2.0.28",
                models: Some(&models),
            },
            usage_limits: None,
        });
        assert_eq!(draft.status, ServerProviderState::Ready);
        assert_eq!(draft.auth.status, ServerProviderAuthStatus::Authenticated);
        assert_eq!(draft.models[0].name.as_str(), "Alpha");
        assert_eq!(
            draft.models[0].sub_provider, None,
            "OpenCode is not a sub-provider"
        );
        assert_eq!(
            draft.models[1].sub_provider.as_ref().map(|s| s.as_str()),
            Some("OpenCode Zen")
        );
        assert_eq!(message(&draft), "OpenCode 2.0.28 lists 2 models.");
        assert_eq!(draft.show_interaction_mode_toggle, Some(true));
        assert_eq!(draft.slash_commands[0].name.as_str(), "compact");
    }

    #[test]
    fn a_2x_server_that_lists_nothing_is_a_warning_and_a_failed_catalog_an_error() {
        let empty = build_open_code_snapshot(OpenCodeSnapshotInput {
            enabled: true,
            server_url: "",
            checked_at: checked_at(),
            outcome: ProbeOutcome::Ready2 {
                version: "2.0.28",
                models: Some(&[]),
            },
            usage_limits: None,
        });
        assert_eq!(empty.status, ServerProviderState::Warning);
        assert_eq!(
            message(&empty),
            "OpenCode 2 is running, but it did not list any models yet."
        );
        let failed = build_open_code_snapshot(OpenCodeSnapshotInput {
            enabled: true,
            server_url: "",
            checked_at: checked_at(),
            outcome: ProbeOutcome::Ready2 {
                version: "2.0.28",
                models: None,
            },
            usage_limits: None,
        });
        assert_eq!(failed.status, ServerProviderState::Error);
        assert_eq!(message(&failed), "OpenCode could not load its model list.");
        assert!(failed.models.is_empty());
    }

    #[tokio::test]
    async fn the_2x_snapshot_loads_the_catalog_from_the_server() {
        let server = crate::test_support::FakeServer::spawn(|request| {
            if request.target.contains("/api/model") {
                return crate::test_support::FakeResponse::json(
                    200,
                    r#"{"location":{},"data":[{"providerID":"zen","id":"big","name":"Big","variants":[]}]}"#,
                );
            }
            crate::test_support::FakeResponse::json(200, r#"{"location":{},"data":[]}"#)
        })
        .await;
        let client = V2Client::connect(&server.url, "pw").unwrap();
        let draft = build_open_code2_snapshot(&client, "/work", true, checked_at(), "2.0.28").await;
        assert_eq!(draft.status, ServerProviderState::Ready);
        assert_eq!(draft.models.len(), 1);
        assert_eq!(draft.models[0].slug.as_str(), "zen/big");
        // No provider was named, so the sub-provider is the provider id.
        assert_eq!(
            draft.models[0].sub_provider.as_ref().map(|s| s.as_str()),
            Some("zen")
        );
    }
}
