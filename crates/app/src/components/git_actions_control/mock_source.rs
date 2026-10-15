//! In-memory [`GitActionsBackend`] until T22 wires `turin-git`. Not an
//! upstream module. `TURIN_GIT_SCENARIO` (screenshots) picks the repository
//! state: `dirty` (default), `ahead`, `clean`, `behind`, `no-remote`,
//! `not-repo`, `default-ref`, `pr-open`, `detached`, `running`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Task};
use serde_json::{Value, json};
use turin_contracts::git::{GitRunStackedActionResult, VcsPullResult, VcsStatusResult};
use turin_contracts::source_control::SourceControlPublishRepositoryResult;

use super::backend::{
    GitActionTarget, GitActionsBackend, GitStatusView, PublishProviderState,
    PublishRepositoryInput, RunStackedActionInput, VcsActionState,
};
use crate::timestamp_format::now_ms;

pub struct MockGitActionsBackend {
    state: Rc<RefCell<MockState>>,
}

struct MockState {
    status: Value,
    action: VcsActionState,
}

fn status_json(scenario: &str) -> Value {
    let mut base = json!({
        "isRepo": true,
        "sourceControlProvider": { "kind": "github", "name": "GitHub", "baseUrl": "https://github.com" },
        "hasPrimaryRemote": true,
        "isDefaultRef": false,
        "refName": "task/U1-palette-git-toolbar",
        "hasWorkingTreeChanges": true,
        "workingTree": {
            "files": [
                { "path": "crates/app/src/components/command_palette.rs", "insertions": 1240, "deletions": 18 },
                { "path": "crates/app/src/components/git_actions_control.rs", "insertions": 612, "deletions": 0 },
                { "path": "crates/app/src/components/branch_toolbar.rs", "insertions": 190, "deletions": 4 },
                { "path": "PORTING.md", "insertions": 36, "deletions": 2 },
            ],
            "insertions": 2078,
            "deletions": 24,
        },
        "branchChanges": { "baseRef": "main", "insertions": 2310, "deletions": 31 },
        "hasUpstream": true,
        "aheadCount": 0,
        "behindCount": 0,
        "aheadOfDefaultCount": 2,
        "pr": null,
    });
    let object = base.as_object_mut().unwrap();
    let mut set = |key: &str, value: Value| {
        object.insert(key.to_owned(), value);
    };
    let clean = || json!({ "files": [], "insertions": 0, "deletions": 0 });
    match scenario {
        "ahead" => {
            set("hasWorkingTreeChanges", json!(false));
            set("workingTree", clean());
            set("aheadCount", json!(3));
        }
        "clean" => {
            set("hasWorkingTreeChanges", json!(false));
            set("workingTree", clean());
            set("aheadOfDefaultCount", json!(0));
        }
        "behind" => {
            set("hasWorkingTreeChanges", json!(false));
            set("workingTree", clean());
            set("behindCount", json!(2));
        }
        "no-remote" => {
            set("hasPrimaryRemote", json!(false));
            set("hasUpstream", json!(false));
        }
        "not-repo" => {
            set("isRepo", json!(false));
        }
        "default-ref" => {
            set("refName", json!("main"));
            set("isDefaultRef", json!(true));
        }
        "detached" => {
            set("refName", Value::Null);
        }
        "pr-open" => {
            set("hasWorkingTreeChanges", json!(false));
            set("workingTree", clean());
            set("aheadCount", json!(1));
            set(
                "pr",
                json!({
                    "number": 42,
                    "title": "Palette, git actions and branch toolbar",
                    "url": "https://github.com/acme/turin/pull/42",
                    "baseRef": "main",
                    "headRef": "task/U1-palette-git-toolbar",
                    "state": "open",
                }),
            );
        }
        _ => {}
    }
    base
}

impl MockGitActionsBackend {
    pub fn new() -> Self {
        let scenario = std::env::var("TURIN_GIT_SCENARIO").unwrap_or_default();
        let mut action = VcsActionState::default();
        if scenario == "running" {
            action = VcsActionState {
                is_running: true,
                operation: Some("run_change_request".into()),
                current_label: Some("Generating commit message...".into()),
                last_output_line: Some("Staging 4 files for the commit".into()),
                phase_started_at_ms: Some(now_ms() - 12_000),
                hook_started_at_ms: None,
            };
        }
        Self {
            state: Rc::new(RefCell::new(MockState {
                status: status_json(&scenario),
                action,
            })),
        }
    }
}

impl Default for MockGitActionsBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("mock fixture decodes")
}

