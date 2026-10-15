//! Port of upstream/apps/server/src/persistence/OrchestrationEventStore.ts
//!
//! Payloads are `serde_json::Value`. Known event types are the upstream
//! discriminants; a known type still has to be JSON, and an empty object is
//! rejected until the typed contracts can validate the payload. Unknown types
//! fail a strict read and are dropped when `skip_unknown_event_types` is set.
//!
//! `publish_committed` is in-memory and must run after `Db::write` returns.

use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params_from_iter};
use tokio::sync::mpsc;

use turin_contracts::{CommandId, EventId, NodeId, RawEventId, RunId, ThreadId};

use crate::db::{AbortOnDrop, Db};
use crate::errors::{PersistenceDecodeError, PersistenceError, PersistenceSqlError};
use crate::sql_value::SqlValue;

const READ_PAGE_SIZE: i64 = 500;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const PROMPT_HINT_CAP: usize = 4096;
pub const LIVE_STREAM_MAX_ITEMS: usize = 1_000;
const LIVE_STREAM_MAX_BYTES: usize = 8 * 1024 * 1024;

const RESTORED_PAYLOAD: &str = "
CASE
  WHEN orchestration_events.event_type IN ('subagent.updated', 'turn-item.updated')
    AND json_type(orchestration_events.payload_json, '$.promptSequence') = 'integer'
  THEN json_set(
    json_remove(orchestration_events.payload_json, '$.promptSequence'),
    '$.prompt',
    (SELECT json_extract(anchor.payload_json, '$.prompt')
     FROM orchestration_events AS anchor
     WHERE anchor.sequence = json_extract(orchestration_events.payload_json, '$.promptSequence'))
  )
  ELSE orchestration_events.payload_json
