//! Port of upstream/apps/web/src/components/Sidebar.drag.ts
//!
//! dnd-kit's collision and sorting-strategy callbacks, as pure functions
//! over measured rects. The sidebar view measures its list items (GPUI
//! prepaint bounds), feeds them here on every pointer move, and applies the
//! resulting transforms as relative offsets. `closestCenter` and
//! `verticalListSortingStrategy` from dnd-kit are ported alongside since
//! the upstream code (and its tests) lean on them.
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use std::collections::HashMap;

use super::sidebar_logic::{
    SidebarListItem, SidebarListMarker, SidebarSection, resolve_sidebar_drop_target,
    sidebar_list_item_id, sidebar_marker_id,
};

/// A client rect (`DOMRect` / dnd-kit `ClientRect`), in window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub top: f32,
    pub height: f32,
    pub left: f32,
    pub width: f32,
}

impl Rect {
    pub fn new(top: f32, height: f32, left: f32, width: f32) -> Self {
        Self {
            top,
            height,
            left,
            width,
        }
    }

    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }

    pub fn right(&self) -> f32 {
        self.left + self.width
    }

    fn center(&self) -> (f32, f32) {
        (self.left + self.width / 2., self.top + self.height / 2.)
    }
}

/// dnd-kit `Transform`. `scale_y == 0` is the "hide this node" sentinel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
}

pub const STATIONARY: Transform = Transform {
    x: 0.,
    y: 0.,
    scale_x: 1.,
    scale_y: 1.,
};

const HIDDEN: Transform = Transform {
    scale_y: 0.,
    ..STATIONARY
};

impl Transform {
    pub fn with_y(y: f32) -> Self {
        Self { y, ..STATIONARY }
    }

    pub fn is_hidden(&self) -> bool {
        self.scale_y == 0.
    }
}

fn is_shelf_header(item: Option<&SidebarListItem>) -> bool {
    matches!(item, Some(SidebarListItem::Marker(marker)) if marker.is_shelf_header())
}

/// `restrictBelowSidebarLabel`: keep the lifted card below the Pins label,
/// including when Pins is empty. The container rect follows scrolling; the
/// offset is measured once at pickup.
pub fn restrict_below_sidebar_label(
    transform: Transform,
    container: Option<Rect>,
    dragging: Option<Rect>,
    offset: f32,
) -> Transform {
    let (Some(container), Some(dragging)) = (container, dragging) else {
        return transform;
    };
    let minimum_y = container.top + offset - dragging.top;
    if transform.y < minimum_y {
        Transform {
            y: minimum_y,
            ..transform
        }
    } else {
        transform
    }
}

