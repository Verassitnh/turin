//! The composition root. Opens Turin's database, builds the projection read
//! services and the write sink, and feeds the client store through
//! [`ClientStateSender`]. Serves the store's [`ClientStateRequest`]s.
//!
//! Everything here is async and runs on the tokio runtime `main` builds. Views
//! read cached state and never await this module.
//!
//! ```text
//! data dir ─▶ Db ─▶ ProjectionStore / ProjectStore / OrchestrationEventStore / EventSink
//!                        │
//!        shell snapshot ─┴─▶ ClientStateSender::shell ─▶ store
//!        shell feed (application events) ───────────▶ store
//!        OpenThread ─▶ bounded snapshot + EventSink::stream ─▶ store
//!        LoadEarlier ─▶ get_thread_history_page ─────────────▶ store
//! ```

pub mod data_dir;
mod feed_retry;
mod feeds;
mod requests;
#[cfg(test)]
pub(crate) mod tests;
// The write path: the UI seams (composer send, thread actions) consume it as
// they are bound to live threads.
#[allow(dead_code)]
pub mod write;

// Synthetic data for `examples/seed_dev_db.rs` and the read-path test. Not a
// runtime path, so the app binary never calls it.
#[allow(dead_code)]
pub mod dev_seed;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::task::AbortHandle;
use tokio::task::JoinSet;
use turin_contracts::ThreadId;
use turin_orchestration::pipeline::effect_outbox::EffectOutbox;
use turin_orchestration::pipeline::event_sink::EventSink;
use turin_orchestration::projection::applier::ProjectionWriter;
use turin_orchestration::projection::project_store::ProjectStore;
use turin_orchestration::projection::store::ProjectionStore;
use turin_persistence::{Db, OrchestrationEventStore};

use crate::state::events::{ClientStateEvent, ClientStateRequests, ClientStateSender, FeedStatus};
use crate::state::shell_reducer::ShellStreamItem;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ShellSnapshot, OrchestrationV2ShellStreamItem,
};

/// Every read and write service over one database connection pair.
pub struct Stores {
    pub db: Db,
    pub projections: Arc<ProjectionStore>,
    pub projects: Arc<ProjectStore>,
    pub events: OrchestrationEventStore,
    pub sink: EventSink,
    /// The outbox the sink enqueues into. The effect worker must claim from
    /// this same value: its wake-up semaphore is shared state.
    pub outbox: EffectOutbox,
}

impl Stores {
    /// Creates the data dir if needed, opens its database (migrations and the
    /// V1 import run here), and builds the services.
    pub async fn open(data_dir: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .with_context(|| format!("creating Turin data dir {}", data_dir.display()))?;
        let db = Db::open_configured(data_dir::database_path(data_dir))
            .await
            .context("opening Turin database")?;
        Ok(Self::from_db(db))
    }

    pub fn from_db(db: Db) -> Self {
        let events = OrchestrationEventStore::new(db.clone());
        let outbox = EffectOutbox::new(db.clone());
        let sink = EventSink::new(events.clone(), outbox.clone(), Arc::new(ProjectionWriter));
        Self {
            projections: Arc::new(ProjectionStore::new(db.clone())),
            projects: Arc::new(ProjectStore::new(db.clone())),
            db,
            events,
            sink,
            outbox,
        }
    }
}

/// Starts the write path and the feeds for the client store. Returns when the
/// store drops its request channel or the database cannot be opened. An
/// unopenable database leaves the store empty; the UI still starts, and the
/// command bus reports why every command fails.
pub async fn run(
    data_dir: Option<std::path::PathBuf>,
    sender: ClientStateSender,
    requests: ClientStateRequests,
    bus: write::bus::CommandBus,
) {
    let Some(data_dir) = data_dir else {
        let message = "no Turin data dir: set TURIN_DATA_DIR or HOME";
        tracing::error!("{message}; the sidebar stays empty");
        bus.fail(message.into());
        sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
            message.into(),
        )));
        return;
    };
    let stores = match Stores::open(&data_dir).await {
        Ok(stores) => Arc::new(stores),
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to open Turin database; the sidebar stays empty");
            bus.fail(format!("{error:#}"));
            sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
                error.to_string(),
            )));
            return;
        }
    };
    // Held until `serve` returns: dropping it stops the effect worker and the
    // provider sessions.
    let _write = match write::runtime::WriteRuntime::build(
        Arc::clone(&stores),
        &data_dir,
        write::settings::load_server_settings(
            &data_dir,
            std::env::var(write::settings::CLAUDE_BINARY_ENV)
                .ok()
                .as_deref(),
        ),
    )
    .await
    {
        Ok(runtime) => {
            bus.install(write::commands::Commands::new(Arc::clone(&runtime)));
            Some(runtime)
        }
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to start the write path; the app is read-only");
            bus.fail(format!("{error:#}"));
            None
        }
    };
    serve(stores, sender, requests).await;
}

/// Loads the shell, starts its feed after the snapshot's sequence, then serves
/// requests until the store goes away.
pub async fn serve(
    stores: Arc<Stores>,
    sender: ClientStateSender,
    mut requests: ClientStateRequests,
) {
    let snapshot = match load_shell(&stores).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to read the shell snapshot");
            sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(
                error.to_string(),
            )));
            return;
        }
    };
    let after_sequence = snapshot.snapshot_sequence;
    sender.shell(vec![ShellStreamItem::from(
        OrchestrationV2ShellStreamItem::Snapshot {
            snapshot,
            resolved_repository_identity_roots: None,
        },
    )]);

    // Every task here is aborted when this function returns: JoinSet aborts on drop.
    let mut tasks = JoinSet::new();
    let resync_stores = Arc::clone(&stores);
    let resync_sender = sender.clone();
    let resync: feeds::ShellResync = Arc::new(move || {
        let stores = Arc::clone(&resync_stores);
        let sender = resync_sender.clone();
        Box::pin(async move {
            let snapshot = load_shell(&stores).await?;
            sender.shell(vec![ShellStreamItem::from(
                OrchestrationV2ShellStreamItem::Snapshot {
                    snapshot: snapshot.clone(),
                    resolved_repository_identity_roots: None,
                },
            )]);
            Ok(snapshot.snapshot_sequence)
        })
    });
    tasks.spawn(feeds::shell_supervised(
        Arc::clone(&stores),
        sender.clone(),
        after_sequence,
        resync,
    ));
    let mut thread_feeds: HashMap<ThreadId, AbortHandle> = HashMap::new();
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { break };
                requests::handle(&mut tasks, &mut thread_feeds, &stores, &sender, request);
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

async fn load_shell(stores: &Stores) -> anyhow::Result<OrchestrationV2ShellSnapshot> {
    Ok(stores.projections.get_active_shell_snapshot().await?)
}
