//! The OpenCode status check (`checkOpenCodeProviderStatus`, `status.ts`) and the
//! per-workspace skills and commands listing (the driver's `snapshotForCwd`).
//!
//! [`OpenCodeRuntime`] is one instance's shared state: its settings, the memoized
//! version probe, the owner of a spawned 1.x server and the one OpenCode 2 server.
//! The status check, text generation and the orchestration adapter all lean on it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::UtcDateTime;
use turin_contracts::model::{CustomModelSetting, ModelCapabilities};
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_provider_core::custom_models::provider_models_from_settings;
use turin_provider_core::rt::BoxFuture;
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderDraft, ServerProviderSkill, ServerProviderSlashCommand,
};
use turin_provider_opencode::{
    ConnectOpenCodeServer, GoUsageInput, MINIMUM_OPENCODE_VERSION, OpenCode2Model, OpenCode2Server,
    OpenCodeGeneration, OpenCodeRuntimeError, OpenCodeRuntimeProbe, OpenCodeServerOwner,
    OpenCodeSettings, OpenCodeSnapshotInput, ProbeOutcome, ProbedOpenCode, V1Client, V1Inventory,
    build_open_code_snapshot, commands_from_list, compare_semver_versions,
    connect_to_open_code_server, default_v1_verify, load_open_code2_catalog,
    open_code_runtime_error_detail, read_open_code_go_usage_limits, skills_from_list,
};

/// A server loads its catalog lazily (upstream `makeOpenCode2ModelLoader`): an empty list is
/// read again this often, for at most [`MODEL_LOAD_WINDOW`].
const MODEL_LOAD_INTERVAL: Duration = Duration::from_millis(250);
const MODEL_LOAD_WINDOW: Duration = Duration::from_secs(5);
/// A 2.x server scans a directory it has not served yet in the background; its first read
/// lists no commands at all (upstream waits for `command.updated` + `skill.updated`).
const WORKSPACE_SCAN_INTERVAL: Duration = Duration::from_millis(250);
const WORKSPACE_SCAN_WINDOW: Duration = Duration::from_secs(10);
/// Upstream's `Effect.timeout("10 seconds")` on the 1.x command list.
const COMMAND_LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// A status check must not hold the registry's refresh forever on a server that never answers.
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) type ProbeFn =
    Box<dyn Fn() -> BoxFuture<'static, Result<ProbedOpenCode, OpenCodeRuntimeError>> + Send + Sync>;

pub(super) struct OpenCodeRuntime {
    pub kind: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    pub settings: OpenCodeSettings,
    pub custom_models: Vec<CustomModelSetting>,
    pub environment: BTreeMap<String, String>,
    /// The host's working directory: where the instance's own servers start and the
    /// directory the machine-level status reads.
    pub cwd: String,
    pub probe: OpenCodeRuntimeProbe<ProbeFn>,
    /// Spawns and lends the 1.x server (status, workspace listing, text generation).
    pub owner: OpenCodeServerOwner,
    /// The instance's one OpenCode 2 server, spawned on first use or reached at `serverUrl`.
    pub v2: Arc<OpenCode2Server>,
    last_models: parking_lot::Mutex<Vec<OpenCode2Model>>,
}

