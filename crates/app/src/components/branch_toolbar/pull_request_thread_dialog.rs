//! Port of upstream/apps/web/src/components/PullRequestThreadDialog.tsx.
//!
//! "Checkout pull request": the branch picker's "Checkout pull request" row opens this dialog
//! with the typed reference. It resolves the reference (a URL, a checkout command, `123` or
//! `#123`) after a 450ms pause and previews the pull request. "Local" then checks its branch
//! out in the project checkout and "Worktree" (also Enter) in a new worktree; the host binds
//! the not yet started thread to the result.
//!
//! Not ported: the write-scope gates (`AuthOrchestrationOperateScope`; one process has no
//! sessions) and the source-control presentation lookup (Turin ships GitHub only).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use turin_contracts::ThreadId;
use turin_contracts::git::{
    GitPreparePullRequestThreadMode, GitPullRequestState, GitResolvedPullRequest,
};

use super::backend::SharedBranchBackend;
use crate::pull_request_reference::parse_pull_request_reference;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{RADIUS_XL, TEXT_SM, TEXT_XS};
use crate::ui::text_input::text_input_frame;
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// Upstream's `useDebouncedValue(reference, { wait: 450 })`.
const RESOLVE_DEBOUNCE: Duration = Duration::from_millis(450);

/// What a prepared checkout hands the host (`onPrepared`'s input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedPullRequestThread {
    pub branch: String,
    pub worktree_path: Option<String>,
}

pub type OnPrepared = Rc<dyn Fn(PreparedPullRequestThread, &mut Window, &mut App)>;

/// `validationMessage`: shown once the reader has typed, never for a valid reference.
pub fn validation_message(dirty: bool, reference: &str, parsed: Option<&str>) -> Option<String> {
    if !dirty {
        return None;
    }
    if reference.trim().is_empty() {
        return Some("Paste a pull request URL, checkout command, or enter 123 / #123.".to_owned());
    }
    parsed
        .is_none()
        .then(|| "Use a pull request URL, checkout command, 123, or #123.".to_owned())
}

/// `isResolving`: a reference is waiting for (or in) its lookup.
pub fn is_resolving(parsed: Option<&str>, resolved: bool, lookup_in_flight: bool) -> bool {
    parsed.is_some() && !resolved && lookup_in_flight
}

/// Whether "Local"/"Worktree" can run (`handleConfirm`'s guards).
pub fn can_confirm(cwd_known: bool, resolved: bool, resolving: bool, preparing: bool) -> bool {
    cwd_known && resolved && !resolving && !preparing
}

pub struct PullRequestThreadDialog {
    backend: SharedBranchBackend,
    cwd: String,
    thread_id: ThreadId,
    on_prepared: OnPrepared,
    input: Entity<InputState>,
    reference: String,
    dirty: bool,
    /// The reference the preview below was resolved for.
    resolved: Option<(String, GitResolvedPullRequest)>,
    /// The pending debounce + lookup; replaced on every keystroke.
    lookup: Option<Task<()>>,
    lookup_in_flight: bool,
    lookup_error: Option<String>,
    preparing: Option<GitPreparePullRequestThreadMode>,
    prepare_error: Option<String>,
    _subscription: Subscription,
}

