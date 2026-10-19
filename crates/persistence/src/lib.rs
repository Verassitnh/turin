//! Port of upstream/apps/server/src/persistence
//!
//! SQLite connection, Effect-compatible migrations, and the repositories Turin keeps.

mod application_event_source;
mod db;
mod errors;
mod initialize_v2_database;
mod legacy_pull_request_key;
mod migration_manifest;
mod migrations;
mod orchestration_command_receipts;
mod orchestration_event_store;
mod provider_session_runtime;
mod pull_request_files_viewed;
mod pull_request_review_guides;
mod sql_value;
mod t3_import;
mod time;

pub use db::{Db, WAL_SIZE_LIMIT_BYTES};
pub use errors::{
    MigrationErrorKind, OrchestrationCommandReceiptRepositoryError, OrchestrationEventStoreError,
    PersistenceDecodeError, PersistenceError, PersistenceErrorCorrelation, PersistenceSqlError,
    ProviderSessionRuntimeRepositoryError, PullRequestFilesViewedRepositoryError,
    PullRequestReviewGuideRepositoryError,
};
pub use initialize_v2_database::initialize_v2_database;
pub use migrations::{RunMigrationsOptions, migration_manifest, run_migrations};
pub use orchestration_command_receipts::{
    AggregateKind, OrchestrationCommandReceipt, ReceiptStatus, get_by_command_id, insert_if_absent,
    upsert,
};
pub use orchestration_event_store::{
    AgentEvent, AgentReplayStats, AppendAgentEvents, ApplicationStoredEvent, EventSubscription,
    LIVE_STREAM_MAX_ITEMS, OrchestrationEventStore, ProjectEvent, ProjectEventInput,
    ReadAgentEvents, ReadApplicationEvents, ReplayStats, StoredAgentEvent,
    latest_application_sequence,
};
pub use provider_session_runtime::{
    ProviderSessionRuntime, ProviderSessionStatus, UpsertConflict, delete_by_thread_id,
    get_by_thread_id as get_provider_session, list as list_provider_sessions,
    record_imported_transcript, upsert as upsert_provider_session,
};
pub use pull_request_files_viewed::{
    MAX_FILES_VIEWED_ROWS, PullRequestFileViewedMark, PullRequestFileViewedWrite,
    PullRequestFilesViewedPage, PullRequestFilesViewedScope,
    list as list_pull_request_files_viewed, set as set_pull_request_files_viewed,
};
pub use pull_request_review_guides::{
    ThreadPullRequestKey, get as get_pull_request_review_guide,
    put as put_pull_request_review_guide,
};
pub use sql_value::SqlValue;
pub use t3_import::{
    AdoptStaged, EventReadReport, ImportPlan, ImportReport, STAGED_IMPORT_DIR, T3Import,
    T3ImportError, T3Snapshot, TURIN_DATABASE_FILE,
};
pub use time::{format_utc_millis, utc_millis_now};

#[cfg(test)]
mod behavior_tests;
#[cfg(test)]
mod migrations_test;
#[cfg(test)]
mod upstream_migration_tests;
