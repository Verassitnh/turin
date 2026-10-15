//! Port of upstream/apps/server/src/orchestration-v2/threadHistoryPaging.ts
//!
//! Cursor paging over a thread's visible timeline. Cut (PORTING.md "No
//! client/server split"): `buildBoundedThreadProjection`,
//! `projectedRowBoundedSnapshotEncodedBytes`, `boundedTimelineEncodedBytes`,
//! `computeLatestLocalTurnOrdinal` and `BoundedProjectionResult` exist only to
//! fit a snapshot into a socket frame; the GPUI app reads projections
//! in-process.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2ProjectedTurnItem, OrchestrationV2TurnItem,
    OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::{ThreadId, TurnItemId};

use crate::projection::turn_item;

/// Match the V1 conversation windows. Item/byte budgets only apply to histories
/// without turn starts; tool activity must not split an ordinary conversation turn.
pub const THREAD_HISTORY_PAGE_POLICY: ThreadHistoryPagePolicy = ThreadHistoryPagePolicy {
    max_user_turns: Some(10),
    max_items: 75,
    max_encoded_bytes: 1_048_576,
};

pub const OLDER_THREAD_USER_TURN_LIMIT: usize = 20;

pub const THREAD_HISTORY_MAX_RAW_TURNS: usize = 150;

/// Extra rows let the projection query retain an inclusive cursor and prove another page exists.
pub const THREAD_HISTORY_SNAPSHOT_ROW_LIMIT: usize = THREAD_HISTORY_PAGE_POLICY.max_items + 2;

/// Reject absurd cursors before base64 work or JSON parse.
pub const THREAD_HISTORY_CURSOR_MAX_LENGTH: usize = 4_096;

