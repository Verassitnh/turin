//! Port of `ArchivedThreadsPanel` (SettingsPanels.tsx) for the single local
//! environment. The thread context menu (Unarchive / Delete on right click) is
//! not ported; the Unarchive button covers the common path and Delete lives on
//! the thread's own sidebar menu (PORTING.md Deviations).
//! Also ports upstream/apps/web/src/lib/archivedThreadsState.ts.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    rems,
};

use super::super::layout::{SettingsRow, SettingsSection};
use super::super::search::searchable_setting;
use super::super::time_format::format_relative_time_label;
use super::super::view::SettingsView;
use crate::components::project_monogram::ProjectMonogram;
use crate::theme::ActiveAppTheme;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

impl SettingsView {
    pub(super) fn render_archived(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let (id, title) = searchable_setting("archive");
        let now = chrono::Utc::now();

        // Groups in the order the projects first appear; threads newest first.
        let mut groups: Vec<(
            String,
            String,
            u32,
            Vec<&super::super::view_model::ArchivedThread>,
        )> = Vec::new();
        for thread in &vm.archived_threads {
            match groups
                .iter_mut()
                .find(|(name, ..)| *name == thread.project_name)
            {
                Some((_, _, _, threads)) => threads.push(thread),
                None => groups.push((
                    thread.project_name.clone(),
                    thread.project_monogram.clone(),
                    thread.project_color,
                    vec![thread],
                )),
            }
        }
        if groups.is_empty() {
            return vec![
                SettingsSection::new(title)
                    .id(id)
                    .child(
                        SettingsRow::new(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::Archive)
                                        .size(rems(0.875))
                                        .color(theme.muted_foreground),
                                )
                                .child("No archived threads"),
                        )
                        .description("Archived threads will appear here."),
                    )
                    .into_any_element(),
            ];
        }
        let mut sections = Vec::new();
        for (index, (project, monogram, project_color, mut threads)) in
            groups.into_iter().enumerate()
        {
            threads.sort_by(|left, right| {
                let left_key = if left.archived_at.is_empty() {
                    &left.created_at
                } else {
                    &left.archived_at
                };
                let right_key = if right.archived_at.is_empty() {
                    &right.created_at
                } else {
                    &right.archived_at
                };
                right_key.cmp(left_key).then_with(|| right.id.cmp(&left.id))
            });
            let mut section = SettingsSection::new(project).icon(ProjectMonogram::new(
                monogram,
                gpui_kit::rgb(project_color).into(),
            ));
            if index == 0 {
                section = section.id(id);
            }
            for thread in threads {
                let archived = format_relative_time_label(
                    if thread.archived_at.is_empty() {
                        &thread.created_at
                    } else {
                        &thread.archived_at
                    },
                    now,
                );
                let created = format_relative_time_label(&thread.created_at, now);
                let thread_id = thread.id.clone();
                section = section.child(
                    SettingsRow::new(thread.title.clone())
                        .description(format!("Archived {archived} \u{b7} Created {created}"))
                        .control(
                            Button::new(SharedString::from(format!("unarchive-{thread_id}")))
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Xs)
                                .icon(IconName::ArchiveRestore)
                                .icon_size(rems(0.875))
                                .label("Unarchive")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let source = this.store.read(cx).source();
                                    source.unarchive_thread(&thread_id, cx);
                                })),
                        ),
                );
            }
            sections.push(section.into_any_element());
        }
        sections
    }
}