/// dnd-kit `closestCenter`: droppables sorted by center distance to the
/// collision rect (stable for ties).
pub fn closest_center(collision_rect: Rect, droppables: &[(String, Rect)]) -> Vec<String> {
    let (cx, cy) = collision_rect.center();
    let mut scored: Vec<(f32, &String)> = droppables
        .iter()
        .map(|(id, rect)| {
            let (x, y) = rect.center();
            (((x - cx).powi(2) + (y - cy).powi(2)).sqrt(), id)
        })
        .collect();
    scored.sort_by(|(left, _), (right, _)| {
        left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.into_iter().map(|(_, id)| id.clone()).collect()
}

/// One collision pass's inputs (dnd-kit `CollisionDetection` args).
#[derive(Clone, Debug)]
pub struct CollisionArgs<'a> {
    pub active_id: &'a str,
    pub collision_rect: Rect,
    /// Enabled droppable containers and their measured rects, in list order.
    pub droppables: &'a [(String, Rect)],
    pub pointer: Option<(f32, f32)>,
    /// The visible `.sidebar-drag-boundary-label` inside the pinned divider.
    pub divider_label: Option<Rect>,
    /// Top of the first shelf header node (working, snoozed, settled).
    pub next_shelf_header_top: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundarySection {
    Pinned,
    Active,
}

/// `createSidebarCollisionDetection`: reject the nearest unsupported target
/// without selecting another section. Recreate when eligibility changes.
pub struct SidebarCollisionDetection {
    is_valid_target: Box<dyn Fn(&str) -> bool>,
    items: Option<Vec<SidebarListItem>>,
    validity: HashMap<String, bool>,
    sections: HashMap<String, Option<SidebarSection>>,
    previous_pointer_y: Option<f32>,
    boundary_section: Option<BoundarySection>,
}

impl SidebarCollisionDetection {
    pub fn new(
        is_valid_target: impl Fn(&str) -> bool + 'static,
        items: Option<Vec<SidebarListItem>>,
        activation_y: Option<f32>,
    ) -> Self {
        Self {
            is_valid_target: Box::new(is_valid_target),
            items,
            validity: HashMap::new(),
            sections: HashMap::new(),
            previous_pointer_y: activation_y,
            boundary_section: None,
        }
    }

    /// Ordered collisions; the first is the sortable `over`.
    pub fn detect(&mut self, args: &CollisionArgs<'_>) -> Vec<String> {
        let mut collisions = closest_center(args.collision_rect, args.droppables);
        let source_section = self.items.as_ref().and_then(|items| {
            items.iter().find_map(|item| match item {
                SidebarListItem::Thread { key, section } if key == args.active_id => Some(*section),
                _ => None,
            })
        });
        if let (Some(items), Some(boundary), Some(source), Some((px, py))) = (
            self.items.as_ref(),
            args.divider_label,
            source_section,
            args.pointer,
        ) {
            let boundary_section =
                *self
                    .boundary_section
                    .get_or_insert(if source == SidebarSection::Pinned {
                        BoundarySection::Pinned
                    } else {
                        BoundarySection::Active
                    });
            // Only pointer movement can change sections: opening the
            // destination moves this row, but must not toggle a stationary
            // gesture back.
            let previous_y = self.previous_pointer_y.unwrap_or(py);
            self.previous_pointer_y = Some(py);
            if px >= boundary.left && px <= boundary.right() {
                let mut section = boundary_section;
                if py < previous_y && py <= boundary.bottom() {
                    section = BoundarySection::Pinned;
                } else if py > previous_y && py >= boundary.top {
                    section = BoundarySection::Active;
                }
                self.boundary_section = Some(section);
                let in_band = section == BoundarySection::Pinned
                    || args.next_shelf_header_top.is_some_and(|bottom| py < bottom);
                if in_band {
                    let wanted = match section {
                        BoundarySection::Pinned => SidebarSection::Pinned,
                        BoundarySection::Active => SidebarSection::Active,
                    };
                    let target = collisions.iter().position(|id| {
                        let resolved = self.sections.entry(id.clone()).or_insert_with(|| {
                            resolve_sidebar_drop_target(items, args.active_id, id)
                                .map(|target| target.section.into())
                        });
                        *resolved == Some(wanted)
                    });
                    if let Some(index) = target {
                        let id = collisions.remove(index);
                        collisions.insert(0, id);
                    }
                }
            }
        }
        let Some(nearest) = collisions.first().cloned() else {
            return collisions;
        };
        if nearest == args.active_id {
            return collisions;
        }
        let valid = match self.validity.get(&nearest) {
            Some(valid) => *valid,
            None => {
                let valid = (self.is_valid_target)(&nearest);
                self.validity.insert(nearest.clone(), valid);
                valid
            }
        };
        if valid {
            collisions
        } else {
            collisions
                .into_iter()
                .filter(|id| id == args.active_id)
                .collect()
        }
    }
}

/// dnd-kit's sorting-strategy arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct SortLayout {
    pub rects: Vec<Rect>,
    pub active_index: usize,
    pub over_index: Option<usize>,
}

/// dnd-kit `verticalListSortingStrategy`, the shape the projection reproduces
/// for in-section moves.
pub fn vertical_list_sorting_strategy(layout: &SortLayout, index: usize) -> Option<Transform> {
    let active = *layout.rects.get(layout.active_index)?;
    let over_index = layout.over_index?;
    let active_index = layout.active_index;
    if index == active_index {
        let over = layout.rects.get(over_index)?;
        let y = if active_index < over_index {
            over.top + over.height - (active.top + active.height)
        } else {
            over.top - active.top
        };
        return Some(Transform::with_y(y));
    }
    let gap = item_gap(&layout.rects, index, active_index);
    if index > active_index && index <= over_index {
        return Some(Transform::with_y(-active.height - gap));
    }
    if index < active_index && index >= over_index {
        return Some(Transform::with_y(active.height + gap));
    }
    Some(STATIONARY)
}

fn item_gap(rects: &[Rect], index: usize, active_index: usize) -> f32 {
    let Some(current) = rects.get(index) else {
        return 0.;
    };
    let previous = index.checked_sub(1).and_then(|i| rects.get(i));
    let next = rects.get(index + 1);
    let gap_after = |a: &Rect, b: &Rect| b.top - (a.top + a.height);
    if active_index < index {
        previous
            .map(|previous| gap_after(previous, current))
            .or(next.map(|next| gap_after(current, next)))
            .unwrap_or(0.)
    } else {
        next.map(|next| gap_after(current, next))
            .or(previous.map(|previous| gap_after(previous, current)))
            .unwrap_or(0.)
    }
}

/// `createSidebarSortingStrategy` input.
#[derive(Clone, Debug, Default)]
pub struct SortingStrategyInput {
    pub items: Vec<SidebarListItem>,
    /// Suspend the reorder preview while the thread is dragged out as context.
    pub enabled: Option<bool>,
    pub settled_order: Vec<String>,
    /// Time-ordered inbox (Working beta): where the lifted row would land.
    pub active_order: Option<Vec<String>>,
    pub settled_expanded: bool,
    pub settled_visible_count: Option<usize>,
    pub route_thread_key: Option<String>,
    pub snoozed_thread_count: Option<usize>,
    pub card_height: Option<f32>,
    pub slim_height: Option<f32>,
    /// Space each pinned boundary opens for its label while dragging.
    pub boundary_label_height: Option<f32>,
}

/// `createSidebarSortingStrategy`: preview the committed section layout
/// without moving or mounting nodes. A zero `scale_y` marks rows/markers to
/// hide while keeping their measured nodes.
pub struct SidebarSortingStrategy {
    input: SortingStrategyInput,
    indices: HashMap<String, usize>,
    previous: Option<SortLayout>,
    transforms: Vec<Transform>,
}

impl SidebarSortingStrategy {
    pub fn new(input: SortingStrategyInput) -> Self {
        let indices = input
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (sidebar_list_item_id(item), index))
            .collect();
        Self {
            input,
            indices,
            previous: None,
            transforms: Vec::new(),
        }
    }

    /// The transform for the item at `index`.
    pub fn transform(&mut self, layout: &SortLayout, index: usize) -> Transform {
        if self.input.enabled == Some(false) {
            return STATIONARY;
        }
        if self.previous.as_ref() != Some(layout) {
            self.previous = Some(layout.clone());
            self.transforms = self.project(layout);
        }
        self.transforms.get(index).copied().unwrap_or(STATIONARY)
    }

    fn project(&self, layout: &SortLayout) -> Vec<Transform> {
        let items = &self.input.items;
        let rects = &layout.rects;
        let Some(active) = items.get(layout.active_index) else {
            return Vec::new();
        };
        let over = layout
            .over_index
            .and_then(|index| items.get(index))
            .unwrap_or(active);
        let SidebarListItem::Thread {
            key: active_key,
            section: active_section,
        } = active
        else {
            return Vec::new();
        };
        let Some(first_rect) = rects.first() else {
            return Vec::new();
        };
        let Some(target) =
            resolve_sidebar_drop_target(items, active_key, &sidebar_list_item_id(over))
        else {
            return Vec::new();
        };
        let target_section: SidebarSection = target.section.into();
        let mut groups: HashMap<SidebarSection, Vec<SidebarListItem>> = HashMap::new();
        let mut card_height = self.input.card_height;
        let mut slim_height = self.input.slim_height;
        let mut header_scale: Option<f32> = None;
        for (index, item) in items.iter().enumerate() {
            match item {
                SidebarListItem::Marker(marker) => {
                    if marker.is_shelf_header()
                        && let Some(rect) = rects.get(index)
                        && rect.height != 0.
                        && header_scale.is_none()
                    {
                        header_scale = Some(rect.height / 32.);
                    }
                }
                SidebarListItem::Thread { key, section } => {
                    let height = rects.get(index).map(|rect| rect.height);
                    if matches!(
                        section,
                        SidebarSection::Pinned | SidebarSection::Active | SidebarSection::Working
                    ) {
                        card_height = card_height.or(height);
                    } else {
                        slim_height = slim_height.or(height);
                    }
                    if key != active_key {
                        groups.entry(*section).or_default().push(item.clone());
                    }
                }
            }
        }
        // Cards are 4.875rem + 0.25rem padding; slim rows/placeholders are h-9.
        let scale = match slim_height {
            Some(slim) => slim / 36.,
            None => header_scale.unwrap_or(card_height.unwrap_or(82.) / 82.),
        };
        let card_height = card_height.unwrap_or(82. * scale);
        let slim_height = slim_height.unwrap_or(36. * scale);
        let label_height = self.input.boundary_label_height.unwrap_or(0.) * scale;
        let order: &[String] = match target.section {
            super::sidebar_logic::DropSection::Pinned => &target.pinned_order,
            super::sidebar_logic::DropSection::Settled => &self.input.settled_order,
            super::sidebar_logic::DropSection::Active => self
                .input
                .active_order
                .as_deref()
                .unwrap_or(&target.active_order),
        };
        let ranks: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(index, key)| (key.as_str(), index))
            .collect();
        let rank_of = |key: &str| ranks.get(key).copied().unwrap_or(usize::MAX);
        let rank = rank_of(active_key);
        {
            let group = groups.entry(target_section).or_default();
            let position = group.iter().position(|item| match item {
                SidebarListItem::Thread { key, .. } => {
                    let item_rank = rank_of(key);
                    // `Infinity > Infinity` is false.
                    item_rank > rank && !(item_rank == usize::MAX && rank == usize::MAX)
                }
                SidebarListItem::Marker(_) => false,
            });
            group.insert(
                position.unwrap_or(group.len()),
                SidebarListItem::thread(active_key.clone(), target_section),
            );
        }
        let settled_source: Vec<String> = if self.input.settled_order.is_empty() {
            groups
                .get(&SidebarSection::Settled)
                .map(|group| group.iter().map(sidebar_list_item_id).collect())
                .unwrap_or_default()
        } else {
            self.input.settled_order.clone()
        };
        let settled_order: Vec<String> = settled_source
            .into_iter()
            .filter(|key| key != active_key || target_section == SidebarSection::Settled)
            .collect();
        let mut visible: Vec<String> = if self.input.settled_expanded {
            let count = self
                .input
                .settled_visible_count
                .unwrap_or(settled_order.len())
                .min(settled_order.len());
            settled_order[..count].to_vec()
        } else {
            Vec::new()
        };
        if let Some(route) = &self.input.route_thread_key
            && settled_order.contains(route)
            && !visible.contains(route)
        {
            visible.push(route.clone());
        }
        groups.insert(
            SidebarSection::Settled,
            visible
                .into_iter()
                .map(|key| SidebarListItem::thread(key, SidebarSection::Settled))
                .collect(),
        );
        let has_marker =
            |marker: SidebarListMarker| items.contains(&SidebarListItem::Marker(marker));
        let group = |section: SidebarSection| groups.get(&section).cloned().unwrap_or_default();
        let mut projected: Vec<SidebarListItem> = Vec::new();
        let section_or_placeholder =
            |projected: &mut Vec<SidebarListItem>,
             section: SidebarSection,
             placeholder: SidebarListMarker| {
                let rows = group(section);
                if rows.is_empty() {
                    projected.push(SidebarListItem::Marker(placeholder));
                } else {
                    projected.extend(rows);
                }
            };
        projected.push(SidebarListItem::Marker(SidebarListMarker::PinnedHeader));
        projected.extend(group(SidebarSection::Pinned));
        projected.push(SidebarListItem::Marker(SidebarListMarker::PinnedDivider));
        section_or_placeholder(
            &mut projected,
            SidebarSection::Active,
            SidebarListMarker::ActivePlaceholder,
        );
        if has_marker(SidebarListMarker::WorkingHeader) {
            projected.push(SidebarListItem::Marker(SidebarListMarker::WorkingHeader));
            projected.extend(group(SidebarSection::Working));
        }
        let snoozed = group(SidebarSection::Snoozed);
        if !snoozed.is_empty()
            || ((*active_section != SidebarSection::Snoozed
                || self.input.snoozed_thread_count.unwrap_or(0) > 1)
                && has_marker(SidebarListMarker::SnoozedHeader))
        {
            projected.push(SidebarListItem::Marker(SidebarListMarker::SnoozedHeader));
            projected.extend(snoozed);
        }
        projected.push(SidebarListItem::Marker(SidebarListMarker::SettledHeader));
        section_or_placeholder(
            &mut projected,
            SidebarSection::Settled,
            SidebarListMarker::SettledPlaceholder,
        );
        let heights: Vec<f32> = projected
            .iter()
            .map(|item| {
                let rect = self
                    .indices
                    .get(&sidebar_list_item_id(item))
                    .and_then(|index| rects.get(*index));
                match item {
                    SidebarListItem::Marker(
                        SidebarListMarker::PinnedHeader | SidebarListMarker::PinnedDivider,
                    ) => label_height,
                    SidebarListItem::Marker(marker) if marker.is_placeholder() => slim_height,
                    SidebarListItem::Marker(_) => rect.map_or(slim_height, |rect| rect.height),
                    SidebarListItem::Thread { key, section } => {
                        let fallback = if matches!(
                            section,
                            SidebarSection::Pinned
                                | SidebarSection::Active
                                | SidebarSection::Working
                        ) {
                            card_height
                        } else {
                            slim_height
                        };
                        if key == active_key {
                            fallback
                        } else {
                            rect.map_or(fallback, |rect| rect.height)
                        }
                    }
                }
            })
            .collect();
        let first_shelf = items.iter().position(|item| is_shelf_header(Some(item)));
        let shelf_rect = first_shelf.and_then(|index| rects.get(index));
        let before_shelf = first_shelf
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| rects.get(index));
        let last_rect = rects.last();
        // Consume the shelf's auto margin as drag labels and resized rows
        // need room, keeping the combined shelves at their measured bottom.
        let mut shelf_space = match (shelf_rect, before_shelf, last_rect) {
            (Some(shelf), Some(before), Some(last)) if shelf.top > before.bottom() + 1. => {
                let used = heights.iter().map(|height| height + 1.).sum::<f32>() - 1.;
                (last.bottom() - first_rect.top - used).max(0.)
            }
            _ => 0.,
        };
        let mut result = vec![HIDDEN; items.len()];
        let mut top = first_rect.top;
        for (projected_index, item) in projected.iter().enumerate() {
            if is_shelf_header(Some(item)) {
                top += shelf_space;
                shelf_space = 0.;
            }
            if let Some(index) = self.indices.get(&sidebar_list_item_id(item))
                && let Some(rect) = rects.get(*index)
            {
                result[*index] = Transform::with_y(top - rect.top);
            }
            top += heights[projected_index] + 1.;
        }
        if let Some(active) = result.get_mut(layout.active_index) {
            *active = STATIONARY;
        }
        result
    }
}

