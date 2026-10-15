//! Port of the section model inside upstream/apps/web/src/components/Sidebar.tsx
//! (the `useMemo` partition into pinned / active / working / snoozed /
//! settled, the settled paging and collapsed-shelf route exceptions, the
//! sortable list items, and the optimistic-drop hold). Pure, so the view
//! recomputes it from the store each render.

use std::collections::{HashMap, HashSet};

use super::runtime::models::{EnvironmentThreadShell, SettledOverride};
use super::runtime::thread_inbox::InboxReturnTracker;
use super::runtime::thread_settled::effective_snoozed;
use super::runtime::thread_sort::sort_settled_threads;
use super::sidebar_logic::{
    DropSection, SidebarListItem, SidebarListMarker, SidebarSection, apply_sidebar_thread_drop,
    filter_sidebar_v2_visible_threads, first_valid_timestamp_ms, is_sidebar_thread_working,
    order_items_by_preferred_ids, resolve_sidebar_thread_section, sort_inbox_threads_by_return,
    sort_pinned_threads_for_sidebar, sort_threads_for_sidebar, sort_working_threads_by_send,
};
use super::view_model::SidebarSnapshot;

/// Settled-tail paging: recent history is the common lookup; the deep tail
/// stays behind an explicit Show more.
pub const SETTLED_TAIL_INITIAL_COUNT: usize = 10;
pub const SETTLED_TAIL_PAGE_COUNT: usize = 25;

/// A drop held at its destination while the server applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimisticDrop {
    pub key: String,
    pub source_section: SidebarSection,
    pub section: DropSection,
    pub occurred_at: String,
    pub clears_snooze: bool,
    /// Full destination order for pinned and active drops.
    pub order: Option<Vec<String>>,
    /// Destination order keys before the drop, to recognize concurrent writes.
    pub keys_at_drop: HashMap<String, Option<String>>,
    /// The keys this drop writes; the hold lasts until all of them land.
    pub assigned_keys: HashMap<String, String>,
}

/// The five shelves after classification and sorting.
#[derive(Clone, Debug, Default)]
pub struct SidebarPartition {
    pub pinned: Vec<EnvironmentThreadShell>,
    pub active: Vec<EnvironmentThreadShell>,
    pub working: Vec<EnvironmentThreadShell>,
    pub snoozed: Vec<EnvironmentThreadShell>,
    pub settled: Vec<EnvironmentThreadShell>,
    /// Rows whose server supports pin + pin reorder (drag sources).
    pub draggable: HashSet<String>,
    pub active_reorderable: HashSet<String>,
}

impl SidebarPartition {
    pub fn total(&self) -> usize {
        self.pinned.len()
            + self.active.len()
            + self.working.len()
            + self.snoozed.len()
            + self.settled.len()
    }

    pub fn section_of(&self, key: &str) -> Option<SidebarSection> {
        let has = |list: &[EnvironmentThreadShell]| list.iter().any(|thread| thread.key() == key);
        if has(&self.pinned) {
            Some(SidebarSection::Pinned)
        } else if has(&self.active) {
            Some(SidebarSection::Active)
        } else if has(&self.working) {
            Some(SidebarSection::Working)
        } else if has(&self.snoozed) {
            Some(SidebarSection::Snoozed)
        } else if has(&self.settled) {
            Some(SidebarSection::Settled)
        } else {
            None
        }
    }
}

