//! Port of upstream/apps/web/src/components/settings/StorageSettings.tsx
//! (`StorageSettingsPanel`) for the single local environment.
//!
//! At a project scope the page shows the project's own worktree cleanup tier
//! (inherit, off or custom rules); the environment-wide rows return at the
//! all-projects scope.
//! Also ports upstream/apps/web/src/lib/formatBytes.ts.
//!
//! Not ported (PORTING.md Deviations): the browser-artifact retention row
//! (the browser integration is cut) and the "Mixed across selected machines"
//! status of rows (one environment).

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    px, rems,
};
use serde_json::{Value, json};

use super::super::controls::{FieldSize, SelectWidth, switch, text_field};
use super::super::folded_settings_section::FoldedSection;
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button, settings_group};
use super::super::search::searchable_setting;
use super::super::time_format::format_relative_time_label;
use super::super::view::{SettingsView, Which};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant};

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
    pub running: bool,
}

/// Which tier of worktree cleanup a project uses (`projectMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectCleanupMode {
    Inherit,
    Off,
    Custom,
}

impl ProjectCleanupMode {
    fn label(self) -> &'static str {
        match self {
            Self::Inherit => "Inherit",
            Self::Off => "Off",
            Self::Custom => "Custom",
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Off => "off",
            Self::Custom => "custom",
        }
    }

    /// The row's description under the mode.
    pub fn description(self) -> &'static str {
        match self {
            Self::Off => "Keep this project's worktrees until you delete them manually.",
            Self::Custom => "Use these rules for this project.",
            Self::Inherit => "Use each machine's worktree cleanup settings.",
        }
    }
}

/// `projectMode(target)`: a project's own `worktreeCleanup` mode, else inherit.
pub fn project_cleanup_mode(
    target: &crate::settings_logic::scoped_settings::ScopedSettingsTarget,
) -> ProjectCleanupMode {
    use turin_contracts::settings::WorktreeCleanup;
    use turin_shared::project_settings::ProjectSettingSource;
    if target.sources.get("worktreeCleanup").copied() != Some(ProjectSettingSource::Project) {
        return ProjectCleanupMode::Inherit;
    }
    match target.settings.worktree_cleanup {
        Some(WorktreeCleanup::Off) => ProjectCleanupMode::Off,
        Some(WorktreeCleanup::Custom { .. }) => ProjectCleanupMode::Custom,
        None => ProjectCleanupMode::Inherit,
    }
}

/// The storage settings the page shows: `{...storageCleanup, ...resolveWorktreeCleanup(settings, null)}`.
pub fn effective_storage(settings: &turin_contracts::settings::ServerSettings) -> Value {
    let mut storage = serde_json::to_value(&settings.storage_cleanup).unwrap_or(Value::Null);
    let rules = turin_shared::project_settings::resolve_worktree_cleanup(
        settings,
        None,
        super::super::scope::text_generation_fallback(),
    );
    if let (Some(storage), Some(rules)) = (
        storage.as_object_mut(),
        serde_json::to_value(&rules)
            .ok()
            .and_then(|v| v.as_object().cloned()),
    ) {
        storage.extend(rules);
    }
    storage
}