END
";

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEvent {
    pub sequence: i64,
    pub event_id: EventId,
    pub event_type: String,
    pub aggregate_kind: String,
    /// Upstream `ProjectId` for project rows. Left as text so a non-project aggregate can round-trip.
    pub aggregate_id: String,
    pub occurred_at: String,
    pub command_id: Option<CommandId>,
    pub causation_event_id: Option<EventId>,
    pub correlation_id: Option<CommandId>,
    pub payload: serde_json::Value,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEventInput {
    pub event_id: EventId,
    pub aggregate_id: String,
    pub event_type: String,
    pub occurred_at: String,
    pub command_id: Option<CommandId>,
    pub causation_event_id: Option<EventId>,
    pub correlation_id: Option<CommandId>,
    pub payload: serde_json::Value,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub event_type: String,
    pub occurred_at: String,
    pub payload: serde_json::Value,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub driver: Option<String>,
    pub provider_instance_id: Option<String>,
    pub raw_event_id: Option<RawEventId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAgentEvent {
    pub sequence: i64,
    pub command_id: Option<CommandId>,
    pub event: AgentEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationStoredEvent {
    Project(ProjectEvent),
    Agent(StoredAgentEvent),
}

impl ApplicationStoredEvent {
    pub fn sequence(&self) -> i64 {
        match self {
            Self::Project(event) => event.sequence,
            Self::Agent(event) => event.sequence,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppendAgentEvents {
    pub command_id: Option<CommandId>,
    pub events: Vec<AgentEvent>,
}

#[derive(Debug, Clone)]
pub struct ReadAgentEvents {
    pub after_sequence: Option<i64>,
    pub through_sequence: Option<i64>,
    pub thread_id: Option<ThreadId>,
    pub command_id: Option<CommandId>,
    pub event_type: Option<String>,
    pub limit: Option<u64>,
    pub skip_unknown_event_types: bool,
}

#[derive(Debug, Clone)]
pub struct ReadApplicationEvents {
    pub after_sequence: i64,
    pub through_sequence: i64,
    pub skip_unknown_event_types: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayStats {
    pub event_count: i64,
    pub raw_payload_bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentReplayStats {
    pub event_count: i64,
    pub raw_payload_bytes: i64,
    pub has_create_event: bool,
}

#[derive(Clone, Default)]
struct PromptHints {
    order: VecDeque<String>,
    values: HashMap<String, i64>,
}

impl PromptHints {
    fn get(&self, key: &str) -> Option<i64> {
        self.values.get(key).copied()
    }

    fn remember(&mut self, key: String, sequence: i64) {
        if self.values.contains_key(&key) {
            self.order.retain(|existing| existing != &key);
        }
        self.order.push_back(key.clone());
        self.values.insert(key, sequence);
        while self.values.len() > PROMPT_HINT_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }
}

struct EventRow {
    sequence: i64,
    event_id: String,
    command_id: Option<String>,
    aggregate_kind: String,
    stream_id: String,
    event_type: String,
    occurred_at: String,
    payload_json: String,
    metadata_json: String,
    causation_event_id: Option<String>,
    correlation_id: Option<String>,
}

#[derive(Clone)]
pub struct OrchestrationEventStore {
    db: Db,
    hints: Arc<Mutex<PromptHints>>,
    hub: Arc<LiveHub>,
}

impl OrchestrationEventStore {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            hints: Arc::new(Mutex::new(PromptHints::default())),
            hub: Arc::new(LiveHub::default()),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub async fn append_project_event(
        &self,
        event: ProjectEventInput,
    ) -> Result<ProjectEvent, PersistenceError> {
        self.db
            .write(move |tx| append_project_event(tx, &event))
            .await
    }

    pub async fn append_agent_events(
        &self,
        input: AppendAgentEvents,
    ) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        let hints = Arc::clone(&self.hints);
        self.db
            .write(move |tx| append_agent_events(tx, &hints, &input))
            .await
    }

    /// Append inside an existing writer transaction. Used when the caller rolls
    /// the transaction back after the hint map has been updated.
    pub fn append_agent_events_in(
        &self,
        conn: &Connection,
        input: &AppendAgentEvents,
    ) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        append_agent_events(conn, &self.hints, input)
    }

    pub async fn read_agent_events(
        &self,
        query: ReadAgentEvents,
    ) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
        self.db
            .read(move |conn| read_agent_events(conn, &query))
            .await
    }

    pub async fn read_application_events(
        &self,
        query: ReadApplicationEvents,
    ) -> Result<Vec<ApplicationStoredEvent>, PersistenceError> {
        self.db
            .read(move |conn| read_application_events(conn, &query))
            .await
    }

    pub async fn latest_agent_sequence(
        &self,
        thread_id: Option<ThreadId>,
    ) -> Result<i64, PersistenceError> {
        self.db
            .read(move |conn| latest_agent_sequence(conn, thread_id.as_ref().map(ThreadId::as_str)))
            .await
    }

    pub async fn latest_application_sequence(&self) -> Result<i64, PersistenceError> {
        self.db.read(latest_application_sequence).await
    }

    pub async fn get_agent_replay_stats(
        &self,
        thread_id: String,
        after_sequence: i64,
        through_sequence: i64,
        max_events: u64,
    ) -> Result<AgentReplayStats, PersistenceError> {
        self.db
            .read(move |conn| {
                agent_replay_stats(
                    conn,
                    &thread_id,
                    after_sequence,
                    through_sequence,
                    max_events,
                )
            })
            .await
    }

    pub async fn get_replay_stats(
        &self,
        after_sequence: i64,
        through_sequence: i64,
    ) -> Result<ReplayStats, PersistenceError> {
        self.db
            .read(move |conn| replay_stats(conn, after_sequence, through_sequence))
            .await
    }

    /// In-memory only. Call after the write that stored `events` has committed.
    pub fn publish_committed(&self, events: Vec<ApplicationStoredEvent>) {
        self.hub.publish(events);
    }

    #[cfg(test)]
    pub fn live_subscriber_count(&self) -> usize {
        self.hub
            .subs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }

    pub fn stream_application_events(&self, after_sequence: i64) -> EventSubscription {
        let (tx, rx) = mpsc::channel(1);
        let db = self.db.clone();
        let hub = Arc::clone(&self.hub);
        let sub = hub.subscribe();
        let unregister = Unregister {
            id: sub.id,
            hub: Arc::clone(&hub),
        };
        #[cfg(test)]
        let replay_pages = Arc::new(ReplayPages::default());
        #[cfg(test)]
        let replay_pages_task = Arc::clone(&replay_pages);
        let task = AbortOnDrop::new(tokio::spawn(async move {
            if let Err(err) = drive_stream(
                db,
                sub,
                after_sequence,
                tx.clone(),
                #[cfg(test)]
                replay_pages_task,
            )
            .await
            {
                let _ = tx.send(Err(err)).await;
            }
        }));
        EventSubscription {
            rx,
            _unregister: unregister,
            _task: task,
            #[cfg(test)]
            replay_pages,
        }
    }
}

pub struct EventSubscription {
    pub rx: mpsc::Receiver<Result<ApplicationStoredEvent, PersistenceError>>,
    /// Dropped before the drive task so a closed stream leaves the hub immediately.
    _unregister: Unregister,
    _task: AbortOnDrop<()>,
    #[cfg(test)]
    replay_pages: Arc<ReplayPages>,
}

#[cfg(test)]
impl EventSubscription {
    /// `(max pages alive at once, max events in one page, page fetches)`.
    pub fn replay_page_stats(&self) -> (usize, usize, usize) {
        (
            self.replay_pages.max_live.load(Ordering::SeqCst),
            self.replay_pages.max_len.load(Ordering::SeqCst),
            self.replay_pages.fetches.load(Ordering::SeqCst),
        )
    }
}

#[cfg(test)]
#[derive(Default)]
struct ReplayPages {
    live: AtomicUsize,
    max_live: AtomicUsize,
    max_len: AtomicUsize,
    fetches: AtomicUsize,
}

#[cfg(test)]
struct ReplayPageGuard<'a> {
    pages: &'a ReplayPages,
}

#[cfg(test)]
impl ReplayPages {
    fn enter(&self, len: usize) -> ReplayPageGuard<'_> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.max_len.fetch_max(len, Ordering::SeqCst);
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_live.fetch_max(live, Ordering::SeqCst);
        ReplayPageGuard { pages: self }
    }
}

#[cfg(test)]
impl Drop for ReplayPageGuard<'_> {
    fn drop(&mut self) {
        self.pages.live.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Unregister {
    id: u64,
    hub: Arc<LiveHub>,
}

impl Drop for Unregister {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

async fn drive_stream(
    db: Db,
    sub: LiveSub,
    after_sequence: i64,
    tx: mpsc::Sender<Result<ApplicationStoredEvent, PersistenceError>>,
    #[cfg(test)] replay_pages: Arc<ReplayPages>,
) -> Result<(), PersistenceError> {
    let high = db.read(latest_application_sequence).await?;
    sub.fail_if_over_budget()?;
    let mut cursor = after_sequence;
    loop {
        let page_after = cursor;
        let page = db
            .read(move |conn| read_application_page(conn, page_after, high, false))
            .await?;
        let Some(last) = page.last_sequence else {
            break;
        };
        let done = page.raw_len < READ_PAGE_SIZE || last >= high;
        {
            #[cfg(test)]
            let _guard = replay_pages.enter(page.events.len());
            for event in page.events {
                tx.send(Ok(event))
                    .await
                    .map_err(|_| PersistenceError::Closed)?;
            }
        }
        if done {
            break;
        }
        cursor = last;
    }
    loop {
        sub.fail_if_over_budget()?;
        let pending = sub.drain_after(high);
        if pending.is_empty() {
            sub.wait().await;
            continue;
        }
        for event in pending {
            tx.send(Ok(event))
                .await
                .map_err(|_| PersistenceError::Closed)?;
        }
    }
}

pub(crate) fn append_project_event(
    conn: &Connection,
    event: &ProjectEventInput,
) -> Result<ProjectEvent, PersistenceError> {
    if !is_known_project_event_type(&event.event_type) {
        return Err(decode_err(
            "OrchestrationEventStore.appendProjectEvent:decode",
            format!("unknown event type {}", event.event_type),
        ));
    }
    let payload_json = serde_json::to_string(&event.payload).map_err(|err| {
        PersistenceError::Decode(PersistenceDecodeError::new(
            "OrchestrationEventStore.appendProjectEvent:decode",
            err.to_string(),
        ))
    })?;
    let metadata_json = serde_json::to_string(&event.metadata).map_err(|err| {
        PersistenceError::Decode(PersistenceDecodeError::new(
            "OrchestrationEventStore.appendProjectEvent:decode",
            err.to_string(),
        ))
    })?;
    let actor = infer_actor_kind(event);
    let mut stmt = conn
        .prepare(
            "
            INSERT INTO orchestration_events (
              event_id, aggregate_kind, stream_id, stream_version, event_type,
              occurred_at, command_id, causation_event_id, correlation_id,
              actor_kind, payload_json, metadata_json, application_event_version
            )
            VALUES (
              ?1, 'project', ?2,
              COALESCE(
                (
                  SELECT stream_version + 1
                  FROM orchestration_events
                  WHERE aggregate_kind = 'project' AND stream_id = ?2
                  ORDER BY stream_version DESC
                  LIMIT 1
                ),
                0
              ),
              ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 2
            )
            RETURNING sequence, event_id, command_id, aggregate_kind, stream_id, event_type,
              occurred_at, payload_json, metadata_json, causation_event_id, correlation_id
            ",
        )
        .map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendProjectEvent:insert", &err)
        })?;
    let mut rows = stmt
        .query(params_from_iter([
            SqlValue::text(&event.event_id),
            SqlValue::text(&event.aggregate_id),
            SqlValue::text(&event.event_type),
            SqlValue::text(&event.occurred_at),
            SqlValue::opt_text(event.command_id.clone()),
            SqlValue::opt_text(event.causation_event_id.clone()),
            SqlValue::opt_text(event.correlation_id.clone()),
            SqlValue::text(actor),
            SqlValue::text(payload_json),
            SqlValue::text(metadata_json),
        ]))
        .map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendProjectEvent:insert", &err)
        })?;
    let row = rows
        .next()
        .map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendProjectEvent:insert", &err)
        })?
        .ok_or_else(|| {
            PersistenceError::sql(
                "OrchestrationEventStore.appendProjectEvent:insert",
                &rusqlite::Error::QueryReturnedNoRows,
            )
        })?;
    row_to_project(
        &map_row(row).map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendProjectEvent:insert", &err)
        })?,
        "OrchestrationEventStore.appendProjectEvent:decode",
    )
}

