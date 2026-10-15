//! Port of `upstream/packages/provider-opencode/src/server/driver.ts`
//! `selectOpenCodeRuntimeAdapter`.
//!
//! A hot capability read uses the last successful probe and does not wait.
//! Opening a session waits for a probe. A failed probe stays on 1.x. Only an
//! explicit 2.x probe selects the v2 adapter. Installed 1.19.0 is v1.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use turin_contracts::{
    orchestration_v2::OrchestrationV2ProviderCapabilities,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::id_allocator::derive;

use crate::adapter::api::*;
use crate::adapter::logic::{opencode_driver, select_generation, v1_capabilities, v2_capabilities};
use crate::adapter::v1::OpenCodeV1Session;
use crate::adapter::v2::OpenCodeV2Session;
use crate::error::OpenCodeRuntimeError;
use crate::runtime::{
    ConnectOpenCodeServer, OpenCodeServerProcess, connect_to_open_code_server, default_v1_verify,
};
use crate::v2_client::V2Client;
use crate::v2_server::{OpenCode2Config, OpenCode2Server, ServerLoan};
use crate::version_probe::{
    OpenCodeBinary, OpenCodeCommandResult, OpenCodeGeneration, ProbeSettings,
    probe_open_code_runtime,
};

/// Settings the OpenCode driver reads. Mirrors `OpenCodeSettings` in
/// `server/settings.ts`: enabled, binary path, external server URL, password.
#[derive(Debug, Clone)]
pub struct OpenCodeSettings {
    pub enabled: bool,
    pub binary_path: String,
    pub server_url: String,
    pub server_password: String,
}

impl Default for OpenCodeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            binary_path: "opencode".to_owned(),
            server_url: String::new(),
            server_password: String::new(),
        }
    }
}

struct Endpoint {
    generation: OpenCodeGeneration,
    base_url: String,
    password: String,
}

enum Keepalive {
    V1(OpenCodeServerProcess),
    V2(ServerLoan),
}

enum SessionKind {
    V1(Arc<OpenCodeV1Session>),
    V2(Arc<OpenCodeV2Session>),
}

/// A session plus the server process that must outlive it.
pub struct OpenCodeSession {
    kind: SessionKind,
    keepalive: Mutex<Option<Keepalive>>,
}

impl Drop for OpenCodeSession {
    fn drop(&mut self) {
        let Some(keepalive) = self.keepalive.lock().take() else {
            return;
        };
        match keepalive {
            Keepalive::V1(process) => drop(process),
            Keepalive::V2(loan) => drop(loan),
        }
    }
}

/// Selects the OpenCode generation and opens its session runtime.
pub struct OpenCodeAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    settings: OpenCodeSettings,
    probed: Mutex<Option<OpenCodeGeneration>>,
    endpoint: Option<Endpoint>,
    v2_server: Mutex<Option<Arc<OpenCode2Server>>>,
}

impl OpenCodeAdapter {
    pub fn new(instance_id: ProviderInstanceId, settings: OpenCodeSettings) -> Self {
        Self {
            instance_id,
            driver: opencode_driver(),
            settings,
            probed: Mutex::new(None),
            endpoint: None,
            v2_server: Mutex::new(None),
        }
    }

    /// Tests and the gated smoke talk to a server that is already listening.
    pub fn connecting(
        instance_id: ProviderInstanceId,
        generation: OpenCodeGeneration,
        base_url: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            instance_id,
            driver: opencode_driver(),
            settings: OpenCodeSettings {
                enabled: true,
                ..OpenCodeSettings::default()
            },
            probed: Mutex::new(Some(generation)),
            endpoint: Some(Endpoint {
                generation,
                base_url: base_url.into(),
                password: password.into(),
            }),
            v2_server: Mutex::new(None),
        }
    }

    fn generation(&self) -> OpenCodeGeneration {
        let probed = self
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.generation)
            .or_else(|| *self.probed.lock());
        select_generation(probed)
    }

    /// The instance's one OpenCode 2 server, built on first use.
    fn v2_server(&self) -> Result<Arc<OpenCode2Server>, OpenCodeRuntimeError> {
        let mut slot = self.v2_server.lock();
        if let Some(server) = slot.as_ref() {
            return Ok(Arc::clone(server));
        }
        let server = Arc::new(OpenCode2Server::start(OpenCode2Config {
            binary_path: self.settings.binary_path.clone(),
            server_url: self.settings.server_url.clone(),
            server_password: self.settings.server_password.clone(),
            directory: host_cwd(),
            environment: std::env::vars().collect(),
            idle: None,
            shutdown_grace: None,
        })?);
        *slot = Some(Arc::clone(&server));
        Ok(server)
    }

    /// Stops a spawned OpenCode 2 server. Dropping the adapter kills it too.
    pub async fn close(&self) {
        let server = self.v2_server.lock().take();
        if let Some(server) = server {
            server.close().await;
        }
    }
}