/// The partition `useMemo`. Snooze classification uses a precise clock
/// (`now`), not the quantized minute.
pub fn partition_sidebar_threads(
    snapshot: &SidebarSnapshot,
    scoped_project_keys: Option<&HashSet<String>>,
    optimistic_drop: Option<&OptimisticDrop>,
    inbox_returns: &mut InboxReturnTracker,
    now: &str,
) -> SidebarPartition {
    let working_enabled = snapshot.settings.working_shelf_enabled;
    // Subagent child threads live in the parent's Agents surface.
    let visible = filter_sidebar_v2_visible_threads(&snapshot.threads, scoped_project_keys);
    inbox_returns.observe(working_enabled.then_some(snapshot.threads.as_slice()));
    let mut partition = SidebarPartition::default();
    let mut pinned = Vec::new();
    let mut active = Vec::new();
    let mut working = Vec::new();
    let mut snoozed = Vec::new();
    let mut settled = Vec::new();
    for thread in visible {
        let capabilities = snapshot.capabilities(&thread.environment_id);
        // Threads on servers without settlement never classify as settled:
        // the user could neither un-settle nor pin them.
        let supports_settlement = capabilities.is_some_and(|c| c.thread_settlement);
        let supports_snooze = capabilities.is_some_and(|c| c.thread_snooze);
        let key = thread.key();
        if capabilities.is_some_and(|c| c.thread_active_reorder) {
            partition.active_reorderable.insert(key.clone());
        }
        if capabilities.is_some_and(|c| c.thread_pinning && c.thread_pin_reorder) {
            partition.draggable.insert(key.clone());
        }
        let inbox_is_working =
            |thread: &EnvironmentThreadShell| working_enabled && is_sidebar_thread_working(thread);
        if let Some(drop) = optimistic_drop.filter(|drop| drop.key == key) {
            let projected = apply_sidebar_thread_drop(
                thread,
                drop.section,
                &drop.occurred_at,
                drop.assigned_keys.get(&key).map(String::as_str),
            );
            let projected = if drop.clears_snooze {
                projected
            } else {
                EnvironmentThreadShell {
                    snoozed_at: thread.snoozed_at.clone(),
                    snoozed_until: thread.snoozed_until.clone(),
                    ..projected
                }
            };
            match drop.section {
                DropSection::Pinned => pinned.push(projected),
                DropSection::Settled => settled.push(projected),
                DropSection::Active if inbox_is_working(&projected) => working.push(projected),
                DropSection::Active => active.push(projected),
            }
            continue;
        }
        let section = resolve_sidebar_thread_section(
            supports_snooze && effective_snoozed(thread, now),
            supports_settlement && thread.settled_override == Some(SettledOverride::Settled),
            thread.pinned_at.is_some(),
        );
        match section {
            SidebarSection::Snoozed => snoozed.push(thread.clone()),
            SidebarSection::Settled => settled.push(thread.clone()),
            SidebarSection::Pinned => pinned.push(thread.clone()),
            _ if inbox_is_working(thread) => working.push(thread.clone()),
            _ => active.push(thread.clone()),
        }
    }
    // One shared rule on every platform: user-arranged keys first, keyless
    // threads in creation order below. Capability gates dragging, not sort.
    let sorted_pinned = sort_pinned_threads_for_sidebar(pinned);
    let sorted_active = if working_enabled {
        sort_inbox_threads_by_return(active, |thread| inbox_returns.returned_at(thread))
    } else {
        sort_threads_for_sidebar(active)
    };
    let held = |section: DropSection, sorted: Vec<EnvironmentThreadShell>| match optimistic_drop {
        Some(drop) if drop.section == section => match &drop.order {
            Some(order) => {
                order_items_by_preferred_ids(&sorted, order, |thread| thread.key(), None)
            }
            None => sorted,
        },
        _ => sorted,
    };
    partition.pinned = held(DropSection::Pinned, sorted_pinned);
    partition.active = held(DropSection::Active, sorted_active);
    // Newest send first; finishing and waking again do not move a row.
    partition.working = sort_working_threads_by_send(working);
    // Soonest wake first: "what comes back next" is the shelf's question.
    snoozed.sort_by_key(|thread| first_valid_timestamp_ms(&[thread.snoozed_until.as_deref()]));
    partition.snoozed = snoozed;
    partition.settled = sort_settled_threads(settled);
    partition
}

/// The rendered list: shelves after paging, collapse and route exceptions.
#[derive(Clone, Debug, Default)]
pub struct SidebarListLayout {
    /// What the settled shelf renders (collapsed: only the route thread).
    pub rendered_settled: Vec<EnvironmentThreadShell>,
    pub hidden_settled_count: usize,
    /// Keyboard/jump order: every rendered row.
    pub ordered_keys: Vec<String>,
    pub items: Vec<SidebarListItem>,
}

/// Collapsed shelves keep only the route thread's row, so the open thread
/// never vanishes behind a collapsed shelf.
fn route_only(
    list: &[EnvironmentThreadShell],
    route_key: Option<&str>,
) -> Vec<EnvironmentThreadShell> {
    route_key
        .and_then(|route| list.iter().find(|thread| thread.key() == route))
        .cloned()
        .into_iter()
        .collect()
}