fn append_agent_events(
    conn: &Connection,
    hints: &Mutex<PromptHints>,
    input: &AppendAgentEvents,
) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
    let mut stored = Vec::with_capacity(input.events.len());
    for event in &input.events {
        if !is_known_v2_event_type(&event.event_type) {
            return Err(decode_err(
                "OrchestrationEventStore.appendAgentEvents:decode",
                format!("unknown event type {}", event.event_type),
            ));
        }
        let payload_json = serde_json::to_string(&event.payload).map_err(|err| {
            PersistenceError::Decode(PersistenceDecodeError::new(
                "OrchestrationEventStore.appendAgentEvents:decode",
                err.to_string(),
            ))
        })?;
        let metadata_json = serde_json::to_string(&metadata_for_v2(event)).map_err(|err| {
            PersistenceError::Decode(PersistenceDecodeError::new(
                "OrchestrationEventStore.appendAgentEvents:decode",
                err.to_string(),
            ))
        })?;
        let prompt = subagent_prompt(event);
        let hint = prompt
            .as_ref()
            .and_then(|(key, _)| hints.lock().ok().and_then(|guard| guard.get(key)));
        let actor = if event.raw_event_id.is_none() {
            "server"
        } else {
            "provider"
        };
        let (sql, params) = agent_insert_sql(
            event,
            input.command_id.as_ref().map(CommandId::as_str),
            actor,
            &payload_json,
            &metadata_json,
            hint,
            prompt.as_ref().map(|(_, text)| text.as_str()),
        );
        let mut stmt = conn.prepare(&sql).map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendAgentEvents:insert", &err)
        })?;
        let mut rows = stmt.query(params_from_iter(params)).map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendAgentEvents:insert", &err)
        })?;
        let row = rows
            .next()
            .map_err(|err| {
                PersistenceError::sql("OrchestrationEventStore.appendAgentEvents:insert", &err)
            })?
            .ok_or_else(|| {
                PersistenceError::sql(
                    "OrchestrationEventStore.appendAgentEvents:insert",
                    &rusqlite::Error::QueryReturnedNoRows,
                )
            })?;
        let sequence: i64 = row.get(0).map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.appendAgentEvents:insert", &err)
        })?;
        let stores_full = if hint.is_some() {
            row.get::<_, i64>(1).unwrap_or(0) == 1
        } else {
            true
        };
        if let Some((key, _)) = prompt
            && let Ok(mut guard) = hints.lock()
        {
            let remembered = if hint.is_none() || stores_full {
                sequence
            } else {
                hint.unwrap_or(sequence)
            };
            guard.remember(key, remembered);
        }
        stored.push(StoredAgentEvent {
            sequence,
            command_id: input.command_id.clone(),
            event: event.clone(),
        });
    }
    Ok(stored)
}

