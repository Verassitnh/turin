//! Port of upstream/apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.ts
//!
//! The LiveStreamBudget and serialized wire accounting are cut: GPUI reads
//! this feed in-process. The 50 ms coalescing window and 512-update flush cap
//! remain unchanged.

use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use turin_contracts::orchestration_v2::{
    OrchestrationV2DomainEvent, OrchestrationV2ThreadStreamItem,
};

pub const COALESCE_WINDOW: Duration = Duration::from_millis(50);
pub const MAX_PENDING_UPDATES: usize = 512;

fn is_tool_updated(item: &OrchestrationV2ThreadStreamItem) -> bool {
    let OrchestrationV2ThreadStreamItem::Event {
        event: OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. },
        ..
    } = item
    else {
        return false;
    };
    let Ok(value) = serde_json::to_value(payload) else {
        return false;
    };
    value["status"] == "running"
        && matches!(
            value["type"].as_str(),
            Some(
                "command_execution" | "file_change" | "file_search" | "web_search" | "dynamic_tool"
            )
        )
}

fn identity(item: &OrchestrationV2ThreadStreamItem) -> Option<String> {
    let OrchestrationV2ThreadStreamItem::Event {
        event:
            OrchestrationV2DomainEvent::TurnItemUpdated {
                thread_id,
                run_id,
                payload,
                ..
            },
        ..
    } = item
    else {
        return None;
    };
    let value = serde_json::to_value(payload).ok()?;
    Some(format!(
        "{}\0{}\0{}",
        thread_id.as_str(),
        run_id.as_ref().map(|id| id.as_str()).unwrap_or(""),
        value["id"].as_str()?
    ))
}

/// Keep the last running update per stable item ID within each contiguous run.
/// A terminal or unrelated event is a hard boundary and is always retained.
pub fn coalesce_live_tool_updated_events(
    events: &[OrchestrationV2ThreadStreamItem],
) -> Vec<OrchestrationV2ThreadStreamItem> {
    fn flush(
        pending: &mut Vec<OrchestrationV2ThreadStreamItem>,
        output: &mut Vec<OrchestrationV2ThreadStreamItem>,
    ) {
        let mut seen = HashSet::new();
        let mut latest = Vec::new();
        for event in pending.drain(..).rev() {
            if identity(&event).is_none_or(|key| seen.insert(key)) {
                latest.push(event);
            }
        }
        latest.reverse();
        output.extend(latest);
    }
    let mut output = Vec::new();
    let mut pending = Vec::new();
    for event in events {
        if is_tool_updated(event) {
            pending.push(event.clone());
        } else {
            flush(&mut pending, &mut output);
            output.push(event.clone());
        }
    }
    flush(&mut pending, &mut output);
    output
}

/// Owned in-process feed. Dropping it aborts its worker through `JoinSet`.
pub struct ThreadLiveEventCoalescer {
    input: Option<mpsc::UnboundedSender<OrchestrationV2ThreadStreamItem>>,
    output: mpsc::UnboundedReceiver<OrchestrationV2ThreadStreamItem>,
    workers: JoinSet<()>,
}

impl ThreadLiveEventCoalescer {
    pub fn new() -> Self {
        Self::with_window(COALESCE_WINDOW)
    }

    pub fn with_window(window: Duration) -> Self {
        let (input, mut incoming) = mpsc::unbounded_channel();
        let (outgoing, output) = mpsc::unbounded_channel();
        let mut workers = JoinSet::new();
        workers.spawn(async move {
            let mut pending = Vec::new();
            let mut deadline: Option<tokio::time::Instant> = None;
            loop {
                let next = if let Some(at) = deadline {
                    tokio::select! { item = incoming.recv() => item, _ = tokio::time::sleep_until(at) => { flush_pending(&mut pending, &outgoing); deadline = None; continue; } }
                } else { incoming.recv().await };
                let Some(item) = next else { break; };
                if is_tool_updated(&item) {
                    pending.push(item);
                    if deadline.is_none() { deadline = Some(tokio::time::Instant::now() + window); }
                    if pending.len() >= MAX_PENDING_UPDATES { flush_pending(&mut pending, &outgoing); deadline = None; }
                } else {
                    flush_pending(&mut pending, &outgoing);
                    deadline = None;
                    if outgoing.send(item).is_err() { return; }
                }
            }
            flush_pending(&mut pending, &outgoing);
        });
        Self {
            input: Some(input),
            output,
            workers,
        }
    }

