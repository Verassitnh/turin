//! Port of upstream/packages/provider-core/src/server/errors.ts.

use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

use crate::failure::ProviderFailureCause;

/// A driver `create` call failed before producing an instance. The registry
/// marks the entry as an unavailable shadow snapshot rather than crashing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Provider driver '{driver}' failed to create instance '{instance_id}': {detail}")]
pub struct ProviderDriverError {
    pub driver: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    pub detail: String,
    pub cause: Option<ProviderFailureCause>,
}

/// Reading or writing a provider's stored credentials failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderCredentialOperation {
    Get,
    Set,
    Remove,
}

impl ProviderCredentialOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Set => "set",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Could not {operation} stored provider credentials.")]
pub struct ProviderCredentialError {
    pub operation: ProviderCredentialOperation,
    pub cause: ProviderFailureCause,
}

impl std::fmt::Display for ProviderCredentialOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
