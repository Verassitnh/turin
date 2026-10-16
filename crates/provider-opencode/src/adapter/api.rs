//! The OpenCode adapter implements provider-core's `ProviderAdapterV2`.
//!
//! `protocol` is the local constructor for a driver-tagged protocol error.
//! The trait and its input types live in `turin_provider_core::provider_adapter`.

pub use turin_provider_core::provider_adapter::*;
pub use turin_provider_core::rt::BoxFuture;
pub use turin_provider_core::selection_transition::{
    ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan,
    turn_scoped_selection_transition,
};

/// In-scope constructor so call sites can keep `ProviderAdapterV2Error::protocol`.
pub trait OpenCodeProtocolError {
    fn protocol(detail: impl Into<String>) -> Self;
}

impl OpenCodeProtocolError for ProviderAdapterV2Error {
    fn protocol(detail: impl Into<String>) -> Self {
        Self::new(ProviderAdapterErrorKind::Protocol {
            driver: crate::adapter::logic::opencode_driver(),
            detail: detail.into(),
            payload: None,
        })
    }
}