fn agent_insert_sql(
    event: &AgentEvent,
    command_id: Option<&str>,
    actor: &str,
    payload_json: &str,
    metadata_json: &str,
    hint: Option<i64>,
    prompt: Option<&str>,
) -> (String, Vec<SqlValue>) {
    let mut params = vec![
        SqlValue::text(&event.id),
        SqlValue::text(&event.thread_id),
        SqlValue::text(&event.event_type),
        SqlValue::text(&event.occurred_at),
        SqlValue::opt_text(command_id.map(str::to_owned)),
        SqlValue::text(actor),
        SqlValue::text(payload_json),
    ];
    let (payload_expr, metadata_param, returning) = if let Some(hint) = hint {
        params.push(SqlValue::Integer(hint));
        params.push(SqlValue::text(prompt.unwrap_or("")));
        params.push(SqlValue::text(metadata_json));
        (
            "CASE WHEN EXISTS (
                SELECT 1 FROM orchestration_events
                WHERE sequence = ?8
                  AND json_type(payload_json, '$.promptSequence') IS NULL
                  AND json_extract(payload_json, '$.prompt') = ?9
              ) THEN json_set(?7, '$.prompt', '', '$.promptSequence', CAST(?8 AS INTEGER))
              ELSE ?7 END",
            "?10",
            "RETURNING sequence, json_type(payload_json, '$.promptSequence') IS NULL AS stores_full_prompt",
        )
    } else {
        params.push(SqlValue::text(metadata_json));
        ("?7", "?8", "RETURNING sequence")
    };
    let sql = format!(
        "
        INSERT INTO orchestration_events (
          event_id, aggregate_kind, stream_id, stream_version, event_type,
          occurred_at, command_id, causation_event_id, correlation_id,
          actor_kind, payload_json, metadata_json, application_event_version
        )
        VALUES (
          ?1, 'thread', ?2,
          COALESCE(
            (
              SELECT MAX(stream_version) + 1
              FROM orchestration_events
              WHERE aggregate_kind = 'thread' AND stream_id = ?2
            ),
            0
          ),
          ?3, ?4, ?5, NULL, ?5, ?6, {payload_expr}, {metadata_param}, 2
        )
        {returning}
        "
    );
    (sql, params)
}

fn subagent_prompt(event: &AgentEvent) -> Option<(String, String)> {
    let payload = event.payload.as_object()?;
    let is_subagent = event.event_type == "subagent.updated"
        || (event.event_type == "turn-item.updated"
            && payload.get("type").and_then(|v| v.as_str()) == Some("subagent"));
    if !is_subagent {
        return None;
    }
    let id = payload.get("id")?.as_str()?;
    let prompt = payload.get("prompt")?.as_str()?.to_owned();
    Some((format!("{}:{id}", event.event_type), prompt))
}

fn metadata_for_v2(event: &AgentEvent) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    if let Some(run_id) = &event.run_id {
        meta.insert(
            "runId".into(),
            serde_json::Value::String(run_id.as_str().to_owned()),
        );
    }
    if let Some(node_id) = &event.node_id {
        meta.insert(
            "nodeId".into(),
            serde_json::Value::String(node_id.as_str().to_owned()),
        );
    }
    if let Some(driver) = &event.driver {
        meta.insert("driver".into(), serde_json::Value::String(driver.clone()));
    }
    if let Some(instance) = &event.provider_instance_id {
        meta.insert(
            "providerInstanceId".into(),
            serde_json::Value::String(instance.clone()),
        );
    }
    if let Some(raw) = &event.raw_event_id {
        meta.insert(
            "rawEventId".into(),
            serde_json::Value::String(raw.as_str().to_owned()),
        );
    }
    serde_json::Value::Object(meta)
}

fn infer_actor_kind(event: &ProjectEventInput) -> &'static str {
    if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("provider:"))
    {
        return "provider";
    }
    if event
        .command_id
        .as_ref()
        .is_some_and(|id| id.as_str().starts_with("server:"))
    {
        return "server";
    }
    if let Some(metadata) = event.metadata.as_object()
        && (metadata.contains_key("providerTurnId")
            || metadata.contains_key("providerItemId")
            || metadata.contains_key("adapterKey"))
    {
        return "provider";
    }
    if event.command_id.is_none() {
        "server"
    } else {
        "client"
    }
}

pub fn read_agent_events(
    conn: &Connection,
    query: &ReadAgentEvents,
) -> Result<Vec<StoredAgentEvent>, PersistenceError> {
    let total = query
        .limit
        .map(|limit| i64::try_from(limit).unwrap_or(i64::MAX).max(0))
        .unwrap_or(MAX_SAFE_INTEGER);
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut cursor = query.after_sequence.unwrap_or(0);
    let mut remaining = total;
    let mut out = Vec::new();
    loop {
        let page_limit = remaining.min(READ_PAGE_SIZE);
        let rows = query_rows(
            conn,
            "OrchestrationEventStore.readAgentEvents:query",
            RowFilter {
                after: cursor,
                through: query.through_sequence,
                thread_id: query.thread_id.as_ref().map(ThreadId::as_str),
                command_id: query.command_id.as_ref().map(CommandId::as_str),
                event_type: query.event_type.as_deref(),
                only_agent: true,
                limit: page_limit,
            },
        )?;
        let n = rows.len() as i64;
        let last = rows.last().map(|row| row.sequence);
        for row in rows {
            if keep_row(
                &row,
                query.skip_unknown_event_types,
                "OrchestrationEventStore.readAgentEvents:decode",
            )? {
                out.push(row_to_agent(
                    &row,
                    "OrchestrationEventStore.readAgentEvents:decode",
                )?);
            }
        }
        let Some(last) = last else { break };
        remaining -= n;
        if n < page_limit || remaining <= 0 || query.through_sequence.is_some_and(|end| last >= end)
        {
            break;
        }
        cursor = last;
    }
    Ok(out)
}

