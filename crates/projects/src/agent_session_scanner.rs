//! Port of upstream/apps/server/src/project/AgentSessionScanner.ts: the model
//! types and the `recentThreads` seam. The transcript scan itself follows in
//! later slices of this module.

use futures::stream::BoxStream;
use turin_contracts::agent_sessions::{
    AgentSessionImportSource, AgentSessionScanError, AgentSessionSource,
};
use turin_contracts::provider_instance::ProviderInstanceId;

/// Author of a transcript message. Upstream `"user" | "assistant"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionMessageRole {
    User,
    Assistant,
}

/// One user or assistant message recovered from a transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSessionThreadMessage {
    pub role: AgentSessionMessageRole,
    pub text: String,
    /// ISO-8601, normalized by the scanner (`normalizeTimestamp`).
    pub created_at: String,
}

/// A transcript read back into a thread the importer can write.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSessionThread {
    pub source: AgentSessionSource,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<AgentSessionThreadMessage>,
}

/// What the scanner decided about one transcript. Upstream `AgentSessionRecentThread`.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentSessionRecentThread {
    /// New transcript: the importer should write it as a thread.
    Importable {
        thread: AgentSessionThread,
        source: AgentSessionImportSource,
    },
    /// Its file identity matches a source an earlier import completed.
    AlreadyImported {
        source: AgentSessionImportSource,
    },
    /// Same provider session appears under another file in this scan.
    Duplicate {
        source: AgentSessionImportSource,
    },
    Skipped,
}

/// Transcript discovery for one workspace. The scanner is best-effort:
/// unreadable transcripts are `Skipped`, and only a failing read of the
/// scan itself becomes an error item.
pub trait AgentSessionScanner: Send + Sync {
    /// Recent transcripts whose `cwd` is `workspace_root`, newest first.
    /// `completed_sources` are the file identities already imported for it.
    fn recent_threads<'a>(
        &'a self,
        workspace_root: &'a str,
        completed_sources: &'a [AgentSessionImportSource],
    ) -> BoxStream<'a, Result<AgentSessionRecentThread, AgentSessionScanError>>;
}
