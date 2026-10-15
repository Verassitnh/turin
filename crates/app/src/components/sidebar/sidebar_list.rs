//! The thread list from upstream Sidebar.tsx: the draft block, the sortable
//! items (rows, `SidebarDragBoundary` labels, `SidebarSectionPlaceholder`
//! targets, shelf `SidebarSectionHeader`s), "Show N more", the empty
//! state, and the list motion (`createSidebarListMotion`).
//!
//! Geometry is predicted from the known row heights (card 82px, slim 36px,
//! shelf header 32px, markers 0, `gap-px`) plus the shelves' `mt-auto`
//! space, so motion and drag work from the layout this frame will produce
//! rather than last frame's DOM measurement.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Bounds, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px, rems,
};

use super::runtime::models::EnvironmentThreadShell;
use super::sidebar_logic::{
    SidebarListItem, SidebarListMarker, SidebarSection, resolve_sidebar_drop_verb,
    resolve_sidebar_row_accessibility, sidebar_list_item_id,
};
use super::sidebar_sections::SETTLED_TAIL_PAGE_COUNT;
use super::sidebar_thread_row::{CARD_ROW_HEIGHT, RowInput, SLIM_ROW_HEIGHT};
use super::view_model::{Shelf, SidebarDraft};
use super::{Sidebar, SidebarEvent, SidebarRoute};
use crate::components::project_favicon::ProjectFavicon;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    RADIUS_MD, SIDEBAR_CONTENT_INSET, SIDEBAR_ROW_CONTENT_INSET, TEXT_2XS, TEXT_SM, TEXT_XS,
};
use crate::ui::collapsible_section_header::{CollapsibleSectionHeader, CollapsibleSectionTone};
use crate::ui::{Icon, IconName, Tooltip};

/// The shelf header box (`h-8`).
const SHELF_HEADER_HEIGHT: f32 = 2.;

/// `(top, height)` of each item relative to the list top, in px.
pub fn predicted_layout(
    items: &[SidebarListItem],
    rem: f32,
    list_height: f32,
    _dragging: bool,
) -> Vec<(f32, f32)> {
    let heights: Vec<f32> = items
        .iter()
        .map(|item| match item {
            SidebarListItem::Thread { section, .. } => match section {
                SidebarSection::Pinned | SidebarSection::Active | SidebarSection::Working => {
                    CARD_ROW_HEIGHT * rem
                }
                SidebarSection::Snoozed | SidebarSection::Settled => SLIM_ROW_HEIGHT * rem,
            },
            SidebarListItem::Marker(marker) if marker.is_shelf_header() => {
                SHELF_HEADER_HEIGHT * rem
            }
            SidebarListItem::Marker(_) => 0.,
        })
        .collect();
    let content: f32 = heights.iter().map(|height| height + 1.).sum::<f32>()
        - if heights.is_empty() { 0. } else { 1. };
    let mut shelf_space = (list_height - content).max(0.);
    let mut top = 0.;
    items
        .iter()
        .zip(heights)
        .map(|(item, height)| {
            if matches!(item, SidebarListItem::Marker(marker) if marker.is_shelf_header()) {
                top += shelf_space;
                shelf_space = 0.;
            }
            let entry = (top, height);
            top += height + 1.;
            entry
        })
        .collect()
}

impl Sidebar {
    /// The list for the current frame.
    pub(crate) fn render_thread_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let (partition, layout) = self.compute_sections(cx);
        let snapshot = self.snapshot(cx).clone();
        let rem = f32::from(window.rem_size());
        let list_height = self.list_box().map_or(0., |(_, height, _, _)| height);
        let geometry = predicted_layout(&layout.items, rem, list_height, self.drag.is_some());

