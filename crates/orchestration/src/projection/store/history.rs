//! Port of the timeline reads in `ProjectionStore.ts`: `readTimelineIndex`,
//! `getThreadHistoryPage`, `getTimelinePage`, `getMessageCount`,
//! `getNextTurnItemOrdinal`, and `getThreadAttachmentIds`.
//!
//! The index loads light rows (type, run, node, intent, actor, message id) and
//! decodes full payloads only for rows a page returns, as upstream does. Wire
//! projection (`projectTurnItemForWire`) is cut, so page items are returned as
//! stored.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2ForkSource, OrchestrationV2ProjectedTurnItem,
    OrchestrationV2ProjectedTurnItemVisibility, OrchestrationV2ThreadHistoryPage,
    OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
    OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::{
    MessageId, NodeId, NonNegativeInt, RunId, ThreadId, TrimmedNonEmptyString, TurnItemId,
};

use crate::projection::error::{ProjectionStoreError, ReadFailure};
use crate::projection::shared::timeline::{
    TimelineTurnItem, TurnItemVisibility, create_orchestration_v2_turn_item_visibility,
    is_orchestration_v2_superseded_interrupt, timeline_attempts, timeline_runs,
};
use crate::projection::sql::{query_map, query_payload};
use crate::projection::store::canonical::{
    ProjectionRecordField, is_turn_item_at_or_before_run, make_fork_marker_turn_item,
    read_canonical_projection,
};
use crate::projection::thread_history_paging::{
    HistoryItem, HistoryRow, OLDER_THREAD_USER_TURN_LIMIT, THREAD_HISTORY_PAGE_POLICY,
    ThreadHistoryPagePolicy, is_conversation_history_item, is_thread_history_user_turn,
    projected_row_encoded_bytes, select_history_page_from_cursor, select_recent_timeline_window,
};
use crate::projection::turn_item;

/// The light row a timeline index holds for each turn item.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexItem {
    pub id: TurnItemId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub type_tag: String,
    pub input_intent: Option<OrchestrationV2UserMessageInputIntent>,
    pub created_by: Option<OrchestrationV2Actor>,
    pub message_id: Option<MessageId>,
}

impl IndexItem {
    /// Same fields the light SQL selects, read from a full item.
    fn from_turn_item(item: &OrchestrationV2TurnItem) -> Result<Self, ReadFailure> {
        let common = turn_item::common(item);
        let type_tag = turn_item::type_tag(item).to_owned();
        let value = serde_json::to_value(item)?;
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        let user_message = type_tag == "user_message";
        let has_message_id = user_message || type_tag == "assistant_message";
        Ok(Self {
            id: common.id.clone(),
            thread_id: common.thread_id.clone(),
            run_id: common.run_id.cloned(),
            node_id: common.node_id.cloned(),
            input_intent: if user_message {
                text("inputIntent")
                    .map(|text| serde_json::from_value(json!(text)))
                    .transpose()?
            } else {
                None
            },
            created_by: if user_message {
                text("createdBy")
                    .map(|text| serde_json::from_value(json!(text)))
                    .transpose()?
            } else {
                None
            },
            message_id: if has_message_id {
                text("messageId")
                    .map(|text| serde_json::from_value(json!(text)))
                    .transpose()?
            } else {
                None
            },
            type_tag,
        })
    }
}

impl TimelineTurnItem for IndexItem {
    fn type_tag(&self) -> &str {
        &self.type_tag
    }
    fn run_id(&self) -> Option<&RunId> {
        self.run_id.as_ref()
    }
    fn node_id(&self) -> Option<&NodeId> {
        self.node_id.as_ref()
    }
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent> {
        self.input_intent
    }
}

impl HistoryItem for IndexItem {
    fn type_tag(&self) -> &str {
        &self.type_tag
    }
    fn id(&self) -> Option<&str> {
        Some(self.id.as_str())
    }
    fn message_id(&self) -> Option<&str> {
        self.message_id.as_ref().map(|id| id.as_str())
    }
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent> {
        self.input_intent
    }
    fn created_by(&self) -> Option<OrchestrationV2Actor> {
        self.created_by
    }
    fn run_id(&self) -> Option<&str> {
        self.run_id.as_ref().map(|id| id.as_str())
    }
}