struct ApplicationPage {
    events: Vec<ApplicationStoredEvent>,
    last_sequence: Option<i64>,
    raw_len: i64,
}

fn read_application_page(
    conn: &Connection,
    after_sequence: i64,
    through_sequence: i64,
    skip_unknown_event_types: bool,
) -> Result<ApplicationPage, PersistenceError> {
    let rows = query_rows(
        conn,
        "OrchestrationEventStore.readApplicationEvents:query",
        RowFilter {
            after: after_sequence,
            through: Some(through_sequence),
            thread_id: None,
            command_id: None,
            event_type: None,
            only_agent: false,
            limit: READ_PAGE_SIZE,
        },
    )?;
    let raw_len = rows.len() as i64;
    let last_sequence = rows.last().map(|row| row.sequence);
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        if keep_row(
            &row,
            skip_unknown_event_types,
            "OrchestrationEventStore.readApplicationEvents:decode",
        )? {
            events.push(row_to_application(
                &row,
                "OrchestrationEventStore.readApplicationEvents:decode",
            )?);
        }
    }
    Ok(ApplicationPage {
        events,
        last_sequence,
        raw_len,
    })
}

pub fn read_application_events(
    conn: &Connection,
    query: &ReadApplicationEvents,
) -> Result<Vec<ApplicationStoredEvent>, PersistenceError> {
    let mut cursor = query.after_sequence;
    let mut out = Vec::new();
    loop {
        let rows = query_rows(
            conn,
            "OrchestrationEventStore.readApplicationEvents:query",
            RowFilter {
                after: cursor,
                through: Some(query.through_sequence),
                thread_id: None,
                command_id: None,
                event_type: None,
                only_agent: false,
                limit: READ_PAGE_SIZE,
            },
        )?;
        let n = rows.len() as i64;
        let last = rows.last().map(|row| row.sequence);
        for row in rows {
            if keep_row(
                &row,
                query.skip_unknown_event_types,
                "OrchestrationEventStore.readApplicationEvents:decode",
            )? {
                out.push(row_to_application(
                    &row,
                    "OrchestrationEventStore.readApplicationEvents:decode",
                )?);
            }
        }
        let Some(last) = last else { break };
        if n < READ_PAGE_SIZE || last >= query.through_sequence {
            break;
        }
        cursor = last;
    }
    Ok(out)
}

struct RowFilter<'a> {
    after: i64,
    through: Option<i64>,
    thread_id: Option<&'a str>,
    command_id: Option<&'a str>,
    event_type: Option<&'a str>,
    only_agent: bool,
    limit: i64,
}

fn query_rows(
    conn: &Connection,
    operation: &'static str,
    filter: RowFilter<'_>,
) -> Result<Vec<EventRow>, PersistenceError> {
    let index = if filter.command_id.is_some() {
        "INDEXED BY idx_orch_events_command_id"
    } else if filter.only_agent {
        ""
    } else {
        "INDEXED BY idx_orchestration_events_application_high_water"
    };
    let kind = if filter.only_agent {
        "application_event_version = 2 AND aggregate_kind = 'thread'"
    } else {
        "aggregate_kind = 'project' OR (application_event_version = 2 AND aggregate_kind = 'thread')"
    };
    let mut clauses = Vec::new();
    let mut params = vec![
        SqlValue::Integer(filter.after),
        SqlValue::Integer(filter.through.unwrap_or(MAX_SAFE_INTEGER)),
    ];
    if let Some(thread_id) = filter.thread_id {
        clauses.push("stream_id = ?");
        params.push(SqlValue::text(thread_id));
    }
    if let Some(command_id) = filter.command_id {
        clauses.push("command_id = ?");
        params.push(SqlValue::text(command_id));
    }
    if let Some(event_type) = filter.event_type {
        clauses.push("event_type = ?");
        params.push(SqlValue::text(event_type));
    }
    let extra = if clauses.is_empty() {
        "1=1".to_owned()
    } else {
        clauses.join(" AND ")
    };
    params.push(SqlValue::Integer(filter.limit));
    let sql = format!(
        "
        SELECT sequence, event_id, command_id, aggregate_kind, stream_id, event_type,
               occurred_at, {RESTORED_PAYLOAD} AS payload_json, metadata_json,
               causation_event_id, correlation_id
        FROM orchestration_events {index}
        WHERE sequence > ? AND sequence <= ?
          AND ({kind})
          AND {extra}
        ORDER BY sequence ASC
        LIMIT ?
        "
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|err| PersistenceError::sql(operation, &err))?;
    let rows = stmt
        .query_map(params_from_iter(params), map_row)
        .map_err(|err| PersistenceError::sql(operation, &err))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| PersistenceError::sql(operation, &err))
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        sequence: row.get(0)?,
        event_id: row.get(1)?,
        command_id: row.get(2)?,
        aggregate_kind: row.get(3)?,
        stream_id: row.get(4)?,
        event_type: row.get(5)?,
        occurred_at: row.get(6)?,
        payload_json: row.get(7)?,
        metadata_json: row.get(8)?,
        causation_event_id: row.get(9)?,
        correlation_id: row.get(10)?,
    })
}

