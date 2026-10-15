//! Port of upstream/apps/web/src/components/Sidebar.logic.ts
//!
//! Pure sidebar rules: shelves and drag targets, statuses, search, project
//! ordering, menus. React hooks (`useSidebarRowSubscriptionLease`,
//! `useRetainedValue`, `useThreadJumpHintVisibility`), the dnd-kit
//! `animateSidebarLayoutChanges` hook and DOM helpers
//! (`shouldClearThreadSelectionOnMouseDown`, `isSidebarNestedLinkClick`)
//! have no GPUI counterpart; their behavior lives in the sidebar view.
//! `resolveSidebarStageBadgeLabel` belongs to branding (not ported).
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::future::Future;

// Re-exports upstream keeps on Sidebar.logic so the view has one import site.
use super::runtime::collate::locale_compare;
use super::runtime::models::{
    EnvironmentThreadShell, GoalStatus, InteractionMode, PendingSubagentRequest,
    ProviderFailureClass, RunStatus, SettledOverride, ThreadRelationship, ThreadRunSummary,
    ThreadRuntimeSummary, background_work_holds_completion, is_latest_run_settled,
    resolve_thread_working_started_at, thread_pull_request_search_terms,
};
#[allow(unused_imports)]
pub use super::runtime::thread_inbox::{
    is_thread_working as is_sidebar_thread_working, sort_inbox_threads_by_return,
    sort_working_threads_by_send,
};
use super::runtime::thread_settled::effective_snoozed;
use super::runtime::thread_sort::{
    OrderKeyAssignment, SortableThread, ThreadSortOrder, get_thread_sort_timestamp,
    plan_pinned_reorder, sort_threads, to_sortable_timestamp,
};
#[allow(unused_imports)]
pub use super::runtime::thread_sort::{
    pin_order_key_between, sort_active_threads_by_order_key as sort_threads_for_sidebar,
    sort_pinned_threads_by_order_key as sort_pinned_threads_for_sidebar,
};
use crate::timestamp_format::parse_timestamp_ms;
use turin_contracts::ThreadId;

/// Delay before holding the jump modifier reveals the ⌘1…9 hints.
pub const THREAD_JUMP_HINT_SHOW_DELAY_MS: u64 = 200;

/// Which action parked a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkAction {
    Settle,
    Snooze,
}

/// `shouldNavigateAfterThreadPark`: only move forward if the user is still
/// on the parked thread and the park actually took.
pub fn should_navigate_after_thread_park(
    thread_key: &str,
    current_thread_key: Option<&str>,
    action: ParkAction,
    now: &str,
    thread: Option<&EnvironmentThreadShell>,
) -> bool {
    Some(thread_key) == current_thread_key
        && thread.is_some_and(|thread| match action {
            ParkAction::Settle => thread.settled_override == Some(SettledOverride::Settled),
            ParkAction::Snooze => effective_snoozed(thread, now),
        })
}

/// `resolveSidebarRowAccessibility`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowAccessibility {
    pub label: String,
    /// `aria-current="page"`.
    pub current: bool,
}

/// The title is the row's identity and must lead when users scan tasks.
pub fn resolve_sidebar_row_accessibility(
    title: &str,
    status_label: Option<&str>,
    project_display_name: Option<&str>,
    is_active: bool,
) -> RowAccessibility {
    let label = [Some(title), status_label, project_display_name]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    RowAccessibility {
        label,
        current: is_active,
    }
}

/// Visible rows upstream prewarms into the thread-detail cache.
pub const SIDEBAR_THREAD_PREWARM_LIMIT: usize = 3;

/// `getSidebarThreadIdsToPrewarm`.
pub fn get_sidebar_thread_ids_to_prewarm<T: Clone>(visible: &[T], limit: usize) -> Vec<T> {
    visible[..visible.len().min(limit)].to_vec()
}

/// The shelf a visible thread belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarSection {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}

/// `resolveSidebarThreadSection`: snooze wins until its wake boundary;
/// settlement then wins over a stale pin.
pub fn resolve_sidebar_thread_section(
    snoozed: bool,
    settled: bool,
    pinned: bool,
) -> SidebarSection {
    if snoozed {
        SidebarSection::Snoozed
    } else if settled {
        SidebarSection::Settled
    } else if pinned {
        SidebarSection::Pinned
    } else {
        SidebarSection::Active
    }
}

const SIDEBAR_MARKER_PREFIX: &str = "sidebar-marker-";

/// Structural list items that share the sortable list with the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarListMarker {
    /// The top boundary is also a landing target when there are no pins.
    PinnedHeader,
    /// Stand-in rows so an empty section has somewhere for the gap to open.
    ActivePlaceholder,
    SettledPlaceholder,
    /// The boundary between pinned and active rows.
    PinnedDivider,
    WorkingHeader,
    SnoozedHeader,
    SettledHeader,
}

impl SidebarListMarker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PinnedHeader => "pinned-header",
            Self::ActivePlaceholder => "active-placeholder",
            Self::SettledPlaceholder => "settled-placeholder",
            Self::PinnedDivider => "pinned-divider",
            Self::WorkingHeader => "working-header",
            Self::SnoozedHeader => "snoozed-header",
            Self::SettledHeader => "settled-header",
        }
    }

    pub fn is_placeholder(self) -> bool {
        matches!(self, Self::ActivePlaceholder | Self::SettledPlaceholder)
    }

    pub fn is_shelf_header(self) -> bool {
        matches!(
            self,
            Self::WorkingHeader | Self::SnoozedHeader | Self::SettledHeader
        )
    }
}

/// `sidebarMarkerId`: colon-free, unlike scoped thread keys.
pub fn sidebar_marker_id(marker: SidebarListMarker) -> String {
    format!("{SIDEBAR_MARKER_PREFIX}{}", marker.as_str())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarListItem {
    Thread {
        key: String,
        section: SidebarSection,
    },
    Marker(SidebarListMarker),
}

impl SidebarListItem {
    pub fn thread(key: impl Into<String>, section: SidebarSection) -> Self {
        Self::Thread {
            key: key.into(),
            section,
        }
    }
}

/// `sidebarListItemId`.
pub fn sidebar_list_item_id(item: &SidebarListItem) -> String {
    match item {
        SidebarListItem::Thread { key, .. } => key.clone(),
        SidebarListItem::Marker(marker) => sidebar_marker_id(*marker),
    }
}

/// The section a slot belongs to, read off the markers above it.
fn section_at_sidebar_slot(items: &[SidebarListItem], index: usize) -> SidebarSection {
    let mut section = SidebarSection::Pinned;
    for item in items.iter().take(index) {
        if let SidebarListItem::Marker(marker) = item {
            match marker {
                SidebarListMarker::PinnedDivider => section = SidebarSection::Active,
                SidebarListMarker::WorkingHeader => section = SidebarSection::Working,
                SidebarListMarker::SnoozedHeader => section = SidebarSection::Snoozed,
                SidebarListMarker::SettledHeader => section = SidebarSection::Settled,
                _ => {}
            }
        }
    }
    section
}

/// Drop destinations (the working and snoozed shelves never are).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DropSection {
    Pinned,
    Active,
    Settled,
}

impl From<DropSection> for SidebarSection {
    fn from(section: DropSection) -> Self {
        match section {
            DropSection::Pinned => Self::Pinned,
            DropSection::Active => Self::Active,
            DropSection::Settled => Self::Settled,
        }
    }
}

/// `SidebarDropTarget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarDropTarget {
    pub section: DropSection,
    pub pinned_order: Vec<String>,
    pub active_order: Vec<String>,
}

/// `resolveSidebarDropTarget`: the destination section and manual order of
/// an `arrayMove` across the separators.
pub fn resolve_sidebar_drop_target(
    items: &[SidebarListItem],
    active_key: &str,
    over_id: &str,
) -> Option<SidebarDropTarget> {
    let active_index = items
        .iter()
        .position(|item| sidebar_list_item_id(item) == active_key)?;
    let over_index = items
        .iter()
        .position(|item| sidebar_list_item_id(item) == over_id)?;
    if !matches!(items[active_index], SidebarListItem::Thread { .. }) {
        return None;
    }
    let mut moved: Vec<SidebarListItem> = items
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != active_index)
        .map(|(_, item)| item.clone())
        .collect();
    moved.insert(over_index.min(moved.len()), items[active_index].clone());
    let section = match section_at_sidebar_slot(&moved, over_index) {
        SidebarSection::Pinned => DropSection::Pinned,
        SidebarSection::Active => DropSection::Active,
        SidebarSection::Settled => DropSection::Settled,
        SidebarSection::Working | SidebarSection::Snoozed => return None,
    };
    let mut pinned_order = Vec::new();
    let mut active_order = Vec::new();
    let mut current = SidebarSection::Pinned;
    for item in &moved {
        match item {
            SidebarListItem::Marker(SidebarListMarker::PinnedDivider) => {
                current = SidebarSection::Active
            }
            SidebarListItem::Marker(marker) if marker.is_shelf_header() => break,
            SidebarListItem::Marker(_) => {}
            SidebarListItem::Thread { key, .. } => {
                if current == SidebarSection::Pinned {
                    pinned_order.push(key.clone());
                } else {
                    active_order.push(key.clone());
                }
            }
        }
    }
    Some(SidebarDropTarget {
        section,
        pinned_order,
        active_order,
    })
}

/// `SidebarThreadDropPlan`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarThreadDropPlan {
    None,
    /// Within the pinned block: the existing key writes.
    ReorderPinned {
        order: Vec<String>,
        assignments: Vec<OrderKeyAssignment>,
    },
    /// From another section into the pinned block.
    Pin {
        order: Vec<String>,
        order_key: Option<String>,
        extra_assignments: Vec<OrderKeyAssignment>,
    },
    MoveActive {
        /// `None` when the inbox is time-ordered: the drop has no placement.
        order: Option<Vec<String>>,
        assignments: Vec<OrderKeyAssignment>,
        unpin: bool,
        unsettle: bool,
        unsnooze: bool,
    },
    Settle,
}

/// What dropping in `to` does to a thread lifted from `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarDropVerb {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Wake,
}

/// `resolveSidebarDropVerb`.
pub fn resolve_sidebar_drop_verb(
    from: SidebarSection,
    to: Option<SidebarSection>,
) -> Option<SidebarDropVerb> {
    let to = to?;
    if to == from || matches!(to, SidebarSection::Working | SidebarSection::Snoozed) {
        return None;
    }
    Some(match to {
        SidebarSection::Pinned => SidebarDropVerb::Pin,
        SidebarSection::Settled => SidebarDropVerb::Settle,
        _ if from == SidebarSection::Pinned => SidebarDropVerb::Unpin,
        _ if from == SidebarSection::Settled => SidebarDropVerb::Unsettle,
        _ => SidebarDropVerb::Wake,
    })
}

/// `resolveSidebarSweepKeys`: eligible rows between the pressed action and
/// the pointer, in sidebar order.
pub fn resolve_sidebar_sweep_keys(
    ordered_keys: &[String],
    origin_key: &str,
    target_key: &str,
    can_apply: impl Fn(&str) -> bool,
) -> Vec<String> {
    let (Some(origin), Some(target)) = (
        ordered_keys.iter().position(|key| key == origin_key),
        ordered_keys.iter().position(|key| key == target_key),
    ) else {
        return Vec::new();
    };
    ordered_keys[origin.min(target)..=origin.max(target)]
        .iter()
        .filter(|key| can_apply(key))
        .cloned()
        .collect()
}

/// `sidebarThreadKeyAtY` over measured rows `(key, top)` in list order: the
/// row at a pointer height, clamped to the visible viewport. A gap between
/// rows resolves to the row above it.
pub fn sidebar_thread_key_at_y(
    rows: &[(String, f32)],
    y: f32,
    viewport: Option<(f32, f32)>,
) -> Option<String> {
    let visible_y = match viewport {
        Some((top, bottom)) => y.max(top).min(bottom - 1.),
        None => y,
    };
    let mut key: Option<&String> = None;
    for (row_key, top) in rows {
        if key.is_some() && *top > visible_y {
            break;
        }
        key = Some(row_key);
    }
    key.cloned()
}

/// Input to [`plan_sidebar_thread_drop`] (`planSidebarThreadDrop`).
#[derive(Clone, Debug)]
pub struct SidebarDropInput<'a> {
    pub active_key: &'a str,
    pub active_section: SidebarSection,
    /// Snoozed threads can retain pinning and settlement beneath the shelf.
    pub active_pinned: Option<bool>,
    pub active_settled: Option<bool>,
    pub supports_settlement: Option<bool>,
    pub target: &'a SidebarDropTarget,
    /// All pinned keys in displayed order before the drop.
    pub pinned_order: &'a [String],
    pub pinned_keys_by_id: &'a HashMap<String, Option<String>>,
    pub reorderable_keys: Option<&'a HashSet<String>>,
    pub active_order: &'a [String],
    pub active_keys_by_id: &'a HashMap<String, Option<String>>,
    pub active_reorderable_keys: Option<&'a HashSet<String>>,
    /// Working beta: the inbox sorts by time, so drops only change lifecycle.
    pub active_time_ordered: bool,
}