/// One index row: where the item lives, how it is visible, and (for the fork
/// marker) the synthetic item itself, since no payload row backs it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IndexRow {
    pub position: u64,
    pub visibility: OrchestrationV2ProjectedTurnItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    pub item: IndexItem,
    pub synthetic: Option<OrchestrationV2TurnItem>,
}

impl HistoryRow for IndexRow {
    type Item = IndexItem;
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
    fn item(&self) -> &IndexItem {
        &self.item
    }
}

/// `readTimelineIndex`: the thread's run and attempt records, its local rows,
/// and the rows visible to a reader (fork-inherited rows, then the fork marker,
/// then the local visible rows).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TimelineIndex {
    pub records: OrchestrationV2ThreadProjection,
    pub local: Vec<IndexRow>,
    pub visible: Vec<IndexRow>,
}

pub(crate) fn read_timeline_index(
    conn: &Connection,
    thread_id: &ThreadId,
    seen: &[ThreadId],
) -> Result<TimelineIndex, ReadFailure> {
    let records = read_canonical_projection(
        conn,
        thread_id,
        None,
        Some(&[ProjectionRecordField::Runs, ProjectionRecordField::Attempts]),
        None,
    )?;
    let items = query_map(
        conn,
        "SELECT turn_item_id, run_id, node_id, type,
           CASE WHEN type = 'user_message' THEN json_extract(payload_json, '$.inputIntent') END AS input_intent,
           CASE WHEN type = 'user_message' THEN json_extract(payload_json, '$.createdBy') END AS created_by,
           CASE WHEN type IN ('user_message', 'assistant_message') THEN json_extract(payload_json, '$.messageId') END AS message_id
         FROM orchestration_v2_projection_turn_items WHERE thread_id = ?1
         ORDER BY ordinal ASC, turn_item_id ASC",
        [thread_id.as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        },
    )?
    .into_iter()
    .map(|(id, run_id, node_id, type_tag, intent, created_by, message_id)| {
        Ok(IndexItem {
            id: TurnItemId::new(&id).map_err(|error| ReadFailure::Cause(Box::new(error)))?,
            thread_id: thread_id.clone(),
            run_id: run_id.map(|id| RunId::new(&id)).transpose().map_err(|error| ReadFailure::Cause(Box::new(error)))?,
            node_id: node_id.map(|id| NodeId::new(&id)).transpose().map_err(|error| ReadFailure::Cause(Box::new(error)))?,
            type_tag,
            input_intent: intent.map(|text| serde_json::from_value(json!(text))).transpose()?,
            created_by: created_by.map(|text| serde_json::from_value(json!(text))).transpose()?,
            message_id: message_id.map(|id| MessageId::new(&id)).transpose().map_err(|error| ReadFailure::Cause(Box::new(error)))?,
        })
    })
    .collect::<Result<Vec<IndexItem>, ReadFailure>>()?;

    let local: Vec<IndexRow> = items
        .iter()
        .map(|item| IndexRow {
            position: 0,
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: thread_id.clone(),
            source_item_id: item.id.clone(),
            item: item.clone(),
            synthetic: None,
        })
        .collect();
    let visibility: TurnItemVisibility = create_orchestration_v2_turn_item_visibility(
        &timeline_runs(&records.runs),
        &timeline_attempts(&records.attempts),
        items.iter(),
    );
    let visible_local: Vec<IndexRow> = local
        .iter()
        .filter(|row| visibility.is_visible(&row.item))
        .cloned()
        .collect();

    let Some(OrchestrationV2ForkSource::Run {
        thread_id: source_thread_id,
        run_id: source_run_id,
    }) = records.thread.forked_from.clone()
    else {
        return Ok(TimelineIndex {
            records,
            local,
            visible: visible_local,
        });
    };
    if seen.contains(&source_thread_id) {
        return Ok(TimelineIndex {
            records,
            local,
            visible: visible_local,
        });
    }

    let mut next_seen = seen.to_vec();
    next_seen.push(thread_id.clone());
    let source = read_timeline_index(conn, &source_thread_id, &next_seen)?;
    let source_run = source
        .records
        .runs
        .iter()
        .find(|run| run.id == source_run_id);
    let mut inherited: Vec<IndexRow> = Vec::new();
    if let Some(source_run) = source_run {
        let ordinals: HashMap<RunId, u64> = source
            .records
            .runs
            .iter()
            .map(|run| (run.id.clone(), run.ordinal.get()))
            .collect();
        let source_attempts = timeline_attempts(&source.records.attempts);
        let source_items: Vec<&IndexItem> = source.local.iter().map(|row| &row.item).collect();
        let history_origin = source.records.thread.history_origin;
        inherited.extend(
            source
                .visible
                .iter()
                .filter(|row| row.item.thread_id != source_thread_id || row.item.type_tag == "fork")
                .cloned(),
        );
        inherited.extend(
            source
                .local
                .iter()
                .filter(|row| {
                    !is_orchestration_v2_superseded_interrupt(
                        &row.item,
                        &source_attempts,
                        source_items.iter().copied(),
                    ) && is_turn_item_at_or_before_run(
                        history_origin,
                        row.item.run_id.as_ref(),
                        &ordinals,
                        source_run.ordinal.get(),
                    )
                })
                .cloned(),
        );
    }
    let inherited = inherited.into_iter().map(|mut row| {
        row.visibility = OrchestrationV2ProjectedTurnItemVisibility::Inherited;
        row
    });

    let marker = make_fork_marker_turn_item(&records.thread, &source_thread_id, &source_run_id);
    let marker_row = IndexRow {
        position: 0,
        visibility: OrchestrationV2ProjectedTurnItemVisibility::Synthetic,
        source_thread_id: source_thread_id.clone(),
        source_item_id: turn_item::common(&marker).id.clone(),
        item: IndexItem::from_turn_item(&marker)?,
        synthetic: Some(marker),
    };

    let visible = inherited
        .chain(std::iter::once(marker_row))
        .chain(visible_local)
        .collect();
    Ok(TimelineIndex {
        records,
        local,
        visible,
    })
}