/// `visibleSettledThreads`, `renderedSettledThreads`, `visibleSnoozedThreads`,
/// `visibleWorkingThreads`, `orderedThreads` and `sidebarListItems`.
pub fn layout_sidebar_list(
    partition: &SidebarPartition,
    route_key: Option<&str>,
    settled_visible_count: usize,
    settled_expanded: bool,
    snoozed_expanded: bool,
    working_expanded: bool,
) -> SidebarListLayout {
    let mut visible_settled: Vec<EnvironmentThreadShell> = partition
        .settled
        .iter()
        .take(settled_visible_count)
        .cloned()
        .collect();
    // The open thread must never hide under "Show more".
    if partition.settled.len() > settled_visible_count
        && let Some(route) = route_key
        && let Some(thread) = partition.settled[settled_visible_count..]
            .iter()
            .find(|thread| thread.key() == route)
    {
        visible_settled.push(thread.clone());
    }
    let hidden_settled_count = partition.settled.len() - visible_settled.len();
    let rendered_settled = if settled_expanded {
        visible_settled.clone()
    } else {
        route_only(&visible_settled, route_key)
    };
    let visible_snoozed = if snoozed_expanded {
        partition.snoozed.clone()
    } else {
        route_only(&partition.snoozed, route_key)
    };
    let visible_working = if working_expanded {
        partition.working.clone()
    } else {
        route_only(&partition.working, route_key)
    };
    let ordered_keys: Vec<String> = partition
        .pinned
        .iter()
        .chain(&partition.active)
        .chain(&visible_working)
        .chain(&visible_snoozed)
        .chain(&rendered_settled)
        .map(EnvironmentThreadShell::key)
        .collect();
    let mut items = Vec::new();
    if partition.total() > 0 {
        let rows = |list: &[EnvironmentThreadShell], section| {
            list.iter()
                .map(move |thread| SidebarListItem::thread(thread.key(), section))
                .collect::<Vec<_>>()
        };
        items.push(SidebarListItem::Marker(SidebarListMarker::PinnedHeader));
        items.extend(rows(&partition.pinned, SidebarSection::Pinned));
        items.push(SidebarListItem::Marker(SidebarListMarker::PinnedDivider));
        items.push(SidebarListItem::Marker(
            SidebarListMarker::ActivePlaceholder,
        ));
        items.extend(rows(&partition.active, SidebarSection::Active));
        if !partition.working.is_empty() {
            items.push(SidebarListItem::Marker(SidebarListMarker::WorkingHeader));
            items.extend(rows(&visible_working, SidebarSection::Working));
        }
        if !partition.snoozed.is_empty() {
            items.push(SidebarListItem::Marker(SidebarListMarker::SnoozedHeader));
            items.extend(rows(&visible_snoozed, SidebarSection::Snoozed));
        }
        items.push(SidebarListItem::Marker(SidebarListMarker::SettledHeader));
        items.push(SidebarListItem::Marker(
            SidebarListMarker::SettledPlaceholder,
        ));
        items.extend(rows(&rendered_settled, SidebarSection::Settled));
    }
    SidebarListLayout {
        rendered_settled,
        hidden_settled_count,
        ordered_keys,
        items,
    }
}

/// The optimistic-drop release `useEffect`: true once the hold should end
/// (all writes landed, a failure or concurrent change, or the thread left).
pub fn should_release_optimistic_drop(
    drop: &OptimisticDrop,
    threads: &[EnvironmentThreadShell],
    pinned_keys: &[String],
    active_keys: &[String],
    now: &str,
) -> bool {
    let canonical: HashMap<String, &EnvironmentThreadShell> = threads
        .iter()
        .map(|thread| (thread.key(), thread))
        .collect();
    let Some(thread) = canonical.get(&drop.key) else {
        return true;
    };
    if thread.archived_at.is_some() {
        return true;
    }
    let canonical_section = if effective_snoozed(thread, now) {
        SidebarSection::Snoozed
    } else if thread.settled_override == Some(SettledOverride::Settled) {
        SidebarSection::Settled
    } else if thread.pinned_at.is_some() {
        SidebarSection::Pinned
    } else {
        SidebarSection::Active
    };
    let target: SidebarSection = drop.section.into();
    if canonical_section != drop.source_section && canonical_section != target {
        return true;
    }
    let Some(order) = &drop.order else {
        // Settle also emits unpin/unsnooze events: wait for the whole move.
        return canonical_section == target
            && thread.pinned_at.is_none()
            && (!drop.clears_snooze || thread.snoozed_until.is_none());
    };
    if canonical_section != target {
        return false;
    }
    if drop.clears_snooze && thread.snoozed_until.is_some() {
        return false;
    }
    let destination_keys = if drop.section == DropSection::Pinned {
        pinned_keys
    } else {
        active_keys
    };
    let key_of = |key: &str| {
        canonical.get(key).and_then(|thread| {
            if drop.section == DropSection::Pinned {
                thread.pin_order_key.clone()
            } else {
                thread.active_order_key.clone()
            }
        })
    };
    let held: HashSet<&String> = order.iter().collect();
    let membership_changed = destination_keys.len() != order.len()
        || destination_keys.iter().any(|key| !held.contains(key));
    let foreign_key_landed = destination_keys.iter().any(|key| {
        let current = key_of(key);
        if current == drop.keys_at_drop.get(key).cloned().flatten() {
            return false;
        }
        current.as_ref() != drop.assigned_keys.get(key)
    });
    let all_landed = drop
        .assigned_keys
        .iter()
        .all(|(key, order_key)| key_of(key).as_ref() == Some(order_key));
    membership_changed || foreign_key_landed || all_landed
}

