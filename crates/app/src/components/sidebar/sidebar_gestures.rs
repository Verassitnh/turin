//! The row drag (reorder / move between shelves) and the action sweep from
//! upstream Sidebar.tsx (`handleThreadDragStart/Over/End`, `startActionSweep`)
//! on GPUI input. dnd-kit's role is played by [`super::sidebar_pointer`]
//! (gesture), [`super::sidebar_drag`] (collision + projection) and the
//! list's predicted geometry (`sidebar_list::predicted_layout`).
//!
//! Not ported: dragging threads out of the list as composer context
//! (`threadContextDrag`, composer task), auto-scroll while dragging, and
//! dropping files on rows.

use std::collections::{HashMap, HashSet};

use gpui_kit::{
    AnyElement, App, Bounds, Context, DispatchPhase, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Styled as _, Window, canvas,
};

use super::Sidebar;
use crate::state::models::EnvironmentThreadShell;
use super::sidebar_drag::{
    CollisionArgs, Rect, STATIONARY, SidebarCollisionDetection, SidebarSortingStrategy, SortLayout,
    SortingStrategyInput, Transform, restrict_below_sidebar_label,
};
use super::sidebar_logic::{
    DropSection, SidebarDropInput, SidebarListItem, SidebarListMarker, SidebarSection,
    SidebarThreadDropPlan, plan_sidebar_thread_drop, resolve_sidebar_drop_target,
    resolve_sidebar_sweep_keys, sidebar_list_item_id, sidebar_thread_key_at_y,
    sort_inbox_threads_by_return,
};
use super::sidebar_pointer::{Point, PointerSensorHandler, SidebarPointerSensor};
use super::sidebar_sections::OptimisticDrop;
use super::view_model::ThreadRef;
use crate::timestamp_format::now_iso;

/// Pointer travel before a press on a row starts a drag, or a press on its
/// action button starts a sweep. Shorter presses stay clicks.
pub const SIDEBAR_DRAG_DISTANCE: f32 = 6.;

/// Zero-height markers reserve no label space at rest. During a drag the
/// sorting strategy opens 24px for a 16px label with 4px clearance.
pub const SIDEBAR_DRAG_LABEL_HEIGHT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepAction {
    Settle,
    Unsettle,
    Unsnooze,
}

/// An in-progress row drag.
pub(crate) struct DragSession {
    pub key: String,
    pub sensor: SidebarPointerSensor,
    pub origin: Point,
    pub active: Option<DragActive>,
}

pub(crate) struct DragActive {
    pub active_section: SidebarSection,
    pub items: Vec<SidebarListItem>,
    /// Item rects at pickup, window coordinates, without transforms.
    pub rects: Vec<Rect>,
    pub active_index: usize,
    pub over_index: Option<usize>,
    pub target_section: Option<SidebarSection>,
    pub pointer: Point,
    pub detector: SidebarCollisionDetection,
    pub strategy: SidebarSortingStrategy,
    /// `dragLabelOffsetRef`: Pins label bottom relative to the list top.
    pub label_offset: f32,
    pub transforms: Vec<Transform>,
}

/// An in-progress action sweep.
pub(crate) struct SweepSession {
    pub action: SweepAction,
    pub origin_key: String,
    pub sensor: SidebarPointerSensor,
    pub target: Option<String>,
    pub keys: Vec<String>,
}

/// Collects sensor callbacks so they can be applied with `&mut Sidebar`.
#[derive(Default)]
struct Events {
    start: bool,
    moves: Vec<Point>,
    end: bool,
    cancel: bool,
    finish: Option<bool>,
}

impl PointerSensorHandler for Events {
    fn on_start(&mut self, _: Point) {
        self.start = true;
    }
    fn on_move(&mut self, point: Point) {
        self.moves.push(point);
    }
    fn on_end(&mut self) {
        self.end = true;
    }
    fn on_cancel(&mut self) {
        self.cancel = true;
    }
    fn on_finish(&mut self, started: bool) {
        self.finish = Some(started);
    }
}

fn point(position: gpui_kit::Point<Pixels>) -> Point {
    Point::new(f32::from(position.x), f32::from(position.y))
}

