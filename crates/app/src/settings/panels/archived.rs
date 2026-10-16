//! Port of `ArchivedThreadsPanel` (SettingsPanels.tsx) for the single local
//! environment. The thread context menu (Unarchive / Delete on right click)
//! opens under the row instead of at the pointer (PORTING.md Deviations).
//! Also ports upstream/apps/web/src/lib/archivedThreadsState.ts.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, Styled as _, Window, div, px, rems,
};

use super::super::controls::{PopupAlign, SelectItem, popup_list, with_popup};
use super::super::dialog::{ConfirmSpec, Dialog};
use super::super::layout::{SettingsRow, SettingsSection};
use super::super::search::searchable_setting;
use super::super::time_format::format_relative_time_label;
use super::super::view::SettingsView;
use crate::components::project_monogram::ProjectMonogram;
use crate::pull_request::presentation_view::Spinner;
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
            // `isLoadingArchive` / `archiveError` / the empty list share one row.
            let loading = vm.archived_threads_loading;
            let error = vm.archived_threads_error.clone();
            let heading = if loading {
                "Loading archived threads"
            } else if error.is_some() {
                "Could not load archived threads"
            } else {
                "No archived threads"
            };
            let description = if loading {
                "Checking connected environments.".to_owned()
            } else {
                error.unwrap_or_else(|| "Archived threads will appear here.".to_owned())
            };
            let section = SettingsSection::new(title).child(
                SettingsRow::new(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(if loading {
                            Spinner::new("archived-loading")
                                .size(rems(0.875))
                                .color(theme.muted_foreground)
                                .into_any_element()
                        } else {
                            Icon::new(IconName::Archive)
                                .size(rems(0.875))
                                .color(theme.muted_foreground)
                                .into_any_element()
                        })
                        .child(heading),
                )
                .description(description),
            );
            // `id={isLoadingArchive ? undefined : …}`: the search target waits for the load.
            return vec![if loading { section } else { section.id(id) }.into_any_element()];
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
                let thread_title = thread.title.clone();
                let menu_id = SharedString::from(format!("archived-menu-{thread_id}"));
                let open = self.menus.is_open(&menu_id);
                let row = SettingsRow::new(thread.title.clone())
                    .description(format!("Archived {archived} \u{b7} Created {created}"))
                    .control({
                        let thread_id = thread_id.clone();
                        Button::new(SharedString::from(format!("unarchive-{thread_id}")))
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Xs)
                            .icon(IconName::ArchiveRestore)
                            .icon_size(rems(0.875))
                            .label("Unarchive")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let source = this.store.read(cx).source();
                                source.unarchive_thread(&thread_id, cx);
                            }))
                    });
                let toggle_id = menu_id.clone();
                let trigger = div()
                    .id(SharedString::from(format!("archived-row-{thread_id}")))
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.menus.open(toggle_id.clone());
                            cx.notify();
                        }),
                    )
                    .child(row);
                let popup = open.then(|| {
                    let entity = cx.entity();
                    let pick_id = thread_id.clone();
                    let pick_title = thread_title.clone();
                    popup_list(
                        SharedString::from(format!("archived-menu-list-{thread_id}")),
                        vec![
                            SelectItem::new("unarchive", "Unarchive"),
                            SelectItem::new("delete", "Delete"),
                        ],
                        Some(px(144.)),
                        cx,
                        Rc::new(move |choice, _, cx| {
                            let pick_id = pick_id.clone();
                            let pick_title = pick_title.clone();
                            entity.update(cx, |this, cx| {
                                this.menus.close();
                                match choice.as_ref() {
                                    "unarchive" => {
                                        let source = this.store.read(cx).source();
                                        source.unarchive_thread(&pick_id, cx);
                                    }
                                    "delete" => {
                                        this.confirm_and_delete_thread(pick_id, &pick_title, cx)
                                    }
                                    _ => {}
                                }
                                cx.notify();
                            });
                        }),
                    )
                    .into_any_element()
                });
                section = section.child(
                    with_popup(
                        trigger,
                        popup,
                        PopupAlign::Start,
                        rems(3.5).to_pixels(px(16.)),
                    )
                    .w_full(),
                );
            }
            sections.push(section.into_any_element());
        }
        sections
    }

    /// `confirmAndDeleteThread`: asks first when `confirmThreadDelete` is on.
    fn confirm_and_delete_thread(
        &mut self,
        thread_id: String,
        title: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.vm(cx).client.confirm_thread_delete {
            let source = self.store.read(cx).source();
            source.delete_thread(&thread_id, cx);
            return;
        }
        self.dialog = Some(Dialog::Confirm(ConfirmSpec {
            title: format!("Delete thread \"{title}\"?").into(),
            description: "This permanently clears conversation history for this thread.".into(),
            confirm_label: "Delete".into(),
            destructive: true,
            on_confirm: Rc::new(move |this, _, cx| {
                let source = this.store.read(cx).source();
                source.delete_thread(&thread_id, cx);
            }),
        }));
        cx.notify();
    }
}
