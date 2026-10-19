//! Port of upstream/apps/server/src/persistence/initializeV2Database.ts
//! (online backup), upstream/apps/server/src/persistence/Migrations.ts
//! (ledger fail-closed), and the decision in
//! upstream/apps/server/src/persistence/reconcileV2PreviewMigration.ts
//! to keep a preview ledger from migrating under the wrong names.
//!
//! Upstream opens its own `statev2.sqlite` and migrates that file in place.
//! Turin is a different process, so it never writes the T3 file. Adoption is
//! opt-in and first-run only:
//!
//! 1. The user points at a T3 v2 database (`~/.t3/userdata/statev2.sqlite`).
//! 2. [`T3Import::import_into`] copies it with the SQLite online backup API
//!    (`SQLITE_OPEN_READ_ONLY`, busy timeout). Uncommitted rows stay behind.
//! 3. The copy's `effect_sql_migrations` ledger is checked. A V2 id (55 and
//!    up) this build does not have, or recorded under another name, is refused.
//!    A V2 gap that the migrator would skip is refused the same way
//!    `assertV2MigrationsCanRun` refuses. Earlier ids are accepted, as upstream
//!    accepts them.
//! 4. [`crate::run_migrations`] applies only Turin's pending migrations.
//! 5. The copy is kept only when its normalized `sqlite_master` matches a
//!    fresh Turin database, pre-existing row counts are unchanged, and every
//!    event the event store can read decodes. Unknown event types are counted
//!    and are not decode errors. Legacy v1 thread rows stay in the table; the
//!    v2 reader does not surface them.
//! 6. The verified file is hard-linked to `{turin_data_dir}/statev2.sqlite`
//!    ([`TURIN_DATABASE_FILE`]). Hard-link fails if that path already exists,
//!    so a second run cannot replace a database the app has started writing.
//!
//! Later launches call [`crate::Db::open`] on `~/.turin/statev2.sqlite`. They
//! do not open the T3 file. `reconcileV2PreviewMigration` is still not run;
//! a preview id fails closed in the ledger check and in `run_migrations`.

mod adopt;
mod plan;
mod snapshot;
mod verify;

pub use adopt::{AdoptStaged, NOT_LOADED_IMPORT_FILE, STAGED_IMPORT_DIR, TURIN_DATABASE_FILE};
pub use plan::ImportPlan;
pub use snapshot::T3Snapshot;
pub use verify::{EventReadReport, ImportReport};

use std::path::Path;

use adopt::{adopt_staged, import_into, stage_into};
use plan::plan;
use snapshot::backup_to;
use verify::migrate_and_verify;

/// Read-only copy of a T3 v2 database, then migrate and verify the copy.
pub struct T3Import;

impl T3Import {
    /// Consistent online backup of `src` into a new directory under `/tmp`.
    ///
    /// The source is opened with `SQLITE_OPEN_READ_ONLY` and a busy timeout.
    /// The returned path is the only file later steps migrate. Dropping the
    /// snapshot deletes that directory.
    pub fn snapshot(src: impl AsRef<Path>) -> Result<T3Snapshot, T3ImportError> {
        snapshot::snapshot(src.as_ref())
    }

    /// Ledger of a database: what is recorded, and which Turin migrations would run.
    ///
    /// Opens `path` with `SQLITE_OPEN_READ_ONLY`, so it is safe on a T3 file
    /// that is in use. A missing path is an error, not a new file. Refuses when
    /// the ledger is newer than this build or would make the migrator skip a
    /// Turin migration.
    pub fn plan(path: impl AsRef<Path>) -> Result<ImportPlan, T3ImportError> {
        let conn = snapshot::open_read_only(path.as_ref())?;
        plan(&conn)
    }

    /// Apply pending Turin migrations to a snapshot, then check schema, row
    /// counts, and event-store reads. Only a [`T3Snapshot`] is accepted, so the
    /// live T3 file cannot reach the writable open.
    pub fn migrate_and_verify(snapshot: &T3Snapshot) -> Result<ImportReport, T3ImportError> {
        let conn = snapshot::open_snapshot(snapshot.path())?;
        migrate_and_verify(&conn)
    }

    /// Snapshot `src`, migrate and verify the copy, then hard-link it to
    /// `{turin_data_dir}/statev2.sqlite`.
    ///
    /// Refuses when the destination file already exists. `src` is never opened
    /// for writing. A failed check deletes the copy and leaves the destination
    /// absent.
    pub fn import_into(
        src: impl AsRef<Path>,
        turin_data_dir: impl AsRef<Path>,
    ) -> Result<ImportReport, T3ImportError> {
        import_into(src.as_ref(), turin_data_dir.as_ref(), backup_to)
    }

    /// Like [`Self::import_into`], for an app that already has its database open: verify the
    /// copy and park it under `{turin_data_dir}/t3-import-staged` for [`Self::adopt_staged`].
    /// A newer stage replaces an earlier one.
    pub fn stage_into(
        src: impl AsRef<Path>,
        turin_data_dir: impl AsRef<Path>,
    ) -> Result<ImportReport, T3ImportError> {
        stage_into(src.as_ref(), turin_data_dir.as_ref(), backup_to)
    }

    /// Swap a staged import in for Turin's database. Call before the database is opened. Only an
    /// absent database or one with no projects and no threads is replaced; the old file is kept as
    /// `statev2.sqlite.before-t3-import`.
    pub fn adopt_staged(turin_data_dir: impl AsRef<Path>) -> Result<AdoptStaged, T3ImportError> {
        adopt_staged(turin_data_dir.as_ref())
    }
}

/// Failure while copying, checking, or adopting a T3 database.
#[derive(Debug, thiserror::Error)]
pub enum T3ImportError {
    #[error("refusing to import a T3 database this Turin build does not understand: {message}")]
    NewerThanTurin { message: String },
    #[error("{message}")]
    WouldSkipMigrations { message: String },
    #[error("{message}")]
    SchemaMismatch { message: String },
    #[error("{message}")]
    RowCountChanged { message: String },
    #[error("event decode failed: {message}")]
    Decode { message: String },
    #[error(
        "Turin database already exists at {path}. Import only creates it when that file is absent."
    )]
    DestinationExists { path: String },
    #[error("source and destination are the same file")]
    SameFile,
    #[error("not a T3 v2 database (missing effect_sql_migrations)")]
    NotV2,
    #[error(transparent)]
    Persistence(#[from] crate::errors::PersistenceError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl T3ImportError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests;
