//! Port of upstream/packages/provider-core/src/server/McpProviderSessions.ts.
//!
//! In-memory map the session manager writes before a session starts and
//! adapters read when they build launch options. Nothing in Turin fills it
//! while the orchestrator MCP server stays cut.

use std::collections::HashMap;

use parking_lot::Mutex;
use turin_contracts::ThreadId;

use crate::mcp_session::McpProviderSessionConfig;

#[derive(Default)]
pub struct McpProviderSessions {
    sessions: Mutex<HashMap<ThreadId, McpProviderSessionConfig>>,
}

impl McpProviderSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, config: McpProviderSessionConfig) {
        self.sessions
            .lock()
            .insert(config.thread_id.clone(), config);
    }

    pub fn read(&self, thread_id: &ThreadId) -> Option<McpProviderSessionConfig> {
        self.sessions.lock().get(thread_id).cloned()
    }

    pub fn clear(&self, thread_id: &ThreadId) {
        self.sessions.lock().remove(thread_id);
    }
}