fn host_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_owned())
}

struct CommandBinary;

impl OpenCodeBinary for CommandBinary {
    #[allow(clippy::manual_async_fn)]
    fn run_version<'a>(
        &'a self,
        binary_path: &'a str,
        environment: Option<&'a BTreeMap<String, String>>,
    ) -> impl Future<Output = Result<OpenCodeCommandResult, OpenCodeRuntimeError>> + Send + 'a {
        async move {
            let mut command = tokio::process::Command::new(binary_path);
            command.arg("--version").kill_on_drop(true);
            if let Some(environment) = environment {
                command.env_clear();
                for (key, value) in environment {
                    command.env(key, value);
                }
            }
            let output = command
                .output()
                .await
                .map_err(|err| OpenCodeRuntimeError::new("probeOpenCodeBinary", err.to_string()))?;
            Ok(OpenCodeCommandResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                code: output.status.code().unwrap_or(1),
            })
        }
    }
}

#[async_trait::async_trait]
impl ProviderAdapterV2 for OpenCodeAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }

    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(match self.generation() {
            OpenCodeGeneration::V2 => v2_capabilities(),
            OpenCodeGeneration::V1 => v1_capabilities(),
        })
    }

    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(turn_scoped_selection_transition())
    }

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let cwd = input
            .runtime_policy
            .cwd
            .clone()
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(host_cwd);
        let model = input.model_selection.model.as_str();
        if let Some(endpoint) = &self.endpoint {
            return open_endpoint(
                &self.instance_id,
                &input.provider_session_id,
                &cwd,
                model,
                endpoint,
            )
            .await;
        }
        let http = reqwest::Client::new();
        let probed = probe_open_code_runtime(
            &http,
            &CommandBinary,
            &ProbeSettings {
                binary_path: self.settings.binary_path.clone(),
                server_url: self.settings.server_url.clone(),
                server_password: self.settings.server_password.clone(),
            },
            None,
        )
        .await
        .ok();
        let generation = select_generation(probed.as_ref().map(|probed| probed.generation));
        *self.probed.lock() = Some(generation);
        match generation {
            OpenCodeGeneration::V1 => {
                let connection = connect_to_open_code_server(
                    ConnectOpenCodeServer {
                        binary_path: self.settings.binary_path.clone(),
                        directory: cwd.clone(),
                        server_url: nonempty(&self.settings.server_url),
                        server_password: nonempty(&self.settings.server_password),
                        environment: None,
                        hostname: None,
                        port: None,
                        timeout: None,
                        shutdown_grace: None,
                        ledger: None,
                    },
                    default_v1_verify(),
                )
                .await
                .map_err(|err| ProviderAdapterV2Error::protocol(err.detail))?;
                let session = OpenCodeV1Session::open(
                    self.instance_id.clone(),
                    input.provider_session_id,
                    &cwd,
                    &connection.url,
                    connection.v1_client_password(),
                    Some(model),
                )
                .await?;
                Ok(Arc::new(OpenCodeSession {
                    kind: SessionKind::V1(session),
                    keepalive: Mutex::new(connection.process.map(Keepalive::V1)),
                }))
            }
            OpenCodeGeneration::V2 => {
                let (connection, loan) = self
                    .v2_server()
                    .map_err(|err| ProviderAdapterV2Error::protocol(err.detail))?
                    .borrow()
                    .await
                    .map_err(|err| ProviderAdapterV2Error::protocol(err.detail))?;
                let session = OpenCodeV2Session::open(
                    self.instance_id.clone(),
                    input.provider_session_id,
                    &cwd,
                    connection.client,
                    Some(model),
                )?;
                Ok(Arc::new(OpenCodeSession {
                    kind: SessionKind::V2(session),
                    keepalive: Mutex::new(Some(Keepalive::V2(loan))),
                }))
            }
        }
    }
}

