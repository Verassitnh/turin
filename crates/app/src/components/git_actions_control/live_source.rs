//! Live [`GitActionsBackend`] via `turin-git`. Switches to
//! `Commands::git_status` when the runtime facade lands (see WIRE-runtime.md).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, AppContext as _, Task};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::git::{GitRunStackedActionResult, VcsPullResult, VcsStatusInput};
use turin_contracts::source_control::SourceControlPublishRepositoryResult;
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::process::ProcessRunner;
use turin_git::vcs_process::VcsProcess;

use super::backend::{
    GitActionTarget, GitActionsBackend, GitStatusView, PublishProviderState,
    PublishRepositoryInput, RunStackedActionInput, VcsActionState,
};
use crate::app_runtime::write::bus::CommandBus;

fn shared_driver() -> Arc<GitVcsDriver> {
    Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner)))
}

pub struct LiveGitActionsBackend {
    bus: CommandBus,
    driver: Arc<GitVcsDriver>,
    status_by_cwd: Rc<RefCell<HashMap<String, GitStatusView>>>,
    action: Rc<RefCell<VcsActionState>>,
}

impl LiveGitActionsBackend {
    pub fn new(bus: CommandBus) -> Self {
        Self {
            bus,
            driver: shared_driver(),
            status_by_cwd: Rc::new(RefCell::new(HashMap::new())),
            action: Rc::new(RefCell::new(VcsActionState::default())),
        }
    }

    fn fetch_status(&self, cwd: &str, cx: &mut App) {
        let cwd = cwd.to_owned();
        let driver = Arc::clone(&self.driver);
        let cache = self.status_by_cwd.clone();
        let handle = self.bus.handle().clone();
        cx.spawn(async move |cx| {
            let result = handle
                .spawn(async move {
                    let input = VcsStatusInput {
                        cwd: TrimmedNonEmptyString::new(cwd.clone()).expect("non-empty cwd"),
                    };
                    driver.status(&input).await
                })
                .await
                .ok();
            let view = match result {
                Some(Ok(status)) => GitStatusView {
                    status: Some(status),
                    error: None,
                },
                Some(Err(error)) => GitStatusView {
                    status: None,
                    error: Some(error.to_string()),
                },
                None => GitStatusView {
                    status: None,
                    error: Some("Git status was interrupted.".into()),
                },
            };
            cache.borrow_mut().insert(cwd, view);
            cx.update(|cx| cx.notify());
        })
        .detach();
    }
}

impl GitActionsBackend for LiveGitActionsBackend {
    fn status(&self, cwd: &str, _: &App) -> GitStatusView {
        self.status_by_cwd
            .borrow()
            .get(cwd)
            .cloned()
            .unwrap_or(GitStatusView {
                status: None,
                error: None,
            })
    }

    fn action_state(&self, _: &str, _: &App) -> VcsActionState {
        self.action.borrow().clone()
    }

    fn refresh_status(&self, cwd: &str, cx: &mut App) {
        self.fetch_status(cwd, cx);
    }

    fn run_stacked_action(
        &self,
        _: &GitActionTarget,
        _: RunStackedActionInput,
        cx: &mut App,
    ) -> Task<Result<GitRunStackedActionResult, String>> {
        cx.spawn(async move |_| {
            Err("Git actions are not wired to the orchestrator yet.".into())
        })
    }

    fn pull(
        &self,
        _: &GitActionTarget,
        cx: &mut App,
    ) -> Task<Result<VcsPullResult, String>> {
        cx.spawn(async move |_| Err("Pull is not wired yet.".into()))
    }

    fn init(&self, _: &GitActionTarget, cx: &mut App) -> Task<Result<(), String>> {
        cx.spawn(async move |_| Err("Init is not wired yet.".into()))
    }

    fn publish_provider(&self, _: &App) -> PublishProviderState {
        PublishProviderState {
            ready: false,
            hint: Some("Connect GitHub in Settings.".into()),
            account: None,
            host: "github.com".into(),
        }
    }

    fn publish_repository(
        &self,
        _: &GitActionTarget,
        _: PublishRepositoryInput,
        cx: &mut App,
    ) -> Task<Result<SourceControlPublishRepositoryResult, String>> {
        cx.spawn(async move |_| Err("Publish is not wired yet.".into()))
    }

    fn update_thread_branch(
        &self,
        _: &GitActionTarget,
        _: Option<String>,
        _: Option<String>,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        cx.spawn(async move |_| Err("Branch metadata update is not wired yet.".into()))
    }

    fn open_in_editor(&self, _: String, _: &str, cx: &mut App) -> Task<Result<(), String>> {
        cx.spawn(async move |_| Err("Open in editor is not wired yet.".into()))
    }

    fn open_link(&self, url: String, cx: &mut App) {
        let _ = cx.open_url(&url);
    }
}