        // ── Motion (paused while a drag owns the transforms) ───────────
        let now = self.motion_now();
        let dragging = self.drag.as_ref().is_some_and(|drag| drag.active.is_some());
        if !dragging {
            let positions = layout
                .items
                .iter()
                .zip(&geometry)
                .map(|(item, (top, height))| {
                    (
                        sidebar_list_item_id(item),
                        super::sidebar_motion::RowPosition {
                            top: *top,
                            left: 0.,
                            width: self.list_box().map_or(0., |(_, _, _, width)| width),
                            height: *height,
                        },
                    )
                })
                .collect();
            let has_rows = !layout.items.is_empty() || !snapshot.drafts.is_empty();
            self.motion.update(has_rows, positions, now);
            self.motion.tick(now);
            if self.motion.is_animating(now) {
                window.request_animation_frame();
            }
        }

        let route_key = self.route_thread_key().map(str::to_string);
        let from = self.drag_source().map(|(_, section)| section);
        let target = self.drag_target_section().flatten();
        let threads_by_key: std::collections::HashMap<
            String,
            (EnvironmentThreadShell, SidebarSection),
        > = partition
            .pinned
            .iter()
            .map(|t| (t, SidebarSection::Pinned))
            .chain(partition.active.iter().map(|t| (t, SidebarSection::Active)))
            .chain(
                partition
                    .working
                    .iter()
                    .map(|t| (t, SidebarSection::Working)),
            )
            .chain(
                partition
                    .snoozed
                    .iter()
                    .map(|t| (t, SidebarSection::Snoozed)),
            )
            .chain(
                partition
                    .settled
                    .iter()
                    .map(|t| (t, SidebarSection::Settled)),
            )
            .map(|(thread, section)| (thread.key(), (thread.clone(), section)))
            .collect();
        let sweep = self
            .sweep
            .as_ref()
            .map(|sweep| (sweep.action, sweep.keys.clone()));
        let lifted_key = self.drag_source().map(|(key, _)| key);