/// Load conversation rows before the bulk of a turn's folded tool activity.
pub fn is_conversation_history_item(type_tag: &str) -> bool {
    !matches!(
        type_tag,
        "command_execution" | "reasoning" | "file_change" | "dynamic_tool"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadHistoryPagePolicy {
    pub max_user_turns: Option<usize>,
    pub max_items: usize,
    pub max_encoded_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadHistoryCursorPayload {
    pub v: u8,
    pub seq: u64,
    pub st: String,
    pub si: String,
    pub p: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Invalid thread history cursor.")]
pub struct InvalidThreadHistoryCursorError;

/// The item fields history paging reads. The timeline index carries only
/// these; full projected rows carry the whole item.
pub trait HistoryItem {
    fn type_tag(&self) -> &str;
    fn id(&self) -> Option<&str>;
    fn message_id(&self) -> Option<&str>;
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent>;
    fn created_by(&self) -> Option<OrchestrationV2Actor>;
    fn run_id(&self) -> Option<&str>;
}

impl HistoryItem for OrchestrationV2TurnItem {
    fn type_tag(&self) -> &str {
        turn_item::type_tag(self)
    }
    fn id(&self) -> Option<&str> {
        Some(turn_item::common(self).id.as_str())
    }
    fn message_id(&self) -> Option<&str> {
        turn_item::message_id(self).map(|id| id.as_str())
    }
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent> {
        turn_item::input_intent(self)
    }
    fn created_by(&self) -> Option<OrchestrationV2Actor> {
        turn_item::created_by(self)
    }
    fn run_id(&self) -> Option<&str> {
        turn_item::common(self).run_id.map(|id| id.as_str())
    }
}

/// A timeline row: `Pick<ProjectedTurnItem, position | visibility | source*>`
/// plus its item.
pub trait HistoryRow: Clone {
    type Item: HistoryItem;
    fn position(&self) -> u64;
    fn set_position(&mut self, position: u64);
    fn source_thread_id(&self) -> &ThreadId;
    fn source_item_id(&self) -> &TurnItemId;
    fn item(&self) -> &Self::Item;
}

impl HistoryRow for OrchestrationV2ProjectedTurnItem {
    type Item = OrchestrationV2TurnItem;
    fn position(&self) -> u64 {
        self.position
    }
    fn set_position(&mut self, position: u64) {
        self.position = position;
    }
    fn source_thread_id(&self) -> &ThreadId {
        &self.source_thread_id
    }
    fn source_item_id(&self) -> &TurnItemId {
        &self.source_item_id
    }
    fn item(&self) -> &OrchestrationV2TurnItem {
        &self.item
    }
}

#[derive(Debug, Clone)]
pub struct SelectTimelinePageResult<Row> {
    pub items: Vec<Row>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}

/// Ordinary history-page cost: one projected row (item is nested once).
pub fn projected_row_encoded_bytes(row: &OrchestrationV2ProjectedTurnItem) -> usize {
    serde_json::to_vec(row)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn renumber_positions<Row: HistoryRow>(items: Vec<Row>) -> Vec<Row> {
    items
        .into_iter()
        .enumerate()
        .map(|(position, mut row)| {
            if row.position() != position as u64 {
                row.set_position(position as u64);
            }
            row
        })
        .collect()
}

pub fn encode_thread_history_cursor(
    snapshot_sequence: u64,
    source_thread_id: &str,
    source_item_id: &str,
    position: u64,
) -> String {
    let payload = ThreadHistoryCursorPayload {
        v: 1,
        seq: snapshot_sequence,
        st: source_thread_id.to_owned(),
        si: source_item_id.to_owned(),
        p: position,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("cursor payload serializes"))
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

fn is_thread_history_turn_start<I: HistoryItem + ?Sized>(item: &I) -> bool {
    item.type_tag() == "user_message"
        && matches!(
            item.input_intent(),
            Some(
                OrchestrationV2UserMessageInputIntent::TurnStart
                    | OrchestrationV2UserMessageInputIntent::QueuedTurn
            )
        )
}

/// Marks the turn starts of a chronological timeline. A wake (PR watch, task
/// completion) starts its run with a notification instead of a user message, so
/// a notification starts a turn when it is its run's first input. One steered
/// into a running turn comes later. ProjectionStore's `run_starts` window query
/// applies the same rule.
pub fn thread_history_turn_starts<'a, I: HistoryItem + 'a>(
    items: impl IntoIterator<Item = &'a I>,
) -> Vec<bool> {
    let mut started_runs: HashSet<String> = HashSet::new();
    items
        .into_iter()
        .map(|item| {
            let type_tag = item.type_tag();
            if type_tag != "user_message" && type_tag != "notification" {
                return false;
            }
            let first_input = match item.run_id() {
                Some(run_id) => started_runs.insert(run_id.to_owned()),
                None => false,
            };
            if type_tag == "notification" {
                first_input
            } else {
                is_thread_history_turn_start(item)
            }
        })
        .collect()
}

/// Steering belongs to its existing turn and must not consume another page slot.
pub fn is_thread_history_user_turn<I: HistoryItem + ?Sized>(item: &I) -> bool {
    is_thread_history_turn_start(item)
        && item.type_tag() == "user_message"
        && item.created_by() == Some(OrchestrationV2Actor::User)
}

/// Walk a chronological timeline backward from the exclusive end, collecting rows
/// through complete user turns. Histories without turn starts use row budgets
/// and always admit at least one row so oversized items cannot deadlock paging.
fn select_older_timeline_page<Row: HistoryRow>(
    items: &[Row],
    exclusive_end_index: usize,
    snapshot_sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
    row_encoded_bytes: &dyn Fn(&Row) -> usize,
) -> SelectTimelinePageResult<Row> {
    let policy = policy.unwrap_or(THREAD_HISTORY_PAGE_POLICY);
    let end = exclusive_end_index.min(items.len());
    if end == 0 {
        return SelectTimelinePageResult {
            items: Vec::new(),
            next_cursor: None,
            has_more_history: false,
        };
    }

    let mut selected: Vec<Row> = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut user_turns = 0usize;
    let mut raw_turns = 0usize;
    let candidates = &items[..end];
    let turn_starts = thread_history_turn_starts(candidates.iter().map(|row| row.item()));
    let turn_limit = if candidates
        .iter()
        .any(|row| is_thread_history_user_turn(row.item()))
    {
        policy.max_user_turns
    } else {
        None
    };
    for index in (0..end).rev() {
        let row = &items[index];
        let row_bytes = if turn_limit.is_none() {
            row_encoded_bytes(row)
        } else {
            0
        };
        if !selected.is_empty() {
            let full = match turn_limit {
                None => {
                    selected.len() >= policy.max_items
                        || encoded_bytes + row_bytes > policy.max_encoded_bytes
                }
                Some(limit) => user_turns >= limit || raw_turns >= THREAD_HISTORY_MAX_RAW_TURNS,
            };
            if full {
                break;
            }
        }
        selected.push(row.clone());
        encoded_bytes += row_bytes;
        if is_thread_history_user_turn(row.item()) {
            user_turns += 1;
        }
        if turn_starts[index] {
            raw_turns += 1;
        }
    }
    selected.reverse();

    let oldest_index = selected.first().and_then(|oldest| {
        items.iter().position(|row| {
            row.source_thread_id() == oldest.source_thread_id()
                && row.source_item_id() == oldest.source_item_id()
        })
    });
    let has_more_history = oldest_index.is_some_and(|index| index > 0);
    let next_cursor = if has_more_history {
        selected.first().map(|oldest| {
            encode_thread_history_cursor(
                snapshot_sequence,
                oldest.source_thread_id().as_str(),
                oldest.source_item_id().as_str(),
                oldest.position(),
            )
        })
    } else {
        None
    };

    SelectTimelinePageResult {
        items: renumber_positions(selected),
        next_cursor,
        has_more_history,
    }
}

pub fn select_recent_timeline_window<Row: HistoryRow>(
    items: &[Row],
    snapshot_sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
    row_encoded_bytes: &dyn Fn(&Row) -> usize,
) -> SelectTimelinePageResult<Row> {
    select_older_timeline_page(
        items,
        items.len(),
        snapshot_sequence,
        policy,
        row_encoded_bytes,
    )
}

fn find_cursor_index<Row: HistoryRow>(items: &[Row], cursor: &ThreadHistoryCursorPayload) -> usize {
    let by_identity = items.iter().position(|row| {
        row.source_thread_id().as_str() == cursor.st && row.source_item_id().as_str() == cursor.si
    });
    if let Some(index) = by_identity {
        return index;
    }
    // Identity miss can happen if the cursor item was deleted; fall back to the
    // recorded position clamped into the current timeline so pagination can
    // continue from a stable-ish anchor rather than failing hard. Clamp to
    // items.length (exclusive end) so a shrunken timeline never 400s forever.
    // decodeThreadHistoryCursor already guarantees p is a non-negative integer.
    (cursor.p as usize).min(items.len())
}

pub fn select_history_page_from_cursor<Row: HistoryRow>(
    items: &[Row],
    cursor: &str,
    snapshot_sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
    row_encoded_bytes: &dyn Fn(&Row) -> usize,
    through_entry_id: Option<&str>,
) -> Result<SelectTimelinePageResult<Row>, InvalidThreadHistoryCursorError> {
    let decoded = decode_thread_history_cursor(cursor)?;
    let anchor_index = find_cursor_index(items, &decoded);
    let mut policy = policy.unwrap_or(ThreadHistoryPagePolicy {
        max_user_turns: Some(OLDER_THREAD_USER_TURN_LIMIT),
        ..THREAD_HISTORY_PAGE_POLICY
    });
    if let Some(through) = through_entry_id {
        let target_index = items.iter().position(|row| {
            let item = row.item();
            item.message_id().or(item.id()) == Some(through)
        });
        if let Some(target_index) = target_index
            && target_index < anchor_index
        {
            // A search jump can combine two pages, but never grow into an unbounded read.
            let turns = items[target_index..anchor_index]
                .iter()
                .filter(|row| is_thread_history_user_turn(row.item()))
                .count();
            let target_is_turn = is_thread_history_user_turn(items[target_index].item());
            policy =
                ThreadHistoryPagePolicy {
                    max_user_turns: Some((OLDER_THREAD_USER_TURN_LIMIT * 2).min(
                        OLDER_THREAD_USER_TURN_LIMIT.max(turns + usize::from(!target_is_turn)),
                    )),
                    max_items: policy.max_items * 2,
                    ..policy
                };
        }
    }
    Ok(select_older_timeline_page(
        items,
        anchor_index,
        snapshot_sequence,
        Some(policy),
        row_encoded_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn make_row(index: u64, output_bytes: Option<usize>) -> OrchestrationV2ProjectedTurnItem {
        let output = output_bytes
            .map(|bytes| "x".repeat(bytes))
            .unwrap_or_else(|| format!("output-{index}"));
        serde_json::from_value(json!({
            "position": index,
            "visibility": "local",
            "sourceThreadId": "thread-1",
            "sourceItemId": format!("item-{index}"),
            "item": {
                "id": format!("item-{index}"), "type": "command_execution", "threadId": "thread-1",
                "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": index + 1,
                "status": "completed", "title": format!("Command {index}"),
                "input": format!("cmd-{index}"), "output": output, "exitCode": 0,
                "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW
            }
        }))
        .unwrap()
    }

    fn rows(count: u64) -> Vec<OrchestrationV2ProjectedTurnItem> {
        (0..count).map(|index| make_row(index, None)).collect()
    }

    fn ids(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
        rows.iter()
            .map(|row| row.source_item_id.to_string())
            .collect()
    }

    fn recent(
        items: &[OrchestrationV2ProjectedTurnItem],
        sequence: u64,
        policy: Option<ThreadHistoryPagePolicy>,
    ) -> SelectTimelinePageResult<OrchestrationV2ProjectedTurnItem> {
        select_recent_timeline_window(items, sequence, policy, &projected_row_encoded_bytes)
    }

    fn older(
        items: &[OrchestrationV2ProjectedTurnItem],
        cursor: &str,
        sequence: u64,
        policy: Option<ThreadHistoryPagePolicy>,
    ) -> SelectTimelinePageResult<OrchestrationV2ProjectedTurnItem> {
        select_history_page_from_cursor(
            items,
            cursor,
            sequence,
            policy,
            &projected_row_encoded_bytes,
            None,
        )
        .unwrap()
    }

    fn with_item(
        mut row: OrchestrationV2ProjectedTurnItem,
        patch: serde_json::Value,
    ) -> OrchestrationV2ProjectedTurnItem {
        let mut value = serde_json::to_value(&row.item).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        row.item = serde_json::from_value(value).unwrap();
        row
    }

    fn prompt_row(index: u64, created_by: &str, text: &str) -> OrchestrationV2ProjectedTurnItem {
        with_item(
            make_row(index, None),
            json!({
                "type": "user_message", "createdBy": created_by, "creationSource": "provider",
                "inputIntent": "turn_start", "messageId": format!("prompt-{index}"),
                "text": text, "attachments": []
            }),
        )
    }

    #[test]
    fn keeps_background_turns_with_user_turns_with_a_150_turn_fan_out_ceiling() {
        let items: Vec<_> = (0..161u64)
            .flat_map(|turn| {
                [
                    prompt_row(
                        turn * 2,
                        if turn == 0 { "user" } else { "agent" },
                        &format!("Prompt {turn}"),
                    ),
                    make_row(turn * 2 + 1, None),
                ]
            })
            .collect();
        let first = recent(&items, 1, None);
        assert_eq!(first.items.len(), 300);
        assert_eq!(first.items[0].source_item_id.as_str(), "item-22");
        let page = older(&items, first.next_cursor.as_deref().unwrap(), 1, None);
        assert_eq!(page.items.len(), 22);
        assert!(!page.has_more_history);
        let mut all = ids(&page.items);
        all.extend(ids(&first.items));
        assert_eq!(all, ids(&items));
    }

    #[test]
    fn counts_runs_that_a_notification_started_toward_the_150_turn_ceiling() {
        let notification = |summary: String| json!({"type": "notification", "source": {"kind": "monitor"}, "outcome": "completed", "summary": summary});
        let in_run = |index: u64, run: u64, patch: serde_json::Value| {
            let mut patch = patch.as_object().cloned().unwrap_or_default();
            patch.insert("runId".into(), json!(format!("run-{run}")));
            with_item(make_row(index, None), serde_json::Value::Object(patch))
        };
        let items: Vec<_> = (0..161u64)
            .flat_map(|run| {
                let second = if run == 0 {
                    json!({"type": "user_message", "createdBy": "user", "creationSource": "web",
                        "inputIntent": "turn_start", "messageId": "prompt-0", "text": "Prompt 0",
                        "attachments": []})
                } else {
                    notification(format!("Wake {run}"))
                };
                [
                    in_run(run * 4, run, json!({})),
                    in_run(run * 4 + 1, run, second),
                    in_run(run * 4 + 2, run, notification(format!("Steer {run}"))),
                    in_run(run * 4 + 3, run, json!({})),
                ]
            })
            .collect();
        let first = recent(&items, 1, None);
        // 150 runs from the start of run 11. Its earlier row goes to the older page.
        assert_eq!(first.items.len(), 150 * 4 - 1);
        assert_eq!(
            first.items[0].source_item_id.as_str(),
            format!("item-{}", 11 * 4 + 1)
        );
        let page = older(&items, first.next_cursor.as_deref().unwrap(), 1, None);
        let mut all = ids(&page.items);
        all.extend(ids(&first.items));
        assert_eq!(all, ids(&items));
    }

    #[test]
    fn pages_agent_only_child_transcripts_instead_of_dropping_their_earlier_activity() {
        let mut items = vec![prompt_row(0, "agent", "Inspect this project")];
        items.extend((0..90).map(|index| make_row(index + 1, None)));
        let first = recent(&items, 1, None);
        assert_eq!(first.items.len(), THREAD_HISTORY_PAGE_POLICY.max_items);
        assert!(first.has_more_history);
        let page = older(&items, first.next_cursor.as_deref().unwrap(), 1, None);
        assert_eq!(turn_item::type_tag(&page.items[0].item), "user_message");
        assert_eq!(page.items.len() + first.items.len(), items.len());
    }

    #[test]
    fn encodes_opaque_cursors_with_stable_source_identity() {
        let cursor = encode_thread_history_cursor(9, "thread-1", "item-3", 3);
        assert!(!cursor.contains('{'));
        assert_eq!(
            decode_thread_history_cursor(&cursor).unwrap(),
            ThreadHistoryCursorPayload {
                v: 1,
                seq: 9,
                st: "thread-1".into(),
                si: "item-3".into(),
                p: 3
            }
        );
    }

    #[test]
    fn rejects_malformed_cursors() {
        assert!(decode_thread_history_cursor("not-valid").is_err());
        assert!(decode_thread_history_cursor(&URL_SAFE_NO_PAD.encode("{}")).is_err());
    }

    #[test]
    fn rejects_cursors_longer_than_the_maximum_length_before_decoding() {
        let oversized = "A".repeat(THREAD_HISTORY_CURSOR_MAX_LENGTH + 1);
        assert!(decode_thread_history_cursor(&oversized).is_err());
    }

    fn items_budget(max_items: usize) -> Option<ThreadHistoryPagePolicy> {
        Some(ThreadHistoryPagePolicy {
            max_user_turns: None,
            max_items,
            max_encoded_bytes: 10_000_000,
        })
    }

    #[test]
    fn selects_a_recent_window_by_item_count() {
        let items = rows(120);
        let page = recent(&items, 4, items_budget(10));
        assert_eq!(page.items.len(), 10);
        assert_eq!(page.items[0].source_item_id.as_str(), "item-110");
        assert_eq!(page.items[9].source_item_id.as_str(), "item-119");
        assert!(page.has_more_history);
        let cursor = decode_thread_history_cursor(page.next_cursor.as_deref().unwrap()).unwrap();
        assert_eq!(cursor.si, "item-110");
        assert_eq!(cursor.seq, 4);
    }

    #[test]
    fn always_includes_at_least_one_pathological_oversized_item() {
        let items = vec![make_row(0, Some(2_000_000)), make_row(1, Some(10))];
        let policy = Some(ThreadHistoryPagePolicy {
            max_user_turns: None,
            max_items: 50,
            max_encoded_bytes: 1_024,
        });
        let page = recent(&items, 1, policy);
        assert_eq!(ids(&page.items), ["item-1"]);
        assert!(page.has_more_history);
        let before = older(&items, page.next_cursor.as_deref().unwrap(), 1, policy);
        assert_eq!(ids(&before.items), ["item-0"]);
        assert!(!before.has_more_history);
        assert!(before.next_cursor.is_none());
    }

    #[test]
    fn recovers_identity_miss_cursors_when_position_is_past_a_shrunken_timeline() {
        let items = rows(5);
        let cursor = encode_thread_history_cursor(3, "thread-1", "item-deleted-long-ago", 40);
        let page = older(&items, &cursor, 3, items_budget(2));
        // Clamp exclusive end to items.length and page backward so the client can
        // dedupe and advance rather than permanently failing on a dead cursor.
        assert_eq!(ids(&page.items), ["item-3", "item-4"]);
        assert!(page.has_more_history);
        assert!(page.next_cursor.is_some());
    }

    #[test]
    fn keeps_cursor_resolution_stable_when_newer_items_append() {
        let initial = rows(20);
        let page = recent(&initial, 2, items_budget(5));
        let cursor = page.next_cursor.unwrap();
        let grown = rows(22);
        let before = older(&grown, &cursor, 5, items_budget(5));
        assert_eq!(
            ids(&before.items),
            ["item-10", "item-11", "item-12", "item-13", "item-14"]
        );
        assert!(before.has_more_history);
    }

    #[test]
    fn exposes_conservative_default_page_budgets() {
        assert_eq!(THREAD_HISTORY_PAGE_POLICY.max_user_turns, Some(10));
        assert_eq!(THREAD_HISTORY_PAGE_POLICY.max_items, 75);
        assert_eq!(THREAD_HISTORY_PAGE_POLICY.max_encoded_bytes, 1_048_576);
        assert_eq!(THREAD_HISTORY_SNAPSHOT_ROW_LIMIT, 77);
    }

    #[test]
    fn a_search_jump_widens_the_page_up_to_twice_the_older_turn_limit() {
        // 45 user turns, each followed by one command row.
        let items: Vec<_> = (0..45u64)
            .flat_map(|turn| {
                [
                    prompt_row(turn * 2, "user", "p"),
                    make_row(turn * 2 + 1, None),
                ]
            })
            .collect();
        let cursor = encode_thread_history_cursor(1, "thread-1", "item-89", 89);
        let plain = select_history_page_from_cursor(
            &items,
            &cursor,
            1,
            None,
            &projected_row_encoded_bytes,
            None,
        )
        .unwrap();
        let user_turns = |page: &SelectTimelinePageResult<OrchestrationV2ProjectedTurnItem>| {
            page.items
                .iter()
                .filter(|row| is_thread_history_user_turn(&row.item))
                .count()
        };
        assert_eq!(user_turns(&plain), OLDER_THREAD_USER_TURN_LIMIT);
        // The first prompt message is 44 turns before the anchor: capped at 2x.
        let jump = select_history_page_from_cursor(
            &items,
            &cursor,
            1,
            None,
            &projected_row_encoded_bytes,
            Some("prompt-0"),
        )
        .unwrap();
        assert_eq!(user_turns(&jump), OLDER_THREAD_USER_TURN_LIMIT * 2);
    }
}
