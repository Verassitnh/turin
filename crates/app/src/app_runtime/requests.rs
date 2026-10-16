//! Answers the client store's [`ClientStateRequest`]s. Each request runs as its
//! own task on the runtime; nothing here awaits on the request loop.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task::AbortHandle;
use tokio::task::JoinSet;
use turin_contracts::ThreadId;

use super::{Stores, feeds};
use crate::state::events::{ClientStateEvent, ClientStateRequest, ClientStateSender};
use crate::state::thread_history_merge::ThreadHistoryPageMeta;
use crate::state::thread_projection::VisibleTurnItem;

pub fn handle(
    tasks: &mut JoinSet<()>,
    thread_feeds: &mut HashMap<ThreadId, AbortHandle>,
    stores: &Arc<Stores>,
    sender: &ClientStateSender,
    request: ClientStateRequest,
) {
    match request {
        // The store may offer `after_sequence` to resume. Resuming is an
        // optimisation, so this always starts with a fresh bounded snapshot.
        ClientStateRequest::OpenThread {
            thread_id,
            after_sequence: _,
        } => {
            if let Some(previous) = thread_feeds.remove(&thread_id) {
                previous.abort();
            }
            let handle = tasks.spawn(feeds::thread(
                Arc::clone(stores),
                sender.clone(),
                thread_id.clone(),
            ));
            thread_feeds.insert(thread_id, handle);
        }
        ClientStateRequest::CloseThread(thread_id) => {
            if let Some(feed) = thread_feeds.remove(&thread_id) {
                feed.abort();
            }
        }
        ClientStateRequest::LoadEarlier { thread_id, cursor } => {
            tasks.spawn(load_earlier(
                Arc::clone(stores),
                sender.clone(),
                thread_id,
                cursor,
            ));
        }
    }
}

/// The page before `cursor`, as the timeline's "load earlier" control asks for it.
async fn load_earlier(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    thread_id: ThreadId,
    cursor: String,
) {
    match stores
        .projections
        .get_thread_history_page(&thread_id, &cursor, None, false)
        .await
    {
        Ok(page) => sender.send(ClientStateEvent::ThreadHistoryPage {
            thread_id,
            request_cursor: cursor,
            items: page.items.into_iter().map(VisibleTurnItem::from).collect(),
            meta: ThreadHistoryPageMeta {
                next_cursor: page.next_cursor.map(|next| next.as_str().to_owned()),
                has_more_history: page.has_more_history,
            },
        }),
        Err(error) => sender.send(ClientStateEvent::ThreadHistoryFailed {
            thread_id,
            request_cursor: cursor,
            message: error.to_string(),
        }),
    }
}
