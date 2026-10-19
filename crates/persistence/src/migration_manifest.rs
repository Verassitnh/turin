//! Migration scripts copied from
//! `upstream/apps/server/src/persistence/Migrations`. No generator is checked
//! in. SQL is the upstream template text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationKind {
    Sql,
    /// Each `ALTER TABLE ... ADD COLUMN` runs only when `PRAGMA table_info` lacks that column.
    /// Other statements in the script (indexes) always run. Matches upstream's table_info guards.
    AddColumnIfMissing,
    /// Each statement is attempted on its own and errors are ignored. Migration 023's `Effect.catch`.
    CatchStatementErrors,
    PullRequests,
    OrchestrationV2,
    /// Migration 064: no SQL, only the paged JSON rewrite in
    /// `legacy_pull_request_key::canonicalize_thread_pull_request_links`.
    CanonicalizeThreadPullRequestLinks,
}

pub struct MigrationSpec {
    pub id: i64,
    pub name: &'static str,
    pub sql: &'static str,
    pub kind: MigrationKind,
}

pub const APPLICATION_EVENT_SOURCE_MARKER: &str = "-- TURIN_APPLICATION_EVENT_SOURCE --";

pub static MIGRATIONS: &[MigrationSpec] = &[
    MigrationSpec {
        id: 1,
        name: "OrchestrationEvents",
        sql: include_str!("../sql/001_OrchestrationEvents.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 2,
        name: "OrchestrationCommandReceipts",
        sql: include_str!("../sql/002_OrchestrationCommandReceipts.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 3,
        name: "CheckpointDiffBlobs",
        sql: include_str!("../sql/003_CheckpointDiffBlobs.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 4,
        name: "ProviderSessionRuntime",
        sql: include_str!("../sql/004_ProviderSessionRuntime.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 5,
        name: "Projections",
        sql: include_str!("../sql/005_Projections.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 6,
        name: "ProjectionThreadSessionRuntimeModeColumns",
        sql: include_str!("../sql/006_ProjectionThreadSessionRuntimeModeColumns.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 7,
        name: "ProjectionThreadMessageAttachments",
        sql: include_str!("../sql/007_ProjectionThreadMessageAttachments.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 8,
        name: "ProjectionThreadActivitySequence",
        sql: include_str!("../sql/008_ProjectionThreadActivitySequence.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 9,
        name: "ProviderSessionRuntimeMode",
        sql: include_str!("../sql/009_ProviderSessionRuntimeMode.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 10,
        name: "ProjectionThreadsRuntimeMode",
        sql: include_str!("../sql/010_ProjectionThreadsRuntimeMode.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 11,
        name: "OrchestrationThreadCreatedRuntimeMode",
        sql: include_str!("../sql/011_OrchestrationThreadCreatedRuntimeMode.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 12,
        name: "ProjectionThreadsInteractionMode",
        sql: include_str!("../sql/012_ProjectionThreadsInteractionMode.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 13,
        name: "ProjectionThreadProposedPlans",
        sql: include_str!("../sql/013_ProjectionThreadProposedPlans.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 14,
        name: "ProjectionThreadProposedPlanImplementation",
        sql: include_str!("../sql/014_ProjectionThreadProposedPlanImplementation.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 15,
        name: "ProjectionTurnsSourceProposedPlan",
        sql: include_str!("../sql/015_ProjectionTurnsSourceProposedPlan.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 16,
        name: "CanonicalizeModelSelections",
        sql: include_str!("../sql/016_CanonicalizeModelSelections.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 17,
        name: "ProjectionThreadsArchivedAt",
        sql: include_str!("../sql/017_ProjectionThreadsArchivedAt.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 18,
        name: "ProjectionThreadsArchivedAtIndex",
        sql: include_str!("../sql/018_ProjectionThreadsArchivedAtIndex.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 19,
        name: "ProjectionSnapshotLookupIndexes",
        sql: include_str!("../sql/019_ProjectionSnapshotLookupIndexes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 20,
        name: "AuthAccessManagement",
        sql: include_str!("../sql/020_AuthAccessManagement.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 21,
        name: "AuthSessionClientMetadata",
        sql: include_str!("../sql/021_AuthSessionClientMetadata.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 22,
        name: "AuthSessionLastConnectedAt",
        sql: include_str!("../sql/022_AuthSessionLastConnectedAt.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 23,
        name: "ProjectionThreadShellSummary",
        sql: include_str!("../sql/023_ProjectionThreadShellSummary.sql"),
        kind: MigrationKind::CatchStatementErrors,
    },
    MigrationSpec {
        id: 24,
        name: "BackfillProjectionThreadShellSummary",
        sql: include_str!("../sql/024_BackfillProjectionThreadShellSummary.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 25,
        name: "CleanupInvalidProjectionPendingApprovals",
        sql: include_str!("../sql/025_CleanupInvalidProjectionPendingApprovals.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 26,
        name: "CanonicalizeModelSelectionOptions",
        sql: include_str!("../sql/026_CanonicalizeModelSelectionOptions.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 27,
        name: "ProviderSessionRuntimeInstanceId",
        sql: include_str!("../sql/027_ProviderSessionRuntimeInstanceId.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 28,
        name: "ProjectionThreadSessionInstanceId",
        sql: include_str!("../sql/028_ProjectionThreadSessionInstanceId.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 29,
        name: "ProjectionThreadDetailOrderingIndexes",
        sql: include_str!("../sql/029_ProjectionThreadDetailOrderingIndexes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 30,
        name: "ProjectionThreadShellArchiveIndexes",
        sql: include_str!("../sql/030_ProjectionThreadShellArchiveIndexes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 31,
        name: "AuthAuthorizationScopes",
        sql: include_str!("../sql/031_AuthAuthorizationScopes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 32,
        name: "AuthPairingProofKeyThumbprint",
        sql: include_str!("../sql/032_AuthPairingProofKeyThumbprint.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 33,
        name: "ProjectionThreadsSettled",
        sql: include_str!("../sql/033_ProjectionThreadsSettled.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 34,
        name: "ProjectionThreadsSnoozed",
        sql: include_str!("../sql/034_ProjectionThreadsSnoozed.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 35,
        name: "ProjectionThreadTitleRegeneration",
        sql: include_str!("../sql/035_ProjectionThreadTitleRegeneration.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 36,
        name: "ProjectionThreadsPinned",
        sql: include_str!("../sql/036_ProjectionThreadsPinned.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 37,
        name: "ProjectionTurnsKeysetIndex",
        sql: include_str!("../sql/037_ProjectionTurnsKeysetIndex.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 38,
        name: "ProjectionThreadsPinOrderKey",
        sql: include_str!("../sql/038_ProjectionThreadsPinOrderKey.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 39,
        name: "ProjectionProjectsDefaultThreadEnvMode",
        sql: include_str!("../sql/039_ProjectionProjectsDefaultThreadEnvMode.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 40,
        name: "ProjectionProjectFaviconPath",
        sql: include_str!("../sql/040_ProjectionProjectFaviconPath.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 41,
        name: "AuthSessionClientConnection",
        sql: include_str!("../sql/041_AuthSessionClientConnection.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 42,
        name: "ProjectionThreadLinkedPullRequest",
        sql: include_str!("../sql/042_ProjectionThreadLinkedPullRequest.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 43,
        name: "ProjectionThreadsUnsettledAt",
        sql: include_str!("../sql/043_ProjectionThreadsUnsettledAt.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 44,
        name: "ClearAutomaticProjectModelDefaults",
        sql: include_str!("../sql/044_ClearAutomaticProjectModelDefaults.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 45,
        name: "ProjectionProjectsAutoPull",
        sql: include_str!("../sql/045_ProjectionProjectsAutoPull.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 46,
        name: "RepairAutomaticSettlementTimestamps",
        sql: include_str!("../sql/046_RepairAutomaticSettlementTimestamps.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 47,
        name: "ProjectionProjectIcon",
        sql: include_str!("../sql/047_ProjectionProjectIcon.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 48,
        name: "ProjectionThreadBranchPullRequest",
        sql: include_str!("../sql/048_ProjectionThreadBranchPullRequest.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 49,
        name: "ProjectionThreadsActiveOrderKey",
        sql: include_str!("../sql/049_ProjectionThreadsActiveOrderKey.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 50,
        name: "ProjectionThreadPullRequests",
        sql: include_str!("../sql/050_ProjectionThreadPullRequests.sql"),
        kind: MigrationKind::PullRequests,
    },
    MigrationSpec {
        id: 51,
        name: "ProjectionThreadMessageContext",
        sql: include_str!("../sql/051_ProjectionThreadMessageContext.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    MigrationSpec {
        id: 52,
        name: "ProjectionThreadTitleState",
        sql: include_str!("../sql/052_ProjectionThreadTitleState.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 53,
        name: "PullRequestFilesViewed",
        sql: include_str!("../sql/053_PullRequestFilesViewed.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 54,
        name: "ProjectionThreadsAutoSettleDisabledAt",
        sql: include_str!("../sql/054_ProjectionThreadsAutoSettleDisabledAt.sql"),
        kind: MigrationKind::AddColumnIfMissing,
    },
    // Released as 53 and 54 in V2 previews. Preserve this migration's schema.
    // Future V2 schema changes need new migrations.
    MigrationSpec {
        id: 55,
        name: "OrchestrationV2",
        sql: include_str!("../sql/055_OrchestrationV2.sql"),
        kind: MigrationKind::OrchestrationV2,
    },
    MigrationSpec {
        id: 56,
        name: "RemoveRedundantProjectionIndexes",
        sql: include_str!("../sql/056_RemoveRedundantProjectionIndexes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 57,
        name: "ScheduledTaskWebhooks",
        sql: include_str!("../sql/057_ScheduledTaskWebhooks.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 58,
        name: "WebhookRelayDeliveries",
        sql: include_str!("../sql/058_WebhookRelayDeliveries.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 59,
        name: "McpAppModelContext",
        sql: include_str!("../sql/059_McpAppModelContext.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 60,
        name: "ThreadSnapshotWindowIndexes",
        sql: include_str!("../sql/060_ThreadSnapshotWindowIndexes.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 61,
        name: "PullRequestReviewGuides",
        sql: include_str!("../sql/061_PullRequestReviewGuides.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 62,
        name: "ThreadFailureLookupIndex",
        sql: include_str!("../sql/062_ThreadFailureLookupIndex.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 63,
        name: "DropUnusedV1Projections",
        sql: include_str!("../sql/063_DropUnusedV1Projections.sql"),
        kind: MigrationKind::Sql,
    },
    MigrationSpec {
        id: 64,
        name: "CanonicalizeThreadPullRequestLinks",
        sql: include_str!("../sql/064_CanonicalizeThreadPullRequestLinks.sql"),
        kind: MigrationKind::CanonicalizeThreadPullRequestLinks,
    },
];
