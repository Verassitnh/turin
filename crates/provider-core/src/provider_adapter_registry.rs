//! Port of upstream/apps/server/src/orchestration-v2/ProviderAdapterRegistry.ts.
//!
//! Production lookup goes through [`ProviderInstanceRegistry`], so a hot
//! reload is visible without a second settings watcher. `open_session` on
//! that path checks credential-binding peers before the adapter runs.

use std::{any::Any, sync::Arc};

use serde_json::Value;
use thiserror::Error;
use tracing::warn;
use turin_contracts::{
    provider_instance::{ProviderDriverKind, ProviderInstanceConfigMap, ProviderInstanceId},
    provider_setup::ProviderSetupError,
    orchestration_v2::OrchestrationV2ProviderCapabilities,
    settings::provider_instance_config_enabled_flag,
};

use crate::{
    adapter_driver::{
        ProviderAdapterDriver, ProviderAdapterDriverCreateError, ProviderAdapterDriverCreateInput,
    },
    auth::AdmittedFailure,
    driver::ProviderInstanceScope,
    failure::ProviderFailureCause,
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2Error,
        ProviderAdapterV2OpenSessionInput, ProviderSessionRuntime,
    },
    provider_instance_registry::ProviderInstanceRegistry,
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("No orchestration provider adapter is registered for {instance_id}.")]
pub struct ProviderAdapterRegistryLookupError {
    pub instance_id: ProviderInstanceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Failed to read provider capabilities for {instance_id}: {detail}")]
pub struct ProviderAdapterRegistryMetadataError {
    pub instance_id: ProviderInstanceId,
    pub detail: String,
}

#[derive(Debug, Error)]
pub enum ProviderAdapterRegistryError {
    #[error(transparent)]
    Lookup(#[from] ProviderAdapterRegistryLookupError),
    #[error(transparent)]
    Metadata(#[from] ProviderAdapterRegistryMetadataError),
    #[error(transparent)]
    Create(#[from] ProviderAdapterDriverCreateError),
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterMetadata {
    pub driver: ProviderDriverKind,
    pub continuation_key: String,
    pub enabled: bool,
    pub capabilities: OrchestrationV2ProviderCapabilities,
}

enum RegistrySource {
    Instances(Arc<ProviderInstanceRegistry>),
    Adapters {
        adapters: Vec<Arc<dyn ProviderAdapterV2>>,
        /// Held so instance scopes stay open for as long as the registry does.
        #[allow(dead_code)]
        scopes: Vec<Arc<ProviderInstanceScope>>,
    },
}

pub struct ProviderAdapterRegistry {
    source: RegistrySource,
}

impl ProviderAdapterRegistry {
    pub fn from_instances(registry: Arc<ProviderInstanceRegistry>) -> Self {
        Self {
            source: RegistrySource::Instances(registry),
        }
    }

    pub fn from_adapters(adapters: Vec<Arc<dyn ProviderAdapterV2>>) -> Self {
        Self {
            source: RegistrySource::Adapters {
                adapters,
                scopes: Vec::new(),
            },
        }
    }

    /// Build adapters from a config map. An unknown driver is skipped. A
    /// decode or create failure fails the whole build, unlike the instance
    /// registry, which records a shadow snapshot.
    pub async fn from_drivers(
        drivers: Vec<Arc<dyn ProviderAdapterDriver>>,
        config_map: ProviderInstanceConfigMap,
    ) -> Result<Self, ProviderAdapterDriverCreateError> {
        let mut adapters = Vec::new();
        let mut scopes = Vec::new();
        for (instance_id, entry) in config_map {
            let Some(driver) = drivers.iter().find(|driver| driver.driver_kind() == &entry.driver) else {
                warn!(
                    instance_id = instance_id.as_str(),
                    driver = entry.driver.as_str(),
                    "Skipping orchestration-v2 provider adapter with unknown driver"
                );
                continue;
            };
            let raw = match &entry.config {
                None | Some(Value::Null) => driver.default_config(),
                Some(value) => value.clone(),
            };
            let typed = driver.decode_config(&raw).map_err(|detail| ProviderAdapterDriverCreateError {
                driver: entry.driver.clone(),
                instance_id: instance_id.clone(),
                detail: format!("Invalid provider instance config. {detail}"),
                cause: None,
            })?;
            let scope = ProviderInstanceScope::new();
            let enabled = entry
                .enabled
                .or_else(|| provider_instance_config_enabled_flag(Some(&typed)))
                .unwrap_or(true);
            match driver
                .create(ProviderAdapterDriverCreateInput {
                    instance_id: instance_id.clone(),
                    display_name: entry.display_name.clone(),
                    accent_color: entry.accent_color.clone(),
                    environment: entry.environment.clone().unwrap_or_default(),
                    enabled,
                    config: typed,
                    scope: Arc::clone(&scope),
                })
                .await
            {
                Ok(adapter) => {
                    adapters.push(adapter);
                    scopes.push(scope);
                }
                Err(error) => {
                    scope.close().await;
                    return Err(error);
                }
            }
        }
        Ok(Self {
            source: RegistrySource::Adapters { adapters, scopes },
        })
    }

    pub fn get(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Result<Arc<dyn ProviderAdapterV2>, ProviderAdapterRegistryLookupError> {
        match &self.source {
            RegistrySource::Instances(registry) => {
                let instance = registry.get_instance(instance_id).ok_or_else(|| {
                    ProviderAdapterRegistryLookupError {
                        instance_id: instance_id.clone(),
                    }
                })?;
                if instance.auth.is_none() {
                    return Ok(Arc::clone(&instance.orchestration_adapter));
                }
                Ok(Arc::new(CredentialGuardedAdapter {
                    inner: Arc::clone(&instance.orchestration_adapter),
                    registry: Arc::clone(registry),
                    instance_id: instance_id.clone(),
                }))
            }
            RegistrySource::Adapters { adapters, .. } => adapters
                .iter()
                .find(|adapter| adapter.instance_id() == instance_id)
                .cloned()
                .ok_or_else(|| ProviderAdapterRegistryLookupError {
                    instance_id: instance_id.clone(),
                }),
        }
    }

    pub fn list(&self) -> Vec<ProviderInstanceId> {
        match &self.source {
            RegistrySource::Instances(registry) => registry
                .list_instances()
                .into_iter()
                .map(|instance| instance.instance_id.clone())
                .collect(),
            RegistrySource::Adapters { adapters, .. } => {
                adapters.iter().map(|adapter| adapter.instance_id().clone()).collect()
            }
        }
    }

    pub async fn get_metadata(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Result<ProviderAdapterMetadata, ProviderAdapterRegistryError> {
        let RegistrySource::Instances(registry) = &self.source else {
            return Err(ProviderAdapterRegistryMetadataError {
                instance_id: instance_id.clone(),
                detail: "metadata is only available from the instance registry".to_owned(),
            }
            .into());
        };
        let instance = registry.get_instance(instance_id).ok_or_else(|| {
            ProviderAdapterRegistryLookupError {
                instance_id: instance_id.clone(),
            }
        })?;
        let capabilities = instance
            .orchestration_adapter
            .get_capabilities()
            .await
            .map_err(|error| ProviderAdapterRegistryMetadataError {
                instance_id: instance_id.clone(),
                detail: error.to_string(),
            })?;
        Ok(ProviderAdapterMetadata {
            driver: instance.driver_kind.clone(),
            continuation_key: instance.continuation_identity.continuation_key.clone(),
            enabled: instance.enabled,
            capabilities,
        })
    }
}

struct CredentialGuardedAdapter {
    inner: Arc<dyn ProviderAdapterV2>,
    registry: Arc<ProviderInstanceRegistry>,
    instance_id: ProviderInstanceId,
}

#[async_trait::async_trait]
impl ProviderAdapterV2 for CredentialGuardedAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        self.inner.instance_id()
    }

    fn driver(&self) -> &ProviderDriverKind {
        self.inner.driver()
    }

    async fn get_capabilities(&self) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        self.inner.get_capabilities().await
    }

    async fn plan_selection_transition(
        &self,
        input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        self.inner.plan_selection_transition(input).await
    }

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let current = self.registry.get_instance(&self.instance_id).ok_or_else(|| {
            open_session_error(self.inner.driver(), &input, "The provider instance is no longer registered.")
        })?;
        let binding = current
            .auth
            .as_ref()
            .and_then(|auth| auth.credential_binding.clone());
        let related = if let Some(binding) = binding {
            self.registry
                .list_instances()
                .into_iter()
                .filter(|instance| {
                    instance
                        .auth
                        .as_ref()
                        .and_then(|auth| auth.credential_binding.as_ref())
                        .is_some_and(|peer| peer.key == binding.key && peer.owner == binding.owner)
                })
                .collect::<Vec<_>>()
        } else {
            vec![current]
        };

        for instance in &related {
            let changing = instance
                .auth
                .as_ref()
                .and_then(|auth| auth.is_changing_credentials.as_ref());
            if let Some(changing) = changing {
                if changing().await {
                    return Err(setup_open_error(
                        self.inner.driver(),
                        &input,
                        ProviderSetupError::SetupError {
                            instance_id: self.instance_id.clone(),
                            operation: "session".to_owned(),
                            detail: "This provider's sign-in is changing. Try again after it finishes."
                                .to_owned(),
                            cause: None,
                        },
                    ));
                }
            }
        }

        let session_id = input.provider_session_id.clone();
        let driver = self.inner.driver().clone();
        let inner = Arc::clone(&self.inner);
        let mut task = crate::auth::AdmittedTask::new(async move { inner.open_session(input).await });
        for instance in &related {
            let Some(gate) = instance.auth.as_ref().and_then(|auth| auth.with_access.clone()) else {
                continue;
            };
            task = crate::auth::AdmittedTask::from_output(gate(task));
        }
        match task.wait::<Arc<dyn ProviderSessionRuntime>>().await {
            Ok(runtime) => Ok(runtime),
            Err(AdmittedFailure::Setup(error)) => Err(setup_open_error(&driver, &OpenSessionIds {
                provider_session_id: session_id,
            }, error)),
            Err(AdmittedFailure::Other(error)) => match error.downcast::<ProviderAdapterV2Error>() {
                Ok(error) => Err(*error),
                Err(error) => Err(ProviderAdapterV2Error::new(ProviderAdapterErrorKind::OpenSession {
                    driver,
                    provider_session_id: session_id,
                })
                .with_cause(unreadable_cause(error))),
            },
        }
    }
}

struct OpenSessionIds {
    provider_session_id: turin_contracts::ProviderSessionId,
}

fn open_session_error(
    driver: &ProviderDriverKind,
    input: &ProviderAdapterV2OpenSessionInput,
    detail: &str,
) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::OpenSession {
        driver: driver.clone(),
        provider_session_id: input.provider_session_id.clone(),
    })
    .with_cause(ProviderFailureCause::tagged("ProviderAdapterOpenSessionError").with_message(detail))
}

fn setup_open_error(
    driver: &ProviderDriverKind,
    input: &impl SessionId,
    error: ProviderSetupError,
) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::OpenSession {
        driver: driver.clone(),
        provider_session_id: input.session_id(),
    })
    .with_cause(
        ProviderFailureCause::tagged("ProviderSetupError")
            .with_message(error.to_string()),
    )
}

fn unreadable_cause(error: Box<dyn Any + Send>) -> ProviderFailureCause {
    let _ = error;
    ProviderFailureCause::Unreadable
}

trait SessionId {
    fn session_id(&self) -> turin_contracts::ProviderSessionId;
}

impl SessionId for ProviderAdapterV2OpenSessionInput {
    fn session_id(&self) -> turin_contracts::ProviderSessionId {
        self.provider_session_id.clone()
    }
}

impl SessionId for OpenSessionIds {
    fn session_id(&self) -> turin_contracts::ProviderSessionId {
        self.provider_session_id.clone()
    }
}
