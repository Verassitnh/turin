//! The thread view's "Pull requests" tab over the live pull request store: the
//! thread's linked pull requests (`ThreadPullRequestsPanel`) and the link dialog
//! its Link button opens (`LinkPullRequestDialog`, with the thread's project
//! and the environment's linking capability).

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};
use turin_contracts::ThreadId;
use turin_contracts::environment::{ExecutionEnvironmentCapabilities, RepositoryIdentity};

use super::link_dialog::{
    LinkCommands, LinkDialogProject, LinkPullRequestDialog, LinkPullRequestDialogEvent,
};
use super::thread_panel::{ThreadPullRequestsPanel, ThreadPullRequestsPanelEvent};
use super::view_model::PullRequestStore;
use crate::hooks::use_pull_request_linking::{LinkingProject, PullRequestLinking};
use crate::state::client_state::ClientState;

/// The thread's repository as the link dialog reads it: a bare number
/// resolves against it. The identity's canonical key is `host/owner/name`.
pub fn link_dialog_project(identity: &RepositoryIdentity) -> Option<LinkDialogProject> {
    let (host, repository) = identity.canonical_key.as_str().split_once('/')?;
    (!host.is_empty() && repository.contains('/')).then(|| LinkDialogProject {
        host: host.to_owned(),
        repository: repository.to_owned(),
        readable_repositories: vec![(host.to_owned(), repository.to_owned())],
    })
}

/// `PullRequestLinking` for this environment: it links threads to any pull
/// request it can read, resolved against the known projects.
pub fn environment_linking(state: &ClientState) -> PullRequestLinking {
    let projects = state
        .shell_view
        .as_ref()
        .map(|view| {
            view.projects
                .iter()
                .map(|project| LinkingProject {
                    id: project.id.clone(),
                    repository_identity: project
                        .source
                        .as_ref()
                        .and_then(|source| source.repository_identity.clone().flatten()),
                })
                .collect()
        })
        .unwrap_or_default();
    PullRequestLinking::new(
        Some(ExecutionEnvironmentCapabilities {
            thread_pull_requests: Some(true),
            ..Default::default()
        }),
        projects,
    )
}

pub struct ThreadPullRequestsSurface {
    panel: Entity<ThreadPullRequestsPanel>,
    store: Entity<PullRequestStore>,
    thread_id: ThreadId,
    linking: PullRequestLinking,
    project: Option<LinkDialogProject>,
    dialog: Option<Entity<LinkPullRequestDialog>>,
    _subscription: Subscription,
}

impl ThreadPullRequestsSurface {
    pub fn new(
        store: Entity<PullRequestStore>,
        thread_id: ThreadId,
        linking: PullRequestLinking,
        project: Option<LinkDialogProject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let panel =
            cx.new(|cx| ThreadPullRequestsPanel::new(store.clone(), thread_id.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &ThreadPullRequestsPanelEvent, window, cx| match event {
                ThreadPullRequestsPanelEvent::OpenLinkDialog => this.open_link_dialog(window, cx),
                ThreadPullRequestsPanelEvent::Open { url } => cx.open_url(url),
            },
        );
        Self {
            panel,
            store,
            thread_id,
            linking,
            project,
            dialog: None,
            _subscription: subscription,
        }
    }

    /// Keeps what the dialog resolves against current as projects change.
    pub fn set_linking(&mut self, linking: PullRequestLinking, project: Option<LinkDialogProject>) {
        self.linking = linking;
        self.project = project;
    }

    fn open_link_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commands = LinkCommands {
            linking: self.linking.clone(),
            thread_id: self.thread_id.clone(),
            store: self.store.clone(),
        };
        let project = self.project.clone();
        let dialog =
            cx.new(|cx| LinkPullRequestDialog::new(project, window, cx).with_commands(commands));
        cx.subscribe(&dialog, |this, _, _: &LinkPullRequestDialogEvent, cx| {
            // Linked (the dialog sent the command itself) or dismissed.
            this.dialog = None;
            cx.notify();
        })
        .detach();
        self.dialog = Some(dialog);
        cx.notify();
    }
}

impl Render for ThreadPullRequestsSurface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(self.panel.clone())
            .children(self.dialog.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(key: &str) -> RepositoryIdentity {
        serde_json::from_value(json!({
            "canonicalKey": key,
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "https://example.com/x.git",
            },
        }))
        .expect("identity")
    }

    #[test]
    fn the_dialog_resolves_numbers_against_the_threads_repository() {
        let project = link_dialog_project(&identity("github.com/acme/turin")).expect("project");
        assert_eq!(project.host, "github.com");
        assert_eq!(project.repository, "acme/turin");
        assert_eq!(
            project.readable_repositories,
            vec![("github.com".to_owned(), "acme/turin".to_owned())]
        );
    }

    #[test]
    fn a_key_that_is_not_host_owner_name_has_no_project() {
        assert!(link_dialog_project(&identity("local-only")).is_none());
        assert!(link_dialog_project(&identity("github.com")).is_none());
    }
}
