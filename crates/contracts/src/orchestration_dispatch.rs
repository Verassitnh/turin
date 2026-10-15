//! Port of upstream/packages/contracts/src/orchestrationDispatch.ts

use crate::base_schemas::TrimmedNonEmptyString;
use serde::{Deserialize, Serialize};

/// A command dispatch failure. `bootstrapThreadDisposition` tells a client
/// whether the thread a bootstrap turn would have created was removed or
/// never created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn disposition(value: &str) -> Option<BootstrapThreadDisposition> {
        let OrchestrationDispatchCommandError::OrchestrationDispatchCommandError {
            bootstrap_thread_disposition,
            ..
        } = serde_json::from_value(serde_json::json!({
            "_tag": "OrchestrationDispatchCommandError",
            "message": "Failed to create worktree.",
            "bootstrapThreadDisposition": value,
        }))
        .unwrap();
        bootstrap_thread_disposition
    }

    #[test]
    fn decodes_a_dispatch_error_after_its_bootstrap_thread_was_deleted() {
        assert_eq!(
            disposition("deleted"),
            Some(BootstrapThreadDisposition::Deleted)
        );
    }

    #[test]
    fn decodes_a_dispatch_error_before_its_bootstrap_thread_was_created() {
        assert_eq!(
            disposition("not-created"),
            Some(BootstrapThreadDisposition::NotCreated)
        );
    }

    #[test]
    fn message_must_be_non_blank_and_optionals_are_omitted() {
        assert!(
            serde_json::from_value::<OrchestrationDispatchCommandError>(serde_json::json!({
                "_tag": "OrchestrationDispatchCommandError", "message": "  ",
            }))
            .is_err()
        );
        let error: OrchestrationDispatchCommandError = serde_json::from_value(serde_json::json!({
            "_tag": "OrchestrationDispatchCommandError", "message": "x",
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({ "_tag": "OrchestrationDispatchCommandError", "message": "x" })
        );
    }
}