impl OpenCodeRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: ProviderDriverKind,
        instance_id: ProviderInstanceId,
        settings: OpenCodeSettings,
        custom_models: Vec<CustomModelSetting>,
        environment: BTreeMap<String, String>,
        cwd: String,
        probe: ProbeFn,
        owner: OpenCodeServerOwner,
        v2: Arc<OpenCode2Server>,
    ) -> Self {
        Self {
            kind,
            instance_id,
            settings,
            custom_models,
            environment,
            cwd,
            probe: OpenCodeRuntimeProbe::new(probe),
            owner,
            v2,
            last_models: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn external(&self) -> bool {
        !self.settings.server_url.trim().is_empty()
    }

    /// Upstream `byOpenCodeRuntime`: a failed probe keeps 1.x, whose checks report it.
    pub async fn is_v2(&self) -> bool {
        matches!(
            self.probe.get().await,
            Ok(probed) if probed.generation == OpenCodeGeneration::V2
        )
    }

    /// `checkProvider`: the status check and the Go usage windows, in parallel.
    pub async fn check_provider(&self) -> ServerProvider {
        let checked_at = UtcDateTime::now().to_string();
        let usage = read_open_code_go_usage_limits(GoUsageInput {
            enabled: self.settings.enabled,
            server_url: &self.settings.server_url,
            environment: &self.environment,
            checked_at: checked_at.clone(),
        });
        let (mut draft, usage) = tokio::join!(self.check_draft(&checked_at), usage);
        draft.usage_limits = Some(usage);
        ServerProvider {
            instance_id: self.instance_id.clone(),
            driver: self.kind.clone(),
            draft: self.with_custom_models(draft),
        }
    }

    /// The snapshot before the first probe (`makePendingOpenCodeProvider`).
    pub fn pending_provider(&self) -> ServerProvider {
        ServerProvider {
            instance_id: self.instance_id.clone(),
            driver: self.kind.clone(),
            draft: self.with_custom_models(
                self.build(&UtcDateTime::now().to_string(), ProbeOutcome::Pending),
            ),
        }
    }

    fn build(&self, checked_at: &str, outcome: ProbeOutcome<'_>) -> ServerProviderDraft {
        build_open_code_snapshot(OpenCodeSnapshotInput {
            enabled: self.settings.enabled,
            server_url: &self.settings.server_url,
            checked_at: checked_at.to_owned(),
            outcome,
            usage_limits: None,
        })
    }

    /// `providerModelsFromSettings`: the user's custom models after the discovered ones.
    fn with_custom_models(&self, mut draft: ServerProviderDraft) -> ServerProviderDraft {
        if !self.custom_models.is_empty() {
            draft.models = provider_models_from_settings(
                &draft.models,
                &self.custom_models,
                default_model_capabilities().as_ref(),
            );
        }
        draft
    }

    async fn check_draft(&self, checked_at: &str) -> ServerProviderDraft {
        if !self.settings.enabled {
            return self.build(checked_at, ProbeOutcome::Pending);
        }
        // The status check refreshes the probe, so an in-place upgrade re-routes the instance.
        let probed = match self.probe.refresh().await {
            Ok(probed) => probed,
            Err(error) => {
                return self.build(
                    checked_at,
                    ProbeOutcome::Failed {
                        detail: &open_code_runtime_error_detail(&error),
                        phase: "version",
                    },
                );
            }
        };
        if probed.generation == OpenCodeGeneration::V2 {
            let models = match self.load_v2_models().await {
                Ok(models) => Some(models),
                Err(error) => {
                    // The detail can carry a response body; it stays in the log.
                    tracing::warn!(detail = %error.detail, "OpenCode 2 model list failed");
                    None
                }
            };
            return self.build(
                checked_at,
                ProbeOutcome::Ready2 {
                    version: &probed.version,
                    models: models.as_deref(),
                },
            );
        }
        if compare_semver_versions(&probed.version, MINIMUM_OPENCODE_VERSION) < 0 {
            // Too old to talk to: the snapshot says so without starting a server.
            return self.build(
                checked_at,
                ProbeOutcome::Ready(V1Inventory {
                    version: &probed.version,
                    provider_list: &json!({}),
                    agents: &[],
                    commands: &[],
                    skills: &[],
                }),
            );
        }
        let loaded = match tokio::time::timeout(INVENTORY_TIMEOUT, self.load_v1_inventory()).await {
            Ok(result) => result,
            Err(_elapsed) => Err(OpenCodeRuntimeError::new(
                "inventory",
                "Timed out while loading the OpenCode provider inventory.",
            )),
        };
        match loaded {
            Ok(inventory) => self.build(
                checked_at,
                ProbeOutcome::Ready(V1Inventory {
                    version: &inventory.version,
                    provider_list: &inventory.providers,
                    agents: &inventory.agents,
                    commands: &inventory.commands,
                    skills: &inventory.skills,
                }),
            ),
            Err(error) => {
                let mut draft = self.build(
                    checked_at,
                    ProbeOutcome::Failed {
                        detail: &open_code_runtime_error_detail(&error),
                        phase: "inventory",
                    },
                );
                draft.version = TrimmedNonEmptyString::new(&probed.version).ok();
                draft
            }
        }
    }

    /// `makeOpenCode2ModelLoader` over `loadOpenCode2Catalog`: an empty list is read again for a
    /// while, and the last non-empty list stands in if the catalog still has not loaded.
    async fn load_v2_models(&self) -> Result<Vec<OpenCode2Model>, OpenCodeRuntimeError> {
        let cwd = self.cwd.clone();
        let loaded = self
            .v2
            .with_connection(|connection| async move {
                let poll = async {
                    loop {
                        let models = load_open_code2_catalog(&connection.client, &cwd).await?;
                        if !models.is_empty() {
                            return Ok::<_, OpenCodeRuntimeError>(models);
                        }
                        tokio::time::sleep(MODEL_LOAD_INTERVAL).await;
                    }
                };
                match tokio::time::timeout(MODEL_LOAD_WINDOW, poll).await {
                    Ok(models) => models.map(Some),
                    Err(_elapsed) => Ok(None),
                }
            })
            .await?;
        let mut last = self.last_models.lock();
        if let Some(models) = loaded.filter(|models| !models.is_empty()) {
            *last = models;
        }
        Ok(last.clone())
    }

    async fn connect_external(
        &self,
        directory: &str,
    ) -> Result<turin_provider_opencode::OpenCodeServerConnection, OpenCodeRuntimeError> {
        connect_to_open_code_server(
            ConnectOpenCodeServer {
                binary_path: self.settings.binary_path.clone(),
                directory: directory.to_owned(),
                server_url: Some(self.settings.server_url.clone()),
                server_password: nonempty(&self.settings.server_password),
                environment: Some(self.environment.clone()),
                hostname: None,
                port: None,
                timeout: None,
                shutdown_grace: None,
                ledger: None,
            },
            default_v1_verify(),
        )
        .await
    }

    /// `loadOpenCodeInventory` against the configured server, or a server the owner lends.
    async fn load_v1_inventory(&self) -> Result<V1Loaded, OpenCodeRuntimeError> {
        if self.external() {
            let connection = self.connect_external(&self.cwd).await?;
            return load_v1_inventory(
                &connection.url,
                connection.server_password.as_deref(),
                &self.cwd,
                connection.version.clone(),
            )
            .await;
        }
        let cwd = self.cwd.clone();
        self.owner
            .with_server(|server| async move {
                load_v1_inventory(
                    &server.url,
                    server.server_password.as_deref(),
                    &cwd,
                    server.version.clone(),
                )
                .await
            })
            .await
    }

    /// The driver's `snapshotForCwd` lists: the skills and slash commands one directory has.
    pub async fn list_workspace(
        &self,
        cwd: &str,
    ) -> Result<(Vec<ServerProviderSkill>, Vec<ServerProviderSlashCommand>), OpenCodeRuntimeError>
    {
        if self.is_v2().await {
            let directory = cwd.to_owned();
            let (skills, commands) = self
                .v2
                .with_connection(|connection| async move {
                    list_v2_workspace(&connection.client, &directory).await
                })
                .await?;
            return Ok((v2_skills(&skills), v2_commands(&commands)));
        }
        let (skills, commands) = if self.external() {
            let connection = self.connect_external(cwd).await?;
            list_v1_workspace(&connection.url, connection.server_password.as_deref(), cwd).await?
        } else {
            let directory = cwd.to_owned();
            self.owner
                .with_server(|server| async move {
                    list_v1_workspace(&server.url, server.server_password.as_deref(), &directory)
                        .await
                })
                .await?
        };
        Ok((skills_from_list(&skills), commands_from_list(&commands)))
    }
}

struct V1Loaded {
    version: String,
    providers: Value,
    agents: Vec<Value>,
    commands: Vec<Value>,
    skills: Vec<Value>,
}

/// `loadOpenCodeInventory`: the provider list must load; agents, skills and commands
/// degrade to empty lists.
async fn load_v1_inventory(
    url: &str,
    password: Option<&str>,
    cwd: &str,
    version: String,
) -> Result<V1Loaded, OpenCodeRuntimeError> {
    let client = V1Client::new(url, cwd, password)?;
    let (providers, agents, skills, commands) = tokio::join!(
        client.list_providers(),
        client.list_agents(),
        client.list_skills(),
        client.list_commands()
    );
    Ok(V1Loaded {
        version,
        providers: providers?,
        agents: agents.unwrap_or_default(),
        commands: commands.unwrap_or_default(),
        skills: skills.unwrap_or_default(),
    })
}

/// `loadWorkspaceInventory`: skills must load, commands degrade to none after 10 seconds.
async fn list_v1_workspace(
    url: &str,
    password: Option<&str>,
    cwd: &str,
) -> Result<(Vec<Value>, Vec<Value>), OpenCodeRuntimeError> {
    let client = V1Client::new(url, cwd, password)?;
    let commands = async {
        tokio::time::timeout(COMMAND_LIST_TIMEOUT, client.list_commands())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let (skills, commands) = tokio::join!(client.list_skills(), commands);
    Ok((skills?, commands))
}

/// `loadOpenCode2Workspace`: a directory the server had not served yet lists no commands
/// at first (not even the built-ins), so an empty command list is read again.
async fn list_v2_workspace(
    client: &turin_provider_opencode::V2Client,
    directory: &str,
) -> Result<(Vec<Value>, Vec<Value>), OpenCodeRuntimeError> {
    let read = || async {
        let (skills, commands) = tokio::join!(
            client.list_skills(directory),
            client.list_commands(directory)
        );
        Ok::<_, OpenCodeRuntimeError>((skills?.data, commands?.data))
    };
    let first = read().await?;
    if !first.1.is_empty() {
        return Ok(first);
    }
    let rescan = async {
        loop {
            tokio::time::sleep(WORKSPACE_SCAN_INTERVAL).await;
            let next = read().await?;
            if !next.1.is_empty() {
                return Ok::<_, OpenCodeRuntimeError>(next);
            }
        }
    };
    match tokio::time::timeout(WORKSPACE_SCAN_WINDOW, rescan).await {
        Ok(result) => result,
        Err(_elapsed) => read().await,
    }
}

fn text(value: &Value, key: &str) -> Option<TrimmedNonEmptyString> {
    value
        .get(key)
        .and_then(Value::as_str)
        .and_then(|text| TrimmedNonEmptyString::new(text.trim()).ok())
}

/// `openCode2SkillsToServerProviderSkills`.
fn v2_skills(skills: &[Value]) -> Vec<ServerProviderSkill> {
    let mut out: Vec<ServerProviderSkill> = skills
        .iter()
        .filter_map(|skill| {
            let id = text(skill, "id")?;
            let path = text(skill, "path")?;
            let name = text(skill, "name");
            let description = text(skill, "description");
            Some(ServerProviderSkill {
                display_name: name.filter(|name| name.as_str() != id.as_str()),
                name: id,
                description: description.clone(),
                path,
                scope: None,
                enabled: true,
                short_description: description,
                user_invocation_only: None,
                user_invocable: None,
            })
        })
        .collect();
    out.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    out
}

/// `openCode2CommandsToServerProviderSlashCommands`: `/compact` first, then the server's.
fn v2_commands(commands: &[Value]) -> Vec<ServerProviderSlashCommand> {
    let mut out = commands_from_list(&[]);
    let mut names: std::collections::HashSet<String> =
        out.iter().map(|command| command.name.to_string()).collect();
    for command in commands {
        let Some(name) = text(command, "name") else {
            continue;
        };
        if !names.insert(name.to_string()) {
            continue;
        }
        out.push(ServerProviderSlashCommand {
            name,
            description: text(command, "description"),
            input: None,
        });
    }
    out
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `DEFAULT_OPENCODE_MODEL_CAPABILITIES`: what a custom model offers (Reasoning and Agent).
fn default_model_capabilities() -> Option<ModelCapabilities> {
    serde_json::from_value(json!({
        "optionDescriptors": [
            {
                "id": "variant",
                "label": "Reasoning",
                "type": "select",
                "options": [
                    {"id": "low", "label": "Low"},
                    {"id": "medium", "label": "Medium", "isDefault": true},
                    {"id": "high", "label": "High"},
                    {"id": "xhigh", "label": "Extra High"},
                ],
                "currentValue": "medium",
            },
            {
                "id": "agent",
                "label": "Agent",
                "type": "select",
                "options": [
                    {"id": "build", "label": "Build", "isDefault": true},
                    {"id": "plan", "label": "Plan"},
                ],
                "currentValue": "build",
            },
        ]
    }))
    .ok()
}