/// `sidebarMarkerId` re-export for callers building collision args.
pub fn marker_id(marker: SidebarListMarker) -> String {
    sidebar_marker_id(marker)
}

#[cfg(test)]
mod tests {
    use super::super::sidebar_logic::SidebarListMarker::*;
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn thread(key: &str, section: SidebarSection) -> SidebarListItem {
        SidebarListItem::thread(key, section)
    }

    fn marker(marker: SidebarListMarker) -> SidebarListItem {
        SidebarListItem::Marker(marker)
    }

    use SidebarSection::{Active as A, Pinned as P, Settled as S, Snoozed as Z, Working as W};

    fn layout(
        items: &[SidebarListItem],
        active: &str,
        over: &str,
        scale: f32,
        card: f32,
    ) -> SortLayout {
        let mut top = 100.;
        let rects = items
            .iter()
            .map(|item| {
                let height = match item {
                    SidebarListItem::Thread { section, .. } => {
                        (if matches!(section, P | A) { card } else { 36. }) * scale
                    }
                    SidebarListItem::Marker(PinnedHeader | PinnedDivider) => 0.,
                    SidebarListItem::Marker(marker) if marker.is_placeholder() => 0.,
                    SidebarListItem::Marker(_) => 32. * scale,
                };
                let rect = Rect::new(top, height, 0., 260.);
                top += height + 1.;
                rect
            })
            .collect();
        SortLayout {
            rects,
            active_index: items
                .iter()
                .position(|item| sidebar_list_item_id(item) == active)
                .unwrap(),
            over_index: items
                .iter()
                .position(|item| sidebar_list_item_id(item) == over),
        }
    }