    pub fn offer(
        &self,
        item: OrchestrationV2ThreadStreamItem,
    ) -> Result<(), Box<mpsc::error::SendError<OrchestrationV2ThreadStreamItem>>> {
        self.input
            .as_ref()
            .expect("coalescer ended")
            .send(item)
            .map_err(Box::new)
    }

    pub async fn next(&mut self) -> Option<OrchestrationV2ThreadStreamItem> {
        self.output.recv().await
    }

    pub async fn end(&mut self) {
        self.input.take();
        while self.workers.join_next().await.is_some() {}
    }
}

impl Default for ThreadLiveEventCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

fn flush_pending(
    pending: &mut Vec<OrchestrationV2ThreadStreamItem>,
    outgoing: &mpsc::UnboundedSender<OrchestrationV2ThreadStreamItem>,
) {
    for item in coalesce_live_tool_updated_events(pending) {
        if outgoing.send(item).is_err() {
            break;
        }
    }
    pending.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn tool(sequence: u64, id: &str, run: &str, status: &str) -> OrchestrationV2ThreadStreamItem {
        serde_json::from_value(json!({"kind":"event","sequence":sequence,"event":{
            "type":"turn-item.updated","id":format!("event-{sequence}"),"threadId":"thread-1","runId":run,
            "occurredAt":"2026-01-01T00:00:00.000Z","payload":{
                "type":"command_execution","id":id,"threadId":"thread-1","runId":run,"nodeId":null,
                "providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,"parentItemId":null,
                "ordinal":0,"status":status,"title":"Tool","startedAt":null,"completedAt":null,
                "updatedAt":"2026-01-01T00:00:00.000Z","input":"x","output":"x"}}})).unwrap()
    }
    fn sequences(events: &[OrchestrationV2ThreadStreamItem]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|event| {
                if let OrchestrationV2ThreadStreamItem::Event { sequence, .. } = event {
                    Some(*sequence)
                } else {
                    None
                }
            })
            .collect()
    }
    #[test]
    fn coalesces_by_stable_id_and_respects_boundaries() {
        let events = vec![
            tool(1, "a", "r", "running"),
            tool(2, "b", "r", "running"),
            tool(3, "a", "r", "running"),
        ];
        assert_eq!(
            sequences(&coalesce_live_tool_updated_events(&events)),
            vec![2, 3]
        );
        let events = vec![
            tool(1, "a", "old", "running"),
            tool(2, "a", "new", "running"),
        ];
        assert_eq!(
            sequences(&coalesce_live_tool_updated_events(&events)),
            vec![1, 2]
        );
        let events = vec![
            tool(1, "a", "r", "running"),
            tool(2, "a", "r", "running"),
            tool(3, "a", "r", "completed"),
            tool(4, "a", "r", "running"),
        ];
        assert_eq!(
            sequences(&coalesce_live_tool_updated_events(&events)),
            vec![2, 3, 4]
        );
    }
    #[tokio::test]
    async fn timer_flushes_and_synchronized_marker_flushes_immediately() {
        let mut feed = ThreadLiveEventCoalescer::new();
        feed.offer(tool(1, "a", "r", "running")).unwrap();
        feed.offer(tool(2, "a", "r", "running")).unwrap();
        feed.offer(OrchestrationV2ThreadStreamItem::Synchronized)
            .unwrap();
        assert_eq!(sequences(&[feed.next().await.unwrap()]), vec![2]);
        assert!(matches!(
            feed.next().await,
            Some(OrchestrationV2ThreadStreamItem::Synchronized)
        ));
        feed.offer(tool(3, "a", "r", "running")).unwrap();
        assert_eq!(
            sequences(&[
                tokio::time::timeout(Duration::from_millis(250), feed.next())
                    .await
                    .unwrap()
                    .unwrap()
            ]),
            vec![3]
        );
        feed.end().await;
    }
}
