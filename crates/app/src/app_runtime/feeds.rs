//! The live feeds: the shell (sidebar deltas) and one thread (timeline
//! events). Each follows the committed event log after the sequence its
//! snapshot reflects, so an event committed between the snapshot read and
//! the subscription is replayed, not lost.

use std::sync::Arc;

use futures::StreamExt as _;
use turin_contracts::ProjectId;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ShellStreamItem, OrchestrationV2ThreadShell,
};
use turin_orchestration::pipeline::event_sink::StreamOptions;
use turin_orchestration::projection::shell_stream::{
    ShellApplicationEvent, shell_stream_item_from_thread_shell, to_shell_application_event,
};
use turin_persistence::ApplicationStoredEvent;

use super::Stores;
use crate::state::events::{ClientStateEvent, ClientStateSender, FeedStatus};
use crate::state::shell_reducer::ShellStreamItem;
use crate::state::thread_state::ThreadStreamItem;

/// Sidebar deltas: one shell item per committed project or thread event.
pub async fn shell(stores: Arc<Stores>, sender: ClientStateSender, after_sequence: u64) {
    let mut subscription = stores
        .events
        .stream_application_events(i64::try_from(after_sequence).unwrap_or(i64::MAX));
    while let Some(next) = subscription.rx.recv().await {
        let stored = match next {
            Ok(stored) => stored,
            Err(error) => {
                let message = error.to_string();
                tracing::error!(error = %message, "shell feed stopped");
                sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(message)));
                return;
            }
        };
        match shell_item(&stores, &stored).await {
            Ok(item) => sender.shell(vec![ShellStreamItem::from(item)]),
            Err(error) => {
                // One aggregate failing to refetch must not stop the sidebar; the next event for it retries.
                tracing::warn!(error = %format!("{error:#}"), "dropped a shell update");
            }
        }
    }
}

/// Turns one committed application event into the shell delta it implies,
/// refetching the aggregate (upstream `ShellStream`'s per-event refetch).
async fn shell_item(
    stores: &Stores,
    stored: &ApplicationStoredEvent,
) -> anyhow::Result<OrchestrationV2ShellStreamItem> {
    match to_shell_application_event(stored) {
        ShellApplicationEvent::Project {
            aggregate_id,
            sequence,
            ..
        } => {
            let project_id = ProjectId::new(&aggregate_id)?;
            Ok(match stores.projects.get_shell(project_id.clone()).await? {
                Some(project) => {
                    OrchestrationV2ShellStreamItem::ProjectUpdated { sequence, project }
                }
                None => OrchestrationV2ShellStreamItem::ProjectRemoved {
                    sequence,
                    project_id,
                },
            })
        }
        ShellApplicationEvent::Thread {
            sequence,
            thread_id,
        } => {
            let shell: Option<OrchestrationV2ThreadShell> =
                stores.projections.get_thread_shell(&thread_id).await?;
            Ok(shell_stream_item_from_thread_shell(
                sequence, &thread_id, shell,
            ))
        }
    }
}

/// One thread's timeline: a bounded snapshot, then every event after it.
/// Always sends a snapshot, even when the store could resume: resuming is an
/// optimisation, and `decide_thread_resume` falls back to a snapshot anyway.
pub async fn thread(stores: Arc<Stores>, sender: ClientStateSender, thread_id: ThreadId) {
    let snapshot = match stores
        .projections
        .get_bounded_thread_snapshot(&thread_id)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let message = error.to_string();
            tracing::error!(thread = %thread_id, error = %message, "thread snapshot failed");
            sender.send(ClientStateEvent::ThreadFeed {
                thread_id,
                status: FeedStatus::Failed(message),
            });
            return;
        }
    };
    let after_sequence = snapshot.snapshot_sequence;
    let history = crate::state::thread_history_merge::ThreadHistoryMeta {
        history_cursor: snapshot.history_cursor,
        has_more_history: snapshot.has_more_history,
        loading: false,
        error: None,
        expanded: false,
        latest_local_turn_ordinal: None,
    };
    sender.thread(
        thread_id.clone(),
        vec![ThreadStreamItem::Snapshot {
            snapshot_sequence: after_sequence,
            projection: Arc::new(snapshot.projection.into()),
            history: Some(history),
        }],
    );

    let mut events = match stores
        .sink
        .stream(StreamOptions {
            thread_id: Some(thread_id.clone()),
            after_sequence: i64::try_from(after_sequence).ok(),
            event_type: None,
        })
        .await
    {
        Ok(events) => events,
        Err(error) => {
            sender.send(ClientStateEvent::ThreadFeed {
                thread_id,
                status: FeedStatus::Failed(error.to_string()),
            });
            return;
        }
    };
    while let Some(next) = events.next().await {
        match next {
            Ok(stored) => sender.thread(
                thread_id.clone(),
                vec![ThreadStreamItem::Event {
                    sequence: stored.sequence,
                    event: Box::new(stored.event),
                }],
            ),
            Err(error) => {
                sender.send(ClientStateEvent::ThreadFeed {
                    thread_id,
                    status: FeedStatus::Failed(error.to_string()),
                });
                return;
            }
        }
    }
}
