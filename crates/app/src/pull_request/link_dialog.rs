//! Port of upstream/apps/web/src/components/pullRequest/LinkPullRequestDialog.tsx (the dialog
//! itself; the input resolution lives in `link_pull_request_dialog_logic`).
//!
//! "Link pull request": a URL or a bare `#123` resolves to a pull request of the thread's own
//! repository; the resolved target is previewed under the field, validation shows once the reader
//! has typed (or tried to submit), and Enter submits. The dialog reports
//! [`LinkPullRequestDialogEvent`]s; whoever hosts it links the thread through the source and
//! removes the overlay.
//!
//! Not ported: the per-environment `usePullRequestLinking` capability lookup (the host passes the
//! thread's own project and the set of readable repositories in [`LinkDialogProject`]) and focus
//! restore on close.

use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, px,
};

use super::link_pull_request_dialog_logic::{
    LinkPullRequestProject, LinkPullRequestResolution, ResolvedLink, link_pull_request_can_submit,
    link_pull_request_summary, link_pull_request_validation_message,
    resolve_link_pull_request_input,
};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::dialog::{
    dialog_description, dialog_overlay, dialog_title, overlay_dialog_footer, overlay_dialog_header,
    overlay_dialog_panel,
};
use crate::ui::text_input::text_input_frame;
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// The thread's own project, which a bare number resolves against.
#[derive(Clone, Debug)]
pub struct LinkDialogProject {
    pub host: String,
    pub repository: String,
    /// `github.com/acme/turin`-style repositories this environment can read.
    pub readable_repositories: Vec<(String, String)>,
}

impl LinkDialogProject {
    fn web_url(&self, number: u64) -> Option<String> {
        Some(format!(
            "https://{}/{}/pull/{number}",
            self.host, self.repository
        ))
    }

    fn can_link(&self, link: &ResolvedLink) -> bool {
        self.readable_repositories.iter().any(|(host, repository)| {
            host.eq_ignore_ascii_case(&link.host)
                && repository.eq_ignore_ascii_case(&link.repository)
        })
    }
}

#[derive(Clone, Debug)]
pub enum LinkPullRequestDialogEvent {
    Link(ResolvedLink),
    Dismiss,
}

pub struct LinkPullRequestDialog {
    input: Entity<InputState>,
    project: Option<LinkDialogProject>,
    reference: String,
    dirty: bool,
    submit_error: Option<String>,
    pending: bool,
    _subscription: Subscription,
}

impl EventEmitter<LinkPullRequestDialogEvent> for LinkPullRequestDialog {}

impl LinkPullRequestDialog {
    pub fn new(
        project: Option<LinkDialogProject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Pull request URL or #42"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, state, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.dirty = true;
                    this.reference = state.read(cx).value().to_string();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(cx),
                _ => {}
            },
        );
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        Self {
            input,
            project,
            reference: String::new(),
            dirty: false,
            submit_error: None,
            pending: false,
            _subscription: subscription,
        }
    }

    fn resolution(&self) -> Option<LinkPullRequestResolution> {
        let project = self.project.clone();
        let own = project
            .as_ref()
            .map(|project| (project.host.clone(), project.repository.clone()));
        let web_url = {
            let project = project.clone();
            move |number: u64| project.as_ref().and_then(|project| project.web_url(number))
        };
        let own_project = own
            .as_ref()
            .map(|(host, repository)| LinkPullRequestProject {
                host,
                repository,
                web_url: &web_url,
            });
        resolve_link_pull_request_input(&self.reference, own_project.as_ref(), &|link| {
            project
                .as_ref()
                .is_some_and(|project| project.can_link(link))
        })
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        if let Some(LinkPullRequestResolution::Link(link)) = self.resolution() {
            self.submit_error = None;
            cx.emit(LinkPullRequestDialogEvent::Link(link));
        }
        cx.notify();
    }

    /// Types `text` into the field (demo hook and paste targets).
    pub fn prefill(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.reference = text.to_owned();
        self.dirty = true;
        self.input
            .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        cx.notify();
    }

    /// The host reports a failed link so the dialog can show it and stay open.
    pub fn fail(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.pending = false;
        self.submit_error = Some(message.into());
        cx.notify();
    }
}

impl Render for LinkPullRequestDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let resolution = self.resolution();
        let validation =
            link_pull_request_validation_message(self.dirty, &self.reference, resolution.as_ref());
        let summary = link_pull_request_summary(resolution.as_ref());
        let error = validation.or_else(|| self.submit_error.clone());
        let can_submit = !self.pending && link_pull_request_can_submit(resolution.as_ref());
        let page = cx.entity().downgrade();
        let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = {
            let page = page.clone();
            Rc::new(move |_, cx| {
                page.update(cx, |_, cx| cx.emit(LinkPullRequestDialogEvent::Dismiss))
                    .ok();
            })
        };
        let cancel = dismiss.clone();
        let submit_page = page.clone();
        dialog_overlay(
            "link-pull-request-dialog",
            px(576.),
            dismiss,
            div()
                .flex()
                .flex_col()
                .child(
                    overlay_dialog_header(true)
                        .child(dialog_title("Link pull request"))
                        .child(dialog_description(
                            "Attach a pull request to this thread by its URL, or by its number for this thread's repository.",
                            cx,
                        )),
                )
                .child(
                    overlay_dialog_panel()
                        .child(
                            text_input_frame(cx)
                                .child(Input::new(&self.input).appearance(false).bordered(false)),
                        )
                        .children(summary.map(|summary| {
                            div()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(summary))
                        }))
                        .children(error.map(|error| {
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.destructive)
                                .child(SharedString::from(error))
                        })),
                )
                .child(
                    overlay_dialog_footer(cx)
                        .child(
                            Button::new("link-dialog-cancel")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .disabled(self.pending)
                                .label("Cancel")
                                .on_click(move |_, window, cx| cancel(window, cx)),
                        )
                        .child(
                            Button::new("link-dialog-submit")
                                .size(ButtonSize::Sm)
                                .disabled(!can_submit)
                                .label(if self.pending { "Linking..." } else { "Link" })
                                .on_click(move |_, _, cx| {
                                    submit_page.update(cx, |this, cx| this.submit(cx)).ok();
                                }),
                        ),
                ),
            cx,
        )
    }
}