impl PullRequestThreadDialog {
    pub fn new(
        backend: SharedBranchBackend,
        cwd: String,
        thread_id: ThreadId,
        initial_reference: Option<String>,
        on_prepared: OnPrepared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial_reference.unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("PR URL, checkout command, or #42")
                .default_value(initial.clone())
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.dirty = true;
                    this.reference = state.read(cx).value().to_string();
                    this.prepare_error = None;
                    this.schedule_lookup(window, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.confirm(GitPreparePullRequestThreadMode::Worktree, window, cx)
                }
                _ => {}
            },
        );
        let mut dialog = Self {
            backend,
            cwd,
            thread_id,
            on_prepared,
            input,
            reference: initial,
            dirty: false,
            resolved: None,
            lookup: None,
            lookup_in_flight: false,
            lookup_error: None,
            preparing: None,
            prepare_error: None,
            _subscription: subscription,
        };
        dialog.schedule_lookup(window, cx);
        dialog
    }

    fn parsed(&self) -> Option<String> {
        parse_pull_request_reference(&self.reference)
    }

    /// The preview that belongs to what is typed now (`liveResolvedPullRequest`/cache).
    fn preview(&self) -> Option<&GitResolvedPullRequest> {
        let parsed = self.parsed()?;
        self.resolved
            .as_ref()
            .filter(|(reference, _)| *reference == parsed)
            .map(|(_, pull_request)| pull_request)
    }

    /// Resolves the typed reference after the debounce; a newer keystroke cancels it.
    fn schedule_lookup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lookup = None;
        self.lookup_error = None;
        let Some(parsed) = self.parsed() else {
            self.lookup_in_flight = false;
            return;
        };
        if self.preview().is_some() {
            self.lookup_in_flight = false;
            return;
        }
        self.lookup_in_flight = true;
        self.lookup = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(RESOLVE_DEBOUNCE).await;
            let Ok(task) = this.update_in(cx, |this, _, cx| {
                this.backend.resolve_pull_request(&this.cwd, &parsed, cx)
            }) else {
                return;
            };
            let outcome = task.await;
            this.update_in(cx, |this, _, cx| {
                // A keystroke since replaced this lookup (and dropped it before it got here).
                this.lookup_in_flight = false;
                match outcome {
                    Ok(pull_request) => this.resolved = Some((parsed, pull_request)),
                    Err(message) => this.lookup_error = Some(message),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn confirm(
        &mut self,
        mode: GitPreparePullRequestThreadMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parsed = self.parsed();
        if parsed.is_none() {
            self.dirty = true;
            cx.notify();
            return;
        }
        let resolving = is_resolving(
            parsed.as_deref(),
            self.preview().is_some(),
            self.lookup_in_flight,
        );
        if !can_confirm(
            !self.cwd.is_empty(),
            self.preview().is_some(),
            resolving,
            self.preparing.is_some(),
        ) {
            return;
        }
        let Some(reference) = parsed else {
            return;
        };
        self.prepare_error = None;
        self.preparing = Some(mode);
        let thread_id =
            (mode == GitPreparePullRequestThreadMode::Worktree).then(|| self.thread_id.clone());
        let task = self
            .backend
            .prepare_pull_request_thread(&self.cwd, &reference, mode, thread_id, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.preparing = None;
                match outcome {
                    Ok(result) => {
                        let prepared = PreparedPullRequestThread {
                            branch: result.branch.as_str().to_owned(),
                            worktree_path: result
                                .worktree_path
                                .map(|path| path.as_str().to_owned()),
                        };
                        (this.on_prepared)(prepared, window, cx);
                        window.close_dialog(cx);
                    }
                    Err(message) => this.prepare_error = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for PullRequestThreadDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let parsed = self.parsed();
        let preview = self.preview().cloned();
        let resolving = is_resolving(parsed.as_deref(), preview.is_some(), self.lookup_in_flight);
        let error = validation_message(self.dirty, &self.reference, parsed.as_deref())
            .or_else(|| self.prepare_error.clone())
            .or_else(|| {
                preview
                    .is_none()
                    .then(|| self.lookup_error.clone())
                    .flatten()
            });
        let preparing = self.preparing;
        let enabled = can_confirm(
            !self.cwd.is_empty(),
            preview.is_some(),
            resolving,
            preparing.is_some(),
        );
        let page = cx.entity().downgrade();
        let local_page = page.clone();
        let worktree_page = page;

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        "Resolve a GitHub pull request, then create the draft thread in the main repo or in a dedicated worktree.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Pull request"),
                    )
                    .child(
                        text_input_frame(cx)
                            .child(Input::new(&self.input).appearance(false).bordered(false)),
                    ),
            )
            .children(preview.map(|pull_request| {
                let tone = match pull_request.state {
                    GitPullRequestState::Merged => theme.accent_foreground,
                    GitPullRequestState::Closed => theme.muted_foreground,
                    GitPullRequestState::Open => theme.success,
                };
                let state = match pull_request.state {
                    GitPullRequestState::Merged => "Merged",
                    GitPullRequestState::Closed => "Closed",
                    GitPullRequestState::Open => "Open",
                };
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .rounded(RADIUS_XL)
                    .border_1()
                    .border_color(theme.border)
                    .p_3()
                    .child(
                        div()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(
                                        pull_request.title.as_str().to_owned(),
                                    )),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(format!(
                                        "#{} · {} to {}",
                                        pull_request.number.get(),
                                        pull_request.head_branch.as_str(),
                                        pull_request.base_branch.as_str()
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_XS.size)
                            .text_color(tone)
                            .child(state),
                    )
            }))
            .children(resolving.then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().with_size(Size::Small))
                    .child("Resolving pull request...")
            }))
            .children(error.map(|error| {
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(SharedString::from(error))
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pull-request-thread-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .disabled(preparing.is_some())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("pull-request-thread-local")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label(if preparing == Some(GitPreparePullRequestThreadMode::Local) {
                                "Preparing local..."
                            } else {
                                "Local"
                            })
                            .disabled(!enabled)
                            .on_click(move |_, window, cx| {
                                local_page
                                    .update(cx, |this, cx| {
                                        this.confirm(
                                            GitPreparePullRequestThreadMode::Local,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            }),
                    )
                    .child(
                        Button::new("pull-request-thread-worktree")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Sm)
                            .label(
                                if preparing == Some(GitPreparePullRequestThreadMode::Worktree) {
                                    "Preparing worktree..."
                                } else {
                                    "Worktree"
                                },
                            )
                            .disabled(!enabled)
                            .on_click(move |_, window, cx| {
                                worktree_page
                                    .update(cx, |this, cx| {
                                        this.confirm(
                                            GitPreparePullRequestThreadMode::Worktree,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            }),
                    ),
            )
    }
}

/// Opens the dialog over the window (`PullRequestThreadDialog` with `open`).
#[allow(clippy::too_many_arguments)]
pub fn open(
    backend: SharedBranchBackend,
    cwd: String,
    thread_id: ThreadId,
    initial_reference: Option<String>,
    on_prepared: OnPrepared,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| {
        PullRequestThreadDialog::new(
            backend,
            cwd,
            thread_id,
            initial_reference,
            on_prepared,
            window,
            cx,
        )
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("Checkout pull request")
            .w(px(576.))
            .child(form.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_waits_for_the_reader_and_names_what_to_type() {
        assert_eq!(validation_message(false, "", None), None);
        assert_eq!(
            validation_message(true, "   ", None).as_deref(),
            Some("Paste a pull request URL, checkout command, or enter 123 / #123.")
        );
        assert_eq!(
            validation_message(true, "feature/x", None).as_deref(),
            Some("Use a pull request URL, checkout command, 123, or #123.")
        );
        assert_eq!(validation_message(true, "#42", Some("42")), None);
    }

    #[test]
    fn a_reference_is_resolving_until_its_preview_lands() {
        assert!(is_resolving(Some("42"), false, true));
        assert!(!is_resolving(Some("42"), true, true));
        assert!(!is_resolving(None, false, true));
        assert!(!is_resolving(Some("42"), false, false));
    }

    #[test]
    fn the_checkout_buttons_need_a_preview_and_no_work_in_flight() {
        assert!(can_confirm(true, true, false, false));
        assert!(!can_confirm(false, true, false, false));
        assert!(!can_confirm(true, false, false, false));
        assert!(!can_confirm(true, true, true, false));
        assert!(!can_confirm(true, true, false, true));
    }

    #[test]
    fn references_in_every_accepted_shape_parse_to_the_same_lookup() {
        for input in ["42", "#42", " #42 ", "gh pr checkout 42"] {
            assert_eq!(
                parse_pull_request_reference(input).as_deref(),
                Some("42"),
                "{input}"
            );
        }
        assert_eq!(parse_pull_request_reference("feature/branch"), None);
    }
}
