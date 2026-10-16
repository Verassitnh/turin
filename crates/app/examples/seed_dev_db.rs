//! Writes a synthetic Turin database for dev runs.
//!
//! ```text
//! cargo run -p turin --example seed_dev_db -- /tmp/turin-dev
//! TURIN_DATA_DIR=/tmp/turin-dev cargo run -p turin
//! ```
//!
//! The data lives in `src/app_runtime/dev_seed.rs`, shared with the read-path
//! test, and goes through the same write API the app uses.

// The seed module is shared with the app's tests; this example uses all of it.
#[path = "../src/app_runtime/dev_seed.rs"]
#[allow(dead_code)]
mod dev_seed;

use std::path::PathBuf;
use std::sync::Arc;

use turin_orchestration::pipeline::effect_outbox::EffectOutbox;
use turin_orchestration::pipeline::event_sink::EventSink;
use turin_orchestration::projection::applier::ProjectionWriter;
use turin_persistence::{Db, OrchestrationEventStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: seed_dev_db <dir>"))?,
    );
    std::fs::create_dir_all(&dir)?;
    let database = dir.join("statev2.sqlite");
    let db = Db::open_configured(&database).await?;
    let events = OrchestrationEventStore::new(db.clone());
    let sink = EventSink::new(events, EffectOutbox::new(db), Arc::new(ProjectionWriter));
    dev_seed::seed(&sink).await?;
    println!("seeded {}", database.display());
    Ok(())
}
