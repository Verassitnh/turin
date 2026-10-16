//! Runtime event stream items (upstream `AcpSessionRuntimeEvent`).

use std::sync::Arc;

use crate::errors::AcpError;
use crate::runtime_model::AcpParsedSessionEvent;

#[derive(Debug, Clone)]
pub enum AcpSessionRuntimeEvent {
    Session(AcpParsedSessionEvent),
    ConfigOptionsUpdated {
        config_options: serde_json::Value,
        raw_payload: serde_json::Value,
    },
    EventStreamBarrier {
        acknowledge: Arc<tokio::sync::Notify>,
    },
    ConnectionTerminated {
        error: AcpError,
    },
}