/// `getThreadHistoryPage`: one cursor page of visible history, with bodies
/// loaded only for the rows the page returns.
pub(crate) fn thread_history_page(
    conn: &Connection,
    thread_id: &ThreadId,
    cursor: &str,
    through_entry_id: Option<&str>,
    conversation_only: bool,
) -> Result<OrchestrationV2ThreadHistoryPage, ReadFailure> {
    let index = read_timeline_index(conn, thread_id, &[])?;
    if index.records.thread.deleted_at.is_some() {
        return Err(ReadFailure::Store(ProjectionStoreError::ThreadNotFound {
            thread_id: thread_id.clone(),
        }));
    }
    let snapshot_sequence: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sequence), 0) FROM orchestration_events
         WHERE application_event_version = 2 AND aggregate_kind = 'thread' AND stream_id = ?1",
        [thread_id.as_str()],
        |row| row.get(0),
    )?;
    let snapshot_sequence = u64::try_from(snapshot_sequence).unwrap_or(0);

    // Choose complete turns from the light index before loading any bodies.
    let positioned: Vec<IndexRow> = index
        .visible
        .iter()
        .enumerate()
        .map(|(position, row)| {
            let mut row = row.clone();
            row.position = position as u64;
            row
        })
        .collect();
    let page = select_history_page_from_cursor(
        &positioned,
        cursor,
        snapshot_sequence,
        None,
        &|_: &IndexRow| 0,
        through_entry_id,
    )
    .map_err(|error| ReadFailure::Cause(Box::new(error)))?;

    let end = page.items.last().map_or(0, |row| row.position as usize + 1);
    let has_user_turns = index.visible[..end.min(index.visible.len())]
        .iter()
        .any(|row| is_thread_history_user_turn(&row.item));
    let materialized: Vec<IndexRow> = if conversation_only && has_user_turns {
        page.items
            .iter()
            .filter(|row| is_conversation_history_item(&row.item.type_tag))
            .cloned()
            .collect()
    } else {
        page.items.clone()
    };

    let sources = serde_json::to_string(
        &materialized
            .iter()
            .filter(|row| row.synthetic.is_none())
            .map(|row| json!({ "threadId": row.source_thread_id, "id": row.source_item_id }))
            .collect::<Vec<_>>(),
    )?;
    let payloads = query_map(
        conn,
        "SELECT item.thread_id, item.turn_item_id, item.payload_json
         FROM orchestration_v2_projection_turn_items item JOIN json_each(?1) wanted
           ON item.thread_id = json_extract(wanted.value, '$.threadId')
          AND item.turn_item_id = json_extract(wanted.value, '$.id')",
        [sources],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    let by_identity: HashMap<(String, String), String> = payloads
        .into_iter()
        .map(|(thread, item, payload)| ((thread, item), payload))
        .collect();

    let mut items: Vec<OrchestrationV2ProjectedTurnItem> = Vec::with_capacity(materialized.len());
    for row in &materialized {
        let item = match &row.synthetic {
            Some(item) => item.clone(),
            None => {
                let payload = by_identity
                    .get(&(
                        row.source_thread_id.as_str().to_owned(),
                        row.source_item_id.as_str().to_owned(),
                    ))
                    .ok_or_else(|| {
                        ReadFailure::from("Timeline item disappeared during snapshot")
                    })?;
                serde_json::from_str(payload)?
            }
        };
        items.push(OrchestrationV2ProjectedTurnItem {
            position: non_negative(row.position)?,
            visibility: row.visibility,
            source_thread_id: row.source_thread_id.clone(),
            source_item_id: row.source_item_id.clone(),
            item,
        });
    }

    if has_user_turns {
        let items = items
            .into_iter()
            .enumerate()
            .map(|(position, mut row)| {
                row.position = non_negative(position as u64)?;
                Ok(row)
            })
            .collect::<Result<Vec<_>, ReadFailure>>()?;
        return Ok(OrchestrationV2ThreadHistoryPage {
            snapshot_sequence: non_negative(snapshot_sequence)?,
            items,
            next_cursor: optional_cursor(page.next_cursor)?,
            has_more_history: page.has_more_history,
        });
    }

    // The no-turn fallback also respects the encoded byte budget after projection.
    let older_policy = ThreadHistoryPagePolicy {
        max_user_turns: Some(OLDER_THREAD_USER_TURN_LIMIT),
        max_items: THREAD_HISTORY_PAGE_POLICY.max_items
            * if through_entry_id.is_none() { 1 } else { 2 },
        ..THREAD_HISTORY_PAGE_POLICY
    };
    let bounded = select_recent_timeline_window(
        &items,
        snapshot_sequence,
        Some(older_policy),
        &projected_row_encoded_bytes,
    );
    let kept: Vec<OrchestrationV2ProjectedTurnItem> = if conversation_only {
        bounded
            .items
            .into_iter()
            .filter(|row| is_conversation_history_item(turn_item::type_tag(&row.item)))
            .collect()
    } else {
        bounded.items
    };
    Ok(OrchestrationV2ThreadHistoryPage {
        snapshot_sequence: non_negative(snapshot_sequence)?,
        items: kept,
        next_cursor: optional_cursor(if bounded.has_more_history {
            bounded.next_cursor
        } else {
            page.next_cursor
        })?,
        has_more_history: bounded.has_more_history || page.has_more_history,
    })
}

