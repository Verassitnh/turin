//! Port of upstream/packages/contracts/src/orchestrationDispatch.ts

use crate::base_schemas::TrimmedNonEmptyString;
use serde::{Deserialize, Serialize};

/// A command dispatch failure. `bootstrapThreadDisposition` tells a client
/// whether the thread a bootstrap turn would have created was removed or
/// never created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OrchestrationDispatchCommandError {
    #[serde(rename = "OrchestrationDispatchCommandError")]
    #[error("{message}")]
    OrchestrationDispatchCommandError {
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bootstrap_thread_disposition: Option<BootstrapThreadDisposition>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BootstrapThreadDisposition {
    #[serde(rename = "deleted")]
    Deleted,
    #[serde(rename = "not-created")]
    NotCreated,
}