fn keep_row(
    row: &EventRow,
    skip_unknown: bool,
    operation: &'static str,
) -> Result<bool, PersistenceError> {
    let known = if row.aggregate_kind == "project" {
        is_known_project_event_type(&row.event_type)
    } else {
        is_known_v2_event_type(&row.event_type)
    };
    if !known {
        if skip_unknown {
            tracing::warn!(
                aggregate_kind = %row.aggregate_kind,
                sequence = row.sequence,
                event_type_length = row.event_type.len(),
                "Skipping an application event row with an unknown event type"
            );
            return Ok(false);
        }
        return Err(decode_err(
            operation,
            format!("unknown event type {}", row.event_type),
        ));
    }
    Ok(true)
}

fn row_to_application(
    row: &EventRow,
    operation: &'static str,
) -> Result<ApplicationStoredEvent, PersistenceError> {
    if row.aggregate_kind == "project" {
        Ok(ApplicationStoredEvent::Project(row_to_project(row, operation)?))
    } else {
        Ok(ApplicationStoredEvent::Agent(row_to_agent(row, operation)?))
    }
}

fn row_to_project(
    row: &EventRow,
    operation: &'static str,
) -> Result<ProjectEvent, PersistenceError> {
    Ok(ProjectEvent {
        sequence: row.sequence,
        event_id: parse_brand(operation, &row.event_id, new_event_id)?,
        event_type: row.event_type.clone(),
        aggregate_kind: row.aggregate_kind.clone(),
        aggregate_id: row.stream_id.clone(),
        occurred_at: row.occurred_at.clone(),
        command_id: parse_brand_opt(operation, row.command_id.as_deref(), new_command_id)?,
        causation_event_id: parse_brand_opt(
            operation,
            row.causation_event_id.as_deref(),
            new_event_id,
        )?,
        correlation_id: parse_brand_opt(operation, row.correlation_id.as_deref(), new_command_id)?,
        payload: parse_known_payload(operation, &row.payload_json)?,
        metadata: parse_json(operation, &row.metadata_json)?,
    })
}

fn row_to_agent(
    row: &EventRow,
    operation: &'static str,
) -> Result<StoredAgentEvent, PersistenceError> {
    let payload = parse_known_payload(operation, &row.payload_json)?;
    let metadata = parse_json(operation, &row.metadata_json)?;
    let field = |name: &str| {
        metadata
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };
    Ok(StoredAgentEvent {
        sequence: row.sequence,
        command_id: parse_brand_opt(operation, row.command_id.as_deref(), new_command_id)?,
        event: AgentEvent {
            id: parse_brand(operation, &row.event_id, new_event_id)?,
            thread_id: parse_brand(operation, &row.stream_id, new_thread_id)?,
            event_type: row.event_type.clone(),
            occurred_at: row.occurred_at.clone(),
            payload,
            run_id: parse_brand_opt(operation, field("runId").as_deref(), new_run_id)?,
            node_id: parse_brand_opt(operation, field("nodeId").as_deref(), new_node_id)?,
            driver: field("driver"),
            provider_instance_id: field("providerInstanceId"),
            raw_event_id: parse_brand_opt(
                operation,
                field("rawEventId").as_deref(),
                new_raw_event_id,
            )?,
        },
    })
}

fn new_event_id(value: &str) -> Result<EventId, turin_contracts::ContractError> {
    EventId::new(value)
}
fn new_command_id(value: &str) -> Result<CommandId, turin_contracts::ContractError> {
    CommandId::new(value)
}
fn new_thread_id(value: &str) -> Result<ThreadId, turin_contracts::ContractError> {
    ThreadId::new(value)
}
fn new_run_id(value: &str) -> Result<RunId, turin_contracts::ContractError> {
    RunId::new(value)
}
fn new_node_id(value: &str) -> Result<NodeId, turin_contracts::ContractError> {
    NodeId::new(value)
}
fn new_raw_event_id(value: &str) -> Result<RawEventId, turin_contracts::ContractError> {
    RawEventId::new(value)
}

fn parse_brand<T>(
    operation: &'static str,
    value: &str,
    make: fn(&str) -> Result<T, turin_contracts::ContractError>,
) -> Result<T, PersistenceError> {
    make(value).map_err(|err| decode_err(operation, err.to_string()))
}

fn parse_brand_opt<T>(
    operation: &'static str,
    value: Option<&str>,
    make: fn(&str) -> Result<T, turin_contracts::ContractError>,
) -> Result<Option<T>, PersistenceError> {
    value
        .map(|value| parse_brand(operation, value, make))
        .transpose()
}

fn parse_json(operation: &'static str, json: &str) -> Result<serde_json::Value, PersistenceError> {
    serde_json::from_str(json).map_err(|err| decode_err(operation, err.to_string()))
}

fn parse_known_payload(
    operation: &'static str,
    json: &str,
) -> Result<serde_json::Value, PersistenceError> {
    let value = parse_json(operation, json)?;
    match &value {
        serde_json::Value::Object(object) if !object.is_empty() => Ok(value),
        _ => Err(decode_err(
            operation,
            "known event payload must be a non-empty object".to_owned(),
        )),
    }
}

fn decode_err(operation: &'static str, issue: String) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, issue))
}

pub fn latest_agent_sequence(
    conn: &Connection,
    thread_id: Option<&str>,
) -> Result<i64, PersistenceError> {
    let (sql, params): (&str, Vec<SqlValue>) = if let Some(thread_id) = thread_id {
        (
            "
            SELECT MAX(sequence) AS sequence
            FROM orchestration_events
            INDEXED BY idx_orchestration_events_agent_stream_sequence
            WHERE application_event_version = 2
              AND aggregate_kind = 'thread'
              AND stream_id = ?1
            ",
            vec![SqlValue::text(thread_id)],
        )
    } else {
        (
            "
            SELECT MAX(sequence) AS sequence
            FROM orchestration_events
            WHERE application_event_version = 2
              AND aggregate_kind = 'thread'
            ",
            Vec::new(),
        )
    };
    let sequence: Option<i64> = conn
        .query_row(sql, params_from_iter(params), |row| row.get(0))
        .map_err(|err| {
            PersistenceError::sql("OrchestrationEventStore.latestAgentSequence:query", &err)
        })?;
    Ok(sequence.unwrap_or(0))
}