async fn open_endpoint(
    instance_id: &ProviderInstanceId,
    provider_session_id: &turin_contracts::ProviderSessionId,
    cwd: &str,
    model: &str,
    endpoint: &Endpoint,
) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
    match endpoint.generation {
        OpenCodeGeneration::V1 => {
            let session = OpenCodeV1Session::open(
                instance_id.clone(),
                provider_session_id.clone(),
                cwd,
                &endpoint.base_url,
                (!endpoint.password.is_empty()).then_some(endpoint.password.as_str()),
                Some(model),
            )
            .await?;
            Ok(Arc::new(OpenCodeSession {
                kind: SessionKind::V1(session),
                keepalive: Mutex::new(None),
            }))
        }
        OpenCodeGeneration::V2 => {
            let client = V2Client::connect(&endpoint.base_url, &endpoint.password)
                .map_err(|err| ProviderAdapterV2Error::protocol(err.detail))?;
            let session = OpenCodeV2Session::open(
                instance_id.clone(),
                provider_session_id.clone(),
                cwd,
                client,
                Some(model),
            )?;
            Ok(Arc::new(OpenCodeSession {
                kind: SessionKind::V2(session),
                keepalive: Mutex::new(None),
            }))
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[async_trait::async_trait]
impl ProviderSessionRuntime for OpenCodeSession {
    fn instance_id(&self) -> &ProviderInstanceId {
        match &self.kind {
            SessionKind::V1(session) => session.instance_id(),
            SessionKind::V2(session) => session.instance_id(),
        }
    }
    fn driver(&self) -> &ProviderDriverKind {
        match &self.kind {
            SessionKind::V1(session) => session.driver(),
            SessionKind::V2(session) => session.driver(),
        }
    }
    fn provider_session_id(&self) -> &turin_contracts::ProviderSessionId {
        match &self.kind {
            SessionKind::V1(session) => session.provider_session_id(),
            SessionKind::V2(session) => session.provider_session_id(),
        }
    }
    fn provider_session(
        &self,
    ) -> &turin_contracts::orchestration_v2::OrchestrationV2ProviderSession {
        match &self.kind {
            SessionKind::V1(session) => session.provider_session(),
            SessionKind::V2(session) => session.provider_session(),
        }
    }
    fn events(&self) -> ProviderEventStream {
        match &self.kind {
            SessionKind::V1(session) => session.events(),
            SessionKind::V2(session) => session.events(),
        }
    }
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        match &self.kind {
            SessionKind::V1(session) => session.has_pending_background_work(),
            SessionKind::V2(session) => session.has_pending_background_work(),
        }
    }
    fn get_model_context_window(
        &self,
        model_selection: &turin_contracts::model_selection::ModelSelection,
        cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        match &self.kind {
            SessionKind::V1(session) => session.get_model_context_window(model_selection, cwd),
            SessionKind::V2(session) => session.get_model_context_window(model_selection, cwd),
        }
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        match &self.kind {
            SessionKind::V1(session) => session.ensure_thread(input).await,
            SessionKind::V2(session) => session.ensure_thread(input).await,
        }
    }
    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        match &self.kind {
            SessionKind::V1(session) => session.resume_thread(input).await,
            SessionKind::V2(session) => session.resume_thread(input).await,
        }
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.start_turn(input).await,
            SessionKind::V2(session) => session.start_turn(input).await,
        }
    }
    fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        match &self.kind {
            SessionKind::V1(session) => session.compact_thread(input),
            SessionKind::V2(session) => session.compact_thread(input),
        }
    }
    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.steer_turn(input).await,
            SessionKind::V2(session) => session.steer_turn(input).await,
        }
    }
    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.interrupt_turn(input).await,
            SessionKind::V2(session) => session.interrupt_turn(input).await,
        }
    }
    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.respond_to_runtime_request(input).await,
            SessionKind::V2(session) => session.respond_to_runtime_request(input).await,
        }
    }
    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.read_thread_snapshot(input).await,
            SessionKind::V2(session) => session.read_thread_snapshot(input).await,
        }
    }
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        match &self.kind {
            SessionKind::V1(session) => session.rollback_thread(input).await,
            SessionKind::V2(session) => session.rollback_thread(input).await,
        }
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        match &self.kind {
            SessionKind::V1(session) => session.fork_thread(input).await,
            SessionKind::V2(session) => session.fork_thread(input).await,
        }
    }
}

/// Stable provider-session id for one app thread, matching `derive.provider_session`.
pub fn shared_provider_session_id(
    instance_id: &ProviderInstanceId,
) -> turin_contracts::ProviderSessionId {
    derive::provider_session(instance_id)
}
