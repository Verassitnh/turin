//! Port of upstream/packages/contracts/src/checkpointDiff.ts
//!
//! Upstream attaches the `fromTurnCount <= toTurnCount` check to
//! `TurnCountRange` and preserves it through `mapFields`; the same check is
//! enforced here on each struct that embeds the range.

use crate::base_schemas::{NonNegativeInt, ThreadId, TrimmedNonEmptyString};
use serde::{Deserialize, Deserializer, Serialize};

fn validate_range(from: NonNegativeInt, to: NonNegativeInt) -> Result<(), String> {
    if from <= to {
        Ok(())
    } else {
        Err("fromTurnCount must be less than or equal to toTurnCount".to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnDiff {
    pub from_turn_count: NonNegativeInt,
    pub to_turn_count: NonNegativeInt,
    pub thread_id: ThreadId,
    pub diff: String,
}

impl<'de> Deserialize<'de> for ThreadTurnDiff {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            from_turn_count: NonNegativeInt,
            to_turn_count: NonNegativeInt,
            thread_id: ThreadId,
            diff: String,
        }
        let raw = Raw::deserialize(d)?;
        validate_range(raw.from_turn_count, raw.to_turn_count).map_err(serde::de::Error::custom)?;
        Ok(Self {
            from_turn_count: raw.from_turn_count,
            to_turn_count: raw.to_turn_count,
            thread_id: raw.thread_id,
            diff: raw.diff,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationGetTurnDiffInput {
    pub from_turn_count: NonNegativeInt,
    pub to_turn_count: NonNegativeInt,
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_whitespace: Option<bool>,
}

impl<'de> Deserialize<'de> for OrchestrationGetTurnDiffInput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            from_turn_count: NonNegativeInt,
            to_turn_count: NonNegativeInt,
            thread_id: ThreadId,
            #[serde(default)]
            ignore_whitespace: Option<bool>,
        }
        let raw = Raw::deserialize(d)?;
        validate_range(raw.from_turn_count, raw.to_turn_count).map_err(serde::de::Error::custom)?;
        Ok(Self {
            from_turn_count: raw.from_turn_count,
            to_turn_count: raw.to_turn_count,
            thread_id: raw.thread_id,
            ignore_whitespace: raw.ignore_whitespace,
        })
    }
}

pub type OrchestrationGetTurnDiffResult = ThreadTurnDiff;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationGetFullThreadDiffInput {
    pub thread_id: ThreadId,
    pub to_turn_count: NonNegativeInt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_whitespace: Option<bool>,
}

pub type OrchestrationGetFullThreadDiffResult = ThreadTurnDiff;

/// `OrchestrationGetTurnDiffError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationGetTurnDiffError {
    #[serde(rename = "OrchestrationGetTurnDiffError")]
    #[error("{message}")]
    OrchestrationGetTurnDiffError {
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

/// `OrchestrationGetFullThreadDiffError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationGetFullThreadDiffError {
    #[serde(rename = "OrchestrationGetFullThreadDiffError")]
    #[error("{message}")]
    OrchestrationGetFullThreadDiffError {
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_diff_range_is_validated() {
        let json = serde_json::json!({
            "fromTurnCount": 5,
            "toTurnCount": 2,
            "threadId": "t1",
            "diff": "",
        });
        let err = serde_json::from_value::<ThreadTurnDiff>(json).unwrap_err();
        assert!(err.to_string().contains("fromTurnCount"));

        let ok = serde_json::from_value::<ThreadTurnDiff>(serde_json::json!({
            "fromTurnCount": 1,
            "toTurnCount": 2,
            "threadId": "t1",
            "diff": "d",
        }))
        .unwrap();
        assert_eq!(ok.from_turn_count, 1);
    }

    #[test]
    fn get_turn_diff_input_rejects_inverted_range() {
        assert!(
            serde_json::from_value::<OrchestrationGetTurnDiffInput>(serde_json::json!({
                "fromTurnCount": 3,
                "toTurnCount": 1,
                "threadId": "t1",
            }))
            .is_err()
        );
    }
}