pub fn latest_application_sequence(conn: &Connection) -> Result<i64, PersistenceError> {
    let sequence: Option<i64> = conn
        .query_row(
            "
            SELECT MAX(sequence) AS sequence
            FROM orchestration_events INDEXED BY idx_orchestration_events_application_high_water
            WHERE aggregate_kind = 'project'
              OR (application_event_version = 2 AND aggregate_kind = 'thread')
            ",
            [],
            |row| row.get(0),
        )
        .map_err(|err| {
            PersistenceError::sql(
                "OrchestrationEventStore.latestApplicationSequence:query",
                &err,
            )
        })?;
    Ok(sequence.unwrap_or(0))
}

pub fn agent_replay_stats(
    conn: &Connection,
    thread_id: &str,
    after_sequence: i64,
    through_sequence: i64,
    max_events: u64,
) -> Result<AgentReplayStats, PersistenceError> {
    let limit = i64::try_from(max_events).unwrap_or(i64::MAX).max(0) + 1;
    let sql = format!(
        "
        SELECT
          COUNT(*) AS event_count,
          COALESCE(SUM(octet_length(payload_json)), 0) AS raw_payload_bytes,
          COALESCE(MAX(event_type = 'thread.created'), 0) AS has_create_event
        FROM (
          SELECT {RESTORED_PAYLOAD} AS payload_json, event_type
          FROM orchestration_events
          WHERE aggregate_kind = 'thread'
            AND stream_id = ?1
            AND application_event_version = 2
            AND sequence > ?2
            AND sequence <= ?3
          ORDER BY sequence ASC
          LIMIT ?4
        )
        "
    );
    conn.query_row(
        &sql,
        params_from_iter([
            SqlValue::text(thread_id),
            SqlValue::Integer(after_sequence),
            SqlValue::Integer(through_sequence),
            SqlValue::Integer(limit),
        ]),
        |row| {
            Ok(AgentReplayStats {
                event_count: row.get(0)?,
                raw_payload_bytes: row.get(1)?,
                has_create_event: row.get::<_, i64>(2)? != 0,
            })
        },
    )
    .map_err(|err| PersistenceError::sql("OrchestrationEventStore.getAgentReplayStats:query", &err))
}

pub fn replay_stats(
    conn: &Connection,
    after_sequence: i64,
    through_sequence: i64,
) -> Result<ReplayStats, PersistenceError> {
    let sql = format!(
        "
        SELECT
          COUNT(*) AS event_count,
          COALESCE(SUM(octet_length({RESTORED_PAYLOAD})), 0) AS raw_payload_bytes
        FROM orchestration_events INDEXED BY idx_orchestration_events_application_high_water
        WHERE sequence > ?1
          AND sequence <= ?2
          AND (
            aggregate_kind = 'project'
            OR (application_event_version = 2 AND aggregate_kind = 'thread')
          )
        "
    );
    conn.query_row(
        &sql,
        params_from_iter([
            SqlValue::Integer(after_sequence),
            SqlValue::Integer(through_sequence),
        ]),
        |row| {
            Ok(ReplayStats {
                event_count: row.get(0)?,
                raw_payload_bytes: row.get(1)?,
            })
        },
    )
    .map_err(|err| PersistenceError::sql("OrchestrationEventStore.getReplayStats:query", &err))
}

fn is_known_project_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "project.created" | "project.meta-updated" | "project.deleted"
    )
}

fn is_known_v2_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "thread.created"
            | "thread.archived"
            | "thread.unarchived"
            | "thread.deleted"
            | "thread.settled"
            | "thread.unsettled"
            | "thread.snoozed"
            | "thread.unsnoozed"
            | "thread.pinned"
            | "thread.auto-settle-set"
            | "thread.unpinned"
            | "thread.pin-reordered"
            | "thread.active-reordered"
            | "thread.visited"
            | "thread.marked-unread"
            | "thread.pull-request-synced"
            | "thread.runtime-mode-updated"
            | "thread.interaction-mode-updated"
            | "thread.model-selection-updated"
            | "thread.provider-switched"
            | "thread.metadata-updated"
            | "thread.pull-request-link-updated"
            | "run.created"
            | "run.updated"
            | "run.background-work-cancelled"
            | "run-attempt.created"
            | "run-attempt.updated"
            | "node.updated"
            | "subagent.updated"
            | "provider-session.attached"
            | "provider-session.updated"
            | "provider-session.detached"
            | "provider-thread.updated"
            | "provider-turn.updated"
            | "runtime-request.updated"
            | "message.updated"
            | "turn-item.updated"
            | "plan.updated"
            | "checkpoint-scope.created"
            | "checkpoint.captured"
            | "checkpoint.rollback-requested"
            | "context-handoff.updated"
            | "context-transfer.created"
            | "context-transfer.updated"
    )
}

#[derive(Default)]
struct LiveHub {
    next: Mutex<u64>,
    subs: Mutex<Vec<LiveSub>>,
}

struct LiveSub {
    id: u64,
    state: Arc<Mutex<LiveBuf>>,
    notify: Arc<tokio::sync::Notify>,
}

struct SizedEvent {
    event: Arc<ApplicationStoredEvent>,
    bytes: usize,
}

