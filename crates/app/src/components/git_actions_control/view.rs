//! The rendering of `GitActionsControl`: the split button (panel rows or
//! toolbar outline buttons), its progress and success rows, the chevron menu
//! (`gitItems`) and the "Changes" row.

use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, rems,
};
use turin_contracts::git::{GitStackedAction, VcsStatusResult};
use turin_review::action_state::git_actions_control_logic::{
    GitActionIconName, GitActionMenuItem, GitActionProgressInput, GitQuickAction,
    GitQuickActionKind, format_git_action_elapsed, resolve_git_action_progress_presentation,
    resolve_quick_action,
};

use super::GitActionsControl;
use super::lifecycle::{RunGitActionInput, menu_action_disabled_reason};
use crate::components::chat::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, panel_hover_surface, panel_icon, panel_split_group,
    panel_split_separator,
};
use crate::components::sidebar::thread_status_indicators::format_diff_count;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_SM, TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::timestamp_format::now_ms;
use crate::ui::icon::{Icon, IconName};
use crate::ui::tooltip::{Tooltip, dropdown_glass_background, dropdown_glass_border};
use crate::ui::{Button, ButtonSize, ButtonVariant};

fn quick_action_icon(quick: &GitQuickAction) -> IconName {
    match quick.kind {
        GitQuickActionKind::OpenPublish => IconName::CloudUpload,
        GitQuickActionKind::RunPull => IconName::CloudDownload,
        GitQuickActionKind::RunAction => match quick.action {
            Some(GitStackedAction::Commit) => IconName::GitCommitHorizontal,
            Some(GitStackedAction::Push | GitStackedAction::CommitPush) => IconName::CloudUpload,
            _ => IconName::Github,
        },
        GitQuickActionKind::ShowHint => match quick.label.as_str() {
            "Commit" => IconName::GitCommitHorizontal,
            "Push" => IconName::CloudUpload,
            _ => IconName::Info,
        },
    }
}

fn menu_item_icon(icon: GitActionIconName) -> IconName {
    match icon {
        GitActionIconName::Commit => IconName::GitCommitHorizontal,
        GitActionIconName::Push => IconName::CloudUpload,
        GitActionIconName::Pr => IconName::Github,
    }
}