/// `planSidebarThreadDrop`.
pub fn plan_sidebar_thread_drop(input: &SidebarDropInput<'_>) -> SidebarThreadDropPlan {
    let active_key = input.active_key;
    let active_section = input.active_section;
    let active_pinned = input
        .active_pinned
        .unwrap_or(active_section == SidebarSection::Pinned);
    let active_settled = input
        .active_settled
        .unwrap_or(active_section == SidebarSection::Settled);
    let target = input.target;
    if input.supports_settlement == Some(false)
        && (target.section == DropSection::Settled || active_settled)
    {
        return SidebarThreadDropPlan::None;
    }
    // Rows whose server cannot store an order are never written. Keyless
    // ones sort outside the keyed run, so they leave the plan; keyed ones
    // stay as bounds.
    let arrange = |order: &[String],
                   keys_by_id: &HashMap<String, Option<String>>,
                   writable: Option<&HashSet<String>>| {
        let Some(writable) = writable else {
            return Some(plan_pinned_reorder(order, keys_by_id, active_key));
        };
        if !writable.contains(active_key) {
            return None;
        }
        let ordered: Vec<String> = order
            .iter()
            .filter(|key| {
                writable.contains(*key) || keys_by_id.get(*key).is_some_and(Option::is_some)
            })
            .cloned()
            .collect();
        let assignments = plan_pinned_reorder(&ordered, keys_by_id, active_key);
        assignments
            .iter()
            .all(|assignment| writable.contains(&assignment.id))
            .then_some(assignments)
    };
    match target.section {
        DropSection::Active => {
            // Like the settled tail: threads can enter a time-ordered inbox,
            // but not be arranged inside it.
            if input.active_time_ordered {
                return if active_section == SidebarSection::Active {
                    SidebarThreadDropPlan::None
                } else {
                    SidebarThreadDropPlan::MoveActive {
                        order: None,
                        assignments: Vec::new(),
                        unpin: active_pinned,
                        unsettle: active_settled,
                        unsnooze: active_section == SidebarSection::Snoozed,
                    }
                };
            }
            let order = &target.active_order;
            if active_section == SidebarSection::Active && order.as_slice() == input.active_order {
                return SidebarThreadDropPlan::None;
            }
            let Some(assignments) = arrange(
                order,
                input.active_keys_by_id,
                input.active_reorderable_keys,
            ) else {
                return SidebarThreadDropPlan::None;
            };
            SidebarThreadDropPlan::MoveActive {
                order: Some(order.clone()),
                assignments,
                unpin: active_pinned,
                unsettle: active_settled,
                unsnooze: active_section == SidebarSection::Snoozed,
            }
        }
        DropSection::Settled => {
            if active_section == SidebarSection::Settled {
                SidebarThreadDropPlan::None
            } else {
                SidebarThreadDropPlan::Settle
            }
        }
        DropSection::Pinned => {
            let order = &target.pinned_order;
            // Dropped back where it started: nothing to write.
            if active_section == SidebarSection::Pinned && order.as_slice() == input.pinned_order {
                return SidebarThreadDropPlan::None;
            }
            let Some(assignments) = arrange(order, input.pinned_keys_by_id, input.reorderable_keys)
            else {
                return SidebarThreadDropPlan::None;
            };
            if active_section == SidebarSection::Pinned {
                return if assignments.is_empty() {
                    SidebarThreadDropPlan::None
                } else {
                    SidebarThreadDropPlan::ReorderPinned {
                        order: order.clone(),
                        assignments,
                    }
                };
            }
            let order_key = assignments
                .iter()
                .find(|assignment| assignment.id == active_key)
                .map(|assignment| assignment.order_key.clone());
            let extra_assignments = if active_pinned {
                assignments
            } else {
                assignments
                    .into_iter()
                    .filter(|assignment| assignment.id != active_key)
                    .collect()
            };
            SidebarThreadDropPlan::Pin {
                order: order.clone(),
                order_key,
                extra_assignments,
            }
        }
    }
}

/// `applySidebarThreadDrop`: project a drop's lifecycle fields before
/// sorting its destination, reusing the server's re-entry rules.
pub fn apply_sidebar_thread_drop(
    thread: &EnvironmentThreadShell,
    section: DropSection,
    now: &str,
    order_key: Option<&str>,
) -> EnvironmentThreadShell {
    let was_settled = thread.settled_override == Some(SettledOverride::Settled);
    let mut next = thread.clone();
    next.snoozed_at = None;
    next.snoozed_until = None;
    if section == DropSection::Settled {
        next.pinned_at = None;
        next.pin_order_key = None;
        next.active_order_key = None;
        next.settled_override = Some(SettledOverride::Settled);
        next.settled_at = Some(if was_settled {
            thread.settled_at.clone().unwrap_or_else(|| now.to_string())
        } else {
            now.to_string()
        });
        next.unsettled_at = None;
        return next;
    }
    if was_settled {
        next.settled_override = Some(SettledOverride::Active);
        next.settled_at = None;
        next.unsettled_at = Some(now.to_string());
    }
    if section == DropSection::Pinned {
        next.pinned_at = Some(thread.pinned_at.clone().unwrap_or_else(|| now.to_string()));
        next.pin_order_key = order_key
            .map(str::to_string)
            .or_else(|| thread.pin_order_key.clone());
    } else {
        next.pinned_at = None;
        next.pin_order_key = None;
        if let Some(order_key) = order_key {
            next.active_order_key = Some(order_key.to_string());
        }
    }
    next
}

/// `AtomCommandResult`, as the sidebar's command callbacks report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOutcome<E = String> {
    Success,
    Failure(E),
    /// The command fiber was interrupted (navigation, unmount): no toast.
    Interrupted,
}

/// `deleteSelectedThreadEntries` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteOutcome<E> {
    pub deleted_thread_keys: HashSet<String>,
    pub first_failure: Option<E>,
}

/// `deleteSelectedThreadEntries`: shared-worktree checks exclude only
/// successful deletions; `None` skips an entry the caller can no longer find.
pub async fn delete_selected_thread_entries<E, F, Fut>(
    entries: &[String],
    mut delete: F,
) -> DeleteOutcome<E>
where
    F: FnMut(&str, &HashSet<String>) -> Fut,
    Fut: Future<Output = Option<CommandOutcome<E>>>,
{
    let mut deleted = HashSet::new();
    let mut first_failure = None;
    for key in entries {
        match delete(key, &deleted).await {
            None => continue,
            Some(CommandOutcome::Interrupted) => break,
            Some(CommandOutcome::Failure(error)) => {
                if first_failure.is_none() {
                    first_failure = Some(error);
                }
            }
            Some(CommandOutcome::Success) => {
                deleted.insert(key.clone());
            }
        }
    }
    DeleteOutcome {
        deleted_thread_keys: deleted,
        first_failure,
    }
}

/// `archiveSelectedThreadEntries` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveOutcome<E> {
    pub archived_thread_keys: Vec<String>,
    pub mutation_failure: Option<E>,
    pub followup_failures: Vec<E>,
}

/// `archiveSelectedThreadEntries`: stops at the first mutation failure;
/// failures after the archive landed are follow-ups.
pub async fn archive_selected_thread_entries<E, F, Fut>(
    entries: &[String],
    mut archive: F,
) -> ArchiveOutcome<E>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = (bool, Result<(), E>)>,
{
    let mut archived = Vec::new();
    let mut followups = Vec::new();
    for key in entries {
        let (did_archive, result) = archive(key).await;
        if did_archive || result.is_ok() {
            archived.push(key.clone());
        }
        match result {
            Ok(()) => continue,
            Err(error) if did_archive => followups.push(error),
            Err(error) => {
                return ArchiveOutcome {
                    archived_thread_keys: archived,
                    mutation_failure: Some(error),
                    followup_failures: followups,
                };
            }
        }
    }
    ArchiveOutcome {
        archived_thread_keys: archived,
        mutation_failure: None,
        followup_failures: followups,
    }
}

/// `ContextMenuItem` (contracts ipc.ts): the desktop context-menu shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextMenuItem {
    pub id: String,
    pub label: String,
    pub icon: Option<&'static str>,
    pub disabled: bool,
    pub destructive: bool,
    pub checked: Option<bool>,
    pub separator_before: bool,
    pub children: Vec<ContextMenuItem>,
}

impl ContextMenuItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            disabled: false,
            destructive: false,
            checked: None,
            separator_before: false,
            children: Vec::new(),
        }
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn separator_before(mut self) -> Self {
        self.separator_before = true;
        self
    }

    pub fn children(mut self, children: Vec<ContextMenuItem>) -> Self {
        self.children = children;
        self
    }
}

/// `buildMultiSelectThreadContextMenuItems`.
pub fn build_multi_select_thread_context_menu_items(
    count: usize,
    has_running_thread: bool,
) -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::new("mark-unread", format!("Mark unread ({count})")),
        ContextMenuItem::new("archive", format!("Archive ({count})")).disabled(has_running_thread),
        ContextMenuItem::new("delete", format!("Delete ({count})")).destructive(),
    ]
}

/// `isSidebarSubagentThread`.
pub fn is_sidebar_subagent_thread(thread: &EnvironmentThreadShell) -> bool {
    thread.lineage.relationship_to_parent == Some(ThreadRelationship::Subagent)
}

/// `filterSidebarV2VisibleThreads`: unarchived, top-level, in scope.
pub fn filter_sidebar_v2_visible_threads<'a>(
    threads: impl IntoIterator<Item = &'a EnvironmentThreadShell>,
    scoped_project_keys: Option<&HashSet<String>>,
) -> Vec<&'a EnvironmentThreadShell> {
    threads
        .into_iter()
        .filter(|thread| {
            thread.archived_at.is_none()
                && !is_sidebar_subagent_thread(thread)
                && scoped_project_keys.is_none_or(|keys| keys.contains(&thread.project_key()))
        })
        .collect()
}

/// `getSidebarForkParentThreadId`.
pub fn get_sidebar_fork_parent_thread_id(thread: &EnvironmentThreadShell) -> Option<ThreadId> {
    use super::runtime::models::ForkedFrom;
    if thread.lineage.relationship_to_parent != Some(ThreadRelationship::Fork) {
        return None;
    }
    match &thread.forked_from {
        Some(ForkedFrom::Run { thread_id }) => Some(thread_id.clone()),
        _ => thread.lineage.parent_thread_id.clone(),
    }
}

/// `buildBulkTitleRegenerationContextMenuItem`.
pub fn build_bulk_title_regeneration_context_menu_item(
    supported_count: usize,
    actionable_count: usize,
) -> Option<ContextMenuItem> {
    if supported_count == 0 {
        return None;
    }
    if actionable_count == 0 {
        return Some(
            ContextMenuItem::new(
                "regenerate-title",
                format!("Regenerating… ({supported_count})"),
            )
            .disabled(true),
        );
    }
    Some(ContextMenuItem::new(
        "regenerate-title",
        format!("Regenerate titles ({actionable_count})"),
    ))
}

/// `buildBulkUnpinContextMenuItem`: counts only what the action touches.
pub fn build_bulk_unpin_context_menu_item(pinned_count: usize) -> Option<ContextMenuItem> {
    (pinned_count > 0).then(|| ContextMenuItem::new("unpin", format!("Unpin ({pinned_count})")))
}

/// `ThreadStatusPill["label"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatusPillLabel {
    Working,
    Connecting,
    Completed,
    PendingApproval,
    AwaitingInput,
    Waiting,
    PlanReady,
}

impl ThreadStatusPillLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Connecting => "Connecting",
            Self::Completed => "Completed",
            Self::PendingApproval => "Pending Approval",
            Self::AwaitingInput => "Awaiting Input",
            Self::Waiting => "Waiting",
            Self::PlanReady => "Plan Ready",
        }
    }

    /// `THREAD_STATUS_PRIORITY`, doubled to stay integral (Waiting is 2.5).
    fn priority(self) -> u8 {
        match self {
            Self::PendingApproval => 10,
            Self::AwaitingInput => 8,
            Self::Working | Self::Connecting => 6,
            Self::Waiting => 5,
            Self::PlanReady => 4,
            Self::Completed => 2,
        }
    }
}

/// `ThreadStatusPill` (legacy sidebar and project status dots).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadStatusPill {
    pub label: ThreadStatusPillLabel,
    pub color_class: &'static str,
    pub dot_class: &'static str,
    pub pulse: bool,
}