    fn preview(
        input: SortingStrategyInput,
        active: &str,
        over: &str,
        scale: f32,
    ) -> HashMap<String, Transform> {
        let items = input.items.clone();
        let mut strategy = SidebarSortingStrategy::new(input);
        let args = layout(&items, active, over, scale, 82.);
        items
            .iter()
            .enumerate()
            .map(|(index, item)| (sidebar_list_item_id(item), strategy.transform(&args, index)))
            .collect()
    }

    fn input(
        items: Vec<SidebarListItem>,
        settled_order: &[&str],
        expanded: bool,
    ) -> SortingStrategyInput {
        SortingStrategyInput {
            items,
            settled_order: settled_order.iter().map(|s| s.to_string()).collect(),
            settled_expanded: expanded,
            ..Default::default()
        }
    }

    fn m(marker: SidebarListMarker) -> String {
        sidebar_marker_id(marker)
    }

    fn y(result: &HashMap<String, Transform>, id: &str) -> f32 {
        result[id].y
    }

    // describe("sidebar collision detection")
    fn collision_items(blocked_above_source: bool) -> Vec<SidebarListItem> {
        let mut rows = vec![thread("source", A), thread("blocked", A)];
        if blocked_above_source {
            rows.reverse();
        }
        let mut items = vec![marker(PinnedHeader), marker(PinnedDivider)];
        items.extend(rows);
        items.extend([marker(SettledHeader), marker(SettledPlaceholder)]);
        items
    }

    fn droppables(items: &[SidebarListItem], rects: &[Rect]) -> Vec<(String, Rect)> {
        items
            .iter()
            .zip(rects)
            .map(|(item, rect)| (sidebar_list_item_id(item), *rect))
            .collect()
    }

    #[test]
    fn rejects_unsupported_active_instead_of_selecting_a_neighbor() {
        for (blocked_above, nearby) in [(false, m(SettledHeader)), (true, m(PinnedDivider))] {
            let items = collision_items(blocked_above);
            let l = layout(&items, "source", "blocked", 1., 82.);
            let collision = l.rects[l.over_index.unwrap()];
            let all = droppables(&items, &l.rects);
            let filtered: Vec<(String, Rect)> = all
                .iter()
                .filter(|(id, _)| id != "blocked")
                .cloned()
                .collect();
            assert_eq!(closest_center(collision, &filtered)[0], nearby);
            let mut detector = SidebarCollisionDetection::new(|id| id != "blocked", None, None);
            let args = CollisionArgs {
                active_id: "source",
                collision_rect: collision,
                droppables: &all,
                pointer: None,
                divider_label: None,
                next_shelf_header_top: None,
            };
            assert_eq!(detector.detect(&args), vec!["source".to_string()]);
        }
    }