struct LiveBuf {
    items: VecDeque<SizedEvent>,
    bytes: usize,
    failed: Option<PersistenceError>,
}

impl LiveHub {
    fn subscribe(&self) -> LiveSub {
        let id = {
            let mut next = self.next.lock().unwrap_or_else(|err| err.into_inner());
            let id = *next;
            *next += 1;
            id
        };
        let sub = LiveSub {
            id,
            state: Arc::new(Mutex::new(LiveBuf {
                items: VecDeque::new(),
                bytes: 0,
                failed: None,
            })),
            notify: Arc::new(tokio::sync::Notify::new()),
        };
        self.subs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(clone_sub(&sub));
        sub
    }

    fn unsubscribe(&self, id: u64) {
        self.subs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|sub| sub.id != id);
    }

    fn publish(&self, events: Vec<ApplicationStoredEvent>) {
        let sized: Vec<SizedEvent> = events
            .into_iter()
            .map(|event| {
                let bytes = wire_event_bytes(&event);
                SizedEvent {
                    event: Arc::new(event),
                    bytes,
                }
            })
            .collect();
        let mut subs = self.subs.lock().unwrap_or_else(|err| err.into_inner());
        subs.retain(|sub| {
            let mut state = sub.state.lock().unwrap_or_else(|err| err.into_inner());
            if state.failed.is_some() {
                return false;
            }
            for event in &sized {
                if state.items.len() + 1 > LIVE_STREAM_MAX_ITEMS
                    || state.bytes + event.bytes > LIVE_STREAM_MAX_BYTES
                {
                    state.failed = Some(PersistenceError::Sql(PersistenceSqlError {
                        operation: "OrchestrationEventStore.streamApplicationEvents:buffer".into(),
                        detail: None,
                        correlation: None,
                    }));
                    break;
                }
                state.bytes += event.bytes;
                state.items.push_back(SizedEvent {
                    event: Arc::clone(&event.event),
                    bytes: event.bytes,
                });
            }
            sub.notify.notify_one();
            true
        });
    }
}

fn clone_sub(sub: &LiveSub) -> LiveSub {
    LiveSub {
        id: sub.id,
        state: Arc::clone(&sub.state),
        notify: Arc::clone(&sub.notify),
    }
}

impl LiveSub {
    fn fail_if_over_budget(&self) -> Result<(), PersistenceError> {
        let state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(err) = &state.failed {
            return Err(PersistenceError::Sql(match err {
                PersistenceError::Sql(sql) => sql.clone(),
                other => crate::errors::PersistenceSqlError {
                    operation: "OrchestrationEventStore.streamApplicationEvents:buffer".into(),
                    detail: Some(other.to_string()),
                    correlation: None,
                },
            }));
        }
        Ok(())
    }

    fn drain_after(&self, high: i64) -> Vec<ApplicationStoredEvent> {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        let mut out = Vec::new();
        while let Some(item) = state.items.pop_front() {
            state.bytes = state.bytes.saturating_sub(item.bytes);
            if item.event.sequence() > high {
                out.push(Arc::try_unwrap(item.event).unwrap_or_else(|event| (*event).clone()));
            }
        }
        out
    }

    async fn wait(&self) {
        self.notify.notified().await;
    }
}

/// Byte length of the upstream wire object (`JSON.stringify` of the whole event).
fn wire_event_bytes(event: &ApplicationStoredEvent) -> usize {
    serde_json::to_string(&wire_event(event))
        .map(|json| json.len())
        .unwrap_or(0)
}

fn wire_event(event: &ApplicationStoredEvent) -> serde_json::Value {
    match event {
        ApplicationStoredEvent::Project(event) => serde_json::json!({
            "sequence": event.sequence,
            "eventId": event.event_id,
            "aggregateKind": event.aggregate_kind,
            "aggregateId": event.aggregate_id,
            "occurredAt": event.occurred_at,
            "commandId": event.command_id,
            "causationEventId": event.causation_event_id,
            "correlationId": event.correlation_id,
            "metadata": event.metadata,
            "type": event.event_type,
            "payload": event.payload,
        }),
        ApplicationStoredEvent::Agent(stored) => {
            let event = &stored.event;
            let mut body = serde_json::Map::new();
            body.insert(
                "id".into(),
                serde_json::Value::String(event.id.as_str().to_owned()),
            );
            body.insert(
                "threadId".into(),
                serde_json::Value::String(event.thread_id.as_str().to_owned()),
            );
            if let Some(run_id) = &event.run_id {
                body.insert(
                    "runId".into(),
                    serde_json::Value::String(run_id.as_str().to_owned()),
                );
            }
            if let Some(node_id) = &event.node_id {
                body.insert(
                    "nodeId".into(),
                    serde_json::Value::String(node_id.as_str().to_owned()),
                );
            }
            if let Some(driver) = &event.driver {
                body.insert("driver".into(), serde_json::Value::String(driver.clone()));
            }
            if let Some(instance) = &event.provider_instance_id {
                body.insert(
                    "providerInstanceId".into(),
                    serde_json::Value::String(instance.clone()),
                );
            }
            if let Some(raw) = &event.raw_event_id {
                body.insert(
                    "rawEventId".into(),
                    serde_json::Value::String(raw.as_str().to_owned()),
                );
            }
            body.insert(
                "occurredAt".into(),
                serde_json::Value::String(event.occurred_at.clone()),
            );
            body.insert(
                "type".into(),
                serde_json::Value::String(event.event_type.clone()),
            );
            body.insert("payload".into(), event.payload.clone());
            serde_json::json!({
                "sequence": stored.sequence,
                "commandId": stored.command_id,
                "event": body,
            })
        }
    }
}