impl GitActionsBackend for MockGitActionsBackend {
    fn status(&self, _: &str, _: &App) -> GitStatusView {
        GitStatusView {
            status: Some(parse::<VcsStatusResult>(self.state.borrow().status.clone())),
            error: None,
        }
    }

    fn action_state(&self, _: &str, _: &App) -> VcsActionState {
        self.state.borrow().action.clone()
    }

    fn refresh_status(&self, _: &str, _: &mut App) {}

    fn run_stacked_action(
        &self,
        _: &GitActionTarget,
        input: RunStackedActionInput,
        cx: &mut App,
    ) -> Task<Result<GitRunStackedActionResult, String>> {
        let state = self.state.clone();
        {
            let mut state = state.borrow_mut();
            state.action = VcsActionState {
                is_running: true,
                operation: Some("run_change_request".into()),
                current_label: Some("Running source control action".into()),
                last_output_line: None,
                phase_started_at_ms: Some(now_ms()),
                hook_started_at_ms: None,
            };
        }
        cx.spawn(async move |cx| {
            let steps = [
                ("Generating commit message...", "Reading the staged diff"),
                ("Committing...", "[feature 89abcde] feat: add palette"),
            ];
            for (label, output) in steps {
                cx.background_executor().timer(Duration::from_millis(900)).await;
                let mut state = state.borrow_mut();
                state.action.current_label = Some(label.into());
                state.action.last_output_line = Some(output.into());
            }
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let mut state = state.borrow_mut();
            state.action = VcsActionState::default();
            let object = state.status.as_object_mut().unwrap();
            object.insert("hasWorkingTreeChanges".into(), json!(false));
            object.insert(
                "workingTree".into(),
                json!({ "files": [], "insertions": 0, "deletions": 0 }),
            );
            object.insert("aheadCount".into(), json!(1));
            let branch = if input.feature_branch {
                json!({ "status": "created", "name": "feature/palette" })
            } else {
                json!({ "status": "skipped_not_requested" })
            };
            Ok(parse(json!({
                "action": "commit",
                "branch": branch,
                "commit": { "status": "created", "commitSha": "89abcdef01234567", "subject": "feat: add palette" },
                "push": { "status": "skipped_not_requested" },
                "pr": { "status": "skipped_not_requested" },
                "toast": {
                    "title": "Committed 89abcde",
                    "description": "feat: add palette",
                    "cta": {
                        "kind": "run_action",
                        "label": "Push & create PR",
                        "action": { "kind": "commit_push_pr" },
                    },
                },
            })))
        })
    }

    fn pull(&self, _: &GitActionTarget, _: &mut App) -> Task<Result<VcsPullResult, String>> {
        Task::ready(Ok(parse(json!({
            "status": "pulled",
            "refName": "task/U1-palette-git-toolbar",
            "upstreamRef": "origin/task/U1-palette-git-toolbar",
        }))))
    }

    fn init(&self, _: &GitActionTarget, _: &mut App) -> Task<Result<(), String>> {
        let state = self.state.clone();
        state
            .borrow_mut()
            .status
            .as_object_mut()
            .unwrap()
            .insert("isRepo".into(), json!(true));
        Task::ready(Ok(()))
    }

    fn publish_provider(&self, _: &App) -> PublishProviderState {
        PublishProviderState {
            label: "GitHub".into(),
            ready: true,
            hint: None,
            account: Some("acme".into()),
            host: "github.com".into(),
        }
    }

    fn publish_repository(
        &self,
        _: &GitActionTarget,
        input: PublishRepositoryInput,
        cx: &mut App,
    ) -> Task<Result<SourceControlPublishRepositoryResult, String>> {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(800)).await;
            Ok(parse(json!({
                "repository": {
                    "provider": "github",
                    "nameWithOwner": input.repository,
                    "url": format!("https://github.com/{}", input.repository),
                    "sshUrl": format!("git@github.com:{}.git", input.repository),
                },
                "remoteName": input.remote_name,
                "remoteUrl": format!("git@github.com:{}.git", input.repository),
                "branch": "main",
                "upstreamBranch": "origin/main",
                "status": "pushed",
            })))
        })
    }

    fn update_thread_branch(
        &self,
        _: &GitActionTarget,
        _: Option<String>,
        _: Option<String>,
        _: &mut App,
    ) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }

    fn open_in_editor(&self, _: String, _: &str, _: &mut App) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }

    fn open_link(&self, url: String, cx: &mut App) {
        cx.open_url(&url);
    }
}