    #[test]
    fn selects_the_nearest_supported_target() {
        let items = collision_items(false);
        let l = layout(&items, "source", "blocked", 1., 82.);
        let all = droppables(&items, &l.rects);
        let mut detector = SidebarCollisionDetection::new(|_| true, None, None);
        let args = CollisionArgs {
            active_id: "source",
            collision_rect: l.rects[l.over_index.unwrap()],
            droppables: &all,
            pointer: None,
            divider_label: None,
            next_shelf_header_top: None,
        };
        assert_eq!(detector.detect(&args)[0], "blocked");
    }

    #[test]
    fn switches_on_crossing_the_divider_row() {
        for (source_section, pins) in [(A, 0), (A, 1), (P, 1), (S, 1)] {
            let mut items = vec![marker(PinnedHeader)];
            if pins > 0 {
                items.push(thread("p", P));
            }
            if source_section == P {
                items.push(thread("source", P));
            }
            items.push(marker(PinnedDivider));
            items.push(thread("a", A));
            if source_section == A {
                items.push(thread("source", A));
            }
            items.push(marker(SettledHeader));
            if source_section == S {
                items.push(thread("source", S));
            }
            let l = layout(&items, "source", "a", 1., 82.);
            let source_rect = l.rects[l.active_index];
            let all = droppables(&items, &l.rects);
            let mut detector = SidebarCollisionDetection::new(
                |_| true,
                Some(items.clone()),
                Some(if source_section == P { 200. } else { 600. }),
            );
            let mut at = |center: f32, boundary_top: f32| {
                let collision = Rect::new(
                    center - source_rect.height / 2.,
                    source_rect.height,
                    0.,
                    260.,
                );
                let args = CollisionArgs {
                    active_id: "source",
                    collision_rect: collision,
                    droppables: &all,
                    pointer: Some((130., center)),
                    divider_label: Some(Rect::new(boundary_top, 16., 0., 260.)),
                    next_shelf_header_top: Some(600.),
                };
                let over = detector.detect(&args).first().cloned();
                over.and_then(|over| resolve_sidebar_drop_target(&items, "source", &over))
                    .map(|target| SidebarSection::from(target.section))
            };
            assert_eq!(at(330., 300.), Some(A));
            assert_eq!(at(317., 300.), Some(A));
            assert_eq!(at(316., 300.), Some(P));
            // The preview moves the divider; a stationary pointer must not undo it.
            assert_eq!(at(316., 400.), Some(P));
            assert_eq!(at(399., 400.), Some(P));
            assert_eq!(at(400., 400.), Some(A));
            assert_eq!(at(400., 300.), Some(A));
            assert_eq!(at(317., 300.), Some(A));
            assert_eq!(at(316., 300.), Some(P));
        }
    }

    #[test]
    fn returns_no_collision_if_an_unsupported_target_has_no_source_fallback() {
        let items = collision_items(false);
        let l = layout(&items, "source", "blocked", 1., 82.);
        let all: Vec<(String, Rect)> = droppables(&items, &l.rects)
            .into_iter()
            .filter(|(id, _)| id != "source")
            .collect();
        let mut detector = SidebarCollisionDetection::new(|_| false, None, None);
        let args = CollisionArgs {
            active_id: "source",
            collision_rect: l.rects[l.over_index.unwrap()],
            droppables: &all,
            pointer: None,
            divider_label: None,
            next_shelf_header_top: None,
        };
        assert!(detector.detect(&args).is_empty());
    }

    #[test]
    fn validates_each_hovered_target_once_and_always_allows_returning_to_the_source() {
        let items = collision_items(false);
        let l = layout(&items, "source", "blocked", 1., 82.);
        let all = droppables(&items, &l.rects);
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let mut detector = SidebarCollisionDetection::new(
            move |id| {
                recorded.borrow_mut().push(id.to_string());
                id != "blocked"
            },
            None,
            None,
        );
        let over_rect = l.rects[l.over_index.unwrap()];
        let mut detect = |rect: Rect| {
            detector.detect(&CollisionArgs {
                active_id: "source",
                collision_rect: rect,
                droppables: &all,
                pointer: None,
                divider_label: None,
                next_shelf_header_top: None,
            })[0]
                .clone()
        };
        assert_eq!(detect(over_rect), "source");
        assert_eq!(
            detect(Rect {
                top: over_rect.top + 3.,
                ..over_rect
            }),
            "source"
        );
        assert_eq!(detect(l.rects[l.active_index]), "source");
        assert_eq!(detect(l.rects[items.len() - 1]), m(SettledPlaceholder));
        assert_eq!(
            *calls.borrow(),
            vec!["blocked".to_string(), m(SettledPlaceholder)]
        );
    }

