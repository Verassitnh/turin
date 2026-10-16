//! Port of StorageSettings.tsx (`StorageSettingsPanel`) for the single local
//! environment.
//! Also ports upstream/apps/web/src/lib/formatBytes.ts.
//!
//! Not ported (PORTING.md Deviations): the browser-artifact retention row
//! (the browser integration is cut), the project-scope worktree cleanup
//! override (project scope), and the "Mixed across selected machines" states.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};
use serde_json::{Value, json};

use super::super::controls::{FieldSize, SelectWidth, switch, text_field};
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button, settings_group};
use super::super::search::searchable_setting;
use super::super::time_format::format_relative_time_label;
use super::super::view::{SettingsView, Which};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

/// `formatBytes`.
pub fn format_bytes(value: f64) -> String {
    if value < 1_024. {
        return format!("{} B", value.round() as i64);
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut next = value;
    let mut index: i32 = -1;
    loop {
        next /= 1_024.;
        index += 1;
        if !(next >= 1_024. && (index as usize) < units.len() - 1) {
            break;
        }
    }
    let precision = if next >= 100. {
        0
    } else if next >= 10. {
        1
    } else {
        2
    };
    format!("{next:.precision$} {}", units[index as usize])
}

#[derive(Default)]
pub struct StorageState {
    pub last_run_open: bool,
    pub running: bool,
}

impl SettingsView {
    pub(super) fn render_storage(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let json = self.json(cx);
        let storage = json.get(Which::Server, "storageCleanup").clone();
        let default_storage = json.default_of(Which::Server, "storageCleanup").clone();
        let get_bool = |key: &str| storage.get(key).and_then(Value::as_bool).unwrap_or(false);
        let get_days = |key: &str| storage.get(key).and_then(Value::as_u64);

        let mut worktrees = SettingsSection::new("Worktrees").id("storage-worktrees");
        worktrees = worktrees.child(self.worktrees_directory_row(&json, window, cx));
        worktrees = worktrees.child(self.storage_switch_row(
            "Delete worktrees with deleted threads",
            "Remove unused worktrees when active or archived threads are deleted. Worktrees with local changes are kept.",
            "worktreeOnDelete",
            get_bool("worktreeOnDelete"),
            cx,
        ));
        worktrees = worktrees.child(self.storage_retention_row(
            "Delete inactive worktrees",
            "Remove worktrees after their threads have been inactive for this many days. Branches and thread history are kept.",
            "worktreeAfterDays",
            get_days("worktreeAfterDays"),
            window,
            cx,
        ));
        worktrees = worktrees.child(self.storage_switch_row(
            "Delete merged worktrees",
            "Remove worktrees whose pull request is merged and whose commits are included in the default branch.",
            "worktreeOnMerge",
            get_bool("worktreeOnMerge"),
            cx,
        ));
        worktrees = worktrees.child(self.storage_switch_row(
            "Delete unchanged worktrees",
            "Remove worktrees with no commits beyond the default branch.",
            "worktreeUnchanged",
            get_bool("worktreeUnchanged"),
            cx,
        ));
        let keep_when = storage
            .get("worktreeKeepWhen")
            .and_then(Value::as_str)
            .unwrap_or("uncommitted-changes")
            .to_owned();
        let keep_options: [(&str, &str); 3] = [
            ("any-local-files", "Any local files"),
            ("uncommitted-changes", "Uncommitted changes"),
            ("tracked-changes", "Edited tracked files"),
        ];
        let keep_label = keep_options
            .iter()
            .find(|(v, _)| *v == keep_when)
            .map(|(_, l)| *l)
            .unwrap_or("");
        let keep_control = self.select_control(
            cx,
            "storage-worktree-keep-when",
            keep_label.into(),
            keep_options
                .iter()
                .map(|(v, l)| (json!(v), SharedString::from(*l), *v == keep_when))
                .collect(),
            SelectWidth::Auto,
            false,
            |this, value, _, cx| {
                this.update_server(
                    json!({ "storageCleanup": { "worktreeKeepWhen": value } }),
                    cx,
                )
            },
        );
        let keep_dirty = storage.get("worktreeKeepWhen") != default_storage.get("worktreeKeepWhen");
        let keep_reset = keep_dirty.then(|| {
            let default = default_storage
                .get("worktreeKeepWhen")
                .cloned()
                .unwrap_or(Value::Null);
            let action = self.cb(cx, move |this, _, cx| {
                this.update_server(
                    json!({ "storageCleanup": { "worktreeKeepWhen": default } }),
                    cx,
                )
            });
            setting_reset_button("reset-keep-when", "Reset to default", move |w, c| {
                action(w, c)
            })
            .into_any_element()
        });
        worktrees = worktrees.child(
            Self::base_row("storage-worktree-keep-when")
                .description(match keep_when.as_str() {
                    "any-local-files" => "Includes ignored files, except node_modules.",
                    "tracked-changes" => "Deletes untracked and ignored files, including .env.",
                    _ => "Keeps edits and untracked files. Deletes ignored files, including .env.",
                })
                .reset_action(keep_reset)
                .control(keep_control),
        );

        let logs = SettingsSection::new("Artifacts and logs")
            .id("storage-artifacts")
            .child(self.storage_retention_row(
                "Delete old rotated logs",
                "Delete inactive rotated log files after this many days. Current logs are kept.",
                "logsAfterDays",
                get_days("logsAfterDays"),
                window,
                cx,
            ));
        vec![
            worktrees.into_any_element(),
            logs.into_any_element(),
            self.cleanup_section(&storage, cx),
        ]
    }

    fn worktrees_directory_row(
        &mut self,
        json: &super::super::rows::Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = json.str(Which::Server, "worktreesDirectory").to_owned();
        let input = self.input(
            "storage-worktrees-directory",
            &value,
            "Default",
            window,
            cx,
            |this, text, _, cx| {
                this.update_server(json!({ "worktreesDirectory": text.trim() }), cx);
            },
        );
        Self::base_row("storage-worktrees-location")
            .description("Folder where new worktrees are created, on any drive, such as D:\\worktrees or ~/worktrees. Existing worktrees stay where they are. Leave empty to use the T3 home folder.")
            .reset_action(self.reset_button(cx, json, Which::Server, "worktreesDirectory", "worktree location"))
            .control(div().w(rems(14.)).child(text_field(&input, None, FieldSize::Sm, false, cx)))
            .into_any_element()
    }

    fn storage_switch_row(
        &mut self,
        title: &'static str,
        description: &'static str,
        key: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        SettingsRow::new(title)
            .description(description)
            .control(switch(
                SharedString::from(format!("storage-{key}")),
                checked,
                false,
                cx,
                move |next, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.update_server(json!({ "storageCleanup": { key: next } }), cx)
                    });
                },
            ))
            .into_any_element()
    }

    /// `RetentionControl`: a days field with an on/off switch (on = 8 days).
    fn storage_retention_row(
        &mut self,
        title: &'static str,
        description: &'static str,
        key: &'static str,
        value: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let mut control = div().flex().items_center().gap_3();
        match value {
            Some(days) => {
                let field = self.number_field(
                    &format!("storage-{key}"),
                    days,
                    1,
                    1,
                    window,
                    cx,
                    move |this, next, cx| {
                        let days = next.clamp(1, 3650);
                        this.update_server(json!({ "storageCleanup": { key: days } }), cx);
                    },
                );
                control = control.child(field).child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.foreground)
                        .child("days"),
                );
            }
            None => {
                control = control.child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Off"),
                );
            }
        }
        let entity = cx.entity();
        control = control.child(switch(
            SharedString::from(format!("storage-{key}-switch")),
            value.is_some(),
            false,
            cx,
            move |next, _, cx| {
                let days = if next { json!(8) } else { Value::Null };
                entity.update(cx, |this, cx| {
                    this.update_server(json!({ "storageCleanup": { key: days } }), cx)
                });
            },
        ));
        SettingsRow::new(title)
            .description(description)
            .control(control)
            .into_any_element()
    }

    fn cleanup_section(&mut self, storage: &Value, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let has_rules = ["worktreeAfterDays", "logsAfterDays"]
            .iter()
            .any(|key| storage.get(*key).is_some_and(|v| !v.is_null()))
            || ["worktreeOnMerge", "worktreeOnDelete", "worktreeUnchanged"]
                .iter()
                .any(|key| storage.get(*key).and_then(Value::as_bool).unwrap_or(false));
        let now = chrono::Utc::now();
        let report = vm.storage_cleanup.clone();
        let description: String = match &report {
            Some(report) => {
                let mut text = format!(
                    "Last run {}. {} recovered.",
                    format_relative_time_label(&report.finished_at, now),
                    format_bytes(report.bytes_freed as f64)
                );
                if report.failed > 0 {
                    text.push_str(&format!(
                        " {} {} couldn't be removed.",
                        report.failed,
                        if report.failed == 1 { "item" } else { "items" }
                    ));
                }
                text
            }
            None => "Cleanup hasn't run yet.".into(),
        };
        let delete_now = Button::new("storage-delete-now")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .label(if self.panels.storage.running {
                "Deleting…"
            } else {
                "Delete now"
            })
            .disabled(self.panels.storage.running || !has_rules)
            .on_click(cx.listener(|this, _, _, cx| {
                let source = this.store.read(cx).source();
                source.run_storage_cleanup(cx);
            }));
        let (id, _) = searchable_setting("storage-delete-now");
        let main = settings_group(
            vec![
                SettingsRow::new(searchable_setting("storage-delete-now").1)
                    .id(id)
                    .description(if has_rules {
                        description
                    } else {
                        "Turn on a cleanup rule to use this.".to_owned()
                    })
                    .control(delete_now)
                    .into_any_element(),
            ],
            cx,
        );
        let mut section = SettingsSection::new("Cleanup")
            .id("storage-cleanup")
            .plain()
            .child(main);
        if let Some(report) = report {
            let open = self.panels.storage.last_run_open;
            let summary = format!(
                "{} removed, {} kept{}",
                report.removed,
                report.kept,
                if report.failed > 0 {
                    format!(", {} failed", report.failed)
                } else {
                    String::new()
                }
            );
            let header = div()
                .id("storage-last-run-toggle")
                .flex()
                .items_center()
                .gap_4()
                .px_4()
                .min_h(rems(2.75))
                .cursor(CursorStyle::PointingHand)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.panels.storage.last_run_open = !this.panels.storage.last_run_open;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .flex_1()
                        .items_center()
                        .gap_2()
                        .py_2()
                        .child(
                            // GPUI svgs cannot rotate: an open fold swaps to the down chevron.
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(rems(1.))
                            .color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_SM.size)
                                .font_weight(FontWeight::MEDIUM)
                                .child("Last run"),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(summary),
                        ),
                );
            let mut body = div().flex().flex_col();
            if open {
                body = body
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.5));
                for (index, entry) in report.entries.iter().enumerate() {
                    let mut text = String::new();
                    if let Some(title) = &entry.thread_title
                        && *title != entry.name
                    {
                        text.push_str(&format!("{title}. "));
                    }
                    text.push_str(&entry.reason);
                    let size = [
                        entry
                            .files
                            .filter(|_| entry.name == "Rotated logs")
                            .map(|f| format!("{f} {}", if f == 1 { "file" } else { "files" })),
                        entry.bytes.map(|b| format_bytes(b as f64)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let mut row = SettingsRow::new(entry.name.clone()).description(
                        div()
                            .text_color(if entry.failed {
                                theme.destructive
                            } else {
                                theme.muted_foreground
                            })
                            .child(text),
                    );
                    if entry.removed {
                        row = row.control(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(size),
                        );
                    }
                    body = body.child(
                        div()
                            .when(index > 0, |this| {
                                this.border_t_1()
                                    .border_color(with_alpha(theme.border, 0.5))
                            })
                            .child(row),
                    );
                }
                if report.omitted > 0 {
                    body = body.child(
                        div()
                            .px_4()
                            .py(px(10.))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(format!("{} more results.", report.omitted)),
                    );
                }
                if report.entries.is_empty() && report.omitted == 0 {
                    body = body.child(
                        div()
                            .px_4()
                            .py(px(10.))
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child("Nothing needed cleanup."),
                    );
                }
            }
            section = section.child(settings_group(
                vec![header.into_any_element(), body.into_any_element()],
                cx,
            ));
        }
        section.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn formats_bytes_like_upstream() {
        assert_eq!(format_bytes(512.), "512 B");
        assert_eq!(format_bytes(1_536.), "1.50 KB");
        assert_eq!(format_bytes(15_728_640.), "15.0 MB");
        assert_eq!(format_bytes(482_344_960.), "460 MB");
        assert_eq!(
            format_bytes(5. * 1_024. * 1_024. * 1_024. * 1_024. * 1_024.),
            "5120 TB"
        );
    }
}
