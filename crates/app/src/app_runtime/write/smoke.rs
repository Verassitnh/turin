//! Hands-free smoke for the write path, for runs where nobody can type:
//!
//! ```text
//! TURIN_DATA_DIR=/tmp/turin-smoke \
//! TURIN_CLAUDE_BINARY=<dir>/claude          # a fake CLI, see crates/app/tests/fixtures
//! TURIN_SMOKE_WORKSPACE=<an existing dir>   # the project's folder
//! TURIN_SMOKE_SEND="Respond with exactly: first fixture turn complete"
//! cargo run -p turin
//! ```
//!
//! Once the write path is up it creates (or reuses) a project on the
//! workspace, creates a thread and sends the message through the same
//! [`Commands`] the composer uses. The sidebar then shows the thread working
//! and settling, and the log lines below report each step.

use super::commands::{Commands, CreateThread, Delivery, SendMessage};

pub const WORKSPACE_ENV: &str = "TURIN_SMOKE_WORKSPACE";
pub const SEND_ENV: &str = "TURIN_SMOKE_SEND";

pub async fn run_if_requested(commands: Commands) {
    let (Ok(workspace), Ok(prompt)) = (std::env::var(WORKSPACE_ENV), std::env::var(SEND_ENV))
    else {
        return;
    };
    match run(&commands, &workspace, &prompt).await {
        Ok(()) => tracing::info!("smoke: sent {prompt:?} into a new thread in {workspace}"),
        Err(message) => tracing::error!("smoke failed: {message}"),
    }
}

async fn run(commands: &Commands, workspace: &str, prompt: &str) -> Result<(), String> {
    let title = crate::state::projects::infer_project_title_from_path(workspace);
    let project_id = commands
        .create_project(&title, workspace)
        .await
        .map_err(|error| error.message)?;
    let thread_id = commands
        .create_thread(CreateThread {
            project_id,
            title: "Smoke".into(),
            model_selection: serde_json::json!({
                "instanceId": "claudeAgent",
                "model": "claude-sonnet-4-6",
            }),
            runtime_mode: "auto".into(),
            interaction_mode: "default".into(),
            branch: None,
            worktree_path: None,
        })
        .await
        .map_err(|error| error.message)?;
    tracing::info!(%thread_id, "smoke: thread created");
    commands
        .send_message(SendMessage {
            thread_id,
            text: prompt.to_owned(),
            attachments: Vec::new(),
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
            source_plan_id: None,
        })
        .await
        .map_err(|error| error.message)?;
    Ok(())
}