/// `ThreadJumpHintVisibilityController` as a timer-free state machine: the
/// view arms a timer for [`JumpHintAction::Schedule`] and calls
/// [`Self::fire`] when it elapses.
#[derive(Debug, Default)]
pub struct ThreadJumpHintVisibilityController {
    visible: bool,
    pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpHintAction {
    None,
    /// Start a `THREAD_JUMP_HINT_SHOW_DELAY_MS` timer.
    Schedule,
    /// Visibility flipped to the given value.
    Changed(bool),
}

impl ThreadJumpHintVisibilityController {
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn sync(&mut self, should_show: bool) -> JumpHintAction {
        if !should_show {
            self.pending = false;
            if self.visible {
                self.visible = false;
                return JumpHintAction::Changed(false);
            }
            return JumpHintAction::None;
        }
        if self.visible || self.pending {
            return JumpHintAction::None;
        }
        self.pending = true;
        JumpHintAction::Schedule
    }

    /// The scheduled timer elapsed. A cancelled reveal ignores it.
    pub fn fire(&mut self) -> JumpHintAction {
        if !self.pending {
            return JumpHintAction::None;
        }
        self.pending = false;
        self.visible = true;
        JumpHintAction::Changed(true)
    }

    pub fn dispose(&mut self) {
        self.pending = false;
    }
}

/// `resolveThreadLastVisitedAt`: a server with visited tracking is
/// authoritative (including mark-unread rewinds); the local watermark only
/// carries servers without it. `server` is `None` when the server omits it.
pub fn resolve_thread_last_visited_at(
    server: Option<Option<&str>>,
    local: Option<&str>,
) -> Option<String> {
    match server {
        None => local.map(str::to_string),
        Some(server) => server.map(str::to_string),
    }
}

/// `hasUnseenCompletion`: never-visited counts as read.
pub fn has_unseen_completion(
    latest_run: Option<&ThreadRunSummary>,
    last_visited_at: Option<&str>,
) -> bool {
    let Some(completed_at) = latest_run.and_then(|run| run.completed_at.as_deref()) else {
        return false;
    };
    let Some(completed_at) = parse_timestamp_ms(completed_at) else {
        return false;
    };
    let Some(last_visited_at) = last_visited_at.filter(|value| !value.is_empty()) else {
        return false;
    };
    match parse_timestamp_ms(last_visited_at) {
        None => true,
        Some(visited) => completed_at > visited,
    }
}

/// `isTrailingDoubleClick`: the second click of a double-click must not
/// also navigate.
pub fn is_trailing_double_click(detail: usize) -> bool {
    detail > 1
}

/// `shouldCreateNewThreadInCurrentProject`.
pub fn should_create_new_thread_in_current_project(
    shift_key: bool,
    project_group_count: usize,
) -> bool {
    shift_key || project_group_count <= 1
}

/// Extra ids an item answers to (`getPreferenceIds`).
pub type PreferenceIds<'a, T, Id> = Option<&'a dyn Fn(&T) -> Vec<Id>>;

/// `orderItemsByPreferredIds`.
pub fn order_items_by_preferred_ids<T: Clone, Id: Eq + std::hash::Hash + Clone>(
    items: &[T],
    preferred_ids: &[Id],
    get_id: impl Fn(&T) -> Id,
    get_preference_ids: PreferenceIds<'_, T, Id>,
) -> Vec<T> {
    if preferred_ids.is_empty() {
        return items.to_vec();
    }
    let mut indexes_by_preference: HashMap<Id, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        let mut ids = match get_preference_ids {
            Some(get) => get(item),
            None => vec![get_id(item)],
        };
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(id.clone()));
        for id in ids {
            indexes_by_preference.entry(id).or_default().push(index);
        }
    }
    let mut emitted = HashSet::new();
    let mut ordered = Vec::new();
    for id in preferred_ids {
        let candidate = indexes_by_preference
            .get(id)
            .and_then(|indexes| indexes.iter().find(|index| !emitted.contains(*index)));
        if let Some(index) = candidate {
            emitted.insert(*index);
            ordered.push(items[*index].clone());
        }
    }
    ordered.extend(
        items
            .iter()
            .enumerate()
            .filter(|(index, _)| !emitted.contains(index))
            .map(|(_, item)| item.clone()),
    );
    ordered
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadTraversalDirection {
    Previous,
    Next,
}

/// `resolveAdjacentThreadId`.
pub fn resolve_adjacent_thread_id<T: PartialEq + Clone>(
    thread_ids: &[T],
    current: Option<&T>,
    direction: ThreadTraversalDirection,
) -> Option<T> {
    if thread_ids.is_empty() {
        return None;
    }
    let Some(current) = current else {
        return match direction {
            ThreadTraversalDirection::Previous => thread_ids.last().cloned(),
            ThreadTraversalDirection::Next => thread_ids.first().cloned(),
        };
    };
    let index = thread_ids.iter().position(|id| id == current)?;
    match direction {
        ThreadTraversalDirection::Previous => index.checked_sub(1).map(|i| thread_ids[i].clone()),
        ThreadTraversalDirection::Next => thread_ids.get(index + 1).cloned(),
    }
}

/// `isContextMenuPointerDown`.
pub fn is_context_menu_pointer_down(button: u8, ctrl_key: bool, is_mac: bool) -> bool {
    button == 2 || (is_mac && button == 0 && ctrl_key)
}

// ── Sidebar v2 status model ─────────────────────────────────────────
// Six visual states, three colors: color is reserved for "act now"
// (approval), "in motion" (working), and "broken" (failed). Ready is the
// unlabeled resting state. Waiting (runtime status "idle") is the agent
// stopped with background work that will wake it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarThreadStatus {
    Approval,
    Input,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}

/// `shouldRecedeSidebarThread`.
pub fn should_recede_sidebar_thread(
    status: SidebarThreadStatus,
    is_unread: bool,
    is_woke: bool,
    is_active: bool,
    is_selected: bool,
) -> bool {
    if is_active || is_selected || status == SidebarThreadStatus::Input {
        return false;
    }
    match status {
        SidebarThreadStatus::Working | SidebarThreadStatus::Waiting => true,
        SidebarThreadStatus::Ready | SidebarThreadStatus::Approval => !is_unread && !is_woke,
        _ => false,
    }
}

/// `resolveSidebarThreadStatus`.
pub fn resolve_sidebar_thread_status(thread: &EnvironmentThreadShell) -> SidebarThreadStatus {
    resolve_sidebar_thread_status_parts(
        thread.has_pending_approvals,
        thread.has_pending_user_input,
        thread.pending_subagent_request,
        thread.runtime.as_ref(),
    )
}

