//! The live feeds: the shell (sidebar deltas) and one thread (timeline
//! events). Each follows the committed event log after the sequence its
//! snapshot reflects, so an event committed between the snapshot read and
//! the subscription is replayed, not lost.

use std::sync::Arc;

use futures::StreamExt as _;
use turin_contracts::ProjectId;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ShellStreamItem, OrchestrationV2ThreadShell, OrchestrationV2ThreadStreamItem,
};
use turin_orchestration::pipeline::event_sink::StreamOptions;
use turin_orchestration::projection::shell_stream::{
    ShellApplicationEvent, shell_stream_item_from_thread_shell, to_shell_application_event,
};
use turin_orchestration::projection::thread_live_event_coalescer::ThreadLiveEventCoalescer;
use turin_persistence::ApplicationStoredEvent;

use super::Stores;
use super::feed_retry::{
    refetch_max_attempts, refetch_retry_delay_ms, stream_resubscribe_delay_ms,
};

pub type ShellResync =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, anyhow::Result<u64>> + Send + Sync>;
use crate::state::events::{ClientStateEvent, ClientStateSender, FeedStatus};
use crate::state::shell_reducer::ShellStreamItem;
use crate::state::thread_state::ThreadStreamItem;

/// Sidebar deltas with resubscribe on stream failure (upstream `subscribeDynamic`).
pub async fn shell(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    mut after_sequence: u64,
    resync_shell: ShellResync,
) {
    let mut failure_count = 0u32;
    loop {
        match shell_stream(Arc::clone(&stores), sender.clone(), after_sequence).await {
            Ok(()) => break,
            Err(message) => {
                failure_count += 1;
                tracing::error!(
                    error = %message,
                    failure_count,
                    "shell feed stopped; resubscribing"
                );
                sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
                    message.clone(),
                )));
                let delay = stream_resubscribe_delay_ms(failure_count);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                match (resync_shell)().await {
                    Ok(sequence) => {
                        after_sequence = sequence;
                        failure_count = 0;
                    }
                    Err(error) => {
                        tracing::error!(error = %format!("{error:#}"), "shell resync failed");
                        sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
                            error.to_string(),
                        )));
                        break;
                    }
                }
            }
        }
    }
}

async fn shell_stream(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    after_sequence: u64,
) -> Result<(), String> {
    let mut subscription = stores
        .events
        .stream_application_events(i64::try_from(after_sequence).unwrap_or(i64::MAX));
    while let Some(next) = subscription.rx.recv().await {
        let stored = match next {
            Ok(stored) => stored,
            Err(error) => return Err(error.to_string()),
        };
        match shell_item_with_retry(&stores, &stored).await {
            Ok(item) => sender.shell(vec![ShellStreamItem::from(item)]),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

async fn shell_item_with_retry(
    stores: &Stores,
    stored: &ApplicationStoredEvent,
) -> anyhow::Result<OrchestrationV2ShellStreamItem> {
    let max = refetch_max_attempts();
    let mut attempt = 0u32;
    loop {
        match shell_item(stores, stored).await {
            Ok(item) => return Ok(item),
            Err(error) => {
                attempt += 1;
                if attempt >= max {
                    return Err(error);
                }
                tracing::warn!(
                    error = %format!("{error:#}"),
                    attempt,
                    "shell refetch failed; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(refetch_retry_delay_ms(
                    attempt,
                )))
                .await;
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

/// One thread's timeline with resubscribe on live-stream failure.
pub async fn thread(stores: Arc<Stores>, sender: ClientStateSender, thread_id: ThreadId) {
    let mut failure_count = 0u32;
    loop {
        match thread_once(Arc::clone(&stores), sender.clone(), thread_id.clone()).await {
            Ok(()) => break,
            Err(message) => {
                failure_count += 1;
                tracing::error!(
                    thread = %thread_id,
                    error = %message,
                    failure_count,
                    "thread feed stopped; resubscribing"
                );
                sender.send(ClientStateEvent::ThreadFeed {
                    thread_id: thread_id.clone(),
                    status: FeedStatus::Failed(message.clone()),
                });
                let delay = stream_resubscribe_delay_ms(failure_count);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
        }
    }
}

async fn thread_once(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    thread_id: ThreadId,
) -> Result<(), String> {
    let snapshot = stores
        .projections
        .get_bounded_thread_snapshot(&thread_id)
        .await
        .map_err(|error| error.to_string())?;
    let after_sequence = snapshot.snapshot_sequence;
    let history = crate::state::thread_history_merge::ThreadHistoryMeta {
        history_cursor: snapshot.history_cursor,
        has_more_history: snapshot.has_more_history,
        loading: false,
        error: None,
        expanded: false,
        latest_local_turn_ordinal: snapshot.latest_local_turn_ordinal,
    };
    sender.thread(
        thread_id.clone(),
        vec![ThreadStreamItem::Snapshot {
            snapshot_sequence: after_sequence,
            projection: Arc::new(snapshot.projection.into()),
            history: Some(history),
        }],
    );

    let mut events = stores
        .sink
        .stream(StreamOptions {
            thread_id: Some(thread_id.clone()),
            after_sequence: i64::try_from(after_sequence).ok(),
            event_type: None,
        })
        .await
        .map_err(|error| error.to_string())?;

    let mut coalescer = ThreadLiveEventCoalescer::new();
    loop {
        tokio::select! {
            next = events.next() => {
                match next {
                    None => {
                        coalescer.end().await;
                        while let Some(item) = coalescer.next().await {
                            sender.thread(thread_id.clone(), vec![ThreadStreamItem::from(item)]);
                        }
                        return Ok(());
                    }
                    Some(Ok(stored)) => {
                        if coalescer.offer(OrchestrationV2ThreadStreamItem::Event {
                            sequence: stored.sequence,
                            event: stored.event,
                        }).is_err() {
                            return Ok(());
                        }
                    }
                    Some(Err(error)) => return Err(error.to_string()),
                }
            }
            item = coalescer.next() => {
                if let Some(item) = item {
                    sender.thread(thread_id.clone(), vec![ThreadStreamItem::from(item)]);
                }
            }
        }
    }
}

/// Runs a thread feed in an inner task so a panic surfaces as [`FeedStatus::Failed`].
pub async fn thread_supervised(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    thread_id: ThreadId,
) {
    let thread_for_panic = thread_id.clone();
    let sender_for_panic = sender.clone();
    match tokio::spawn(async move { thread(stores, sender, thread_id).await }).await {
        Ok(()) => {}
        Err(error) if error.is_panic() => {
            tracing::error!(thread = %thread_for_panic, "thread feed panicked");
            sender_for_panic.send(ClientStateEvent::ThreadFeed {
                thread_id: thread_for_panic,
                status: FeedStatus::Failed("thread feed panicked".into()),
            });
        }
        Err(_) => {}
    }
}

/// Runs the shell feed in an inner task so a panic surfaces as [`FeedStatus::Failed`].
pub async fn shell_supervised(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    after_sequence: u64,
    resync_shell: ShellResync,
) {
    let sender_for_panic = sender.clone();
    match tokio::spawn(async move { shell(stores, sender, after_sequence, resync_shell).await })
        .await
    {
        Ok(()) => {}
        Err(error) if error.is_panic() => {
            tracing::error!("shell feed panicked");
            sender_for_panic.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
                "shell feed panicked".into(),
            )));
        }
        Err(_) => {}
    }
}
