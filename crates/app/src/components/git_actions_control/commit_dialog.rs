//! The "Commit changes" dialog of GitActionsControl.tsx: branch, the changed
//! files (with an Edit mode to exclude some), the optional message, and
//! Cancel / Commit on new branch / Commit.

use std::collections::HashSet;

use gpui_kit::AppContext as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, px, rems,
};

use turin_contracts::git::VcsStatusResult;

use super::GitActionsControl;
use crate::components::sidebar::thread_status_indicators::format_diff_count;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, RADIUS_XL, TEXT_SM, tw};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::scroll_area::scroll_area;
use crate::ui::{Button, ButtonSize, ButtonVariant};

pub const COMMIT_DIALOG_TITLE: &str = "Commit changes";
pub const COMMIT_DIALOG_DESCRIPTION: &str =
    "Review and confirm your commit. Leave the message blank to auto-generate one.";

/// `StartTruncatedPath`: the end of a path matters most, so the head gives way.
/// (CSS direction tricks have no GPUI counterpart; the cut is by length.)
pub fn start_truncated_path(path: &str, max_chars: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max_chars {
        return path.to_owned();
    }
    let tail: String = chars[chars.len() - (max_chars - 1)..].iter().collect();
    format!("…{tail}")
}

struct FileRow {
    path: String,
    insertions: u64,
    deletions: u64,
}

pub struct CommitDialogForm {
    control: WeakEntity<GitActionsControl>,
    branch: String,
    is_default_ref: bool,
    files: Vec<FileRow>,
    excluded: HashSet<String>,
    editing: bool,
    message: Entity<TextareaState>,
}

impl CommitDialogForm {
    fn new(
        control: WeakEntity<GitActionsControl>,
        status: Option<VcsStatusResult>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (branch, is_default_ref, files) = match status {
            Some(status) => (
                status
                    .ref_name
                    .as_ref()
                    .map(|name| name.as_str().to_owned())
                    .unwrap_or_else(|| "(detached HEAD)".to_owned()),
                status.is_default_ref,
                status
                    .working_tree
                    .files
                    .iter()
                    .map(|file| FileRow {
                        path: file.path.as_str().to_owned(),
                        insertions: file.insertions,
                        deletions: file.deletions,
                    })
                    .collect(),
            ),
            None => ("(detached HEAD)".to_owned(), false, Vec::new()),
        };
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 6)
                .placeholder("Leave empty to auto-generate")
        });
        Self {
            control,
            branch,
            is_default_ref,
            files,
            excluded: HashSet::new(),
            editing: false,
            message,
        }
    }

    fn selected(&self) -> Vec<&FileRow> {
        self.files
            .iter()
            .filter(|file| !self.excluded.contains(&file.path))
            .collect()
    }

    fn submit(&mut self, feature_branch: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected();
        if selected.is_empty() {
            return;
        }
        let all_selected = self.excluded.is_empty();
        let file_paths =
            (!all_selected).then(|| selected.iter().map(|file| file.path.clone()).collect());
        let message = self.message.read(cx).value().to_string();
        window.close_dialog(cx);
        self.control
            .update(cx, |control, cx| {
                control.run_commit_dialog(message, file_paths, feature_branch, window, cx)
            })
            .ok();
    }
}

fn counts(insertions: u64, deletions: u64, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .flex_shrink_0()
        .child(
            div()
                .text_color(theme.diff_addition_foreground)
                .child(format!("+{}", format_diff_count(insertions))),
        )
        .child(div().text_color(theme.muted_foreground).child(" / "))
        .child(
            div()
                .text_color(theme.diff_deletion_foreground)
                .child(format!("-{}", format_diff_count(deletions))),
        )
        .into_any_element()
}