        let mut children: Vec<AnyElement> = Vec::new();
        let mut lifted: Option<AnyElement> = None;
        for (index, item) in layout.items.iter().enumerate() {
            let id = sidebar_list_item_id(item);
            let (offset, hidden) = match self.drag_offset(index) {
                Some((offset, hidden)) => (offset, hidden),
                None => (self.motion.sample(&id, now).translate_y, false),
            };
            let opacity = if dragging {
                1.
            } else {
                self.motion.sample(&id, now).opacity
            };
            let element: AnyElement = match item {
                SidebarListItem::Thread { key, section } => {
                    let Some((thread, _)) = threads_by_key.get(key) else {
                        continue;
                    };
                    let is_lifted = lifted_key.as_deref() == Some(key.as_str());
                    let row = self.render_thread_row(
                        RowInput {
                            thread,
                            section: *section,
                            is_active: route_key.as_deref() == Some(key.as_str()),
                            jump_index: layout.ordered_keys.iter().position(|k| k == key),
                            drag_lifted: is_lifted,
                            drop_verb: if is_lifted {
                                from.and_then(|from| resolve_sidebar_drop_verb(from, target))
                            } else {
                                None
                            },
                            drag_over_pinned: is_lifted && target == Some(SidebarSection::Pinned),
                            sweep_action: sweep
                                .as_ref()
                                .filter(|(_, keys)| keys.contains(key))
                                .map(|(action, _)| *action),
                            ghost: false,
                        },
                        window,
                        cx,
                    );
                    let wrapped = div()
                        .id(SharedString::from(format!("item-{key}")))
                        .w_full()
                        .flex_shrink_0()
                        .when(
                            matches!(
                                section,
                                SidebarSection::Pinned
                                    | SidebarSection::Active
                                    | SidebarSection::Working
                            ),
                            |this| this.py(px(2.)),
                        )
                        .child(row);
                    if is_lifted {
                        // Paint the lifted card above its peers: an empty slot
                        // in flow, the card in the overlay below.
                        let (top, height) = geometry[index];
                        lifted = Some(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(top + offset))
                                .h(px(height))
                                .child(wrapped)
                                .into_any_element(),
                        );
                        div()
                            .h(px(geometry[index].1))
                            .flex_shrink_0()
                            .into_any_element()
                    } else {
                        wrapped.into_any_element()
                    }
                }
                SidebarListItem::Marker(marker) => {
                    self.render_marker(*marker, from, target, &partition, &layout, &snapshot, cx)
                }
            };
            let first_shelf = matches!(item, SidebarListItem::Marker(marker) if marker.is_shelf_header())
                && !layout.items[..index]
                    .iter()
                    .any(|earlier| matches!(earlier, SidebarListItem::Marker(marker) if marker.is_shelf_header()));
            children.push(
                div()
                    .relative()
                    .flex_shrink_0()
                    .when(first_shelf, |this| this.mt_auto())
                    .top(px(offset))
                    .when(hidden, |this| this.opacity(0.))
                    .when(opacity < 1., |this| this.opacity(opacity))
                    .child(element)
                    .into_any_element(),
            );
        }

        // Exit ghosts: removed rows fading where they were.
        let ghosts: Vec<AnyElement> = self
            .motion
            .exiting()
            .iter()
            .filter_map(|ghost| {
                let (thread, section) = self
                    .last_rows
                    .iter()
                    .find(|(key, _, _)| *key == ghost.key)
                    .map(|(_, thread, section)| (thread.clone(), *section))?;
                Some((ghost.clone(), thread, section))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(ghost, thread, section)| {
                let (translate, opacity) = ghost.sample(now);
                let row = self.render_thread_row(
                    RowInput {
                        thread: &thread,
                        section,
                        is_active: false,
                        jump_index: None,
                        drag_lifted: false,
                        drop_verb: None,
                        drag_over_pinned: false,
                        sweep_action: None,
                        ghost: true,
                    },
                    window,
                    cx,
                );
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(ghost.position.top + translate))
                    .h(px(ghost.position.height))
                    .opacity(opacity)
                    .when(
                        matches!(
                            section,
                            SidebarSection::Pinned
                                | SidebarSection::Active
                                | SidebarSection::Working
                        ),
                        |this| this.py(px(2.)),
                    )
                    .child(row)
                    .into_any_element()
            })
            .collect();
        self.last_rows = threads_by_key
            .into_iter()
            .map(|(key, (thread, section))| (key, thread, section))
            .collect();

        let total = partition.total();
        let drafts = self.render_draft_block(&snapshot, cx);
        let show_more =
            (snapshot.settled_shelf_expanded && layout.hidden_settled_count > 0).then(|| {
                let count = layout.hidden_settled_count.min(SETTLED_TAIL_PAGE_COUNT);
                div()
                    .id("sidebar-show-more")
                    .flex()
                    .h_9()
                    .w_full()
                    .items_center()
                    .gap(rems(0.625))
                    .rounded(RADIUS_MD)
                    .px(rems(0.625))
                    .cursor(CursorStyle::PointingHand)
                    .text_size(TEXT_SM.size)
                    .text_color(with_alpha(scope.sidebar_muted_foreground, 0.55))
                    .hover(move |style| {
                        style
                            .bg(scope.sidebar_row_hover)
                            .text_color(scope.sidebar_foreground)
                    })
                    .child(
                        Icon::new(IconName::Plus)
                            .color(with_alpha(scope.sidebar_muted_foreground, 0.55).into()),
                    )
                    .child(format!("Show {count} more"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settled_visible_count += SETTLED_TAIL_PAGE_COUNT;
                        cx.notify();
                    }))
            });
        let empty = (snapshot.drafts.is_empty() && total == 0).then(|| {
            let muted = with_alpha(theme.muted_foreground, 0.6);
            let scoped = snapshot.project_scope_key.as_ref().and_then(|key| {
                snapshot
                    .project_groups
                    .iter()
                    .find(|group| &group.project_key == key)
            });
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .px_2()
                .py_6()
                .text_size(TEXT_XS.size)
                .text_color(muted)
                .map(|this| {
                    if snapshot.projects.is_empty() {
                        this.child("No projects yet").child(
                            div()
                                .id("sidebar-empty-add-project")
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .rounded(RADIUS_MD)
                                .border_1()
                                .border_color(scope.sidebar_border)
                                .px(rems(0.625))
                                .py_1()
                                .cursor(CursorStyle::PointingHand)
                                .text_size(TEXT_2XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(scope.sidebar_muted_foreground)
                                .hover(move |style| {
                                    style
                                        .bg(scope.sidebar_row_hover)
                                        .text_color(scope.sidebar_foreground)
                                })
                                .child(Icon::new(IconName::Plus).size(rems(0.75)))
                                .child("Add project")
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::AddProject)),
                                ),
                        )
                    } else if let Some(group) = scoped {
                        this.child(format!("No threads in {} yet", group.display_name))
                    } else {
                        this.child("No threads yet")
                    }
                })
        });

        let cell = self.list_box_cell.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .children(drafts)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when(total > 0, |this| this.flex_1())
                    .on_children_prepainted(move |bounds: Vec<Bounds<Pixels>>, window, _| {
                        let Some(bounds) = bounds.first() else {
                            return;
                        };
                        let next = Some((
                            f32::from(bounds.origin.y),
                            f32::from(bounds.size.height),
                            f32::from(bounds.origin.x),
                            f32::from(bounds.size.width),
                        ));
                        if cell.get() != next {
                            cell.set(next);
                            window.refresh();
                        }
                    })
                    .child(
                        div()
                            .id("sidebar-thread-list")
                            .relative()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .when(total > 0, |this| this.flex_1())
                            .children(children)
                            .children(ghosts)
                            .children(lifted),
                    ),
            )
            .children(show_more)
            .children(empty)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_marker(
        &mut self,
        marker: SidebarListMarker,
        from: Option<SidebarSection>,
        target: Option<SidebarSection>,
        partition: &super::sidebar_sections::SidebarPartition,
        layout: &super::sidebar_sections::SidebarListLayout,
        snapshot: &super::view_model::SidebarSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        match marker {
            // `SidebarDragBoundary`: zero-height at rest; a label while dragging.
            SidebarListMarker::PinnedHeader | SidebarListMarker::PinnedDivider => {
                let (label, section) = if marker == SidebarListMarker::PinnedHeader {
                    ("Pinned", SidebarSection::Pinned)
                } else {
                    ("Active", SidebarSection::Active)
                };
                let is_target = target == Some(section);
                div()
                    .relative()
                    .mx(px(2.))
                    .h(px(0.))
                    .when(from.is_some(), |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_2()
                                .right_2()
                                .top_1()
                                .flex()
                                .h_4()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_XS.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if is_target {
                                            theme.primary
                                        } else {
                                            with_alpha(scope.sidebar_foreground, 0.8).into()
                                        })
                                        .child(label),
                                )
                                .child(div().h(px(1.)).flex_1().bg(if is_target {
                                    with_alpha(theme.primary, 0.5)
                                } else {
                                    with_alpha(scope.sidebar_foreground, 0.25)
                                })),
                        )
                    })
                    .into_any_element()
            }
            // `SidebarSectionPlaceholder`: a dashed target while dragging.
            SidebarListMarker::ActivePlaceholder | SidebarListMarker::SettledPlaceholder => {
                let (label, section, rows) = if marker == SidebarListMarker::ActivePlaceholder {
                    ("Active", SidebarSection::Active, partition.active.len())
                } else {
                    (
                        "Settled",
                        SidebarSection::Settled,
                        layout.rendered_settled.len(),
                    )
                };
                let show = from.is_some_and(|from| {
                    rows == 0
                        || (from == section
                            && rows == 1
                            && target.is_some()
                            && target != Some(section))
                });
                let is_target = target == Some(section);
                div()
                    .relative()
                    .mx(px(2.))
                    .h(px(0.))
                    .when(show, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top_0()
                                .flex()
                                .h_9()
                                .items_center()
                                .justify_center()
                                .rounded(RADIUS_MD)
                                .border_1()
                                .border_dashed()
                                .border_color(if is_target {
                                    with_alpha(theme.primary, 0.4)
                                } else {
                                    with_alpha(scope.sidebar_foreground, 0.25)
                                })
                                .when(is_target, |this| this.bg(with_alpha(theme.primary, 0.05)))
                                .text_size(TEXT_XS.size)
                                .text_color(if is_target {
                                    theme.primary
                                } else {
                                    with_alpha(scope.sidebar_foreground, 0.8).into()
                                })
                                .child(label),
                        )
                    })
                    .into_any_element()
            }
            SidebarListMarker::WorkingHeader
            | SidebarListMarker::SnoozedHeader
            | SidebarListMarker::SettledHeader => {
                let (shelf, label, count, expanded) = match marker {
                    SidebarListMarker::WorkingHeader => (
                        Shelf::Working,
                        "Working",
                        partition.working.len(),
                        snapshot.working_shelf_expanded,
                    ),
                    SidebarListMarker::SnoozedHeader => (
                        Shelf::Snoozed,
                        "Snoozed",
                        partition.snoozed.len(),
                        snapshot.snoozed_shelf_expanded,
                    ),
                    _ => (
                        Shelf::Settled,
                        "Settled",
                        partition.settled.len(),
                        snapshot.settled_shelf_expanded,
                    ),
                };
                let label = if expanded {
                    label.to_string()
                } else {
                    format!("{label} ({count})")
                };
                let is_settled = shelf == Shelf::Settled;
                let tone = if is_settled && target == Some(SidebarSection::Settled) {
                    CollapsibleSectionTone::Accent
                } else if is_settled && from.is_some() {
                    CollapsibleSectionTone::Emphasized
                } else if shelf == Shelf::Snoozed {
                    CollapsibleSectionTone::Info
                } else {
                    CollapsibleSectionTone::Muted
                };
                div()
                    .mx(px(2.))
                    .h_8()
                    .child(
                        CollapsibleSectionHeader::new(
                            SharedString::from(format!("sidebar-{label}-shelf-toggle")),
                            label,
                            expanded,
                        )
                        .tone(tone)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_shelf(shelf, cx))),
                    )
                    .into_any_element()
            }
        }
    }

    /// `SidebarDraftBlock`: unsent new-thread drafts above the pinned block.
    fn render_draft_block(
        &mut self,
        snapshot: &super::view_model::SidebarSnapshot,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scoped = self.scoped_project_keys(cx);
        let mut drafts: Vec<&SidebarDraft> = snapshot
            .drafts
            .iter()
            .filter(|draft| {
                scoped
                    .as_ref()
                    .is_none_or(|keys| keys.contains(&draft.project_key()))
            })
            .collect();
        if drafts.is_empty() {
            return None;
        }
        drafts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let rows: Vec<AnyElement> = drafts
            .into_iter()
            .map(|draft| self.render_draft_row(draft, snapshot, cx))
            .collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .children(rows)
                .child(
                    div()
                        .mx(rems(0.625))
                        .my(rems(0.375))
                        .h(px(1.))
                        .bg(with_alpha(scope.sidebar_border, 0.6)),
                )
                .into_any_element(),
        )
    }

    /// `SidebarDraftRow`: project line, then the typed prompt.
    fn render_draft_row(
        &mut self,
        draft: &SidebarDraft,
        snapshot: &super::view_model::SidebarSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let project = snapshot.project(&draft.project_key()).cloned();
        let name = snapshot
            .project_group_for(&draft.project_key())
            .map(|group| group.display_name.clone())
            .unwrap_or_default();
        let preview = if draft.prompt_preview.is_empty() {
            format!(
                "{} attachment{}",
                draft.attachment_count,
                if draft.attachment_count == 1 { "" } else { "s" }
            )
        } else {
            draft.prompt_preview.clone()
        };
        let _ =
            resolve_sidebar_row_accessibility(&preview, Some("Unsent draft"), Some(&name), false);
        let is_active = self.route == Some(SidebarRoute::Draft(draft.draft_id.clone()));
        let key = format!("draft-{}", draft.draft_id);
        let is_hovered = self.hovered_row.as_deref() == Some(key.as_str());
        let draft_id = draft.draft_id.clone();
        let discard_id = draft.draft_id.clone();
        let hover_key = key.clone();
        let warning = theme.warning;
        let row = div()
            .id(SharedString::from(key.clone()))
            .relative()
            .w_full()
            .overflow_hidden()
            .rounded(RADIUS_MD)
            .cursor(CursorStyle::PointingHand)
            .text_color(scope.sidebar_foreground)
            .map(|this| {
                if is_active {
                    this.bg(scope.sidebar_row_active)
                } else if is_hovered {
                    this.bg(with_alpha(warning, 0.08))
                } else {
                    this.bg(with_alpha(warning, 0.04))
                }
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let next = hovered.then(|| hover_key.clone());
                if this.hovered_row != next
                    && (*hovered || this.hovered_row.as_deref() == Some(hover_key.as_str()))
                {
                    this.hovered_row = next;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.navigate_to_draft(&draft_id, cx)))
            .child(
                div()
                    .h(rems(4.875))
                    .px(SIDEBAR_ROW_CONTENT_INSET)
                    .py(SIDEBAR_CONTENT_INSET)
                    .child(
                        div()
                            .flex()
                            .h_5()
                            .min_w_0()
                            .items_center()
                            .gap(rems(0.375))
                            .child(
                                Icon::new(IconName::SquarePen)
                                    .size(rems(0.75))
                                    .color(theme.warning_foreground),
                            )
                            .children(project.as_ref().map(ProjectFavicon::new))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.secondary_label)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .flex()
                                    .h_5()
                                    .min_w_5()
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_end()
                                    .when(is_hovered, |this| {
                                        this.child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "discard-{}",
                                                    discard_id
                                                )))
                                                .flex()
                                                .items_center()
                                                .rounded(RADIUS_MD)
                                                .px_1()
                                                .cursor(CursorStyle::PointingHand)
                                                .text_color(theme.muted_foreground)
                                                .hover(|style| style.text_color(theme.foreground))
                                                .tooltip(|window, cx| {
                                                    Tooltip::new("Discard draft").build(window, cx)
                                                })
                                                .on_mouse_down(
                                                    gpui_kit::MouseButton::Left,
                                                    |_, _, cx| cx.stop_propagation(),
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.discard_draft(&discard_id, cx);
                                                }))
                                                .child(Icon::new(IconName::X).size(rems(0.75))),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .mt(rems(0.125))
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(with_alpha(scope.foreground, 0.9))
                            .child(preview),
                    ),
            );
        div()
            .py(px(2.))
            .child(gpui_kit::component::menu::ContextMenuExt::context_menu(
                row,
                Sidebar::draft_context_menu(cx.entity().downgrade(), draft.draft_id.clone()),
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicts_card_slim_and_shelf_space() {
        let items = vec![
            SidebarListItem::Marker(SidebarListMarker::PinnedHeader),
            SidebarListItem::thread("a", SidebarSection::Active),
            SidebarListItem::Marker(SidebarListMarker::SettledHeader),
            SidebarListItem::thread("s", SidebarSection::Settled),
        ];
        let geometry = predicted_layout(&items, 16., 0., false);
        assert_eq!(geometry, vec![(0., 0.), (1., 82.), (84., 32.), (117., 36.)]);
        let tall = predicted_layout(&items, 16., 300., false);
        // 300 - 153 content = 147 of shelf space before the settled header.
        assert_eq!(tall[2].0, 84. + 147.);
        assert_eq!(tall[3].0 + tall[3].1, 300.);
    }
}
