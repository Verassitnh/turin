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
mod feeds;
mod requests;
#[cfg(test)]
mod tests;

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

use crate::state::events::{ClientStateRequests, ClientStateSender};
use crate::state::shell_reducer::ShellStreamItem;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ShellSnapshot, OrchestrationV2ShellStreamItem,
};

/// Every read and write service over one database connection pair.
pub struct Stores {
    pub projections: ProjectionStore,
    pub projects: ProjectStore,
    pub events: OrchestrationEventStore,
    pub sink: EventSink,
}

impl Stores {
    /// Creates the data dir if needed, opens its database (migrations and the
    /// V1 import run here), and builds the services.
    pub async fn open(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating Turin data dir {}", data_dir.display()))?;
        let db = Db::open_configured(data_dir::database_path(data_dir))
            .await
            .context("opening Turin database")?;
        Ok(Self::from_db(db))
    }

    pub fn from_db(db: Db) -> Self {
        let events = OrchestrationEventStore::new(db.clone());
        let outbox = EffectOutbox::new(db.clone());
        let sink = EventSink::new(events.clone(), outbox, Arc::new(ProjectionWriter));
        Self {
            projections: ProjectionStore::new(db.clone()),
            projects: ProjectStore::new(db),
            events,
            sink,
        }
    }
}

/// Starts the feeds for the client store. Returns when the store drops its
/// request channel or the database cannot be opened. An unopenable database
/// leaves the store empty; the UI still starts.
pub async fn run(
    data_dir: Option<std::path::PathBuf>,
    sender: ClientStateSender,
    requests: ClientStateRequests,
) {
    let Some(data_dir) = data_dir else {
        tracing::error!("no Turin data dir: set TURIN_DATA_DIR or HOME; the sidebar stays empty");
        return;
    };
    let stores = match Stores::open(&data_dir).await {
        Ok(stores) => Arc::new(stores),
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to open Turin database; the sidebar stays empty");
            return;
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
    tasks.spawn(feeds::shell(
        Arc::clone(&stores),
        sender.clone(),
        after_sequence,
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