/// `ProjectionTimelinePageOptions.view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineView {
    /// User, assistant, and proposed-plan rows.
    #[default]
    Messages,
    /// Every visible row.
    Activity,
}

/// `ProjectionTimelinePageOptions`.
#[derive(Debug, Clone, Default)]
pub struct TimelinePageOptions {
    /// Rows strictly after this position; `None` starts at the beginning.
    pub after_position: Option<u64>,
    /// When set, return only this item and ignore position and view.
    pub item_id: Option<TurnItemId>,
    pub view: TimelineView,
    pub limit: usize,
}

/// `ProjectionTimelinePage`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionTimelinePage {
    pub items: Vec<OrchestrationV2ProjectedTurnItem>,
    pub total_items: usize,
    pub has_more: bool,
}

/// `getTimelinePage`.
pub(crate) fn timeline_page(
    conn: &Connection,
    thread_id: &ThreadId,
    options: &TimelinePageOptions,
) -> Result<ProjectionTimelinePage, ReadFailure> {
    let index = read_timeline_index(conn, thread_id, &[])?;
    let matching: Vec<(u64, &IndexRow)> = index
        .visible
        .iter()
        .enumerate()
        .map(|(position, row)| (position as u64, row))
        .filter(|(position, row)| match &options.item_id {
            Some(item_id) => row.source_item_id == *item_id,
            None => {
                options.after_position.is_none_or(|after| *position > after)
                    && (options.view == TimelineView::Activity
                        || matches!(
                            row.item.type_tag.as_str(),
                            "user_message" | "assistant_message" | "proposed_plan"
                        ))
            }
        })
        .collect();
    let page: Vec<(u64, &IndexRow)> = matching.iter().take(options.limit).copied().collect();
    let has_more = matching.len() > page.len();

    let mut items = Vec::with_capacity(page.len());
    for (position, row) in page {
        let item = match &row.synthetic {
            Some(item) => item.clone(),
            None => query_payload::<OrchestrationV2TurnItem>(
                conn,
                "SELECT payload_json FROM orchestration_v2_projection_turn_items
                     WHERE thread_id = ?1 AND turn_item_id = ?2",
                rusqlite::params![row.source_thread_id.as_str(), row.source_item_id.as_str()],
            )?
            .ok_or_else(|| ReadFailure::from("Timeline item disappeared during snapshot"))?,
        };
        items.push(OrchestrationV2ProjectedTurnItem {
            position: non_negative(position)?,
            visibility: row.visibility,
            source_thread_id: row.source_thread_id.clone(),
            source_item_id: row.source_item_id.clone(),
            item,
        });
    }
    Ok(ProjectionTimelinePage {
        items,
        total_items: index.visible.len(),
        has_more,
    })
}