    // describe("sidebar drag projection")
    #[test]
    fn restores_every_row_and_marker_when_dragging_out() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            marker(ActivePlaceholder),
            thread("a1", A),
            thread("a2", A),
            marker(SnoozedHeader),
            thread("z", Z),
            marker(SettledHeader),
            marker(SettledPlaceholder),
            thread("s", S),
        ];
        for (active, over) in [
            ("a2", "a1".to_string()),
            ("p", "a1".to_string()),
            ("s", m(PinnedHeader)),
            ("z", m(SettledHeader)),
        ] {
            let base = SortingStrategyInput {
                boundary_label_height: Some(24.),
                snoozed_thread_count: Some(1),
                ..input(items.clone(), &["z", "s"], true)
            };
            let reordered = preview(base.clone(), active, &over, 1.);
            assert!(reordered.values().any(|t| t.y != 0.));
            let restored = preview(
                SortingStrategyInput {
                    enabled: Some(false),
                    ..base.clone()
                },
                active,
                &over,
                1.,
            );
            assert!(restored.values().all(|t| *t == STATIONARY));
            assert_eq!(
                preview(
                    SortingStrategyInput {
                        enabled: Some(true),
                        ..base
                    },
                    active,
                    &over,
                    1.
                ),
                reordered
            );
        }
    }

    #[test]
    fn keeps_sparse_shelves_at_the_bottom() {
        let items = vec![
            marker(PinnedHeader),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(SnoozedHeader),
            thread("z", Z),
            marker(SettledHeader),
            thread("s", S),
        ];
        for (active, over) in [
            ("a1", "a2".to_string()),
            ("a1", m(SettledHeader)),
            ("z", "a2".to_string()),
        ] {
            let settled: &[&str] = if over == m(SettledHeader) {
                &["a1", "s"]
            } else {
                &["s"]
            };
            let mut strategy = SidebarSortingStrategy::new(SortingStrategyInput {
                boundary_label_height: Some(24.),
                snoozed_thread_count: Some(1),
                ..input(items.clone(), settled, true)
            });
            let mut args = layout(&items, active, &over, 1., 82.);
            for rect in args.rects.iter_mut().skip(4) {
                rect.top += 400.;
            }
            assert_eq!(strategy.transform(&args, items.len() - 1), STATIONARY);
        }
    }

    fn pinned_items() -> Vec<SidebarListItem> {
        vec![
            marker(PinnedHeader),
            thread("p1", P),
            thread("p2", P),
            marker(PinnedDivider),
            thread("a1", A),
            marker(SettledHeader),
            thread("s1", S),
        ]
    }

    #[test]
    fn preserves_existing_pinned_transforms() {
        for (active, over) in [("p1", "p2"), ("p2", "p1")] {
            let items = pinned_items();
            let mut strategy = SidebarSortingStrategy::new(input(items.clone(), &[], true));
            let args = layout(&items, active, over, 1., 82.);
            for index in 0..items.len() {
                if index == args.active_index {
                    continue;
                }
                assert_eq!(
                    Some(strategy.transform(&args, index)),
                    vertical_list_sorting_strategy(&args, index)
                );
            }
        }
    }

    #[test]
    fn keeps_the_pinned_header_above_the_gap_when_a_lower_pin_moves_to_the_top() {
        let result = preview(input(pinned_items(), &[], true), "p2", &m(PinnedHeader), 1.);
        assert_eq!(result[&m(PinnedHeader)], STATIONARY);
        assert_eq!(result["p1"], Transform::with_y(83.));
        assert_eq!(result[&m(PinnedDivider)], STATIONARY);
        assert_eq!(result["a1"], STATIONARY);
    }

    fn standard() -> Vec<SidebarListItem> {
        vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(SettledHeader),
            thread("s", S),
        ]
    }

    #[test]
    fn uses_pinned_dragging_behavior_for_active() {
        for (active, over) in [("a1", "a2"), ("a2", "a1")] {
            let items = standard();
            let mut strategy = SidebarSortingStrategy::new(input(items.clone(), &[], true));
            let args = layout(&items, active, over, 1., 82.);
            for index in 0..items.len() {
                if index == args.active_index {
                    continue;
                }
                assert_eq!(
                    Some(strategy.transform(&args, index)),
                    vertical_list_sorting_strategy(&args, index)
                );
            }
        }
    }

    #[test]
    fn preserves_settled_order_while_opening_the_zero_height_active_target() {
        let items = vec![
            marker(PinnedHeader),
            marker(PinnedDivider),
            marker(ActivePlaceholder),
            marker(SettledHeader),
            thread("first", S),
            thread("second", S),
        ];
        let result = preview(
            input(items, &["first", "second"], true),
            "second",
            "first",
            1.,
        );
        assert_eq!(result[&m(ActivePlaceholder)], STATIONARY);
        assert_eq!(result[&m(SettledHeader)], Transform::with_y(36.));
        assert_eq!(result["first"], Transform::with_y(36.));
        assert_eq!(result["second"], STATIONARY);
    }

    #[test]
    fn opens_the_active_pointer_slot_without_adding_an_empty_pinned_row() {
        for (over, a1, a2) in [
            (m(PinnedDivider), 0., 0.),
            ("a1".to_string(), -83., 0.),
            ("a2".to_string(), -83., -83.),
        ] {
            let result = preview(input(standard(), &[], true), "p", &over, 1.);
            assert_eq!(result[&m(PinnedHeader)], STATIONARY);
            assert_eq!(y(&result, &m(PinnedDivider)), -83.);
            assert_eq!(y(&result, "a1"), a1);
            assert_eq!(y(&result, "a2"), a2);
            assert_eq!(y(&result, &m(SettledHeader)), 0.);
        }
    }

    #[test]
    fn opens_label_space_below_each_pinned_boundary_while_dragging() {
        let result = preview(
            SortingStrategyInput {
                boundary_label_height: Some(16.),
                ..input(standard(), &[], true)
            },
            "a2",
            "a1",
            1.,
        );
        assert_eq!(result[&m(PinnedHeader)], STATIONARY);
        assert_eq!(y(&result, "p"), 16.);
        assert_eq!(y(&result, &m(PinnedDivider)), 16.);
        assert_eq!(result["a2"], STATIONARY);
        assert_eq!(y(&result, "a1"), 32. + 83.);
        assert_eq!(y(&result, &m(SettledHeader)), 32.);
        assert_eq!(y(&result, "s"), 32.);
    }

    #[test]
    fn keeps_label_clearance_when_a_settled_drag_is_over_any_target() {
        for over in ["s1", "missing-target"] {
            let items = vec![
                marker(PinnedHeader),
                thread("p", P),
                marker(PinnedDivider),
                thread("a", A),
                marker(SettledHeader),
                thread("s1", S),
                thread("s2", S),
            ];
            let result = preview(
                SortingStrategyInput {
                    boundary_label_height: Some(24.),
                    ..input(items, &["s1", "s2"], true)
                },
                "s2",
                over,
                1.,
            );
            assert_eq!(y(&result, "p"), 24.);
            assert_eq!(y(&result, &m(PinnedDivider)), 24.);
            assert_eq!(y(&result, "a"), 48.);
            assert_eq!(y(&result, "s1"), 48.);
            assert_eq!(result["s2"], STATIONARY);
        }
    }

    #[test]
    fn stacks_the_labels_with_their_gaps_when_the_pinned_section_is_empty() {
        let items = vec![
            marker(PinnedHeader),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(SettledHeader),
            thread("s", S),
        ];
        let result = preview(
            SortingStrategyInput {
                boundary_label_height: Some(16.),
                ..input(items, &[], true)
            },
            "a2",
            "a1",
            1.,
        );
        assert_eq!(result[&m(PinnedHeader)], STATIONARY);
        assert_eq!(y(&result, &m(PinnedDivider)), 16.);
        assert_eq!(y(&result, "a1"), 32. + 83.);
        assert_eq!(y(&result, &m(SettledHeader)), 32.);
    }

    #[test]
    fn scales_the_label_space_with_the_measured_root_scale() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a1", A),
        ];
        let result = preview(
            SortingStrategyInput {
                boundary_label_height: Some(16.),
                ..input(items, &[], true)
            },
            "a1",
            "p",
            2.,
        );
        assert_eq!(y(&result, &m(PinnedDivider)), 32. + 165.);
    }

    #[test]
    fn keeps_the_pinned_header_above_the_first_arriving_pin() {
        let items = vec![
            marker(PinnedHeader),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(SettledHeader),
            thread("s", S),
        ];
        let result = preview(input(items, &[], true), "a2", &m(PinnedHeader), 1.);
        assert_eq!(result[&m(PinnedHeader)], STATIONARY);
        assert_eq!(y(&result, &m(PinnedDivider)), 83.);
        assert_eq!(y(&result, "a1"), 83.);
        assert_eq!(y(&result, &m(SettledHeader)), 0.);
    }

    #[test]
    fn replaces_the_empty_active_target_when_a_row_enters() {
        for (active, divider, settled) in [("p", -83., -1.), ("s", 0., 82.)] {
            let items = vec![
                marker(PinnedHeader),
                thread("p", P),
                marker(PinnedDivider),
                marker(ActivePlaceholder),
                marker(SettledHeader),
                thread("s", S),
            ];
            let result = preview(input(items, &[], true), active, &m(ActivePlaceholder), 1.);
            assert_eq!(result[&m(ActivePlaceholder)].scale_y, 0.);
            assert_eq!(y(&result, &m(PinnedDivider)), divider);
            assert_eq!(y(&result, &m(SettledHeader)), settled);
        }
    }

    #[test]
    fn uses_the_canonical_settled_rank_and_the_destinations_slim_height() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SettledHeader),
            thread("s1", S),
            thread("s2", S),
        ];
        let result = preview(input(items, &["s1", "a", "s2"], true), "a", "s2", 1.);
        assert_eq!(y(&result, &m(SettledHeader)), -46.);
        assert_eq!(y(&result, "s1"), -46.);
        assert_eq!(y(&result, "s2"), -9.);
    }

    #[test]
    fn reserves_a_full_card_at_the_pointer_slot_when_a_slim_row_enters_active() {
        for (over, first) in [("a1", 83.), ("a2", 0.)] {
            let result = preview(input(standard(), &[], true), "s", over, 1.);
            assert_eq!(y(&result, "a1"), first);
            assert_eq!(y(&result, "a2"), 83.);
            assert_eq!(y(&result, &m(SettledHeader)), 83.);
        }
    }

    #[test]
    fn removes_the_snoozed_header_when_its_last_row_leaves() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SnoozedHeader),
            thread("z", Z),
            marker(SettledHeader),
            thread("s", S),
        ];
        let result = preview(input(items, &[], true), "z", "a", 1.);
        assert_eq!(result[&m(SnoozedHeader)].scale_y, 0.);
        assert_eq!(y(&result, &m(SettledHeader)), 13.);
        assert_eq!(y(&result, "s"), 13.);
    }

    #[test]
    fn keeps_a_collapsed_settled_target_without_inserting_a_hidden_row() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(SettledHeader),
            marker(SettledPlaceholder),
        ];
        let result = preview(input(items, &[], false), "a2", &m(SettledPlaceholder), 1.);
        assert_eq!(y(&result, &m(SettledHeader)), -83.);
        assert_eq!(result[&m(SettledPlaceholder)], Transform::with_y(-83.));
    }

    #[test]
    fn preserves_a_collapsed_snoozed_header_while_another_section_changes() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SnoozedHeader),
            marker(SettledHeader),
            marker(SettledPlaceholder),
        ];
        let result = preview(input(items, &[], false), "a", &m(SettledPlaceholder), 1.);
        assert_eq!(result[&m(SnoozedHeader)], Transform::with_y(-46.));
    }

    #[test]
    fn previews_a_time_ordered_inbox_drop_at_its_time_slot_above_the_working_shelf() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a1", A),
            thread("a2", A),
            marker(WorkingHeader),
            thread("w", W),
            marker(SettledHeader),
            marker(SettledPlaceholder),
        ];
        let base = input(items, &[], false);
        assert_eq!(y(&preview(base.clone(), "p", "a1", 1.), "a2"), 0.);
        let by_time = preview(
            SortingStrategyInput {
                active_order: Some(vec!["a1".into(), "a2".into(), "p".into()]),
                ..base
            },
            "p",
            "a1",
            1.,
        );
        assert_eq!(y(&by_time, "a2"), -83.);
        assert_eq!(by_time[&m(WorkingHeader)], STATIONARY);
        assert_eq!(by_time["w"], STATIONARY);
    }

    #[test]
    fn derives_missing_card_geometry_from_the_measured_root_scale() {
        let items = vec![
            marker(PinnedHeader),
            marker(PinnedDivider),
            marker(ActivePlaceholder),
            marker(SettledHeader),
            thread("s", S),
        ];
        let result = preview(input(items, &[], true), "s", &m(PinnedHeader), 0.75);
        assert_eq!(result[&m(PinnedHeader)], STATIONARY);
        assert_eq!(y(&result, &m(PinnedDivider)), 62.5);
        assert_eq!(y(&result, &m(ActivePlaceholder)), 62.5);
    }

    #[test]
    fn updates_the_projection_when_the_target_or_measured_geometry_changes() {
        let items = pinned_items();
        let mut strategy = SidebarSortingStrategy::new(input(items.clone(), &[], true));
        let args = layout(&items, "p1", "p1", 1., 82.);
        assert_eq!(strategy.transform(&args, 2).y, 0.);
        let moved = SortLayout {
            over_index: Some(4),
            ..args.clone()
        };
        assert_eq!(strategy.transform(&moved, 2).y, -83.);
        let smaller = layout(&items, "p1", "a1", 0.75, 82.);
        assert_eq!(strategy.transform(&smaller, 2).y, -62.5);
    }

    #[test]
    fn reveals_the_mounted_empty_target_when_its_last_row_leaves_and_hides_it_on_return() {
        for section in [A, S] {
            let items = vec![
                marker(PinnedHeader),
                thread("p", P),
                marker(PinnedDivider),
                marker(ActivePlaceholder),
                thread("a", A),
                marker(SettledHeader),
                marker(SettledPlaceholder),
                thread("s", S),
            ];
            let active = if section == A { "a" } else { "s" };
            let placeholder = if section == A {
                m(ActivePlaceholder)
            } else {
                m(SettledPlaceholder)
            };
            let base = input(items, &["s"], true);
            assert_eq!(
                preview(base.clone(), active, active, 1.)[&placeholder].scale_y,
                0.
            );
            assert_eq!(
                preview(base.clone(), active, &m(PinnedHeader), 1.)[&placeholder].scale_y,
                1.
            );
            assert_eq!(preview(base, active, active, 1.)[&placeholder].scale_y, 0.);
        }
    }

    #[test]
    fn uses_shelf_height_for_empty_target_sizing_when_card_height_differs() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SettledHeader),
            marker(SettledPlaceholder),
        ];
        let mut strategy = SidebarSortingStrategy::new(input(items.clone(), &[], false));
        let args = layout(&items, "a", &m(SettledPlaceholder), 1., 78.);
        assert_eq!(strategy.transform(&args, 4).y, -42.);
    }

    #[test]
    fn keeps_the_route_row_visible_after_a_settled_drop_pushes_it_beyond_the_page() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SettledHeader),
            thread("s", S),
        ];
        let base = SortingStrategyInput {
            settled_visible_count: Some(1),
            ..input(items, &["a", "s", "hidden"], true)
        };
        let with_route = preview(
            SortingStrategyInput {
                route_thread_key: Some("s".into()),
                ..base.clone()
            },
            "a",
            "s",
            1.,
        );
        assert_eq!(with_route["s"], Transform::with_y(-9.));
        assert_eq!(preview(base, "a", "s", 1.)["s"].scale_y, 0.);
    }

    #[test]
    fn reserves_the_next_page_row_when_a_visible_settled_thread_leaves() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SettledHeader),
            thread("s1", S),
            thread("route", S),
        ];
        let result = preview(
            SortingStrategyInput {
                settled_visible_count: Some(1),
                route_thread_key: Some("route".into()),
                ..input(items, &["s1", "hidden", "route"], true)
            },
            "s1",
            "a",
            1.,
        );
        assert_eq!(y(&result, &m(SettledHeader)), 83.);
        assert_eq!(y(&result, "route"), 83.);
    }

    #[test]
    fn keeps_the_dropped_route_thread_visible_in_a_collapsed_settled_shelf() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SettledHeader),
            marker(SettledPlaceholder),
        ];
        let result = preview(
            SortingStrategyInput {
                settled_visible_count: Some(1),
                route_thread_key: Some("a".into()),
                ..input(items, &["a", "hidden"], false)
            },
            "a",
            &m(SettledPlaceholder),
            1.,
        );
        assert_eq!(result[&m(SettledPlaceholder)].scale_y, 0.);
        assert_eq!(y(&result, &m(SettledHeader)), -46.);
    }

    #[test]
    fn preserves_hidden_snoozed_membership_when_the_only_rendered_route_row_leaves() {
        let items = vec![
            marker(PinnedHeader),
            thread("p", P),
            marker(PinnedDivider),
            thread("a", A),
            marker(SnoozedHeader),
            thread("z", Z),
            marker(SettledHeader),
            thread("s", S),
        ];
        let result = preview(
            SortingStrategyInput {
                snoozed_thread_count: Some(2),
                ..input(items, &["s"], true)
            },
            "z",
            "a",
            1.,
        );
        assert_eq!(result[&m(SnoozedHeader)], Transform::with_y(83.));
        assert_eq!(y(&result, &m(SettledHeader)), 46.);
    }

    // describe("lifted card clearance")
    fn apply(card_top: f32, card_height: f32, y: f32, list_top: f32, offset: f32) -> Transform {
        restrict_below_sidebar_label(
            Transform::with_y(y),
            Some(Rect::new(list_top, 500., 0., 260.)),
            Some(Rect::new(card_top, card_height, 0., 260.)),
            offset,
        )
    }

    #[test]
    fn keeps_a_row_below_empty_pins_even_past_the_top_edge() {
        for height in [36., 82.] {
            for pointer_y in [150., 136., 100., 0.] {
                assert_eq!(
                    511. + apply(511., height, pointer_y - 529., 136., 32.).y,
                    168.
                );
            }
        }
    }

    #[test]
    fn preserves_pointer_movement_below_the_label() {
        assert_eq!(apply(511., 36., -200., 136., 32.).y, -200.);
    }

    #[test]
    fn follows_the_list_when_it_scrolls_and_includes_content_preceding_pins() {
        assert_eq!(511. + apply(511., 36., -500., 96., 32.).y, 128.);
        assert_eq!(511. + apply(511., 36., -500., 136., 114.).y, 250.);
    }
}