pub fn resolve_sidebar_thread_status_parts(
    has_pending_approvals: bool,
    has_pending_user_input: bool,
    pending_subagent_request: Option<PendingSubagentRequest>,
    runtime: Option<&ThreadRuntimeSummary>,
) -> SidebarThreadStatus {
    if has_pending_approvals || pending_subagent_request == Some(PendingSubagentRequest::Approval) {
        return SidebarThreadStatus::Approval;
    }
    if has_pending_user_input || pending_subagent_request == Some(PendingSubagentRequest::UserInput)
    {
        return SidebarThreadStatus::Input;
    }
    match runtime.map(|runtime| runtime.status) {
        Some(
            RunStatus::Preparing
            | RunStatus::Queued
            | RunStatus::Starting
            | RunStatus::Running
            | RunStatus::Waiting,
        ) => SidebarThreadStatus::Working,
        Some(RunStatus::Idle) => SidebarThreadStatus::Waiting,
        Some(RunStatus::Failed) => {
            if runtime.and_then(|runtime| runtime.last_error_class.as_ref())
                == Some(&ProviderFailureClass::UsageLimit)
            {
                SidebarThreadStatus::Limited
            } else {
                SidebarThreadStatus::Failed
            }
        }
        _ => SidebarThreadStatus::Ready,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarV2TopStatusKind {
    Approval,
    Done,
    Failed,
    Limited,
    Input,
    Waiting,
    Woke,
    Working,
}

/// `resolveSidebarV2TopStatus`.
pub fn resolve_sidebar_v2_top_status(
    status: SidebarThreadStatus,
    is_unread: bool,
    is_woke: bool,
) -> Option<SidebarV2TopStatusKind> {
    Some(match status {
        SidebarThreadStatus::Working => SidebarV2TopStatusKind::Working,
        SidebarThreadStatus::Waiting => SidebarV2TopStatusKind::Waiting,
        SidebarThreadStatus::Approval => SidebarV2TopStatusKind::Approval,
        SidebarThreadStatus::Input => SidebarV2TopStatusKind::Input,
        SidebarThreadStatus::Failed => SidebarV2TopStatusKind::Failed,
        SidebarThreadStatus::Limited => SidebarV2TopStatusKind::Limited,
        SidebarThreadStatus::Ready if is_woke => SidebarV2TopStatusKind::Woke,
        SidebarThreadStatus::Ready if is_unread => SidebarV2TopStatusKind::Done,
        SidebarThreadStatus::Ready => return None,
    })
}

/// `shouldShowSidebarV2Duration`.
pub fn should_show_sidebar_v2_duration(status: SidebarThreadStatus) -> bool {
    status == SidebarThreadStatus::Working
}

/// `firstValidTimestampMs`: the first candidate that parses, else 0.
pub fn first_valid_timestamp_ms(candidates: &[Option<&str>]) -> i64 {
    candidates
        .iter()
        .flatten()
        .find_map(|candidate| parse_timestamp_ms(candidate))
        .unwrap_or(0)
}

/// `threadSearchMatchKey` (client-runtime thread-search).
pub fn thread_search_match_key(environment_id: &str, thread_id: &str) -> String {
    serde_json_like_pair(environment_id, thread_id)
}

fn serde_json_like_pair(a: &str, b: &str) -> String {
    let escape = |value: &str| value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("[\"{}\",\"{}\"]", escape(a), escape(b))
}

/// `searchSidebarThreads`: title or linked PR first, then threads whose
/// messages the server matched; input order is kept.
pub fn search_sidebar_threads<'a>(
    threads: &[&'a EnvironmentThreadShell],
    query: &str,
    content_match_keys: &HashSet<String>,
) -> Vec<&'a EnvironmentThreadShell> {
    let normalized = query.trim().to_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut title_matches = Vec::new();
    let mut content_matches = Vec::new();
    for thread in threads {
        let matches_title = std::iter::once(thread.title.clone())
            .chain(thread_pull_request_search_terms(&thread.pull_requests))
            .any(|term| term.to_lowercase().contains(&normalized));
        if matches_title {
            title_matches.push(*thread);
        } else if !content_match_keys.is_empty()
            && content_match_keys.contains(&thread_search_match_key(
                thread.environment_id.as_str(),
                thread.id.as_str(),
            ))
        {
            content_matches.push(*thread);
        }
    }
    title_matches.extend(content_matches);
    title_matches
}

/// `normalizeSearchText` (lib/utils.ts) without NFKD mark stripping.
pub fn normalize_search_text(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `pullRequestMatchesSearchQuery`: every query token appears in the terms.
pub fn pull_request_matches_search_query(terms: &[String], query: &str) -> bool {
    let normalized = normalize_search_text(query);
    if normalized.is_empty() {
        return false;
    }
    let haystack = normalize_search_text(&terms.join(" "));
    normalized.split(' ').all(|token| haystack.contains(token))
}

/// A project-scope combobox row (`{ value, label }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectScopeItem {
    pub value: String,
    pub label: String,
}

/// `filterSidebarProjectScopeItems`: "All projects" heads the list while
/// the query is empty and drops out while filtering.
pub fn filter_sidebar_project_scope_items(
    items: &[ProjectScopeItem],
    query: &str,
    matches: impl Fn(&ProjectScopeItem, &str) -> bool,
) -> Vec<ProjectScopeItem> {
    let query = query.trim();
    if query.is_empty() {
        return items.to_vec();
    }
    items
        .iter()
        .filter(|item| item.value != "all" && matches(item, query))
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarProjectScopeMenuState {
    pub open: bool,
    pub query: String,
}

pub enum SidebarProjectScopeMenuAction {
    QueryChanged(String),
    OpenChanged(bool),
    ProjectSettingsOpened,
}

/// `reduceSidebarProjectScopeMenuState`.
pub fn reduce_sidebar_project_scope_menu_state(
    state: &SidebarProjectScopeMenuState,
    action: SidebarProjectScopeMenuAction,
) -> SidebarProjectScopeMenuState {
    match action {
        SidebarProjectScopeMenuAction::QueryChanged(query) => SidebarProjectScopeMenuState {
            open: state.open,
            query,
        },
        SidebarProjectScopeMenuAction::OpenChanged(open) => SidebarProjectScopeMenuState {
            open,
            query: String::new(),
        },
        SidebarProjectScopeMenuAction::ProjectSettingsOpened => {
            SidebarProjectScopeMenuState::default()
        }
    }
}

/// `resolveWorkingStartedAt`.
pub fn resolve_working_started_at(thread: &EnvironmentThreadShell) -> Option<String> {
    resolve_thread_working_started_at(thread)
}

/// `formatWorkingDurationLabel`.
pub fn format_working_duration_label(elapsed_ms: f64) -> String {
    let seconds = if elapsed_ms.is_finite() {
        (elapsed_ms / 1000.).floor().max(0.) as u64
    } else {
        0
    };
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// `resolveThreadStatusPill` (legacy rows, project dots, the chat header).
pub fn resolve_thread_status_pill(
    thread: &EnvironmentThreadShell,
    last_visited_at: Option<&str>,
) -> Option<ThreadStatusPill> {
    let pill = |label, color_class, dot_class, pulse| ThreadStatusPill {
        label,
        color_class,
        dot_class,
        pulse,
    };
    if thread.has_pending_approvals
        || thread.pending_subagent_request == Some(PendingSubagentRequest::Approval)
    {
        return Some(pill(
            ThreadStatusPillLabel::PendingApproval,
            "text-amber-600 dark:text-amber-300/90",
            "bg-amber-500 dark:bg-amber-300/90",
            false,
        ));
    }
    if thread.has_pending_user_input
        || thread.pending_subagent_request == Some(PendingSubagentRequest::UserInput)
    {
        return Some(pill(
            ThreadStatusPillLabel::AwaitingInput,
            "text-indigo-600 dark:text-indigo-300/90",
            "bg-indigo-500 dark:bg-indigo-300/90",
            false,
        ));
    }
    match thread.runtime.as_ref().map(|runtime| runtime.status) {
        Some(RunStatus::Running | RunStatus::Waiting) => {
            return Some(pill(
                ThreadStatusPillLabel::Working,
                "text-sky-600 dark:text-sky-300/80",
                "bg-sky-500 dark:bg-sky-300/80",
                true,
            ));
        }
        Some(RunStatus::Preparing | RunStatus::Starting | RunStatus::Queued) => {
            return Some(pill(
                ThreadStatusPillLabel::Connecting,
                "text-sky-600 dark:text-sky-300/80",
                "bg-sky-500 dark:bg-sky-300/80",
                true,
            ));
        }
        _ => {}
    }
    if background_work_holds_completion(&thread.pending_background_tasks) {
        return Some(pill(
            ThreadStatusPillLabel::Waiting,
            "text-sidebar-muted-foreground",
            "bg-sidebar-muted-foreground",
            false,
        ));
    }
    let has_plan_ready_prompt = !thread.has_pending_user_input
        && thread.interaction_mode == InteractionMode::Plan
        && is_latest_run_settled(thread.latest_run.as_ref(), thread.runtime.as_ref())
        && thread.has_actionable_proposed_plan;
    if has_plan_ready_prompt {
        return Some(pill(
            ThreadStatusPillLabel::PlanReady,
            "text-violet-600 dark:text-violet-300/90",
            "bg-violet-500 dark:bg-violet-300/90",
            false,
        ));
    }
    if has_unseen_completion(thread.latest_run.as_ref(), last_visited_at) {
        return Some(pill(
            ThreadStatusPillLabel::Completed,
            "text-emerald-600 dark:text-emerald-300/90",
            "bg-emerald-500 dark:bg-emerald-300/90",
            false,
        ));
    }
    None
}

/// `resolveProjectStatusIndicator`: the highest-priority status.
pub fn resolve_project_status_indicator(
    statuses: &[Option<ThreadStatusPill>],
) -> Option<ThreadStatusPill> {
    let mut best: Option<&ThreadStatusPill> = None;
    for status in statuses.iter().flatten() {
        if best.is_none_or(|best| status.label.priority() > best.label.priority()) {
            best = Some(status);
        }
    }
    best.cloned()
}

/// `getFallbackThreadIdAfterDelete`: the top remaining thread of the
/// deleted thread's project.
pub fn get_fallback_thread_id_after_delete(
    threads: &[EnvironmentThreadShell],
    deleted_thread_id: &ThreadId,
    sort_order: ThreadSortOrder,
    deleted_thread_ids: Option<&HashSet<ThreadId>>,
) -> Option<ThreadId> {
    let deleted = threads
        .iter()
        .find(|thread| &thread.id == deleted_thread_id)?;
    let candidates: Vec<EnvironmentThreadShell> = threads
        .iter()
        .filter(|thread| {
            thread.project_id == deleted.project_id
                && &thread.id != deleted_thread_id
                && deleted_thread_ids.is_none_or(|ids| !ids.contains(&thread.id))
        })
        .cloned()
        .collect();
    sort_threads(&candidates, sort_order)
        .first()
        .map(|thread| thread.id.clone())
}

/// `SidebarProjectSortOrder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSortOrder {
    Manual,
    CreatedAt,
    UpdatedAt,
}

impl ProjectSortOrder {
    fn thread_order(self) -> Option<ThreadSortOrder> {
        match self {
            Self::Manual => None,
            Self::CreatedAt => Some(ThreadSortOrder::CreatedAt),
            Self::UpdatedAt => Some(ThreadSortOrder::UpdatedAt),
        }
    }
}

/// A project as the sort helpers read it.
pub trait SidebarProjectLike {
    fn sort_title(&self) -> &str;
    fn sort_id(&self) -> String;
    fn sort_created_at(&self) -> Option<&str>;
    fn sort_updated_at(&self) -> Option<&str>;
}

/// `getProjectSortTimestamp`.
pub fn get_project_sort_timestamp<P: SidebarProjectLike + ?Sized, T: SortableThread>(
    project: &P,
    project_threads: &[&T],
    sort_order: ThreadSortOrder,
) -> f64 {
    if !project_threads.is_empty() {
        return project_threads
            .iter()
            .map(|thread| get_thread_sort_timestamp(*thread, sort_order))
            .fold(f64::NEG_INFINITY, f64::max);
    }
    let stamp = match sort_order {
        ThreadSortOrder::CreatedAt => to_sortable_timestamp(project.sort_created_at()),
        ThreadSortOrder::UpdatedAt => {
            to_sortable_timestamp(project.sort_updated_at().or(project.sort_created_at()))
        }
    };
    stamp.map_or(f64::NEG_INFINITY, |ms| ms as f64)
}

fn sort_projects_by_activity<'t, P: SidebarProjectLike + Clone, T: SortableThread + 't>(
    projects: &[P],
    sort_order: ProjectSortOrder,
    project_threads: impl Fn(&P) -> Vec<&'t T>,
) -> Vec<P> {
    let Some(order) = sort_order.thread_order() else {
        return projects.to_vec();
    };
    // Each project's timestamp walks all of its threads, so compute it once.
    let mut keyed: Vec<(f64, &P)> = projects
        .iter()
        .map(|project| {
            (
                get_project_sort_timestamp(project, &project_threads(project), order),
                project,
            )
        })
        .collect();
    keyed.sort_by(|(left_ts, left), (right_ts, right)| {
        right_ts
            .partial_cmp(left_ts)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| locale_compare(left.sort_title(), right.sort_title()))
            .then_with(|| locale_compare(&left.sort_id(), &right.sort_id()))
    });
    keyed
        .into_iter()
        .map(|(_, project)| project.clone())
        .collect()
}

/// A logical (grouped) sidebar project: `useSidebarProjectGroups` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalSidebarProject {
    pub project_key: String,
    pub title: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// `${environmentId}:${projectId}` of every member project.
    pub member_project_keys: Vec<String>,
}

impl SidebarProjectLike for LogicalSidebarProject {
    fn sort_title(&self) -> &str {
        &self.title
    }
    fn sort_id(&self) -> String {
        self.project_key.clone()
    }
    fn sort_created_at(&self) -> Option<&str> {
        self.created_at.as_deref()
    }
    fn sort_updated_at(&self) -> Option<&str> {
        self.updated_at.as_deref()
    }
}

/// `sortLogicalProjectsForSidebar`: archived threads never count as activity.
pub fn sort_logical_projects_for_sidebar(
    projects: &[LogicalSidebarProject],
    threads: &[&EnvironmentThreadShell],
    sort_order: ProjectSortOrder,
) -> Vec<LogicalSidebarProject> {
    let group_by_member: HashMap<&str, &str> = projects
        .iter()
        .flat_map(|project| {
            project
                .member_project_keys
                .iter()
                .map(move |member| (member.as_str(), project.project_key.as_str()))
        })
        .collect();
    let mut by_group: HashMap<&str, Vec<&EnvironmentThreadShell>> = HashMap::new();
    for thread in threads {
        if thread.archived_at.is_some() {
            continue;
        }
        if let Some(group) = group_by_member.get(thread.project_key().as_str()) {
            by_group.entry(group).or_default().push(thread);
        }
    }
    sort_projects_by_activity(projects, sort_order, |project| {
        by_group
            .get(project.project_key.as_str())
            .cloned()
            .unwrap_or_default()
    })
}

/// `sortSidebarV2ProjectGroups`: hidden subagent threads never reorder projects.
pub fn sort_sidebar_v2_project_groups(
    projects: &[LogicalSidebarProject],
    threads: &[EnvironmentThreadShell],
    sort_order: ProjectSortOrder,
) -> Vec<LogicalSidebarProject> {
    let visible = filter_sidebar_v2_visible_threads(threads, None);
    sort_logical_projects_for_sidebar(projects, &visible, sort_order)
}

/// Goal-aware Working label (`thread.goal?.status === "active"`).
pub fn working_label(thread: &EnvironmentThreadShell) -> &'static str {
    if thread.goal == Some(GoalStatus::Active) {
        "Goal"
    } else {
        "Working"
    }
}

#[cfg(test)]
mod tests {
    use super::super::runtime::models::fixtures::{
        env, project_id, run_id, runtime, shell, thread_id,
    };
    use super::super::runtime::models::{
        BackgroundTaskKind, ForkedFrom, PendingBackgroundTask, ThreadLineage,
    };
    use super::*;
    use futures::executor::block_on;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    // describe("resolveSidebarRowAccessibility")
    #[test]
    fn leads_with_the_title_without_folding_row_actions_into_its_name() {
        assert_eq!(
            resolve_sidebar_row_accessibility(
                "Can you audit the UI?",
                Some("Working"),
                Some("T3 Code"),
                true
            ),
            RowAccessibility {
                label: "Can you audit the UI?, Working, T3 Code".into(),
                current: true
            }
        );
        assert_eq!(
            resolve_sidebar_row_accessibility("The audit is done", None, Some("T3 Code"), false),
            RowAccessibility {
                label: "The audit is done, T3 Code".into(),
                current: false
            }
        );
        assert_eq!(
            resolve_sidebar_row_accessibility("Untitled task", None, None, false).label,
            "Untitled task"
        );
    }

    // describe("resolveSidebarThreadSection")
    #[test]
    fn keeps_a_pinned_thread_in_the_pinned_shelf() {
        assert_eq!(
            resolve_sidebar_thread_section(false, false, true),
            SidebarSection::Pinned
        );
    }

    #[test]
    fn keeps_lifecycle_shelves_authoritative_over_a_stale_pin() {
        assert_eq!(
            resolve_sidebar_thread_section(true, true, true),
            SidebarSection::Snoozed
        );
        assert_eq!(
            resolve_sidebar_thread_section(false, true, true),
            SidebarSection::Settled
        );
    }

    // describe("deleteSelectedThreadEntries")
    fn entries() -> Vec<String> {
        strings(&["one", "two", "three"])
    }

    #[test]
    fn waits_for_each_delete_and_excludes_only_earlier_successes_from_worktree_checks() {
        let mut checks: Vec<(String, Vec<String>)> = Vec::new();
        let outcome = block_on(delete_selected_thread_entries::<String, _, _>(
            &entries(),
            |key, deleted| {
                let mut keys: Vec<String> = deleted.iter().cloned().collect();
                keys.sort();
                checks.push((key.to_string(), keys));
                async { Some(CommandOutcome::Success) }
            },
        ));
        assert_eq!(
            checks,
            vec![
                ("one".to_string(), vec![]),
                ("two".to_string(), strings(&["one"])),
                ("three".to_string(), strings(&["one", "two"])),
            ]
        );
        assert_eq!(
            outcome,
            DeleteOutcome {
                deleted_thread_keys: entries().into_iter().collect(),
                first_failure: None
            }
        );
    }

    #[test]
    fn continues_after_ordinary_failures_and_keeps_the_first_failure() {
        let mut last: Vec<Vec<String>> = Vec::new();
        let mut all = entries();
        all.push("four".into());
        let outcome = block_on(delete_selected_thread_entries(&all, |key, deleted| {
            let result = match key {
                "one" => CommandOutcome::Failure("Delete failed".to_string()),
                "three" => CommandOutcome::Failure("Later failure".to_string()),
                _ => {
                    if key == "four" {
                        last.push(deleted.iter().cloned().collect());
                    }
                    CommandOutcome::Success
                }
            };
            async move { Some(result) }
        }));
        assert_eq!(last, vec![strings(&["two"])]);
        assert_eq!(
            outcome,
            DeleteOutcome {
                deleted_thread_keys: strings(&["two", "four"]).into_iter().collect(),
                first_failure: Some("Delete failed".to_string())
            }
        );
    }