impl GitActionsControl {
    /// `GitActionProgressButtonContent`.
    fn progress_content(
        &self,
        status: &str,
        output: Option<&str>,
        started_at_ms: Option<i64>,
        panel: bool,
        theme: &AppTheme,
    ) -> AnyElement {
        let elapsed = format_git_action_elapsed(started_at_ms, now_ms());
        let mut column = div().flex().flex_col().min_w_0().flex_1().child(
            div()
                .flex()
                .items_center()
                .gap(rems(if panel { 0.75 } else { 0.625 }))
                .h(rems(if panel { 1.5 } else { 1. }))
                .child(
                    Spinner::new()
                        .with_size(Size::XSmall)
                        .color(theme.muted_foreground),
                )
                .child(div().min_w_0().flex_1().truncate().child(status.to_owned()))
                .when(!panel, |this| {
                    this.children(elapsed.clone().map(|elapsed| {
                        div()
                            .text_size(TEXT_2XS.size)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.muted_foreground)
                            .child(elapsed)
                    }))
                }),
        );
        if let Some(output) = output {
            column = column.child(
                div()
                    .pt(px(2.))
                    .pl(rems(1.75))
                    .truncate()
                    .text_size(TEXT_2XS.size)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.muted_foreground)
                    .child(output.to_owned()),
            );
        }
        column.into_any_element()
    }

    /// `GitActionSuccessButtonContent`.
    fn success_content(
        &self,
        title: &str,
        description: Option<&str>,
        theme: &AppTheme,
    ) -> AnyElement {
        let mut column = div().flex().flex_col().min_w_0().flex_1().child(
            div()
                .flex()
                .items_center()
                .gap(rems(0.75))
                .h(rems(1.5))
                .child(
                    Icon::new(IconName::Check)
                        .size(rems(0.875))
                        .color(theme.success),
                )
                .child(div().min_w_0().truncate().child(title.to_owned())),
        );
        if let Some(description) = description {
            column = column.child(
                div()
                    .pt(px(2.))
                    .pl(rems(1.625))
                    .truncate()
                    .text_size(TEXT_2XS.size)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.muted_foreground)
                    .child(description.to_owned()),
            );
        }
        column.into_any_element()
    }

    /// One `MenuItem` of the chevron menu.
    fn menu_row(
        &self,
        id: impl Into<gpui_kit::ElementId>,
        icon: IconName,
        label: SharedString,
        disabled: bool,
        reason: Option<String>,
        on_click: impl Fn(&mut GitActionsControl, &mut Window, &mut Context<GitActionsControl>)
        + 'static,
        weak: gpui_kit::WeakEntity<GitActionsControl>,
        theme: &AppTheme,
    ) -> AnyElement {
        let accent = theme.accent;
        let accent_foreground = theme.accent_foreground;
        let muted = theme.muted_foreground;
        div()
            .id(id)
            .flex()
            .min_h(rems(1.75))
            .w_full()
            .items_center()
            .gap_2()
            .rounded(RADIUS_SM)
            .px_2()
            .py(rems(0.25))
            .text_size(TEXT_SM.size)
            .child(Icon::new(icon).size(rems(1.)).color(muted))
            .child(label)
            .map(|this| {
                if disabled {
                    this.opacity(0.64).cursor(CursorStyle::OperationNotAllowed)
                } else {
                    this.cursor(CursorStyle::PointingHand)
                        .hover(move |style| style.bg(accent).text_color(accent_foreground))
                }
            })
            .when_some(reason, |this, reason| {
                this.tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
            })
            .when(!disabled, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    weak.update(cx, |this, cx| {
                        this.menu_open = false;
                        on_click(this, window, cx);
                        cx.notify();
                    })
                    .ok();
                })
            })
            .into_any_element()
    }

    /// `gitItems`: the menu's rows and warnings.
    fn menu_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let weak = cx.entity().downgrade();
        let status_view = self.status_for(cx);
        let status = status_view.status.clone();
        let busy = self.is_action_running(cx);
        let has_primary_remote = status
            .as_ref()
            .is_some_and(|status| status.has_primary_remote);
        let items = self.menu_items(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        for item in items {
            let reason =
                menu_action_disabled_reason(&item, status.as_ref(), busy, has_primary_remote);
            let disabled = item.disabled;
            let dialog_action = item.dialog_action;
            rows.push(self.menu_row(
                SharedString::from(format!("git-menu-{:?}", item.id)),
                menu_item_icon(item.icon),
                item.label.clone().into(),
                disabled,
                if disabled { reason } else { None },
                move |this, window, cx| {
                    this.open_dialog_for_menu_item(&item_for(dialog_action), window, cx)
                },
                weak.clone(),
                &theme,
            ));
        }
        let can_publish = status
            .as_ref()
            .is_some_and(|status| status.is_repo && !status.has_primary_remote);
        if can_publish {
            rows.push(self.menu_row(
                "git-menu-publish",
                IconName::CloudUpload,
                "Publish repository...".into(),
                busy,
                None,
                |this, window, cx| this.open_publish_dialog(window, cx),
                weak.clone(),
                &theme,
            ));
        }
        if let Some(status) = &status {
            if status.ref_name.is_none() {
                rows.push(warning_text(
                    "Detached HEAD: create and check out a branch to enable push and pull request actions.",
                    &theme,
                ));
            }
            if status.ref_name.is_some()
                && !status.has_working_tree_changes
                && status.behind_count > 0
                && status.ahead_count == 0
            {
                rows.push(warning_text("Behind upstream. Pull/rebase first.", &theme));
            }
        }
        if let Some(error) = &status_view.error {
            rows.push(
                div()
                    .px_2()
                    .py(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive_foreground)
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .min_w(rems(12.))
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(dropdown_glass_border(&theme))
            .bg(dropdown_glass_background(&theme))
            .shadow(tokens::shadow_popover(theme.is_dark()))
            .text_color(theme.popover_foreground)
            .children(rows)
            .into_any_element()
    }

    /// `openDialogForMenuItem`.
    pub(crate) fn open_dialog_for_menu_item(
        &mut self,
        item: &GitActionMenuItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use turin_review::action_state::git_actions_control_logic::GitDialogAction;
        if item.disabled {
            return;
        }
        let Some(target) = self.current_target() else {
            return;
        };
        match item.dialog_action {
            GitDialogAction::Push => self.run_git_action(
                target,
                RunGitActionInput::new(GitStackedAction::Push),
                window,
                cx,
            ),
            GitDialogAction::CreatePr => self.run_git_action(
                target,
                RunGitActionInput::new(GitStackedAction::CreatePr),
                window,
                cx,
            ),
            GitDialogAction::Commit => self.open_commit_dialog(window, cx),
        }
    }
}

/// The menu item a dialog action stands for (enabled), for re-dispatch.
fn item_for(
    action: turin_review::action_state::git_actions_control_logic::GitDialogAction,
) -> GitActionMenuItem {
    use turin_review::action_state::git_actions_control_logic::{
        GitActionMenuItemId, GitDialogAction,
    };
    GitActionMenuItem {
        id: match action {
            GitDialogAction::Commit => GitActionMenuItemId::Commit,
            GitDialogAction::Push => GitActionMenuItemId::Push,
            GitDialogAction::CreatePr => GitActionMenuItemId::Pr,
        },
        label: String::new(),
        disabled: false,
        icon: GitActionIconName::Commit,
        dialog_action: action,
    }
}

fn warning_text(text: &'static str, theme: &AppTheme) -> AnyElement {
    div()
        .px_2()
        .py(rems(0.375))
        .max_w(rems(16.))
        .text_size(TEXT_XS.size)
        .text_color(theme.warning_foreground)
        .child(text)
        .into_any_element()
}

fn diff_totals(status: Option<&VcsStatusResult>) -> (u64, u64) {
    // Matches the diff panel's Changes view. Older servers only report uncommitted totals.
    match status {
        Some(status) => match &status.branch_changes {
            Some(changes) => (changes.insertions, changes.deletions),
            None => (
                status.working_tree.insertions,
                status.working_tree.deletions,
            ),
        },
        None => (0, 0),
    }
}

impl Render for GitActionsControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(_cwd) = self.props.git_cwd.clone() else {
            return div().into_any_element();
        };
        let theme = cx.app_theme().clone();
        let panel = self.is_panel();
        let weak = cx.entity().downgrade();
        let status_view = self.status_for(cx);
        let status = status_view.status.clone();
        // Default to true while loading so we don't flash init controls.
        let is_repo = status.as_ref().map(|status| status.is_repo).unwrap_or(true);
        let has_primary_remote = status
            .as_ref()
            .is_some_and(|status| status.has_primary_remote);
        let is_default_ref = status.as_ref().is_some_and(|status| status.is_default_ref);
        let busy = self.is_action_running(cx);
        let quick = resolve_quick_action(status.as_ref(), busy, is_default_ref, has_primary_remote);
        let quick_disabled_reason = quick.disabled.then(|| {
            quick
                .hint
                .clone()
                .unwrap_or_else(|| "This action is currently unavailable.".to_owned())
        });
        let action_state = self.action_state(cx);
        let progress = resolve_git_action_progress_presentation(&GitActionProgressInput {
            is_running: action_state.is_running,
            operation: action_state.operation.as_deref(),
            current_label: action_state.current_label.as_deref(),
            last_output_line: action_state.last_output_line.as_deref(),
            phase_started_at_ms: action_state.phase_started_at_ms,
            hook_started_at_ms: action_state.hook_started_at_ms,
        });
        let success = self.visible_inline_success().cloned();

        let mut children: Vec<AnyElement> = Vec::new();

        if !is_repo {
            let label = if self.init_pending {
                "Initializing..."
            } else {
                "Initialize Git"
            };
            let weak_init = weak.clone();
            children.push(if panel {
                ThreadDetailsControl::new("git-init")
                    .part(ThreadDetailsPart::Row)
                    .disabled(self.init_pending)
                    .on_click(move |_, window, cx| {
                        weak_init
                            .update(cx, |this, cx| this.initialize_git(window, cx))
                            .ok();
                    })
                    .child(panel_icon(IconName::GitBranchPlus, &theme))
                    .child(div().min_w_0().truncate().child(label))
                    .into_any_element()
            } else {
                Button::new("git-init")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Xs)
                    .icon(IconName::GitBranchPlus)
                    .label(label)
                    .disabled(self.init_pending)
                    .on_click(move |_, window, cx| {
                        weak_init
                            .update(cx, |this, cx| this.initialize_git(window, cx))
                            .ok();
                    })
                    .into_any_element()
            });
        } else if !(self.props.compact && progress.is_none() && success.is_none()) {
            children.push(self.render_action_group(
                &quick,
                quick_disabled_reason.as_deref(),
                progress.as_ref(),
                success.as_ref(),
                busy,
                window,
                cx,
            ));
        }

        if panel && is_repo {
            let (insertions, deletions) = diff_totals(status.as_ref());
            let weak_changes = weak.clone();
            let can_open = self.props.can_open_changes;
            children.push(
                ThreadDetailsControl::new("git-changes")
                    .part(ThreadDetailsPart::Row)
                    .disabled(!can_open)
                    .on_click(move |_, _, cx| {
                        weak_changes
                            .update(cx, |_, cx| {
                                cx.emit(super::GitActionsControlEvent::OpenChanges)
                            })
                            .ok();
                    })
                    .child(panel_icon(IconName::FileDiff, &theme))
                    .child(div().flex_1().min_w_0().truncate().child("Changes"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .font_family(theme.fonts.mono.clone())
                            .text_size(TEXT_2XS.size)
                            .font_weight(FontWeight::NORMAL)
                            .child(
                                div()
                                    .text_color(theme.success)
                                    .child(format!("+{}", format_diff_count(insertions))),
                            )
                            .child(
                                div()
                                    .text_color(theme.destructive_foreground)
                                    .child(format!("-{}", format_diff_count(deletions))),
                            ),
                    )
                    .into_any_element(),
            );
        }

        div()
            .id("git-actions-control")
            .flex()
            .flex_col()
            .w_full()
            .children(children)
            .into_any_element()
    }
}

