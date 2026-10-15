//! Port of upstream/apps/server/src/orchestration-v2/threadHistoryPaging.ts

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use turin_contracts::orchestration_v2::OrchestrationV2ProjectedTurnItem;

pub const THREAD_HISTORY_MAX_RAW_TURNS: usize = 150;
pub const THREAD_HISTORY_SNAPSHOT_ROW_LIMIT: usize = 77;
pub const THREAD_HISTORY_CURSOR_MAX_LENGTH: usize = 4_096;
pub const OLDER_THREAD_USER_TURN_LIMIT: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct ThreadHistoryPagePolicy {
    pub max_user_turns: Option<usize>,
    pub max_items: usize,
    pub max_encoded_bytes: usize,
}
impl Default for ThreadHistoryPagePolicy {
    fn default() -> Self {
        Self {
            max_user_turns: Some(10),
            max_items: 75,
            max_encoded_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadHistoryCursorPayload {
    pub v: u8,
    pub seq: u64,
    pub st: String,
    pub si: String,
    pub p: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid thread history cursor.")]
pub struct InvalidThreadHistoryCursorError;

#[derive(Debug, Clone)]
pub struct SelectedTimelinePage {
    pub items: Vec<OrchestrationV2ProjectedTurnItem>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}

pub fn encode_thread_history_cursor(
    sequence: u64,
    row: &OrchestrationV2ProjectedTurnItem,
) -> String {
    let payload = ThreadHistoryCursorPayload {
        v: 1,
        seq: sequence,
        st: row.source_thread_id.as_str().to_owned(),
        si: row.source_item_id.as_str().to_owned(),
        p: row.position,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("cursor serialization"))
}

pub fn decode_thread_history_cursor(
    cursor: &str,
) -> Result<ThreadHistoryCursorPayload, InvalidThreadHistoryCursorError> {
    if cursor.is_empty() || cursor.len() > THREAD_HISTORY_CURSOR_MAX_LENGTH {
        return Err(InvalidThreadHistoryCursorError);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| InvalidThreadHistoryCursorError)?;
    let parsed: ThreadHistoryCursorPayload =
        serde_json::from_slice(&bytes).map_err(|_| InvalidThreadHistoryCursorError)?;
    if parsed.v != 1 || parsed.st.is_empty() || parsed.si.is_empty() {
        return Err(InvalidThreadHistoryCursorError);
    }
    Ok(parsed)
}

pub fn projected_row_encoded_bytes(row: &OrchestrationV2ProjectedTurnItem) -> usize {
    serde_json::to_vec(row)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

pub fn projected_row_bounded_snapshot_encoded_bytes(
    row: &OrchestrationV2ProjectedTurnItem,
    thread_id: &str,
) -> usize {
    let row_bytes = projected_row_encoded_bytes(row);
    if row.visibility
        == turin_contracts::orchestration_v2::OrchestrationV2ProjectedTurnItemVisibility::Local
        || row.source_thread_id.as_str() == thread_id
    {
        row_bytes
            + serde_json::to_vec(&row.item)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
    } else {
        row_bytes
    }
}

fn item_json(row: &OrchestrationV2ProjectedTurnItem) -> Value {
    serde_json::to_value(&row.item).unwrap_or(Value::Null)
}
fn is_turn_start(item: &Value) -> bool {
    item["type"] == "user_message"
        && matches!(
            item["inputIntent"].as_str(),
            Some("turn_start" | "queued_turn")
        )
}
fn is_user_turn(item: &Value) -> bool {
    is_turn_start(item) && item["createdBy"] == "user"
}

pub fn thread_history_turn_starts(items: &[OrchestrationV2ProjectedTurnItem]) -> Vec<bool> {
    let mut started_runs = std::collections::HashSet::new();
    items
        .iter()
        .map(|row| {
            let item = item_json(row);
            let kind = item["type"].as_str();
            if !matches!(kind, Some("user_message" | "notification")) {
                return false;
            }
            let first = item["runId"]
                .as_str()
                .is_some_and(|run| started_runs.insert(run.to_owned()));
            if kind == Some("notification") {
                first
            } else {
                is_turn_start(&item)
            }
        })
        .collect()
}

pub fn is_conversation_history_item(row: &OrchestrationV2ProjectedTurnItem) -> bool {
    !matches!(
        item_json(row)["type"].as_str(),
        Some("command_execution" | "reasoning" | "file_change" | "dynamic_tool")
    )
}

fn select_older(
    items: &[OrchestrationV2ProjectedTurnItem],
    end: usize,
    sequence: u64,
    policy: ThreadHistoryPagePolicy,
    cost: fn(&OrchestrationV2ProjectedTurnItem) -> usize,
) -> SelectedTimelinePage {
    let end = end.min(items.len());
    if end == 0 {
        return SelectedTimelinePage {
            items: Vec::new(),
            next_cursor: None,
            has_more_history: false,
        };
    }
    let candidates = &items[..end];
    let turn_starts = thread_history_turn_starts(candidates);
    let turn_limit = candidates
        .iter()
        .any(|row| is_user_turn(&item_json(row)))
        .then_some(policy.max_user_turns)
        .flatten();
    let mut selected = Vec::new();
    let mut encoded_bytes = 0;
    let mut user_turns = 0;
    let mut raw_turns = 0;
    for index in (0..end).rev() {
        let row = &items[index];
        let bytes = if turn_limit.is_none() { cost(row) } else { 0 };
        if !selected.is_empty()
            && if let Some(limit) = turn_limit {
                user_turns >= limit || raw_turns >= THREAD_HISTORY_MAX_RAW_TURNS
            } else {
                selected.len() >= policy.max_items
                    || encoded_bytes + bytes > policy.max_encoded_bytes
            }
        {
            break;
        }
        selected.push(row.clone());
        encoded_bytes += bytes;
        if is_user_turn(&item_json(row)) {
            user_turns += 1;
        }
        if turn_starts[index] {
            raw_turns += 1;
        }
    }
    selected.reverse();
    let oldest_index = selected
        .first()
        .and_then(|oldest| {
            items.iter().position(|row| {
                row.source_thread_id == oldest.source_thread_id
                    && row.source_item_id == oldest.source_item_id
            })
        })
        .unwrap_or(0);
    let has_more_history = oldest_index > 0;
    let next_cursor = if has_more_history {
        selected
            .first()
            .map(|row| encode_thread_history_cursor(sequence, row))
    } else {
        None
    };
    for (position, row) in selected.iter_mut().enumerate() {
        row.position = position as u64;
    }
    SelectedTimelinePage {
        items: selected,
        next_cursor,
        has_more_history,
    }
}

pub fn select_recent_timeline_window(
    items: &[OrchestrationV2ProjectedTurnItem],
    sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
) -> SelectedTimelinePage {
    select_older(
        items,
        items.len(),
        sequence,
        policy.unwrap_or_default(),
        projected_row_encoded_bytes,
    )
}

pub fn select_history_page_from_cursor(
    items: &[OrchestrationV2ProjectedTurnItem],
    cursor: &str,
    sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
    through_entry_id: Option<&str>,
) -> Result<SelectedTimelinePage, InvalidThreadHistoryCursorError> {
    let decoded = decode_thread_history_cursor(cursor)?;
    let anchor = items
        .iter()
        .position(|row| {
            row.source_thread_id.as_str() == decoded.st && row.source_item_id.as_str() == decoded.si
        })
        .unwrap_or((decoded.p as usize).min(items.len()));
    let mut policy = policy.unwrap_or(ThreadHistoryPagePolicy {
        max_user_turns: Some(OLDER_THREAD_USER_TURN_LIMIT),
        ..ThreadHistoryPagePolicy::default()
    });
    if let Some(entry_id) = through_entry_id
        && let Some(target) = items.iter().position(|row| {
            let item = item_json(row);
            item["messageId"].as_str().or_else(|| item["id"].as_str()) == Some(entry_id)
        })
        && target < anchor
    {
        let turns = items[target..anchor]
            .iter()
            .filter(|row| is_user_turn(&item_json(row)))
            .count();
        policy.max_user_turns = Some(
            (turns + usize::from(!is_user_turn(&item_json(&items[target])))).clamp(
                OLDER_THREAD_USER_TURN_LIMIT,
                OLDER_THREAD_USER_TURN_LIMIT * 2,
            ),
        );
        policy.max_items *= 2;
    }
    Ok(select_older(
        items,
        anchor,
        sequence,
        policy,
        projected_row_encoded_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_cursor() {
        for cursor in ["", "!!!", "a".repeat(4097).as_str()] {
            assert!(decode_thread_history_cursor(cursor).is_err());
        }
    }
}