    #[test]
    fn stops_on_interruption_and_preserves_earlier_results() {
        for (first, deleted, failure) in [
            (CommandOutcome::Success, vec!["one"], None),
            (
                CommandOutcome::Failure("Delete failed".to_string()),
                vec![],
                Some("Delete failed".to_string()),
            ),
        ] {
            let mut attempted = Vec::new();
            let outcome = block_on(delete_selected_thread_entries(&entries(), |key, _| {
                attempted.push(key.to_string());
                let result = if key == "one" {
                    first.clone()
                } else {
                    CommandOutcome::Interrupted
                };
                async move { Some(result) }
            }));
            assert_eq!(attempted, strings(&["one", "two"]));
            assert_eq!(
                outcome,
                DeleteOutcome {
                    deleted_thread_keys: strings(&deleted).into_iter().collect(),
                    first_failure: failure
                }
            );
        }
    }

    #[test]
    fn does_not_count_a_skipped_entry_as_deleted() {
        let mut visible: HashSet<String> = entries().into_iter().collect();
        let mut checks: Vec<Vec<String>> = Vec::new();
        let outcome = block_on(delete_selected_thread_entries::<String, _, _>(
            &entries(),
            |key, deleted| {
                let result = if visible.contains(key) {
                    checks.push(deleted.iter().cloned().collect());
                    visible.remove("two");
                    Some(CommandOutcome::Success)
                } else {
                    None
                };
                async move { result }
            },
        ));
        assert_eq!(checks, vec![vec![], strings(&["one"])]);
        assert_eq!(
            outcome.deleted_thread_keys,
            strings(&["one", "three"]).into_iter().collect()
        );
    }

    // describe("archiveSelectedThreadEntries")
    #[test]
    fn records_every_entry_after_full_success() {
        let outcome = block_on(archive_selected_thread_entries::<(), _, _>(
            &entries(),
            |_| async { (true, Ok(())) },
        ));
        assert_eq!(
            outcome,
            ArchiveOutcome {
                archived_thread_keys: entries(),
                mutation_failure: None,
                followup_failures: vec![]
            }
        );
    }

    #[test]
    fn stops_at_a_mutation_failure_and_retains_prior_successes() {
        let mut calls = 0;
        let outcome = block_on(archive_selected_thread_entries(&entries(), |key| {
            calls += 1;
            let result = if key == "two" {
                (false, Err("failure"))
            } else {
                (true, Ok(()))
            };
            async move { result }
        }));
        assert_eq!(calls, 2);
        assert_eq!(
            outcome,
            ArchiveOutcome {
                archived_thread_keys: strings(&["one"]),
                mutation_failure: Some("failure"),
                followup_failures: vec![]
            }
        );
    }

    #[test]
    fn continues_after_a_post_archive_failure() {
        let mut calls = 0;
        let outcome = block_on(archive_selected_thread_entries(&entries(), |key| {
            calls += 1;
            let result = (true, if key == "two" { Err("failure") } else { Ok(()) });
            async move { result }
        }));
        assert_eq!(calls, 3);
        assert_eq!(
            outcome,
            ArchiveOutcome {
                archived_thread_keys: entries(),
                mutation_failure: None,
                followup_failures: vec!["failure"]
            }
        );
    }

    // describe("buildBulkUnpinContextMenuItem")
    #[test]
    fn bulk_unpin_counts_only_pinned_rows() {
        assert_eq!(
            build_bulk_unpin_context_menu_item(2),
            Some(ContextMenuItem::new("unpin", "Unpin (2)"))
        );
        assert_eq!(build_bulk_unpin_context_menu_item(0), None);
    }

    // describe("buildBulkTitleRegenerationContextMenuItem")
    #[test]
    fn bulk_title_regeneration_item() {
        assert_eq!(
            build_bulk_title_regeneration_context_menu_item(4, 3),
            Some(ContextMenuItem::new(
                "regenerate-title",
                "Regenerate titles (3)"
            ))
        );
        assert_eq!(
            build_bulk_title_regeneration_context_menu_item(2, 0),
            Some(ContextMenuItem::new("regenerate-title", "Regenerating… (2)").disabled(true))
        );
        assert_eq!(build_bulk_title_regeneration_context_menu_item(0, 0), None);
    }

    // describe("buildMultiSelectThreadContextMenuItems")
    #[test]
    fn multi_select_archive_item() {
        assert!(
            build_multi_select_thread_context_menu_items(3, false)
                .contains(&ContextMenuItem::new("archive", "Archive (3)"))
        );
        assert!(
            build_multi_select_thread_context_menu_items(2, true)
                .contains(&ContextMenuItem::new("archive", "Archive (2)").disabled(true))
        );
    }

    // describe("sidebar thread lineage helpers")
    #[test]
    fn keeps_only_top_level_unarchived_threads_in_the_sidebar_v2_project_scope() {
        let environment = env("environment-visible");
        let project = project_id("project-visible");
        let make = |id: &str| {
            let mut thread = shell(id);
            thread.environment_id = environment.clone();
            thread.project_id = project.clone();
            thread
        };
        let root = make("thread-parent");
        let mut subagent = make("thread-subagent");
        subagent.lineage = ThreadLineage {
            relationship_to_parent: Some(ThreadRelationship::Subagent),
            parent_thread_id: Some(thread_id("thread-parent")),
        };
        let mut fork = make("thread-fork");
        fork.lineage = ThreadLineage {
            relationship_to_parent: Some(ThreadRelationship::Fork),
            parent_thread_id: Some(thread_id("thread-parent")),
        };
        let mut archived = make("thread-archived");
        archived.archived_at = Some("2026-01-02T00:00:00.000Z".into());
        let mut other = make("thread-other-project");
        other.project_id = project_id("project-other");
        let threads = [root, subagent, fork, archived, other];
        let scope: HashSet<String> = [format!("{environment}:{project}")].into_iter().collect();
        let visible = filter_sidebar_v2_visible_threads(&threads, Some(&scope));
        assert_eq!(
            visible.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["thread-parent", "thread-fork"]
        );
    }

    #[test]
    fn identifies_subagent_threads_so_the_sidebar_can_hide_them() {
        let mut subagent = shell("t");
        subagent.lineage.relationship_to_parent = Some(ThreadRelationship::Subagent);
        assert!(is_sidebar_subagent_thread(&subagent));
        assert!(!is_sidebar_subagent_thread(&shell("t")));
    }

    #[test]
    fn resolves_the_parent_thread_for_fork_sidebar_affordances() {
        let lineage = ThreadLineage {
            relationship_to_parent: Some(ThreadRelationship::Fork),
            parent_thread_id: Some(thread_id("thread-fallback-parent")),
        };
        let mut run_fork = shell("a");
        run_fork.lineage = lineage.clone();
        run_fork.forked_from = Some(ForkedFrom::Run {
            thread_id: thread_id("thread-parent"),
        });
        let mut lineage_fork = shell("b");
        lineage_fork.lineage = lineage;
        assert_eq!(
            get_sidebar_fork_parent_thread_id(&run_fork),
            Some(thread_id("thread-parent"))
        );
        assert_eq!(
            get_sidebar_fork_parent_thread_id(&lineage_fork),
            Some(thread_id("thread-fallback-parent"))
        );
        assert_eq!(get_sidebar_fork_parent_thread_id(&shell("c")), None);
    }

    fn make_latest_run(
        completed_at: Option<Option<&str>>,
        started_at: Option<Option<&str>>,
    ) -> ThreadRunSummary {
        ThreadRunSummary {
            run_id: run_id("turn-1"),
            status: RunStatus::Completed,
            requested_at: Some("2026-03-09T10:00:00.000Z".into()),
            started_at: started_at
                .unwrap_or(Some("2026-03-09T10:00:00.000Z"))
                .map(Into::into),
            completed_at: completed_at
                .unwrap_or(Some("2026-03-09T10:05:00.000Z"))
                .map(Into::into),
        }
    }

    // describe("hasUnseenCompletion")
    #[test]
    fn unseen_completion() {
        let run = make_latest_run(None, None);
        assert!(has_unseen_completion(
            Some(&run),
            Some("2026-03-09T10:04:00.000Z")
        ));
        assert!(!has_unseen_completion(Some(&run), None));
    }

    // describe("shouldRecedeSidebarThread")
    #[test]
    fn recedes_inactive_working_and_waiting_threads() {
        for status in [SidebarThreadStatus::Working, SidebarThreadStatus::Waiting] {
            assert!(should_recede_sidebar_thread(
                status, true, true, false, false
            ));
        }
        for status in [
            SidebarThreadStatus::Ready,
            SidebarThreadStatus::Approval,
            SidebarThreadStatus::Input,
        ] {
            assert!(!should_recede_sidebar_thread(
                status, true, false, false, false
            ));
        }
        assert!(!should_recede_sidebar_thread(
            SidebarThreadStatus::Working,
            true,
            true,
            true,
            false
        ));
        assert!(!should_recede_sidebar_thread(
            SidebarThreadStatus::Working,
            true,
            true,
            false,
            true
        ));
        for unread in [false, true] {
            assert!(!should_recede_sidebar_thread(
                SidebarThreadStatus::Input,
                unread,
                false,
                false,
                false
            ));
        }
    }

    // describe("createThreadJumpHintVisibilityController")
    #[test]
    fn delays_showing_jump_hints_until_the_configured_delay_elapses() {
        let mut controller = ThreadJumpHintVisibilityController::default();
        assert_eq!(controller.sync(true), JumpHintAction::Schedule);
        assert!(!controller.is_visible());
        assert_eq!(controller.fire(), JumpHintAction::Changed(true));
    }

    #[test]
    fn hides_immediately_when_the_modifiers_are_released() {
        let mut controller = ThreadJumpHintVisibilityController::default();
        controller.sync(true);
        controller.fire();
        assert_eq!(controller.sync(false), JumpHintAction::Changed(false));
    }

    #[test]
    fn cancels_a_pending_reveal_when_the_modifier_is_released_early() {
        let mut controller = ThreadJumpHintVisibilityController::default();
        controller.sync(true);
        assert_eq!(controller.sync(false), JumpHintAction::None);
        assert_eq!(controller.fire(), JumpHintAction::None);
        assert!(!controller.is_visible());
    }

    // describe("getSidebarThreadIdsToPrewarm")
    #[test]
    fn prewarm_limits() {
        assert_eq!(
            get_sidebar_thread_ids_to_prewarm(&["t1", "t2", "t3"], 2),
            vec!["t1", "t2"]
        );
        assert_eq!(
            get_sidebar_thread_ids_to_prewarm(&["t1", "t2"], 10),
            vec!["t1", "t2"]
        );
        assert!(get_sidebar_thread_ids_to_prewarm(&["t1", "t2"], 0).is_empty());
    }

    // describe("isTrailingDoubleClick")
    #[test]
    fn trailing_double_click() {
        assert!(!is_trailing_double_click(1));
        assert!(!is_trailing_double_click(0));
        assert!(is_trailing_double_click(2));
        assert!(is_trailing_double_click(3));
    }

    // describe("orderItemsByPreferredIds")
    #[test]
    fn keeps_preferred_ids_first_skips_stale_ids_and_preserves_remaining_order() {
        let items = strings(&["project-1", "project-2", "project-3"]);
        let ordered = order_items_by_preferred_ids(
            &items,
            &strings(&["project-3", "project-missing", "project-1"]),
            |item| item.clone(),
            None,
        );
        assert_eq!(ordered, strings(&["project-3", "project-1", "project-2"]));
    }

    #[test]
    fn does_not_duplicate_items_when_preferred_ids_repeat() {
        let items = strings(&["project-1", "project-2"]);
        let ordered = order_items_by_preferred_ids(
            &items,
            &strings(&["project-2", "project-1", "project-2"]),
            |item| item.clone(),
            None,
        );
        assert_eq!(ordered, strings(&["project-2", "project-1"]));
    }

    #[test]
    fn resolves_legacy_preference_aliases_without_materializing_project_state() {
        let items = vec![
            ("physical-a".to_string(), "/work/a"),
            ("physical-b".to_string(), "/work/b"),
            ("physical-c".to_string(), "/work/c"),
        ];
        let preference = |item: &(String, &str)| vec![item.0.clone(), format!("legacy:{}", item.1)];
        let ordered = order_items_by_preferred_ids(
            &items,
            &strings(&["legacy:/work/c", "legacy:/work/a"]),
            |item| item.0.clone(),
            Some(&preference),
        );
        assert_eq!(
            ordered
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec!["physical-c", "physical-a", "physical-b"]
        );
    }

    // describe("resolveAdjacentThreadId")
    #[test]
    fn resolves_adjacent_thread_ids_in_ordered_sidebar_traversal() {
        let threads = strings(&["thread-1", "thread-2", "thread-3"]);
        use ThreadTraversalDirection::*;
        assert_eq!(
            resolve_adjacent_thread_id(&threads, Some(&threads[1]), Previous),
            Some(threads[0].clone())
        );
        assert_eq!(
            resolve_adjacent_thread_id(&threads, Some(&threads[1]), Next),
            Some(threads[2].clone())
        );
        assert_eq!(
            resolve_adjacent_thread_id(&threads, None, Next),
            Some(threads[0].clone())
        );
        assert_eq!(
            resolve_adjacent_thread_id(&threads, None, Previous),
            Some(threads[2].clone())
        );
        assert_eq!(
            resolve_adjacent_thread_id(&threads, Some(&threads[0]), Previous),
            None
        );
    }