impl Sidebar {
    /// A primary press on a row arms a drag (if the row can move).
    pub(crate) fn press_row(&mut self, key: &str, event: &MouseDownEvent, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left || event.modifiers.platform || event.modifiers.shift {
            return;
        }
        if let Some(sensor) = self.drag.as_mut().map(|drag| &mut drag.sensor) {
            sensor.pointer_down();
        }
        let (partition, _) = self.compute_sections(cx);
        let section = partition.section_of(key);
        let renaming = self.rename.as_ref().is_some_and(|rename| rename.key == key);
        if renaming
            || section.is_none()
            || section == Some(SidebarSection::Working)
            || !partition.draggable.contains(key)
            || self.optimistic_drop.is_some()
        {
            return;
        }
        let origin = point(event.position);
        let mut events = Events::default();
        let sensor = SidebarPointerSensor::new(origin, 0, SIDEBAR_DRAG_DISTANCE, &mut events);
        self.drag = Some(DragSession {
            key: key.to_string(),
            sensor,
            origin,
            active: None,
        });
    }

    /// A press on a row's action button arms a sweep.
    pub(crate) fn start_action_sweep(
        &mut self,
        key: &str,
        action: SweepAction,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let mut events = Events::default();
        let sensor =
            SidebarPointerSensor::new(point(event.position), 0, SIDEBAR_DRAG_DISTANCE, &mut events);
        self.sweep = Some(SweepSession {
            action,
            origin_key: key.to_string(),
            sensor,
            target: None,
            keys: Vec::new(),
        });
        cx.notify();
    }

    /// The release click after a drag must not navigate.
    pub(crate) fn consume_drag_click(&mut self) -> bool {
        match &mut self.drag {
            Some(drag) if drag.sensor.is_finished() => {
                let consumed = drag.sensor.consume_click();
                self.drag = None;
                consumed
            }
            Some(drag) if drag.active.is_none() => {
                // A plain click: the press never became a drag.
                self.drag = None;
                false
            }
            _ => false,
        }
    }

    /// The release click after a sweep is consumed; a plain click applies.
    pub(crate) fn consume_sweep_click(&mut self) -> bool {
        match self.sweep.take() {
            Some(mut sweep) => {
                let consumed = sweep.sensor.consume_click() && !sweep.keys.is_empty();
                if !sweep.sensor.is_finished() {
                    self.sweep = Some(sweep);
                }
                consumed
            }
            None => false,
        }
    }

    fn can_sweep(
        &mut self,
        key: &str,
        origin_section: Option<SidebarSection>,
        action: SweepAction,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(thread) = self.thread(key, cx) else {
            return false;
        };
        let (partition, _) = self.compute_sections(cx);
        let caps = self.snapshot(cx).capabilities(&thread.environment_id);
        partition.section_of(key) == origin_section
            && match action {
                SweepAction::Unsnooze => caps.is_some_and(|c| c.thread_snooze),
                _ => caps.is_some_and(|c| c.thread_settlement),
            }
    }

    fn pointer_moved(
        &mut self,
        position: Point,
        primary_down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(mut sweep) = self.sweep.take() {
            let mut events = Events::default();
            sweep
                .sensor
                .pointer_move(0, position, primary_down, &mut events);
            self.sweep = Some(sweep);
            self.apply_sweep_events(events, window, cx);
            return;
        }
        let Some(mut drag) = self.drag.take() else {
            return;
        };
        let mut events = Events::default();
        drag.sensor
            .pointer_move(0, position, primary_down, &mut events);
        self.drag = Some(drag);
        self.apply_drag_events(events, window, cx);
    }

    /// The screenshot harness's synthetic pointer travel.
    pub(crate) fn debug_pointer_move(
        &mut self,
        position: Point,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pointer_moved(position, true, window, cx);
    }