impl Render for CommitDialogForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        // `bg-zinc-25 ring-1 ring-black/5 dark:bg-white/[0.035] dark:ring-white/5`.
        let panel_bg = if dark {
            with_alpha(tw::WHITE, 0.035)
        } else {
            with_alpha(tw::ZINC_25, 1.)
        };
        let ring = with_alpha(if dark { tw::WHITE } else { tw::BLACK }, 0.05);
        let all_selected = self.excluded.is_empty();
        let selected = self.selected();
        let none_selected = selected.is_empty();
        let total_insertions: u64 = selected.iter().map(|file| file.insertions).sum();
        let total_deletions: u64 = selected.iter().map(|file| file.deletions).sum();
        let selected_count = selected.len();
        let file_count = self.files.len();
        let weak = cx.entity().downgrade();

        let rows: Vec<AnyElement> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let excluded = self.excluded.contains(&file.path);
                let path = file.path.clone();
                let toggle_path = file.path.clone();
                let open_path = file.path.clone();
                let weak_toggle = weak.clone();
                let weak_open = weak.clone();
                div()
                    .id(("commit-file", index))
                    .flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .px_2()
                    .py_1()
                    .font_family(theme.fonts.mono.clone())
                    .text_size(TEXT_SM.size)
                    .hover({
                        let hover = with_alpha(theme.accent, 0.5);
                        move |style| style.bg(hover)
                    })
                    .when(self.editing, |this| {
                        this.child(
                            Checkbox::new(("commit-file-check", index))
                                .checked(!excluded)
                                .on_click(move |_, _, cx| {
                                    let path = toggle_path.clone();
                                    weak_toggle
                                        .update(cx, |this, cx| {
                                            if !this.excluded.remove(&path) {
                                                this.excluded.insert(path);
                                            }
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        )
                    })
                    .child(
                        div()
                            .id(("commit-file-open", index))
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .cursor(CursorStyle::PointingHand)
                            .on_click(move |_, window, cx| {
                                let path = open_path.clone();
                                weak_open
                                    .update(cx, |this, cx| {
                                        this.control
                                            .update(cx, |control, cx| {
                                                control.open_changed_file(path, window, cx)
                                            })
                                            .ok();
                                    })
                                    .ok();
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .when(excluded, |this| this.text_color(theme.muted_foreground))
                                    .child(SharedString::from(start_truncated_path(&path, 52))),
                            )
                            .child(if excluded {
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground)
                                    .child("Excluded")
                                    .into_any_element()
                            } else {
                                counts(file.insertions, file.deletions, &theme)
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let weak_edit = weak.clone();
        let weak_all = weak.clone();
        let all_paths: Vec<String> = self.files.iter().map(|file| file.path.clone()).collect();
        let weak_cancel = weak.clone();
        let weak_branch = weak.clone();
        let weak_commit = weak.clone();

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child(COMMIT_DIALOG_DESCRIPTION),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(RADIUS_XL)
                    .bg(panel_bg)
                    .border_1()
                    .border_color(ring)
                    .p_3()
                    .text_size(TEXT_SM.size)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_color(theme.muted_foreground).child("Branch"))
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(self.branch.clone()),
                                    )
                                    .when(self.is_default_ref, |this| {
                                        this.child(
                                            div()
                                                .text_color(theme.warning_foreground)
                                                .child("Default branch"),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .when(self.editing && file_count > 0, |this| {
                                                this.child(
                                                    Checkbox::new("commit-all")
                                                        .checked(all_selected)
                                                        .on_click(move |_, _, cx| {
                                                            let paths = all_paths.clone();
                                                            weak_all
                                                                .update(cx, |this, cx| {
                                                                    this.excluded = if this
                                                                        .excluded
                                                                        .is_empty()
                                                                    {
                                                                        paths.into_iter().collect()
                                                                    } else {
                                                                        HashSet::new()
                                                                    };
                                                                    cx.notify();
                                                                })
                                                                .ok();
                                                        }),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .text_color(theme.muted_foreground)
                                                    .child("Files"),
                                            )
                                            .when(!all_selected && !self.editing, |this| {
                                                this.child(
                                                    div().text_color(theme.muted_foreground).child(
                                                        format!(
                                                            "({selected_count} of {file_count})"
                                                        ),
                                                    ),
                                                )
                                            }),
                                    )
                                    .when(file_count > 0, |this| {
                                        this.child(
                                            Button::new("commit-edit")
                                                .variant(ButtonVariant::Ghost)
                                                .size(ButtonSize::Xs)
                                                .label(if self.editing { "Done" } else { "Edit" })
                                                .on_click(move |_, _, cx| {
                                                    weak_edit
                                                        .update(cx, |this, cx| {
                                                            this.editing = !this.editing;
                                                            cx.notify();
                                                        })
                                                        .ok();
                                                }),
                                        )
                                    }),
                            )
                            .child(if file_count == 0 {
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("none")
                                    .into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .h(rems(11.))
                                            .rounded(RADIUS_LG)
                                            .bg(if dark {
                                                with_alpha(tw::WHITE, 0.025)
                                            } else {
                                                with_alpha(theme.card, 1.)
                                            })
                                            .border_1()
                                            .border_color(ring)
                                            .child(
                                                scroll_area("commit-files").child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .p_1()
                                                        .children(rows),
                                                ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .font_family(theme.fonts.mono.clone())
                                            .child(counts(
                                                total_insertions,
                                                total_deletions,
                                                &theme,
                                            )),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .child("Commit message (optional)"),
                    )
                    .child(
                        div()
                            .rounded(RADIUS_LG)
                            .border_1()
                            .border_color(theme.input)
                            .px_3()
                            .py_1()
                            .text_size(TEXT_SM.size)
                            .child(Textarea::new(&self.message)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("commit-cancel")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                weak_cancel.update(cx, |_, _| {}).ok();
                            }),
                    )
                    .child(
                        Button::new("commit-new-branch")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Commit on new branch")
                            .disabled(none_selected)
                            .on_click(move |_, window, cx| {
                                weak_branch
                                    .update(cx, |this, cx| this.submit(true, window, cx))
                                    .ok();
                            }),
                    )
                    .child(
                        Button::new("commit-submit")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Sm)
                            .label("Commit")
                            .disabled(none_selected)
                            .on_click(move |_, window, cx| {
                                weak_commit
                                    .update(cx, |this, cx| this.submit(false, window, cx))
                                    .ok();
                            }),
                    ),
            )
    }
}

/// Opens the dialog for the control.
pub fn open(
    control: WeakEntity<GitActionsControl>,
    status: Option<VcsStatusResult>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) {
    let form = cx.new(|cx| CommitDialogForm::new(control, status, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(COMMIT_DIALOG_TITLE)
            .w(px(512.))
            .child(form.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_end_of_a_long_path() {
        assert_eq!(
            start_truncated_path("crates/app/src/lib.rs", 52),
            "crates/app/src/lib.rs"
        );
        let long = "crates/app/src/components/a/very/deeply/nested/folder/structure/file_name.rs";
        let cut = start_truncated_path(long, 30);
        assert!(cut.starts_with('…'));
        assert!(cut.ends_with("structure/file_name.rs"));
        assert_eq!(cut.chars().count(), 30);
    }
}