    // describe("isContextMenuPointerDown")
    #[test]
    fn context_menu_pointer_down() {
        assert!(is_context_menu_pointer_down(2, false, false));
        assert!(is_context_menu_pointer_down(0, true, true));
        assert!(!is_context_menu_pointer_down(0, true, false));
    }

    // describe("resolveSidebarThreadStatus")
    fn status_of(
        approvals: bool,
        input: bool,
        rt: Option<ThreadRuntimeSummary>,
    ) -> SidebarThreadStatus {
        resolve_sidebar_thread_status_parts(approvals, input, None, rt.as_ref())
    }

    #[test]
    fn prioritizes_approval_and_input_over_a_running_runtime() {
        let running = Some(runtime(RunStatus::Running));
        assert_eq!(
            status_of(true, false, running.clone()),
            SidebarThreadStatus::Approval
        );
        assert_eq!(
            status_of(false, true, running.clone()),
            SidebarThreadStatus::Input
        );
        assert_eq!(
            status_of(true, true, running),
            SidebarThreadStatus::Approval
        );
    }

    #[test]
    fn reports_working_for_running_and_starting_runtimes() {
        assert_eq!(
            status_of(false, false, Some(runtime(RunStatus::Running))),
            SidebarThreadStatus::Working
        );
        assert_eq!(
            status_of(false, false, Some(runtime(RunStatus::Starting))),
            SidebarThreadStatus::Working
        );
    }

