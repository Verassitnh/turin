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
        // An import staged by the welcome wizard replaces an empty database before it is opened.
        match turin_persistence::T3Import::adopt_staged(data_dir) {
            Ok(turin_persistence::AdoptStaged::NothingStaged) => {}
            Ok(outcome) => tracing::info!(?outcome, "staged T3 import"),
            Err(error) => tracing::error!(%error, "adopting the staged T3 import failed"),
        }
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

/// What the startup toast says when the database cannot be opened: the
/// data folder and the underlying error, so the user can see this is not data
/// loss.
pub(crate) fn database_open_failure_message(data_dir: &std::path::Path, error: &str) -> String {
    format!(
        "Turin could not open its database in {}. Your projects and threads are not shown until this is fixed. ({error})",
        data_dir.display()
    )
}

/// "Start fresh": renames the database and its WAL/SHM sidecars to
/// `<name>.broken-<stamp>` next to it. Nothing is deleted, so the old data can
/// still be recovered by hand. A name already taken gets a numeric suffix.
/// Returns what was moved, as `(from, to)`.
pub(crate) fn move_database_aside(
    data_dir: &Path,
    stamp: &str,
) -> std::io::Result<Vec<(std::path::PathBuf, std::path::PathBuf)>> {
    let database = data_dir::database_path(data_dir);
    let mut moved = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut from = database.clone().into_os_string();
        from.push(suffix);
        let from = std::path::PathBuf::from(from);
        if !from.exists() {
            continue;
        }
        let mut attempt = 0_u32;
        let to = loop {
            let mut name = database.clone().into_os_string();
            name.push(format!(".broken-{stamp}"));
            if attempt > 0 {
                name.push(format!("-{attempt}"));
            }
            name.push(suffix);
            let candidate = std::path::PathBuf::from(name);
            if !candidate.exists() {
                break candidate;
            }
            attempt += 1;
        };
        std::fs::rename(&from, &to)?;
        moved.push((from, to));
    }
    Ok(moved)
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
    // An unopenable database waits here for the user's choice (retry, or move
    // it aside and start fresh) instead of ending startup.
    let mut recovery = bus.take_recovery_requests();
    let mut requests = requests;
    let stores = loop {
        match Stores::open(&data_dir).await {
            Ok(stores) => {
                bus.clear_failure();
                break Arc::new(stores);
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "failed to open Turin database; the sidebar stays empty");
                let message = database_open_failure_message(&data_dir, &format!("{error:#}"));
                bus.fail_recoverable(message.clone());
                sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(message)));
            }
        }
        let Some(recovery) = recovery.as_mut() else {
            return;
        };
        loop {
            tokio::select! {
                action = recovery.recv() => match action {
                    Some(write::bus::DatabaseRecovery::Retry) => break,
                    Some(write::bus::DatabaseRecovery::StartFresh) => {
                        match move_database_aside(&data_dir, &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()) {
                            Ok(moved) => tracing::warn!(?moved, "moved the unopenable database aside"),
                            Err(error) => {
                                let message = format!(
                                    "Turin could not move the old database aside in {}. ({error})",
                                    data_dir.display()
                                );
                                bus.fail_recoverable(message.clone());
                                sender.send(ClientStateEvent::ShellFeed(FeedStatus::Failed(message)));
                                continue;
                            }
                        }
                        break;
                    }
                    None => return,
                },
                // Nothing is served yet; the UI going away ends startup.
                request = requests.recv() => if request.is_none() {
                    return;
                },
            }
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
            let commands = write::commands::Commands::new(Arc::clone(&runtime));
            tokio::spawn(write::smoke::run_if_requested(commands.clone()));
            write::providers::spawn_status_feed(&runtime.providers, bus.clone());
            // Threads without a project are offered once the Scratch folder is known.
            let scratch = commands.clone();
            let scratch_sender = sender.clone();
            tokio::spawn(async move {
                scratch_sender.send(ClientStateEvent::ScratchWorkspaceRoot(
                    scratch.scratch_root().await,
                ));
            });
            bus.install(commands);
            Some(runtime)
        }
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "failed to start the write path; the app is read-only");
            bus.fail(format!("{error:#}"));
            None
        }
    };
    serve(stores, sender, requests).await;
    if let Some(write) = &_write {
        write.shutdown().await;
    }
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