/// `getMessageCount`.
pub(crate) fn message_count(conn: &Connection, thread_id: &ThreadId) -> Result<u64, ReadFailure> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) AS count FROM orchestration_v2_projection_messages WHERE thread_id = ?1",
        [thread_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// `getNextTurnItemOrdinal`: one past the highest stored ordinal, or 1.
pub(crate) fn next_turn_item_ordinal(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<u64, ReadFailure> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(ordinal) AS ordinal FROM orchestration_v2_projection_turn_items WHERE thread_id = ?1",
        [thread_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(max.unwrap_or(0) + 1).unwrap_or(1))
}

/// `getThreadAttachmentIds`, message half: attachment ids on the thread's
/// messages. The render-derived ids (`htmlRender` and `t3McpApp` tool outputs)
/// need the shared tool-output parsers and are not ported yet; see PORTING.md.
pub(crate) fn message_attachment_ids(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<Vec<String>, ReadFailure> {
    let ids: Vec<Option<String>> = query_map(
        conn,
        "SELECT DISTINCT json_extract(attachment.value, '$.id') AS id
         FROM orchestration_v2_projection_messages AS message,
           json_each(message.payload_json, '$.attachments') AS attachment
         WHERE message.thread_id = ?1",
        [thread_id.as_str()],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let mut seen = HashSet::new();
    Ok(ids
        .into_iter()
        .flatten()
        .filter(|id| seen.insert(id.clone()))
        .collect())
}

fn non_negative(value: u64) -> Result<NonNegativeInt, ReadFailure> {
    Ok(value)
}

fn optional_cursor(cursor: Option<String>) -> Result<Option<TrimmedNonEmptyString>, ReadFailure> {
    cursor
        .map(|cursor| {
            TrimmedNonEmptyString::new(&cursor).map_err(|error| ReadFailure::Cause(Box::new(error)))
        })
        .transpose()
}