#[cfg(test)]
mod tests {
    use super::super::mock_source::mock_snapshot;
    use super::*;
    use crate::timestamp_format::now_iso;

    #[test]
    fn partitions_the_fixture_into_every_shelf() {
        let snapshot = mock_snapshot();
        let mut tracker = InboxReturnTracker::default();
        let partition = partition_sidebar_threads(&snapshot, None, None, &mut tracker, &now_iso());
        assert_eq!(partition.pinned.len(), 2);
        assert_eq!(partition.snoozed.len(), 2);
        assert_eq!(partition.settled.len(), 6);
        assert!(partition.working.is_empty());
        // The keyed pin leads the keyless one.
        assert_eq!(partition.pinned[0].id.as_str(), "pin-roadmap");
        // The hidden subagent never appears.
        assert!(partition.section_of("local:subagent").is_none());
        // The woke thread is back in the inbox.
        assert_eq!(
            partition.section_of("local:woke"),
            Some(SidebarSection::Active)
        );
        // Soonest wake first.
        assert_eq!(partition.snoozed[0].id.as_str(), "snoozed-1");
    }

    #[test]
    fn working_shelf_folds_running_threads_when_enabled() {
        let mut snapshot = mock_snapshot();
        snapshot.settings.working_shelf_enabled = true;
        let mut tracker = InboxReturnTracker::default();
        let partition = partition_sidebar_threads(&snapshot, None, None, &mut tracker, &now_iso());
        let working: Vec<&str> = partition.working.iter().map(|t| t.id.as_str()).collect();
        assert!(working.contains(&"sidebar"));
        assert!(working.contains(&"waiting"));
        // Approval stays in the inbox.
        assert_eq!(
            partition.section_of("local:approval"),
            Some(SidebarSection::Active)
        );
    }

    #[test]
    fn scope_filters_to_one_project() {
        let snapshot = mock_snapshot();
        let scope: HashSet<String> = ["local:website".to_string()].into_iter().collect();
        let mut tracker = InboxReturnTracker::default();
        let partition =
            partition_sidebar_threads(&snapshot, Some(&scope), None, &mut tracker, &now_iso());
        assert!(
            partition
                .pinned
                .iter()
                .chain(&partition.active)
                .chain(&partition.settled)
                .all(|thread| thread.project_id.as_str() == "website")
        );
    }

    #[test]
    fn collapsed_shelves_keep_only_the_route_row_and_paging_keeps_it_visible() {
        let snapshot = mock_snapshot();
        let mut tracker = InboxReturnTracker::default();
        let partition = partition_sidebar_threads(&snapshot, None, None, &mut tracker, &now_iso());
        let last_settled = partition.settled.last().unwrap().key();
        let layout = layout_sidebar_list(&partition, Some(&last_settled), 2, true, false, false);
        assert_eq!(layout.rendered_settled.len(), 3);
        assert_eq!(layout.hidden_settled_count, 3);
        assert!(
            !layout
                .ordered_keys
                .iter()
                .any(|key| key.contains("snoozed"))
        );
        let collapsed =
            layout_sidebar_list(&partition, Some(&last_settled), 2, false, false, false);
        assert_eq!(collapsed.rendered_settled.len(), 1);
        assert!(collapsed.ordered_keys.contains(&last_settled));
        assert_eq!(
            collapsed.items.first(),
            Some(&SidebarListItem::Marker(SidebarListMarker::PinnedHeader))
        );
    }

    #[test]
    fn releases_a_settle_hold_once_the_thread_lands() {
        let mut snapshot = mock_snapshot();
        let drop = OptimisticDrop {
            key: "local:failed".into(),
            source_section: SidebarSection::Active,
            section: DropSection::Settled,
            occurred_at: now_iso(),
            clears_snooze: true,
            order: None,
            keys_at_drop: HashMap::new(),
            assigned_keys: HashMap::new(),
        };
        assert!(!should_release_optimistic_drop(
            &drop,
            &snapshot.threads,
            &[],
            &[],
            &now_iso()
        ));
        let thread = snapshot
            .threads
            .iter_mut()
            .find(|t| t.id.as_str() == "failed")
            .unwrap();
        thread.settled_override = Some(SettledOverride::Settled);
        assert!(should_release_optimistic_drop(
            &drop,
            &snapshot.threads,
            &[],
            &[],
            &now_iso()
        ));
    }
}