impl SettingsView {
    pub(super) fn render_storage(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let json = self.json(cx);
        let context = self.scope_context(cx);
        let project_scope = context.is_project_scope();
        let vm = self.vm(cx);
        let storage = effective_storage(context.settings().unwrap_or(&vm.server));
        let default_storage = json.default_of(Which::Server, "storageCleanup").clone();
        let get_bool = |key: &str| storage.get(key).and_then(Value::as_bool).unwrap_or(false);
        let get_days = |key: &str| storage.get(key).and_then(Value::as_u64);
        let mode = context
            .target
            .as_ref()
            .map(project_cleanup_mode)
            .unwrap_or(ProjectCleanupMode::Inherit);
        let mixed_modes = context
            .targets
            .iter()
            .any(|target| project_cleanup_mode(target) != mode);

        let mut worktrees = SettingsSection::new("Worktrees").id("storage-worktrees");
        if !project_scope {
            worktrees = worktrees.child(self.worktrees_directory_row(&json, window, cx));
        } else {
            worktrees = worktrees.child(self.project_cleanup_mode_row(mode, mixed_modes, cx));
        }
        if !project_scope || (!mixed_modes && mode == ProjectCleanupMode::Custom) {
            worktrees = worktrees.child(self.storage_switch_row(
                "Delete worktrees with deleted threads",
                "Remove unused worktrees when active or archived threads are deleted. Worktrees with local changes are kept.",
                "worktreeOnDelete",
                get_bool("worktreeOnDelete"),
                project_scope,
                cx,
            ));
            worktrees = worktrees.child(self.storage_retention_row(
                "Delete inactive worktrees",
                "Remove worktrees after their threads have been inactive for this many days. Branches and thread history are kept.",
                "worktreeAfterDays",
                get_days("worktreeAfterDays"),
                project_scope,
                window,
                cx,
            ));
            worktrees = worktrees.child(self.storage_switch_row(
                "Delete merged worktrees",
                "Remove worktrees whose pull request is merged and whose commits are included in the default branch.",
                "worktreeOnMerge",
                get_bool("worktreeOnMerge"),
                project_scope,
                cx,
            ));
            worktrees = worktrees.child(self.storage_switch_row(
                "Delete unchanged worktrees",
                "Remove worktrees with no commits beyond the default branch.",
                "worktreeUnchanged",
                get_bool("worktreeUnchanged"),
                project_scope,
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
                move |this, value, _, cx| {
                    this.update_worktree_rule("worktreeKeepWhen", value, project_scope, cx)
                },
            );
            let keep_dirty =
                storage.get("worktreeKeepWhen") != default_storage.get("worktreeKeepWhen");
            let keep_reset = (keep_dirty && !project_scope).then(|| {
                let default = default_storage
                    .get("worktreeKeepWhen")
                    .cloned()
                    .unwrap_or(Value::Null);
                let action = self.cb(cx, move |this, _, cx| {
                    this.update_worktree_rule("worktreeKeepWhen", default.clone(), false, cx)
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
        }

        let mut sections = vec![worktrees.into_any_element()];
        if !project_scope {
            sections.push(
                SettingsSection::new("Artifacts and logs")
                    .id("storage-artifacts")
                    .child(self.storage_retention_row(
                        "Delete old rotated logs",
                        "Delete inactive rotated log files after this many days. Current logs are kept.",
                        "logsAfterDays",
                        get_days("logsAfterDays"),
                        false,
                        window,
                        cx,
                    ))
                    .into_any_element(),
            );
            sections.push(self.cleanup_section(&storage, cx));
        }
        sections
    }

    /// The write of a worktree rule: `worktreeCleanup` custom rules at a
    /// project scope, `storageCleanup` otherwise (`updateWorktree`).
    fn update_worktree_rule(
        &mut self,
        key: &'static str,
        value: Value,
        project_scope: bool,
        cx: &mut Context<Self>,
    ) {
        let patch = if project_scope {
            json!({ "worktreeCleanup": { "mode": "custom", "rules": { key: value } } })
        } else {
            json!({ "storageCleanup": { key: value } })
        };
        self.update(Which::Server, patch, cx);
    }

    /// "Automatic worktree cleanup": a project's own tier.
    fn project_cleanup_mode_row(
        &mut self,
        mode: ProjectCleanupMode,
        mixed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let json = self.json(cx);
        let row = SettingsRow::new("Automatic worktree cleanup").description(mode.description());
        let (row, _) = {
            let (row, scoped_reset, mixed_keys) = self.decorate_server_row(
                cx,
                row,
                "storage-project-cleanup",
                &["worktreeCleanup"],
                "project worktree cleanup",
            );
            let _ = &json;
            (
                match scoped_reset {
                    Some(reset) => row.reset_action(reset),
                    None => row,
                },
                mixed_keys,
            )
        };
        let options = [
            ProjectCleanupMode::Inherit,
            ProjectCleanupMode::Off,
            ProjectCleanupMode::Custom,
        ];
        let control = self.select_control(
            cx,
            "storage-project-cleanup-mode",
            if mixed { "Mixed" } else { mode.label() }.into(),
            options
                .iter()
                .map(|option| {
                    (
                        json!(option.value()),
                        SharedString::from(option.label()),
                        !mixed && *option == mode,
                    )
                })
                .collect(),
            SelectWidth::Auto,
            false,
            |this, value, _, cx| match value.as_str() {
                Some("inherit") => this.clear_scoped_settings(&["worktreeCleanup"], cx),
                Some("off") => {
                    this.update_scoped(json!({ "worktreeCleanup": { "mode": "off" } }), cx)
                }
                Some("custom") => this.update_scoped(
                    json!({ "worktreeCleanup": { "mode": "custom", "rules": {} } }),
                    cx,
                ),
                _ => {}
            },
        );
        row.control(control).into_any_element()
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
        project_scope: bool,
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
                        this.update_worktree_rule(key, json!(next), project_scope, cx)
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
        project_scope: bool,
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
                        this.update_worktree_rule(key, json!(days), project_scope, cx);
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
                    this.update_worktree_rule(key, days, project_scope, cx)
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
            let mut rows: Vec<AnyElement> = Vec::new();
            for entry in report.entries.iter() {
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
                rows.push(row.into_any_element());
            }
            if report.omitted > 0 {
                rows.push(
                    div()
                        .px_4()
                        .py(px(10.))
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(format!("{} more results.", report.omitted))
                        .into_any_element(),
                );
            }
            if report.entries.is_empty() && report.omitted == 0 {
                rows.push(
                    div()
                        .px_4()
                        .py(px(10.))
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Nothing needed cleanup.")
                        .into_any_element(),
                );
            }
            section = section.child(self.folded_settings_section(
                FoldedSection::new("storage-cleanup-last-run", "Last run").summary(Some(summary)),
                rows,
                cx,
            ));
        }
        section.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use turin_contracts::settings::default_server_settings;

    use super::*;

    #[test]
    fn reads_the_storage_the_page_shows_from_the_resolved_worktree_rules() {
        let mut settings = default_server_settings();
        settings.storage_cleanup.logs_after_days = None;
        let storage = effective_storage(&settings);
        assert_eq!(storage["worktreeKeepWhen"], json!("uncommitted-changes"));
        assert_eq!(storage["worktreeOnDelete"], json!(false));
        assert!(storage["logsAfterDays"].is_null());
    }

    #[test]
    fn describes_a_projects_cleanup_tier() {
        assert_eq!(
            ProjectCleanupMode::Off.description(),
            "Keep this project's worktrees until you delete them manually."
        );
        assert_eq!(
            ProjectCleanupMode::Custom.description(),
            "Use these rules for this project."
        );
        assert_eq!(
            ProjectCleanupMode::Inherit.description(),
            "Use each machine's worktree cleanup settings."
        );
        assert_eq!(ProjectCleanupMode::Inherit.label(), "Inherit");
        assert_eq!(ProjectCleanupMode::Custom.value(), "custom");
    }

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
