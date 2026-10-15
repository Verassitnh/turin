//! Port of upstream/packages/provider-core/src/server/ProviderEventLoggers.ts.
//!
//! One stream for native protocol frames and one for canonical orchestration
//! events. Either logger is absent when diagnostics logging could not start.

use std::sync::Arc;

use turin_contracts::ThreadId;

use crate::rt::BoxFuture;

pub struct EventNdjsonLogger {
    pub file_path: String,
    pub write: Arc<dyn Fn(serde_json::Value, Option<ThreadId>) -> BoxFuture<'static, ()> + Send + Sync>,
    pub close: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
}

pub struct ProviderEventLoggers {
    pub native: Option<EventNdjsonLogger>,
    pub canonical: Option<EventNdjsonLogger>,
}

impl ProviderEventLoggers {
    /// Logging turned off: for tests and boot that skips diagnostics.
    pub fn noop() -> Self {
        Self {
            native: None,
            canonical: None,
        }
    }
}
