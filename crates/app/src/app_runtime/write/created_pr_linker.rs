//! Port of upstream/apps/server/src/git/linkCreatedPullRequest.ts (the dispatch half).
//!
//! `GitManager` computes the key of the pull request a `create_pr`-shaped
//! action produced; this linker dispatches `thread.pull-request.link` for it
//! (`source: "created"`) to the thread the action ran beside. It never fails:
//! the git action already succeeded, so a link that cannot be made is logged
//! and dropped. A duplicate link is the decider saying the thread already knew.
//!
//! The git manager is built before the runtime that owns the orchestrator, so
//! the linker is bound to the runtime afterwards ([`LateCreatedPullRequestLinker::bind`]).

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde_json::json;
use turin_git::git_manager::CreatedPullRequestLinker;
use turin_git::link_created_pull_request::CreatedPullRequestKey;

use super::commands::command_json;
use super::runtime::WriteRuntime;

#[derive(Default)]
pub struct LateCreatedPullRequestLinker {
    runtime: OnceLock<Weak<WriteRuntime>>,
}

impl LateCreatedPullRequestLinker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Weak: the runtime owns the git manager that owns this linker.
    pub fn bind(&self, runtime: &Arc<WriteRuntime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }
}

/// The `thread.pull-request.link` command for a created pull request.
pub(super) fn link_command(thread_id: &str, key: &CreatedPullRequestKey) -> serde_json::Value {
    json!({
        "type": "thread.pull-request.link",
        "commandId": uuid::Uuid::new_v4().to_string(),
        "threadId": thread_id,
        "host": key.host,
        "repository": key.repository,
        "number": key.number,
        "url": key.url,
        "source": "created",
    })
}

#[async_trait]
impl CreatedPullRequestLinker for LateCreatedPullRequestLinker {
    async fn link(&self, thread_id: &str, key: &CreatedPullRequestKey) {
        let Some(runtime) = self.runtime.get().and_then(Weak::upgrade) else {
            tracing::warn!(
                thread_id,
                "created pull request not linked: runtime is gone"
            );
            return;
        };
        let command = match command_json(link_command(thread_id, key)) {
            Ok(command) => command,
            Err(error) => {
                tracing::warn!(thread_id, %error, "failed to link created pull request to thread");
                return;
            }
        };
        if let Err(error) = runtime.dispatch(command).await {
            tracing::warn!(thread_id, %error, "failed to link created pull request to thread");
        }
    }
}
