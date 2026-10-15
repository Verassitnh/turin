//! Port of upstream/packages/provider-core/src/server/adapterDriver.ts.
//!
//! The thinner SPI used when a caller materializes adapters directly from a
//! config map. The instance registry's [`crate::driver::ProviderDriver`] is
//! the production path; this one exists because
//! [`crate::provider_adapter_registry::ProviderAdapterRegistry::from_drivers`]
//! builds adapters without a `ProviderInstance`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::{ProviderDriverKind, ProviderInstanceEnvironment, ProviderInstanceId},
};

use crate::{
    driver::ProviderInstanceScope,
    failure::ProviderFailureCause,
    provider_adapter::ProviderAdapterV2,
};

#[derive(Debug, Clone, PartialEq, Error)]
#[error("Provider adapter driver '{driver}' failed to create instance '{instance_id}': {detail}")]
pub struct ProviderAdapterDriverCreateError {
    pub driver: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    pub detail: String,
    pub cause: Option<ProviderFailureCause>,
}

pub struct ProviderAdapterDriverCreateInput {
    pub instance_id: ProviderInstanceId,
    pub display_name: Option<TrimmedNonEmptyString>,
    pub accent_color: Option<TrimmedNonEmptyString>,
    pub environment: ProviderInstanceEnvironment,
    pub enabled: bool,
    pub config: Value,
    pub scope: Arc<ProviderInstanceScope>,
}

#[async_trait]
pub trait ProviderAdapterDriver: Send + Sync {
    fn driver_kind(&self) -> &ProviderDriverKind;
    fn default_config(&self) -> Value;
    fn decode_config(&self, raw: &Value) -> Result<Value, String>;
    async fn create(
        &self,
        input: ProviderAdapterDriverCreateInput,
    ) -> Result<Arc<dyn ProviderAdapterV2>, ProviderAdapterDriverCreateError>;
}