    #[test]
    fn keeps_usage_limit_stops_limited_and_visible_until_the_thread_recovers() {
        let mut limited = runtime(RunStatus::Failed);
        limited.last_error = Some("Plan limit reached".into());
        limited.last_error_class = Some(ProviderFailureClass::UsageLimit);
        assert_eq!(
            status_of(false, false, Some(limited.clone())),
            SidebarThreadStatus::Limited
        );
        assert_eq!(
            status_of(
                false,
                false,
                Some(ThreadRuntimeSummary {
                    status: RunStatus::Running,
                    ..limited.clone()
                })
            ),
            SidebarThreadStatus::Working
        );
        assert_eq!(
            status_of(
                false,
                false,
                Some(ThreadRuntimeSummary {
                    status: RunStatus::Completed,
                    ..limited
                })
            ),
            SidebarThreadStatus::Ready
        );
        assert_eq!(
            resolve_sidebar_v2_top_status(SidebarThreadStatus::Limited, false, false),
            Some(SidebarV2TopStatusKind::Limited)
        );
        assert!(!should_recede_sidebar_thread(
            SidebarThreadStatus::Limited,
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn reports_failed_only_while_the_latest_run_failed() {
        assert_eq!(
            status_of(false, false, Some(runtime(RunStatus::Failed))),
            SidebarThreadStatus::Failed
        );
        assert_eq!(
            status_of(false, false, Some(runtime(RunStatus::Completed))),
            SidebarThreadStatus::Ready
        );
        assert_eq!(
            status_of(false, false, Some(runtime(RunStatus::Idle))),
            SidebarThreadStatus::Waiting
        );
        assert_eq!(status_of(false, false, None), SidebarThreadStatus::Ready);
    }

    #[test]
    fn keeps_waiting_ahead_of_unread_and_static() {
        assert_eq!(
            resolve_sidebar_v2_top_status(SidebarThreadStatus::Waiting, true, true),
            Some(SidebarV2TopStatusKind::Waiting)
        );
        assert!(!should_show_sidebar_v2_duration(
            SidebarThreadStatus::Waiting
        ));
        assert!(should_show_sidebar_v2_duration(
            SidebarThreadStatus::Working
        ));
    }

    // describe("pullRequestMatchesSearchQuery")
    #[test]
    fn pull_request_query_tokens() {
        let terms = strings(&[
            "#123",
            "pingdotgg/t3code#123",
            "https://github.com/pingdotgg/t3code/pull/123",
        ]);
        assert!(pull_request_matches_search_query(&terms, "123"));
        assert!(pull_request_matches_search_query(
            &terms,
            "pingdotgg/t3code #123"
        ));
        assert!(!pull_request_matches_search_query(&terms, "rebase 123"));
        assert!(!pull_request_matches_search_query(&terms, "   "));
    }

    // describe("searchSidebarThreads")
    fn search_threads() -> Vec<EnvironmentThreadShell> {
        [
            ("thread-1", "Fix workspace search"),
            ("thread-2", "Review providers"),
            ("thread-3", "WORKTREE cleanup"),
        ]
        .iter()
        .map(|(id, title)| {
            let mut thread = shell(id);
            thread.title = title.to_string();
            thread
        })
        .collect()
    }

    fn content_keys(ids: &[&str]) -> HashSet<String> {
        ids.iter()
            .map(|id| thread_search_match_key("environment-local", id))
            .collect()
    }

    fn search_ids(
        threads: &[EnvironmentThreadShell],
        query: &str,
        keys: &HashSet<String>,
    ) -> Vec<String> {
        let refs: Vec<&EnvironmentThreadShell> = threads.iter().collect();
        search_sidebar_threads(&refs, query, keys)
            .iter()
            .map(|t| t.id.to_string())
            .collect()
    }

    #[test]
    fn search_matches_titles_and_appends_content_matches() {
        let threads = search_threads();
        let none = HashSet::new();
        assert_eq!(
            search_ids(&threads, "work", &none),
            strings(&["thread-1", "thread-3"])
        );
        assert_eq!(
            search_ids(&threads, "workspace", &none),
            strings(&["thread-1"])
        );
        assert!(search_ids(&threads, "   ", &none).is_empty());
        assert_eq!(
            search_ids(&threads, "work", &content_keys(&["thread-2"])),
            strings(&["thread-1", "thread-3", "thread-2"])
        );
        assert_eq!(
            search_ids(&threads, "work", &content_keys(&["thread-1"])),
            strings(&["thread-1", "thread-3"])
        );
        assert_eq!(
            search_ids(&threads, "work", &content_keys(&["thread-missing"])),
            strings(&["thread-1", "thread-3"])
        );
    }

    // describe("filterSidebarProjectScopeItems")
    #[test]
    fn project_scope_filtering() {
        let items = vec![
            ProjectScopeItem {
                value: "all".into(),
                label: "All projects".into(),
            },
            ProjectScopeItem {
                value: "alpha".into(),
                label: "Alpha workspace".into(),
            },
            ProjectScopeItem {
                value: "beta".into(),
                label: "Beta tools".into(),
            },
        ];
        let filter = |query: &str| {
            filter_sidebar_project_scope_items(&items, query, |item, candidate| {
                item.label
                    .to_lowercase()
                    .contains(&candidate.to_lowercase())
            })
        };
        assert_eq!(filter(""), items);
        assert_eq!(filter("   "), items);
        assert!(filter("all").is_empty());
        assert_eq!(filter("WORK"), vec![items[1].clone()]);
        assert!(filter("missing").is_empty());
    }

    // describe("reduceSidebarProjectScopeMenuState")
    #[test]
    fn project_scope_menu_reducer() {
        let queried = SidebarProjectScopeMenuState {
            open: true,
            query: "alpha".into(),
        };
        assert_eq!(
            reduce_sidebar_project_scope_menu_state(
                &queried,
                SidebarProjectScopeMenuAction::OpenChanged(false)
            ),
            SidebarProjectScopeMenuState::default()
        );
        assert_eq!(
            reduce_sidebar_project_scope_menu_state(
                &queried,
                SidebarProjectScopeMenuAction::ProjectSettingsOpened
            ),
            SidebarProjectScopeMenuState::default()
        );
        assert_eq!(
            reduce_sidebar_project_scope_menu_state(
                &SidebarProjectScopeMenuState {
                    open: true,
                    query: String::new()
                },
                SidebarProjectScopeMenuAction::QueryChanged("beta".into())
            ),
            SidebarProjectScopeMenuState {
                open: true,
                query: "beta".into()
            }
        );
    }

    // describe("resolveWorkingStartedAt")
    fn working_thread(run: Option<ThreadRunSummary>) -> EnvironmentThreadShell {
        let mut thread = shell("t");
        let mut rt = runtime(RunStatus::Running);
        rt.active_run_id = Some(run_id("turn-1"));
        rt.updated_at = "2026-03-09T10:02:00.000Z".into();
        thread.runtime = Some(rt);
        thread.latest_run = run;
        thread
    }

    #[test]
    fn working_started_at_cases() {
        assert_eq!(
            resolve_working_started_at(&working_thread(Some(make_latest_run(Some(None), None))))
                .as_deref(),
            Some("2026-03-09T10:00:00.000Z")
        );
        assert_eq!(
            resolve_working_started_at(&working_thread(Some(make_latest_run(
                Some(None),
                Some(None)
            ))))
            .as_deref(),
            Some("2026-03-09T10:00:00.000Z")
        );
        assert_eq!(
            resolve_working_started_at(&working_thread(Some(make_latest_run(None, None)))),
            None
        );
        assert_eq!(
            resolve_working_started_at(&working_thread(Some(make_latest_run(
                Some(None),
                Some(Some("not-a-date"))
            ))))
            .as_deref(),
            Some("2026-03-09T10:00:00.000Z")
        );
        let mut neither = shell("t");
        neither.runtime = None;
        assert_eq!(resolve_working_started_at(&neither), None);
    }

    #[test]
    fn shares_the_detail_timer_when_a_newer_run_is_queued_or_cancelled() {
        for (status, completed) in [
            (RunStatus::Queued, None),
            (RunStatus::Cancelled, Some("2026-03-09T10:05:00.000Z")),
        ] {
            let mut thread = working_thread(Some(ThreadRunSummary {
                run_id: run_id("newer-run"),
                status,
                requested_at: Some("2026-03-09T10:00:00.000Z".into()),
                started_at: None,
                completed_at: completed.map(Into::into),
            }));
            thread.runtime.as_mut().unwrap().activity_started_at =
                Some(Some("2026-03-09T10:00:00.000Z".into()));
            assert_eq!(
                resolve_working_started_at(&thread).as_deref(),
                Some("2026-03-09T10:00:00.000Z")
            );
        }
    }

    // describe("formatWorkingDurationLabel")
    #[test]
    fn working_duration_label() {
        assert_eq!(format_working_duration_label(0.), "0s");
        assert_eq!(format_working_duration_label(42_000.), "42s");
        assert_eq!(format_working_duration_label(5. * 60_000.), "5m");
        assert_eq!(format_working_duration_label(90. * 60_000.), "1h 30m");
        assert_eq!(format_working_duration_label(-5_000.), "0s");
        assert_eq!(format_working_duration_label(f64::NAN), "0s");
    }

    // describe("resolveThreadStatusPill")
    fn pill_base() -> EnvironmentThreadShell {
        let mut thread = shell("t");
        thread.interaction_mode = InteractionMode::Plan;
        let mut rt = runtime(RunStatus::Running);
        rt.active_run_id = Some(run_id("turn-1"));
        thread.runtime = Some(rt);
        thread
    }

    fn pill_label(
        thread: &EnvironmentThreadShell,
        visited: Option<&str>,
    ) -> Option<(ThreadStatusPillLabel, bool)> {
        resolve_thread_status_pill(thread, visited).map(|pill| (pill.label, pill.pulse))
    }

    #[test]
    fn status_pill_cases() {
        let mut approval = pill_base();
        approval.has_pending_approvals = true;
        approval.has_pending_user_input = true;
        assert_eq!(
            pill_label(&approval, None),
            Some((ThreadStatusPillLabel::PendingApproval, false))
        );

        let mut input = pill_base();
        input.has_pending_user_input = true;
        assert_eq!(
            pill_label(&input, None),
            Some((ThreadStatusPillLabel::AwaitingInput, false))
        );

        assert_eq!(
            pill_label(&pill_base(), None),
            Some((ThreadStatusPillLabel::Working, true))
        );

        let monitor = PendingBackgroundTask {
            task_id: "bg-1".into(),
            description: Some("Watch build".into()),
            kind: BackgroundTaskKind::Monitor,
        };
        let mut waiting = pill_base();
        waiting.pending_background_tasks = vec![monitor];
        let rt = waiting.runtime.as_mut().unwrap();
        rt.status = RunStatus::Idle;
        rt.active_run_id = None;
        let pill = resolve_thread_status_pill(&waiting, None).unwrap();
        assert_eq!(pill.label, ThreadStatusPillLabel::Waiting);
        assert_eq!(pill.color_class, "text-sidebar-muted-foreground");
        assert_eq!(pill.dot_class, "bg-sidebar-muted-foreground");
        assert!(!pill.pulse);

        let mut busy = pill_base();
        busy.pending_background_tasks = vec![PendingBackgroundTask {
            task_id: "bg-1".into(),
            description: Some("sleep 20".into()),
            kind: BackgroundTaskKind::Command,
        }];
        assert_eq!(
            pill_label(&busy, None),
            Some((ThreadStatusPillLabel::Working, true))
        );

        let mut cleared = pill_base();
        let rt = cleared.runtime.as_mut().unwrap();
        rt.status = RunStatus::Idle;
        rt.active_run_id = None;
        assert_eq!(pill_label(&cleared, None), None);

        let mut plan = pill_base();
        plan.has_actionable_proposed_plan = true;
        plan.latest_run = Some(make_latest_run(None, None));
        let rt = plan.runtime.as_mut().unwrap();
        rt.status = RunStatus::Completed;
        rt.active_run_id = None;
        assert_eq!(
            pill_label(&plan, None),
            Some((ThreadStatusPillLabel::PlanReady, false))
        );

        let mut completed = pill_base();
        completed.latest_run = Some(make_latest_run(None, None));
        let rt = completed.runtime.as_mut().unwrap();
        rt.status = RunStatus::Completed;
        rt.active_run_id = None;
        assert_eq!(pill_label(&completed, None), None);
        completed.interaction_mode = InteractionMode::Default;
        assert_eq!(
            pill_label(&completed, Some("2026-03-09T10:04:00.000Z")),
            Some((ThreadStatusPillLabel::Completed, false))
        );
    }

    // describe("resolveProjectStatusIndicator")
    fn pill(label: ThreadStatusPillLabel) -> Option<ThreadStatusPill> {
        Some(ThreadStatusPill {
            label,
            color_class: "",
            dot_class: "",
            pulse: false,
        })
    }

    #[test]
    fn project_status_indicator_priority() {
        use ThreadStatusPillLabel::*;
        assert_eq!(resolve_project_status_indicator(&[None, None]), None);
        assert_eq!(
            resolve_project_status_indicator(&[
                pill(Completed),
                pill(PendingApproval),
                pill(Working)
            ])
            .unwrap()
            .label,
            PendingApproval
        );
        assert_eq!(
            resolve_project_status_indicator(&[pill(Completed), pill(PlanReady)])
                .unwrap()
                .label,
            PlanReady
        );
        assert_eq!(
            resolve_project_status_indicator(&[pill(Waiting), pill(Working)])
                .unwrap()
                .label,
            Working
        );
        assert_eq!(
            resolve_project_status_indicator(&[pill(PlanReady), pill(Waiting)])
                .unwrap()
                .label,
            Waiting
        );
    }

    // describe("getFallbackThreadIdAfterDelete")
    fn project_thread(id: &str, project: &str, created_at: &str) -> EnvironmentThreadShell {
        let mut thread = shell(id);
        thread.project_id = project_id(project);
        thread.created_at = created_at.into();
        thread
    }

    #[test]
    fn fallback_thread_after_delete() {
        let threads = vec![
            project_thread("thread-oldest", "project-1", "2026-03-09T10:00:00.000Z"),
            project_thread("thread-active", "project-1", "2026-03-09T10:05:00.000Z"),
            project_thread("thread-newest", "project-1", "2026-03-09T10:10:00.000Z"),
            project_thread(
                "thread-other-project",
                "project-2",
                "2026-03-09T10:20:00.000Z",
            ),
        ];
        assert_eq!(
            get_fallback_thread_id_after_delete(
                &threads,
                &thread_id("thread-active"),
                ThreadSortOrder::CreatedAt,
                None
            ),
            Some(thread_id("thread-newest"))
        );
        let threads = vec![
            project_thread("thread-active", "project-1", "2026-03-09T10:05:00.000Z"),
            project_thread("thread-newest", "project-1", "2026-03-09T10:10:00.000Z"),
            project_thread("thread-next", "project-1", "2026-03-09T10:07:00.000Z"),
        ];
        let deleted: HashSet<ThreadId> = [thread_id("thread-active"), thread_id("thread-newest")]
            .into_iter()
            .collect();
        assert_eq!(
            get_fallback_thread_id_after_delete(
                &threads,
                &thread_id("thread-active"),
                ThreadSortOrder::CreatedAt,
                Some(&deleted)
            ),
            Some(thread_id("thread-next"))
        );
    }

    // describe("sortLogicalProjectsForSidebar") / ("sortSidebarV2ProjectGroups")
    fn logical(key: &str, project: &str, title: &str) -> LogicalSidebarProject {
        LogicalSidebarProject {
            project_key: key.into(),
            title: title.into(),
            created_at: Some("2026-03-09T10:00:00.000Z".into()),
            updated_at: Some("2026-03-09T10:00:00.000Z".into()),
            member_project_keys: vec![format!("environment-local:{project}")],
        }
    }

    fn updated_thread(id: &str, project: &str, updated_at: &str) -> EnvironmentThreadShell {
        let mut thread = shell(id);
        thread.project_id = project_id(project);
        thread.updated_at = updated_at.into();
        thread
    }

    #[test]
    fn uses_saved_order_only_in_manual_mode_and_activity_order_otherwise() {
        let projects = vec![
            logical("logical-older", "project-older", "Older project"),
            logical("logical-newer", "project-newer", "Newer project"),
        ];
        let threads = [
            updated_thread("thread-1", "project-older", "2026-03-09T10:01:00.000Z"),
            updated_thread("thread-newer", "project-newer", "2026-03-09T10:05:00.000Z"),
        ];
        let refs: Vec<&EnvironmentThreadShell> = threads.iter().collect();
        assert_eq!(
            sort_logical_projects_for_sidebar(&projects, &refs, ProjectSortOrder::Manual),
            projects
        );
        assert_eq!(
            sort_logical_projects_for_sidebar(&projects, &refs, ProjectSortOrder::UpdatedAt)
                .iter()
                .map(|p| p.project_key.as_str())
                .collect::<Vec<_>>(),
            vec!["logical-newer", "logical-older"]
        );
    }

    #[test]
    fn does_not_let_a_hidden_subagent_thread_reorder_projects() {
        let projects = vec![
            logical("logical-older", "project-older", "A older project"),
            logical("logical-newer", "project-newer", "Z newer project"),
        ];
        let mut hidden = updated_thread(
            "thread-hidden-subagent",
            "project-older",
            "2026-03-09T10:10:00.000Z",
        );
        hidden.lineage.relationship_to_parent = Some(ThreadRelationship::Subagent);
        let threads = vec![
            updated_thread(
                "thread-older-root",
                "project-older",
                "2026-03-09T10:01:00.000Z",
            ),
            updated_thread(
                "thread-newer-root",
                "project-newer",
                "2026-03-09T10:05:00.000Z",
            ),
            hidden,
        ];
        assert_eq!(
            sort_sidebar_v2_project_groups(&projects, &threads, ProjectSortOrder::UpdatedAt)
                .iter()
                .map(|p| p.project_key.as_str())
                .collect::<Vec<_>>(),
            vec!["logical-newer", "logical-older"]
        );
    }

    #[test]
    fn project_timestamp_falls_back_to_the_project_when_it_has_no_threads() {
        let mut project = logical("p", "p", "P");
        project.updated_at = Some("2026-03-09T10:10:00.000Z".into());
        assert_eq!(
            get_project_sort_timestamp::<_, EnvironmentThreadShell>(
                &project,
                &[],
                ThreadSortOrder::UpdatedAt
            ),
            parse_timestamp_ms("2026-03-09T10:10:00.000Z").unwrap() as f64
        );
    }

    #[test]
    fn falls_back_to_name_and_id_ordering_when_projects_have_no_sortable_timestamps() {
        let mut beta = logical("project-2", "project-2", "Beta");
        beta.created_at = Some("invalid".into());
        beta.updated_at = Some("invalid".into());
        let mut alpha = logical("project-1", "project-1", "Alpha");
        alpha.created_at = Some("invalid".into());
        alpha.updated_at = Some("invalid".into());
        assert_eq!(
            sort_logical_projects_for_sidebar(&[beta, alpha], &[], ProjectSortOrder::UpdatedAt)
                .iter()
                .map(|p| p.project_key.as_str())
                .collect::<Vec<_>>(),
            vec!["project-1", "project-2"]
        );
    }

    // describe("resolveThreadLastVisitedAt")
    #[test]
    fn last_visited_watermark() {
        assert_eq!(
            resolve_thread_last_visited_at(None, Some("2026-07-30T10:00:00.000Z")).as_deref(),
            Some("2026-07-30T10:00:00.000Z")
        );
        assert_eq!(resolve_thread_last_visited_at(None, None), None);
        assert_eq!(
            resolve_thread_last_visited_at(
                Some(Some("2026-07-30T10:00:00.000Z")),
                Some("2026-07-30T10:00:05.000Z")
            )
            .as_deref(),
            Some("2026-07-30T10:00:00.000Z")
        );
        assert_eq!(
            resolve_thread_last_visited_at(Some(None), Some("2026-07-30T10:00:00.000Z")),
            None
        );
    }

    // describe("pinOrderKeyBetween")
    #[test]
    fn pin_order_keys() {
        let middle = pin_order_key_between(None, None).unwrap();
        let top = pin_order_key_between(None, Some(&middle)).unwrap();
        let bottom = pin_order_key_between(Some(&middle), None).unwrap();
        assert!(top < middle && middle < bottom);
        let between = pin_order_key_between(Some(&top), Some(&middle)).unwrap();
        assert!(top < between && between < middle);
        let key = pin_order_key_between(Some("g"), Some("h")).unwrap();
        assert!("g" < key.as_str() && key.as_str() < "h");

        let mut head: Option<String> = None;
        let mut keys = HashSet::new();
        for _ in 0..100 {
            let key = pin_order_key_between(None, head.as_deref()).unwrap();
            if let Some(head) = &head {
                assert!(&key < head);
            }
            keys.insert(key.clone());
            head = Some(key);
        }
        assert_eq!(keys.len(), 100);

        let mut low = pin_order_key_between(None, None).unwrap();
        let mut high = pin_order_key_between(Some(&low), None).unwrap();
        for i in 0..100 {
            let key = pin_order_key_between(Some(&low), Some(&high)).unwrap();
            assert!(low < key && key < high);
            if i % 2 == 0 {
                low = key;
            } else {
                high = key;
            }
        }

        assert_eq!(pin_order_key_between(Some("z"), Some("a")), None);
        assert_eq!(pin_order_key_between(Some("A!"), None), None);
        assert_eq!(pin_order_key_between(None, Some("ma")), None);
        assert_eq!(pin_order_key_between(Some("m"), Some("m")), None);
    }

    // describe("sortPinnedThreadsForSidebar")
    #[test]
    fn pinned_sort() {
        let pinnable = |id: &str, created_at: &str, key: Option<&str>| {
            let mut thread = shell(id);
            thread.created_at = created_at.into();
            thread.pin_order_key = key.map(Into::into);
            thread
        };
        let sorted = sort_pinned_threads_for_sidebar(vec![
            pinnable("keyless-old", "2026-03-09T08:00:00.000Z", None),
            pinnable("second", "2026-03-09T09:00:00.000Z", Some("t")),
            pinnable("keyless-new", "2026-03-09T12:00:00.000Z", None),
            pinnable("first", "2026-03-09T07:00:00.000Z", Some("g")),
        ]);
        assert_eq!(
            sorted.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second", "keyless-new", "keyless-old"]
        );
        let sorted = sort_pinned_threads_for_sidebar(vec![
            pinnable("b", "2026-03-09T10:00:00.000Z", Some("m")),
            pinnable("a", "2026-03-09T11:00:00.000Z", Some("m")),
        ]);
        assert_eq!(
            sorted.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    // describe("resolveSidebarSweepKeys")
    #[test]
    fn sweep_keys() {
        let ordered = strings(&["a", "b", "c", "d", "blocked"]);
        let can = |key: &str| key != "blocked";
        assert_eq!(
            resolve_sidebar_sweep_keys(&ordered, "b", "b", can),
            strings(&["b"])
        );
        assert_eq!(
            resolve_sidebar_sweep_keys(&ordered, "b", "d", can),
            strings(&["b", "c", "d"])
        );
        assert_eq!(
            resolve_sidebar_sweep_keys(&ordered, "d", "a", can),
            strings(&["a", "b", "c", "d"])
        );
        assert_eq!(
            resolve_sidebar_sweep_keys(&ordered, "c", "blocked", can),
            strings(&["c", "d"])
        );
        assert!(resolve_sidebar_sweep_keys(&ordered, "gone", "a", can).is_empty());
    }

    // describe("navigation after parking a thread")
    #[test]
    fn navigation_after_parking_a_thread() {
        let now = "2026-09-12T10:00:00.000Z";
        // (action, settled override, snoozed until, current route, expected, pending approvals)
        type Case<'a> = (
            ParkAction,
            Option<SettledOverride>,
            Option<&'a str>,
            &'a str,
            bool,
            bool,
        );
        let cases: [Case<'_>; 8] = [
            (
                ParkAction::Settle,
                Some(SettledOverride::Settled),
                None,
                "thread",
                true,
                false,
            ),
            (
                ParkAction::Settle,
                Some(SettledOverride::Active),
                None,
                "thread",
                false,
                false,
            ),
            (
                ParkAction::Settle,
                Some(SettledOverride::Settled),
                None,
                "other-thread",
                false,
                false,
            ),
            (
                ParkAction::Snooze,
                None,
                Some("2099-01-01T00:00:00.000Z"),
                "thread",
                true,
                false,
            ),
            (ParkAction::Snooze, None, None, "thread", false, false),
            (
                ParkAction::Snooze,
                None,
                Some("2026-09-12T09:00:00.000Z"),
                "thread",
                false,
                false,
            ),
            (
                ParkAction::Snooze,
                None,
                Some("2099-01-01T00:00:00.000Z"),
                "thread",
                false,
                true,
            ),
            (
                ParkAction::Snooze,
                None,
                Some("2099-01-01T00:00:00.000Z"),
                "other-thread",
                false,
                false,
            ),
        ];
        for (action, settled, snoozed_until, current, expected, approvals) in cases {
            let mut thread = shell("thread");
            thread.settled_override = settled;
            thread.snoozed_until = snoozed_until.map(Into::into);
            thread.has_pending_approvals = approvals;
            assert_eq!(
                should_navigate_after_thread_park(
                    "thread",
                    Some(current),
                    action,
                    now,
                    Some(&thread)
                ),
                expected
            );
        }
    }

    // describe("unseen completion with background work")
    #[test]
    fn presents_a_completed_thread_with_a_background_roster() {
        for (kind, status, top, receded, pill_label) in [
            (
                BackgroundTaskKind::Command,
                SidebarThreadStatus::Ready,
                SidebarV2TopStatusKind::Done,
                false,
                ThreadStatusPillLabel::Completed,
            ),
            (
                BackgroundTaskKind::Monitor,
                SidebarThreadStatus::Waiting,
                SidebarV2TopStatusKind::Waiting,
                true,
                ThreadStatusPillLabel::Waiting,
            ),
        ] {
            // `presentThreadShell` of a completed run with this roster.
            let mut thread = shell("t");
            thread.latest_run = Some(ThreadRunSummary {
                run_id: run_id("run-background-completion"),
                status: RunStatus::Completed,
                requested_at: None,
                started_at: None,
                completed_at: Some("2026-06-20T01:00:00.000Z".into()),
            });
            thread.pending_background_tasks = vec![PendingBackgroundTask {
                task_id: "background-work".into(),
                description: None,
                kind,
            }];
            let parks = background_work_holds_completion(&thread.pending_background_tasks);
            let mut rt = runtime(if parks {
                RunStatus::Idle
            } else {
                RunStatus::Completed
            });
            rt.active_run_id = None;
            thread.runtime = Some(rt);
            let visited = Some("2026-06-20T00:59:00.000Z");
            let actual = resolve_sidebar_thread_status(&thread);
            let unread = has_unseen_completion(thread.latest_run.as_ref(), visited);
            assert!(unread);
            assert_eq!(actual, status);
            assert_eq!(
                resolve_sidebar_v2_top_status(actual, unread, false),
                Some(top)
            );
            assert_eq!(
                should_recede_sidebar_thread(actual, unread, false, false, false),
                receded
            );
            assert_eq!(is_sidebar_thread_working(&thread), receded);
            assert_eq!(
                resolve_thread_status_pill(&thread, visited).unwrap().label,
                pill_label
            );
        }
    }

    // describe("Working shelf (beta)")
    fn idle_thread() -> EnvironmentThreadShell {
        let mut thread = shell("t");
        thread.latest_run = Some(make_latest_run(None, None));
        thread
    }

    fn waiting_thread() -> EnvironmentThreadShell {
        let mut thread = idle_thread();
        thread.runtime = Some(runtime(RunStatus::Idle));
        thread.pending_background_tasks = vec![PendingBackgroundTask {
            task_id: "bg-1".into(),
            description: Some("Watch build".into()),
            kind: BackgroundTaskKind::Monitor,
        }];
        thread
    }

    #[test]
    fn folds_away_running_threads_and_threads_waiting_on_background_work_only() {
        let mut running = idle_thread();
        running.runtime = Some(runtime(RunStatus::Running));
        assert!(is_sidebar_thread_working(&running));
        assert!(is_sidebar_thread_working(&waiting_thread()));
        assert!(!is_sidebar_thread_working(&idle_thread()));
        let mut approval = running.clone();
        approval.has_pending_approvals = true;
        assert!(!is_sidebar_thread_working(&approval));
        let mut input = running.clone();
        input.has_pending_user_input = true;
        assert!(!is_sidebar_thread_working(&input));
        let mut failed = waiting_thread();
        failed.runtime = Some(runtime(RunStatus::Failed));
        assert!(!is_sidebar_thread_working(&failed));
    }

    #[test]
    fn keeps_a_ready_plan_in_the_inbox_while_background_work_runs() {
        let mut plan = waiting_thread();
        plan.interaction_mode = InteractionMode::Plan;
        plan.has_actionable_proposed_plan = true;
        assert!(!is_sidebar_thread_working(&plan));
    }

    fn inbox_thread(
        id: &str,
        created_at: &str,
        completed_at: Option<Option<&str>>,
        unsettled_at: Option<&str>,
    ) -> EnvironmentThreadShell {
        let mut thread = shell(id);
        thread.created_at = created_at.into();
        thread.unsettled_at = unsettled_at.map(Into::into);
        thread.latest_run = completed_at.map(|completed| ThreadRunSummary {
            requested_at: Some(created_at.into()),
            ..make_latest_run(Some(completed), None)
        });
        thread
    }

    #[test]
    fn puts_the_thread_that_finished_last_on_top_whatever_its_age() {
        let sorted = sort_inbox_threads_by_return(
            vec![
                inbox_thread("new", "2026-03-09T11:00:00.000Z", None, None),
                inbox_thread(
                    "old-finished-now",
                    "2026-03-01T09:00:00.000Z",
                    Some(Some("2026-03-09T12:00:00.000Z")),
                    None,
                ),
                inbox_thread(
                    "reopened",
                    "2026-03-02T09:00:00.000Z",
                    None,
                    Some("2026-03-09T11:30:00.000Z"),
                ),
            ],
            |_| None,
        );
        assert_eq!(
            sorted.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["old-finished-now", "reopened", "new"]
        );
    }

    #[test]
    fn counts_a_return_the_server_does_not_stamp_like_an_approval_request() {
        let waiting = inbox_thread(
            "asks-approval",
            "2026-03-09T09:00:00.000Z",
            Some(None),
            None,
        );
        let finished = inbox_thread(
            "finished",
            "2026-03-09T09:30:00.000Z",
            Some(Some("2026-03-09T11:00:00.000Z")),
            None,
        );
        let ids = |threads: Vec<EnvironmentThreadShell>| {
            threads.iter().map(|t| t.id.to_string()).collect::<Vec<_>>()
        };
        assert_eq!(
            ids(sort_inbox_threads_by_return(
                vec![finished.clone(), waiting.clone()],
                |_| None
            )),
            strings(&["finished", "asks-approval"])
        );
        let returned = parse_timestamp_ms("2026-03-09T11:05:00.000Z");
        assert_eq!(
            ids(sort_inbox_threads_by_return(
                vec![finished, waiting],
                |thread| {
                    (thread.id.as_str() == "asks-approval")
                        .then_some(returned)
                        .flatten()
                }
            )),
            strings(&["asks-approval", "finished"])
        );
    }

    fn dragging_items() -> Vec<SidebarListItem> {
        use SidebarListMarker::*;
        vec![
            SidebarListItem::Marker(PinnedHeader),
            SidebarListItem::thread("p1", SidebarSection::Pinned),
            SidebarListItem::Marker(PinnedDivider),
            SidebarListItem::thread("a1", SidebarSection::Active),
            SidebarListItem::thread("a2", SidebarSection::Active),
            SidebarListItem::Marker(WorkingHeader),
            SidebarListItem::thread("w1", SidebarSection::Working),
            SidebarListItem::Marker(SettledHeader),
            SidebarListItem::thread("s1", SidebarSection::Settled),
        ]
    }

    #[test]
    fn never_drops_into_the_working_shelf_and_keeps_it_out_of_the_inbox_order() {
        let items = dragging_items();
        assert_eq!(resolve_sidebar_drop_target(&items, "a1", "w1"), None);
        assert_eq!(
            resolve_sidebar_drop_target(&items, "p1", "a2"),
            Some(SidebarDropTarget {
                section: DropSection::Active,
                pinned_order: vec![],
                active_order: strings(&["a1", "a2", "p1"]),
            })
        );
        assert_eq!(
            resolve_sidebar_drop_verb(SidebarSection::Active, Some(SidebarSection::Working)),
            None
        );
    }

    fn keys_map(entries: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(Into::into)))
            .collect()
    }

    #[test]
    fn arranges_the_rows_it_can_write_when_another_rows_server_cannot_store_an_order() {
        let empty = HashMap::new();
        let writable: HashSet<String> = strings(&["a1", "a2"]).into_iter().collect();
        let target = SidebarDropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: strings(&["offline", "a2", "a1"]),
        };
        let active_keys = keys_map(&[("offline", None), ("a1", None), ("a2", None)]);
        let active_order = strings(&["offline", "a1", "a2"]);
        let plan = plan_sidebar_thread_drop(&SidebarDropInput {
            active_key: "a2",
            active_section: SidebarSection::Active,
            active_pinned: None,
            active_settled: None,
            supports_settlement: None,
            target: &target,
            pinned_order: &[],
            pinned_keys_by_id: &empty,
            reorderable_keys: None,
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: Some(&writable),
            active_time_ordered: false,
        });
        let SidebarThreadDropPlan::MoveActive { assignments, .. } = plan else {
            panic!("expected move-active, got {plan:?}");
        };
        assert_eq!(
            assignments
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a2", "a1"]
        );
        assert!(assignments[0].order_key < assignments[1].order_key);

        let writable: HashSet<String> = strings(&["a2"]).into_iter().collect();
        let target = SidebarDropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: strings(&["a2", "offline"]),
        };
        let active_keys = keys_map(&[("offline", Some("m")), ("a2", Some("t"))]);
        let active_order = strings(&["offline", "a2"]);
        let above = plan_sidebar_thread_drop(&SidebarDropInput {
            active_key: "a2",
            active_section: SidebarSection::Active,
            active_pinned: None,
            active_settled: None,
            supports_settlement: None,
            target: &target,
            pinned_order: &[],
            pinned_keys_by_id: &empty,
            reorderable_keys: None,
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: Some(&writable),
            active_time_ordered: false,
        });
        assert!(
            matches!(above, SidebarThreadDropPlan::MoveActive { ref assignments, .. } if assignments[0].order_key.as_str() < "m")
        );
    }