    fn pointer_released(&mut self, position: Point, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut sweep) = self.sweep.take() {
            let mut events = Events::default();
            sweep.sensor.pointer_up(0, position, &mut events);
            self.sweep = Some(sweep);
            self.apply_sweep_events(events, window, cx);
            return;
        }
        let Some(mut drag) = self.drag.take() else {
            return;
        };
        let mut events = Events::default();
        drag.sensor.pointer_up(0, position, &mut events);
        self.drag = Some(drag);
        self.apply_drag_events(events, window, cx);
    }

    fn cancel_gesture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut sweep) = self.sweep.take() {
            let mut events = Events::default();
            sweep.sensor.cancel(&mut events);
            self.sweep = Some(sweep);
            self.apply_sweep_events(events, window, cx);
        }
        if let Some(mut drag) = self.drag.take() {
            let mut events = Events::default();
            drag.sensor.cancel(&mut events);
            self.drag = Some(drag);
            self.apply_drag_events(events, window, cx);
        }
    }

    fn apply_sweep_events(&mut self, events: Events, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sweep) = &self.sweep else {
            return;
        };
        let origin = sweep.origin_key.clone();
        let action = sweep.action;
        let (partition, layout) = self.compute_sections(cx);
        let origin_section = partition.section_of(&origin);
        let sweep_to = |this: &mut Self, target: String, cx: &mut Context<Self>| {
            if this.sweep.as_ref().and_then(|s| s.target.as_ref()) == Some(&target) {
                return;
            }
            let eligible: HashSet<String> = layout
                .ordered_keys
                .iter()
                .filter(|key| this.can_sweep(key, origin_section, action, cx))
                .cloned()
                .collect();
            let keys = resolve_sidebar_sweep_keys(&layout.ordered_keys, &origin, &target, |key| {
                eligible.contains(key)
            });
            if let Some(sweep) = &mut this.sweep {
                sweep.target = Some(target);
                sweep.keys = keys;
            }
        };
        if events.start {
            sweep_to(self, origin.clone(), cx);
        }
        if let Some(last) = events.moves.last() {
            let rows = self.row_tops(cx);
            let viewport = self
                .list_box()
                .map(|(top, height, _, _)| (top, top + height));
            if let Some(key) = sidebar_thread_key_at_y(&rows, last.y, viewport) {
                sweep_to(self, key, cx);
            }
        }
        if events.end {
            // Rows that changed section mid-gesture are skipped.
            let keys: Vec<String> = self
                .sweep
                .as_ref()
                .map(|s| s.keys.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|key| self.can_sweep(key, origin_section, action, cx))
                .collect();
            match action {
                SweepAction::Settle => self.settle_threads(&keys, window, cx),
                SweepAction::Unsettle | SweepAction::Unsnooze => {
                    for key in &keys {
                        let Some(thread) = self.thread(key, cx) else {
                            continue;
                        };
                        if action == SweepAction::Unsettle {
                            self.attempt_unsettle(ThreadRef::of(&thread), window, cx);
                        } else {
                            self.attempt_unsnooze(ThreadRef::of(&thread), window, cx);
                        }
                    }
                }
            }
        }
        if events.finish.is_some()
            && let Some(sweep) = &mut self.sweep
        {
            // Keep the finished session until its release click is consumed.
            if sweep.keys.len() <= 1 && !events.cancel {
                sweep.keys.clear();
            }
            if events.finish == Some(false) {
                self.sweep = None;
            }
        }
        cx.notify();
    }

    /// Thread rows with their window tops, list order.
    pub(crate) fn row_tops(&mut self, cx: &App) -> Vec<(String, f32)> {
        let (_, layout) = self.compute_sections(cx);
        let Some((list_top, list_height, _, _)) = self.list_box() else {
            return Vec::new();
        };
        let geometry = super::sidebar_list::predicted_layout(
            &layout.items,
            self.rem_px(cx),
            list_height,
            false,
        );
        layout
            .items
            .iter()
            .zip(geometry)
            .filter_map(|(item, (top, _))| match item {
                SidebarListItem::Thread { key, .. } => Some((key.clone(), list_top + top)),
                SidebarListItem::Marker(_) => None,
            })
            .collect()
    }

    pub(crate) fn rem_px(&self, _: &App) -> f32 {
        16.
    }

    fn apply_drag_events(&mut self, events: Events, window: &mut Window, cx: &mut Context<Self>) {
        if events.start {
            self.begin_drag(cx);
        }
        if let Some(last) = events.moves.last() {
            self.drag_over(*last, cx);
        }
        if events.end {
            self.end_drag(window, cx);
        }
        if let Some(started) = events.finish {
            if started {
                self.release_drag(cx);
            }
            if let Some(drag) = &mut self.drag {
                drag.active = None;
            }
            if !started {
                self.drag = None;
            }
        }
        cx.notify();
    }

    fn begin_drag(&mut self, cx: &mut Context<Self>) {
        let Some((list_top, list_height, list_left, list_width)) = self.list_box() else {
            return;
        };
        let (partition, layout) = self.compute_sections(cx);
        let Some((key, origin)) = self
            .drag
            .as_ref()
            .map(|drag| (drag.key.clone(), drag.origin))
        else {
            return;
        };
        let Some(active_section) = partition.section_of(&key) else {
            return;
        };
        // Stop section motion before measuring the picked-up row.
        self.motion.suspend();
        let rem = self.rem_px(cx);
        let geometry =
            super::sidebar_list::predicted_layout(&layout.items, rem, list_height, false);
        let rects: Vec<Rect> = geometry
            .iter()
            .map(|(top, height)| Rect::new(list_top + top, *height, list_left, list_width))
            .collect();
        let Some(active_index) = layout
            .items
            .iter()
            .position(|item| sidebar_list_item_id(item) == key)
        else {
            return;
        };
        let label_offset = layout
            .items
            .iter()
            .position(|item| *item == SidebarListItem::Marker(SidebarListMarker::PinnedHeader))
            .map(|index| geometry[index].0 + SIDEBAR_DRAG_LABEL_HEIGHT)
            .unwrap_or(0.);
        let snapshot = self.snapshot(cx);
        let working_enabled = snapshot.settings.working_shelf_enabled;
        let occurred_at = now_iso();
        // Settled (and the time-ordered inbox) preview the slot a drop lands in.
        let key_of = EnvironmentThreadShell::key;
        let lifted = partition
            .pinned
            .iter()
            .chain(&partition.active)
            .chain(&partition.snoozed)
            .chain(&partition.settled)
            .find(|thread| thread.key() == key)
            .cloned();
        let settled_order: Vec<String> = lifted
            .as_ref()
            .map(|thread| {
                let mut settled: Vec<EnvironmentThreadShell> = partition
                    .settled
                    .iter()
                    .filter(|t| t.key() != key)
                    .cloned()
                    .collect();
                settled.push(super::sidebar_logic::apply_sidebar_thread_drop(
                    thread,
                    DropSection::Settled,
                    &occurred_at,
                    None,
                ));
                super::runtime::thread_sort::sort_settled_threads(settled)
                    .iter()
                    .map(key_of)
                    .collect()
            })
            .unwrap_or_default();
        let active_order = (working_enabled && lifted.is_some()).then(|| {
            let mut active: Vec<EnvironmentThreadShell> = partition
                .active
                .iter()
                .filter(|t| t.key() != key)
                .cloned()
                .collect();
            active.push(super::sidebar_logic::apply_sidebar_thread_drop(
                lifted.as_ref().expect("checked"),
                DropSection::Active,
                &occurred_at,
                None,
            ));
            sort_inbox_threads_by_return(active, |thread| self.inbox_returns.returned_at(thread))
                .iter()
                .map(key_of)
                .collect()
        });
        let strategy = SidebarSortingStrategy::new(SortingStrategyInput {
            items: layout.items.clone(),
            enabled: Some(true),
            settled_order,
            active_order,
            settled_expanded: snapshot.settled_shelf_expanded,
            settled_visible_count: Some(self.settled_visible_count),
            route_thread_key: self.route_thread_key().map(str::to_string),
            snoozed_thread_count: Some(partition.snoozed.len()),
            card_height: None,
            slim_height: None,
            boundary_label_height: Some(SIDEBAR_DRAG_LABEL_HEIGHT),
        });
        let validity = self.drop_validity(&key, active_section, &layout.items, cx);
        let detector = SidebarCollisionDetection::new(
            move |id| validity.get(id).copied().unwrap_or(false),
            Some(layout.items.clone()),
            Some(origin.y),
        );
        let pointer = origin;
        if let Some(drag) = &mut self.drag {
            drag.active = Some(DragActive {
                active_section,
                transforms: vec![STATIONARY; layout.items.len()],
                items: layout.items,
                rects,
                active_index,
                over_index: Some(active_index),
                target_section: Some(active_section),
                pointer,
                detector,
                strategy,
                label_offset,
            });
        }
        self.hovered_row = None;
    }

    /// Precompute `planSidebarThreadDrop(...).kind !== "none"` per target.
    fn drop_validity(
        &mut self,
        key: &str,
        active_section: SidebarSection,
        items: &[SidebarListItem],
        cx: &mut Context<Self>,
    ) -> HashMap<String, bool> {
        let (partition, _) = self.compute_sections(cx);
        let snapshot = self.snapshot(cx);
        let Some(source) = snapshot.threads.iter().find(|thread| thread.key() == key) else {
            return HashMap::new();
        };
        let pinned_order: Vec<String> = partition
            .pinned
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let active_order: Vec<String> = partition
            .active
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let pinned_keys: HashMap<String, Option<String>> = snapshot
            .threads
            .iter()
            .map(|t| (t.key(), t.pin_order_key.clone()))
            .collect();
        let active_keys: HashMap<String, Option<String>> = snapshot
            .threads
            .iter()
            .map(|t| (t.key(), t.active_order_key.clone()))
            .collect();
        let supports_settlement = snapshot
            .capabilities(&source.environment_id)
            .map(|c| c.thread_settlement);
        items
            .iter()
            .map(|item| {
                let id = sidebar_list_item_id(item);
                let valid = resolve_sidebar_drop_target(items, key, &id).is_some_and(|target| {
                    plan_sidebar_thread_drop(&SidebarDropInput {
                        active_key: key,
                        active_section,
                        active_pinned: Some(source.pinned_at.is_some()),
                        active_settled: Some(
                            source.settled_override
                                == Some(crate::state::models::SettledOverride::Settled),
                        ),
                        supports_settlement,
                        target: &target,
                        pinned_order: &pinned_order,
                        pinned_keys_by_id: &pinned_keys,
                        reorderable_keys: Some(&partition.draggable),
                        active_order: &active_order,
                        active_keys_by_id: &active_keys,
                        active_reorderable_keys: Some(&partition.active_reorderable),
                        active_time_ordered: snapshot.settings.working_shelf_enabled,
                    }) != SidebarThreadDropPlan::None
                });
                (id, valid)
            })
            .collect()
    }

    fn drag_over(&mut self, pointer: Point, _: &mut Context<Self>) {
        let list_box = self.list_box();
        let Some(drag) = &mut self.drag else {
            return;
        };
        let origin = drag.origin;
        let key = drag.key.clone();
        let Some(active) = &mut drag.active else {
            return;
        };
        active.pointer = pointer;
        let initial = active.rects[active.active_index];
        let container =
            list_box.map(|(top, height, left, width)| Rect::new(top, height, left, width));
        let lifted = restrict_below_sidebar_label(
            Transform::with_y(pointer.y - origin.y),
            container,
            Some(initial),
            active.label_offset,
        );
        let collision = Rect {
            top: initial.top + lifted.y,
            ..initial
        };
        let droppables: Vec<(String, Rect)> = active
            .items
            .iter()
            .zip(&active.rects)
            .map(|(item, rect)| (sidebar_list_item_id(item), *rect))
            .collect();
        let transform_of = |index: usize| active.transforms.get(index).map_or(0., |t| t.y);
        let divider_label = active
            .items
            .iter()
            .position(|item| *item == SidebarListItem::Marker(SidebarListMarker::PinnedDivider))
            .map(|index| {
                let rect = active.rects[index];
                Rect::new(
                    rect.top + transform_of(index) + 4.,
                    16.,
                    rect.left + 8.,
                    rect.width - 16.,
                )
            });
        let next_shelf_header_top = active
            .items
            .iter()
            .position(
                |item| matches!(item, SidebarListItem::Marker(marker) if marker.is_shelf_header()),
            )
            .map(|index| active.rects[index].top + transform_of(index));
        let collisions = active.detector.detect(&CollisionArgs {
            active_id: &key,
            collision_rect: collision,
            droppables: &droppables,
            pointer: Some((pointer.x, pointer.y)),
            divider_label,
            next_shelf_header_top,
        });
        let over_index = collisions.first().and_then(|id| {
            active
                .items
                .iter()
                .position(|item| sidebar_list_item_id(item) == *id)
        });
        active.over_index = over_index;
        active.target_section = over_index
            .and_then(|index| {
                resolve_sidebar_drop_target(
                    &active.items,
                    &key,
                    &sidebar_list_item_id(&active.items[index]),
                )
            })
            .map(|target| target.section.into());
        let layout = SortLayout {
            rects: active.rects.clone(),
            active_index: active.active_index,
            over_index,
        };
        active.transforms = (0..active.items.len())
            .map(|index| active.strategy.transform(&layout, index))
            .collect();
        active.transforms[active.active_index] = lifted;
    }

    /// Release: every row glides from where the drag left it.
    fn release_drag(&mut self, _: &mut Context<Self>) {
        let Some((list_top, ..)) = self.list_box() else {
            return;
        };
        let Some(active) = self.drag.as_ref().and_then(|drag| drag.active.as_ref()) else {
            return;
        };
        let tops = active
            .items
            .iter()
            .zip(&active.rects)
            .zip(&active.transforms)
            .map(|((item, rect), transform)| {
                (
                    sidebar_list_item_id(item),
                    rect.top + transform.y - list_top,
                )
            })
            .collect();
        self.motion.release(tops);
    }

    /// `handleThreadDragEnd`.
    fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        let key = drag.key.clone();
        let Some(active) = &drag.active else {
            return;
        };
        let Some(over_index) = active.over_index else {
            return;
        };
        let Some(target) = resolve_sidebar_drop_target(
            &active.items,
            &key,
            &sidebar_list_item_id(&active.items[over_index]),
        ) else {
            return;
        };
        let active_section = active.active_section;
        let (partition, _) = self.compute_sections(cx);
        let snapshot = self.snapshot(cx);
        let Some(thread) = snapshot
            .threads
            .iter()
            .find(|thread| thread.key() == key)
            .cloned()
        else {
            return;
        };
        let pinned_order: Vec<String> = partition
            .pinned
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let active_order: Vec<String> = partition
            .active
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let pinned_keys: HashMap<String, Option<String>> = snapshot
            .threads
            .iter()
            .map(|t| (t.key(), t.pin_order_key.clone()))
            .collect();
        let active_keys: HashMap<String, Option<String>> = snapshot
            .threads
            .iter()
            .map(|t| (t.key(), t.active_order_key.clone()))
            .collect();
        let plan = plan_sidebar_thread_drop(&SidebarDropInput {
            active_key: &key,
            active_section,
            active_pinned: Some(thread.pinned_at.is_some()),
            active_settled: Some(
                thread.settled_override == Some(crate::state::models::SettledOverride::Settled),
            ),
            supports_settlement: snapshot
                .capabilities(&thread.environment_id)
                .map(|c| c.thread_settlement),
            target: &target,
            pinned_order: &pinned_order,
            pinned_keys_by_id: &pinned_keys,
            reorderable_keys: Some(&partition.draggable),
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: Some(&partition.active_reorderable),
            active_time_ordered: snapshot.settings.working_shelf_enabled,
        });
        if plan == SidebarThreadDropPlan::None {
            return;
        }
        if plan == SidebarThreadDropPlan::Settle && self.settling.contains(&key) {
            return;
        }
        let assignments: Vec<_> = match &plan {
            SidebarThreadDropPlan::Pin {
                order_key,
                extra_assignments,
                ..
            } => order_key
                .iter()
                .map(
                    |order_key| super::runtime::thread_sort::OrderKeyAssignment {
                        id: key.clone(),
                        order_key: order_key.clone(),
                    },
                )
                .chain(extra_assignments.iter().cloned())
                .collect(),
            SidebarThreadDropPlan::ReorderPinned { assignments, .. }
            | SidebarThreadDropPlan::MoveActive { assignments, .. } => assignments.clone(),
            _ => Vec::new(),
        };
        let order = match &plan {
            SidebarThreadDropPlan::Pin { order, .. }
            | SidebarThreadDropPlan::ReorderPinned { order, .. } => Some(order.clone()),
            SidebarThreadDropPlan::MoveActive { order, .. } => order.clone(),
            _ => None,
        };
        let drop = OptimisticDrop {
            key: key.clone(),
            source_section: active_section,
            section: target.section,
            occurred_at: now_iso(),
            clears_snooze: matches!(
                plan,
                SidebarThreadDropPlan::Pin { .. } | SidebarThreadDropPlan::Settle
            ) || matches!(
                plan,
                SidebarThreadDropPlan::MoveActive { unsnooze: true, .. }
            ),
            order,
            keys_at_drop: if target.section == DropSection::Active {
                active_keys
            } else {
                pinned_keys
            },
            assigned_keys: assignments
                .iter()
                .map(|a| (a.id.clone(), a.order_key.clone()))
                .collect(),
        };
        self.optimistic_drop = Some(drop.clone());
        let thread_ref = ThreadRef::of(&thread);
        self.run_drop_plan(plan, thread_ref, key, drop, window, cx);
    }

    /// Dispatch a drop's commands in order; a failure releases the hold.
    fn run_drop_plan(
        &mut self,
        plan: SidebarThreadDropPlan,
        thread: ThreadRef,
        key: String,
        drop: OptimisticDrop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backend = self.backend.clone();
        let threads = self.snapshot(cx).threads.clone();
        let plan_forward = if plan == SidebarThreadDropPlan::Settle {
            self.settling.insert(key.clone());
            self.plan_forward_navigation(&key, None, cx)
        } else {
            None
        };
        cx.spawn_in(window, async move |this, cx| {
            use super::sidebar_logic::CommandOutcome;
            let fail = |this: &gpui_kit::WeakEntity<Sidebar>,
                        title: &'static str,
                        outcome: CommandOutcome,
                        cx: &mut gpui_kit::AsyncWindowContext| {
                this.update_in(cx, |this, window, cx| {
                    // A late failure must not cancel a newer drag's preview.
                    if this.optimistic_drop.as_ref() == Some(&drop) {
                        this.optimistic_drop = None;
                    }
                    if let CommandOutcome::Failure(error) = outcome {
                        this.toast_error(title, &error, window, cx);
                    }
                    cx.notify();
                })
                .ok();
            };
            macro_rules! run {
                ($task:expr, $title:expr) => {{
                    let task = this.update(cx, |_, cx| $task(cx)).ok();
                    match task {
                        Some(task) => match task.await {
                            CommandOutcome::Success => true,
                            outcome => {
                                fail(&this, $title, outcome, cx);
                                false
                            }
                        },
                        None => false,
                    }
                }};
            }
            let thread_for = |id: &str| {
                threads
                    .iter()
                    .find(|candidate| candidate.key() == id)
                    .map(ThreadRef::of)
            };
            match &plan {
                SidebarThreadDropPlan::Settle => {
                    let ok = run!(
                        |cx: &mut gpui_kit::Context<Sidebar>| backend.settle(thread.clone(), cx),
                        "Failed to settle thread"
                    );
                    this.update(cx, |this, cx| {
                        this.settling.remove(&key);
                        if ok
                            && super::sidebar_logic::should_navigate_after_thread_park(
                                &key,
                                this.route_thread_key(),
                                super::sidebar_logic::ParkAction::Settle,
                                &now_iso(),
                                this.thread(&key, cx).as_ref(),
                            )
                            && let Some(plan) = plan_forward.clone()
                        {
                            match plan {
                                super::sidebar_actions::ForwardNavigation::Thread(next) => {
                                    this.navigate_to_thread(&next, cx)
                                }
                                super::sidebar_actions::ForwardNavigation::NewThread(
                                    project_key,
                                ) => cx.emit(super::SidebarEvent::NewThread {
                                    project_key: Some(project_key),
                                    branch: None,
                                    worktree_path: None,
                                }),
                                super::sidebar_actions::ForwardNavigation::Home => {
                                    this.navigate_home(cx)
                                }
                            }
                        }
                    })
                    .ok();
                    return;
                }
                SidebarThreadDropPlan::MoveActive {
                    unpin,
                    unsettle,
                    unsnooze,
                    ..
                } => {
                    // The drag expresses unpin intent; no confirmation.
                    if *unpin
                        && !run!(
                            |cx: &mut gpui_kit::Context<Sidebar>| backend.unpin(thread.clone(), cx),
                            "Failed to unpin thread"
                        )
                    {
                        return;
                    }
                    if *unsettle
                        && !run!(
                            |cx: &mut gpui_kit::Context<Sidebar>| backend
                                .unsettle(thread.clone(), cx),
                            "Failed to un-settle thread"
                        )
                    {
                        return;
                    }
                    if *unsnooze
                        && !run!(
                            |cx: &mut gpui_kit::Context<Sidebar>| backend
                                .unsnooze(thread.clone(), cx),
                            "Failed to wake thread"
                        )
                    {
                        return;
                    }
                }
                SidebarThreadDropPlan::Pin { order_key, .. } => {
                    let order_key = order_key.clone();
                    if !run!(
                        |cx: &mut gpui_kit::Context<Sidebar>| backend.pin(
                            thread.clone(),
                            order_key.clone(),
                            cx
                        ),
                        "Failed to pin thread"
                    ) {
                        return;
                    }
                }
                _ => {}
            }
            // Stop on failure; each successful key write remains valid.
            let (writes, active) = match &plan {
                SidebarThreadDropPlan::Pin {
                    extra_assignments, ..
                } => (extra_assignments.clone(), false),
                SidebarThreadDropPlan::ReorderPinned { assignments, .. } => {
                    (assignments.clone(), false)
                }
                SidebarThreadDropPlan::MoveActive { assignments, .. } => {
                    (assignments.clone(), true)
                }
                _ => (Vec::new(), false),
            };
            for assignment in writes {
                let Some(target) = thread_for(&assignment.id) else {
                    continue;
                };
                let order_key = assignment.order_key.clone();
                let ok = if active {
                    run!(
                        |cx: &mut gpui_kit::Context<Sidebar>| backend.reorder_active(
                            target.clone(),
                            order_key.clone(),
                            cx
                        ),
                        "Failed to reorder active threads"
                    )
                } else {
                    run!(
                        |cx: &mut gpui_kit::Context<Sidebar>| backend.reorder_pinned(
                            target.clone(),
                            order_key.clone(),
                            cx
                        ),
                        "Failed to reorder pinned threads"
                    )
                };
                if !ok {
                    return;
                }
            }
        })
        .detach();
    }

    /// Window-level pointer and Escape listeners while a gesture is live
    /// (upstream: document listeners owned by the sensor).
    pub(crate) fn render_gesture_listeners(
        &self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.drag.is_none() && self.sweep.is_none() {
            return None;
        }
        let weak = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_: Bounds<Pixels>, _, window, _| {
                    let on_move = weak.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        let primary = event.pressed_button == Some(MouseButton::Left);
                        on_move
                            .update(cx, |this, cx| {
                                this.pointer_moved(point(event.position), primary, window, cx)
                            })
                            .ok();
                    });
                    let on_up = weak.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        on_up
                            .update(cx, |this, cx| {
                                this.pointer_released(point(event.position), window, cx)
                            })
                            .ok();
                    });
                    let on_key = weak.clone();
                    window.on_key_event(move |event: &KeyDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture || event.keystroke.key != "escape" {
                            return;
                        }
                        on_key
                            .update(cx, |this, cx| this.cancel_gesture(window, cx))
                            .ok();
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }

    /// The vertical offset of a list item this frame (drag projection).
    pub(crate) fn drag_offset(&self, index: usize) -> Option<(f32, bool)> {
        let active = self.drag.as_ref()?.active.as_ref()?;
        let transform = active.transforms.get(index).copied().unwrap_or(STATIONARY);
        Some((transform.y, transform.is_hidden()))
    }

    pub(crate) fn drag_target_section(&self) -> Option<Option<SidebarSection>> {
        self.drag
            .as_ref()
            .and_then(|drag| drag.active.as_ref())
            .map(|active| active.target_section)
    }

    pub(crate) fn drag_source(&self) -> Option<(String, SidebarSection)> {
        let drag = self.drag.as_ref()?;
        drag.active
            .as_ref()
            .map(|active| (drag.key.clone(), active.active_section))
    }
}