impl GitActionsControl {
    /// The split button group (`ActionGroup`).
    #[allow(clippy::too_many_arguments)]
    fn render_action_group(
        &mut self,
        quick: &GitQuickAction,
        quick_disabled_reason: Option<&str>,
        progress: Option<
            &turin_review::action_state::git_actions_control_logic::GitActionProgressPresentation,
        >,
        success: Option<&super::InlineSuccess>,
        busy: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let panel = self.is_panel();
        let weak = cx.entity().downgrade();
        let icon = quick_action_icon(quick);

        let primary: AnyElement = if let Some(progress) = progress {
            ThreadDetailsControl::new("git-primary")
                .part(ThreadDetailsPart::Primary)
                .multiline(true)
                .disabled(true)
                .child(self.progress_content(
                    &progress.status,
                    progress.output.as_deref(),
                    progress.started_at_ms,
                    panel,
                    &theme,
                ))
                .into_any_element()
        } else if let (true, Some(success)) = (panel, success) {
            ThreadDetailsControl::new("git-primary")
                .part(ThreadDetailsPart::Primary)
                .multiline(true)
                .disabled(true)
                .child(self.success_content(&success.title, success.description.as_deref(), &theme))
                .into_any_element()
        } else {
            let weak_click = weak.clone();
            let mut control = ThreadDetailsControl::new("git-primary")
                .part(ThreadDetailsPart::Primary)
                .disabled(busy || quick.disabled)
                .child(panel_icon(icon, &theme))
                .child(div().min_w_0().truncate().child(quick.label.clone()));
            if let Some(reason) = quick_disabled_reason {
                // The `aria-disabled` primary with its hover popover: look
                // enabled-but-inert and say why on hover.
                let reason = reason.to_owned();
                control = control.tooltip(Tooltip::new(reason));
            } else {
                control = control.on_click(move |_, window, cx| {
                    weak_click
                        .update(cx, |this, cx| this.run_quick_action(window, cx))
                        .ok();
                });
            }
            control.into_any_element()
        };

        let tail: AnyElement = if panel && progress.is_some() {
            // The menu is disabled while an action runs, so its chevron slot
            // hosts the elapsed counter instead.
            let elapsed = progress
                .and_then(|progress| format_git_action_elapsed(progress.started_at_ms, now_ms()));
            div()
                .flex()
                .h(rems(2.))
                .flex_shrink_0()
                .items_center()
                .self_start()
                .pr(rems(0.625))
                .text_size(TEXT_2XS.size)
                .font_weight(FontWeight::NORMAL)
                .text_color(theme.muted_foreground)
                .children(elapsed)
                .into_any_element()
        } else {
            let weak_menu = weak.clone();
            let open = self.menu_open;
            let trigger = ThreadDetailsControl::new("git-menu-trigger")
                .part(ThreadDetailsPart::Secondary)
                .disabled(busy)
                .child(panel_icon(IconName::ChevronDown, &theme));
            div()
                .flex()
                .items_center()
                .child(panel_split_separator(&theme))
                .child(
                    Popover::new("git-actions-menu")
                        .appearance(false)
                        .anchor(gpui_kit::Anchor::TopRight)
                        .trigger(trigger)
                        .open(open)
                        .on_open_change(move |open, _, cx| {
                            let open = *open;
                            weak_menu
                                .update(cx, |this, cx| {
                                    this.menu_open = open;
                                    if open && let Some(cwd) = this.props.git_cwd.clone() {
                                        this.backend.refresh_status(&cwd, cx);
                                    }
                                    cx.notify();
                                })
                                .ok();
                        })
                        .content({
                            let weak_content = weak.clone();
                            move |_, _, cx| match weak_content.upgrade() {
                                Some(control) => {
                                    control.update(cx, |control, cx| control.menu_content(cx))
                                }
                                None => div().into_any_element(),
                            }
                        }),
                )
                .into_any_element()
        };

        if panel {
            panel_split_group("git-split-group", progress.is_some(), &theme)
                .child(primary)
                .child(tail)
                .into_any_element()
        } else {
            // Toolbar: an outline `Group` of xs buttons.
            let _ = panel_hover_surface(&theme);
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .child(primary)
                .child(tail)
                .into_any_element()
        }
    }
}

#[allow(dead_code)]
fn _text_color(theme: &AppTheme) -> gpui_kit::Hsla {
    with_alpha(theme.foreground, 0.8).into()
}