    #[test]
    fn only_changes_lifecycle_when_the_inbox_is_time_ordered() {
        let pinned_keys = keys_map(&[("p1", Some("m"))]);
        let active_keys = keys_map(&[("a1", Some("f")), ("a2", Some("t"))]);
        let pinned_order = strings(&["p1"]);
        let active_order = strings(&["a1", "a2"]);
        let within = SidebarDropTarget {
            section: DropSection::Active,
            pinned_order: strings(&["p1"]),
            active_order: strings(&["a2", "a1"]),
        };
        let entering = SidebarDropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: strings(&["a1", "p1", "a2"]),
        };
        let base = SidebarDropInput {
            active_key: "a1",
            active_section: SidebarSection::Active,
            active_pinned: None,
            active_settled: None,
            supports_settlement: None,
            target: &within,
            pinned_order: &pinned_order,
            pinned_keys_by_id: &pinned_keys,
            reorderable_keys: None,
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: None,
            active_time_ordered: true,
        };
        assert_eq!(plan_sidebar_thread_drop(&base), SidebarThreadDropPlan::None);
        let pinned = SidebarDropInput {
            active_key: "p1",
            active_section: SidebarSection::Pinned,
            target: &entering,
            ..base.clone()
        };
        assert_eq!(
            plan_sidebar_thread_drop(&pinned),
            SidebarThreadDropPlan::MoveActive {
                order: None,
                assignments: vec![],
                unpin: true,
                unsettle: false,
                unsnooze: false
            }
        );
    }

    #[test]
    fn thread_key_at_y_resolves_gaps_to_the_row_above() {
        let rows = vec![
            ("a".to_string(), 0.),
            ("b".to_string(), 82.),
            ("c".to_string(), 164.),
        ];
        assert_eq!(
            sidebar_thread_key_at_y(&rows, 40., None).as_deref(),
            Some("a")
        );
        assert_eq!(
            sidebar_thread_key_at_y(&rows, 82., None).as_deref(),
            Some("b")
        );
        assert_eq!(
            sidebar_thread_key_at_y(&rows, -50., None).as_deref(),
            Some("a")
        );
        assert_eq!(
            sidebar_thread_key_at_y(&rows, 500., Some((0., 100.))).as_deref(),
            Some("b")
        );
    }
}
