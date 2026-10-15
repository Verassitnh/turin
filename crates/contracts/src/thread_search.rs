//! Port of upstream/packages/contracts/src/threadSearch.ts

use crate::base_schemas::{
    IsoDateTime, ProjectId, ThreadId, TrimmedNonEmptyString, TrimmedString, js_len,
};
use serde::{Deserialize, Deserializer, Serialize};

/// `TrimmedString.check(isMinLength(2), isMaxLength(200))`: the checks run
/// on the trimmed value.
fn de_query<'de, D: Deserializer<'de>>(d: D) -> Result<TrimmedString, D::Error> {
    let query = TrimmedString::deserialize(d)?;
    match js_len(query.as_str()) {
        2..=200 => Ok(query),
        _ => Err(serde::de::Error::custom(
            "query must be between 2 and 200 characters after trimming",
        )),
    }
}

/// `Schema.optionalKey(Schema.Int.check(isBetween({ minimum: 1, maximum: 50 })))`.
fn de_limit<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    match Option::<i64>::deserialize(d)? {
        Some(limit) if !(1..=50).contains(&limit) => Err(serde::de::Error::custom(
            "limit must be an integer between 1 and 50",
        )),
        limit => Ok(limit),
    }
}

/// `Schema.String.check(isMaxLength(240))`.
fn de_snippet<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let snippet = String::deserialize(d)?;
    if js_len(&snippet) > 240 {
        return Err(serde::de::Error::custom(
            "snippet must be at most 240 characters",
        ));
    }
    Ok(snippet)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestrationThreadSearchSource {
    User,
    Assistant,
}

/// The server's SQLite client is synchronous and single-connection. Bound both
/// scan input and response size so a search cannot monopolize that connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsInput {
    #[serde(deserialize_with = "de_query")]
    pub query: TrimmedString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_limit"
    )]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationThreadSearchMatch {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub source: OrchestrationThreadSearchSource,
    #[serde(deserialize_with = "de_snippet")]
    pub snippet: String,
    pub message_created_at: Option<IsoDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsResult {
    pub matches: Vec<OrchestrationThreadSearchMatch>,
}

/// `OrchestrationSearchThreadsError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OrchestrationSearchThreadsError {
    #[serde(rename = "OrchestrationSearchThreadsError")]
    #[error("{message}")]
    OrchestrationSearchThreadsError {
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn input(value: Value) -> Result<OrchestrationSearchThreadsInput, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn query_is_trimmed_then_bounded() {
        assert_eq!(
            input(json!({"query": "  ab  "})).unwrap().query.as_str(),
            "ab"
        );
        // One character after trimming is below the minimum.
        assert!(input(json!({"query": "  a  "})).is_err());
        assert!(input(json!({"query": ""})).is_err());
        assert!(input(json!({"query": "q".repeat(200)})).is_ok());
        assert!(input(json!({"query": "q".repeat(201)})).is_err());
        // Trimming happens first, so padding does not count against the max.
        assert!(input(json!({"query": format!("  {}  ", "q".repeat(200))})).is_ok());
        // Lengths are UTF-16 code units: one emoji is two.
        assert!(input(json!({"query": "😀"})).is_ok());
        assert!(input(json!({"query": "😀".repeat(100)})).is_ok());
        assert!(input(json!({"query": "😀".repeat(101)})).is_err());
        // JS `trim()` strips U+FEFF but not U+0085.
        assert!(input(json!({"query": "\u{feff}a\u{feff}"})).is_err());
        assert!(input(json!({"query": "a\u{85}"})).is_ok());
    }

    #[test]
    fn limit_is_an_optional_integer_between_one_and_fifty() {
        assert_eq!(input(json!({"query": "ab"})).unwrap().limit, None);
        for ok in [1, 25, 50] {
            assert_eq!(
                input(json!({"query": "ab", "limit": ok})).unwrap().limit,
                Some(ok)
            );
        }
        for bad in [json!(0), json!(51), json!(-1), json!(2.5), json!("5")] {
            assert!(input(json!({"query": "ab", "limit": bad})).is_err());
        }
        // Absent limit is not written back out.
        let absent = input(json!({"query": "ab"})).unwrap();
        assert_eq!(
            serde_json::to_value(&absent).unwrap(),
            json!({"query": "ab"})
        );
    }

    #[test]
    fn match_round_trips_with_null_timestamp_and_bounded_snippet() {
        let value = json!({
            "threadId": "t1", "projectId": "p1", "source": "assistant",
            "snippet": "…found it…", "messageCreatedAt": null,
        });
        let decoded: OrchestrationThreadSearchMatch =
            serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.source, OrchestrationThreadSearchSource::Assistant);
        assert_eq!(decoded.message_created_at, None);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);

        let mut dated = value.clone();
        dated["messageCreatedAt"] = json!("2026-07-01T00:00:00Z");
        dated["snippet"] = json!("x".repeat(240));
        let decoded: OrchestrationThreadSearchMatch =
            serde_json::from_value(dated.clone()).unwrap();
        // IsoDateTime is a passthrough string.
        assert_eq!(serde_json::to_value(&decoded).unwrap(), dated);

        let mut long = value.clone();
        long["snippet"] = json!("x".repeat(241));
        assert!(serde_json::from_value::<OrchestrationThreadSearchMatch>(long).is_err());
        let mut emoji = value.clone();
        emoji["snippet"] = json!("😀".repeat(121));
        assert!(serde_json::from_value::<OrchestrationThreadSearchMatch>(emoji).is_err());
        let mut bad_source = value;
        bad_source["source"] = json!("system");
        assert!(serde_json::from_value::<OrchestrationThreadSearchMatch>(bad_source).is_err());
    }

    #[test]
    fn error_round_trips_with_tag_and_requires_a_message() {
        let wire = json!({"_tag": "OrchestrationSearchThreadsError", "message": "boom"});
        let error: OrchestrationSearchThreadsError = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(error.to_string(), "boom");
        assert_eq!(serde_json::to_value(&error).unwrap(), wire);
        assert!(
            serde_json::from_value::<OrchestrationSearchThreadsError>(
                json!({"_tag": "OrchestrationSearchThreadsError", "message": "  "})
            )
            .is_err()
        );
    }
}
