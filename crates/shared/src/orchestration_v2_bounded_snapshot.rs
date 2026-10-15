//! Port of upstream/packages/shared/src/orchestrationV2BoundedSnapshot.ts
//!
//! Only the in-process half is ported. The compact snapshot is a transport optimisation for the
//! websocket, and Turin has no transport, so `omit` is not needed on the write path. It is kept
//! here with `restore` so a persisted or imported snapshot in upstream's compact shape still
//! decodes the same way.
//!
//! Deviation: upstream refuses a local row whose turn item is equal but not the same object
//! (`projection.turnItems[i] !== row.item`). Rust has no object identity, so the check is
//! structural equality. Restoring from the row then reproduces the original list exactly, which
//! is the guarantee the identity check was there for.

use turin_contracts::base_schemas::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProjectedTurnItemVisibility, OrchestrationV2ThreadProjection,
};

/// `row.visibility === "local" || row.sourceThreadId === projection.thread.id`.
fn is_local_timeline_row(
    thread_id: &ThreadId,
    visibility: OrchestrationV2ProjectedTurnItemVisibility,
    source_thread_id: &ThreadId,
) -> bool {
    visibility == OrchestrationV2ProjectedTurnItemVisibility::Local || source_thread_id == thread_id
}

/// The turn items that remain after dropping the leading local visible ones, or None when the
/// list does not have that exact shape or nothing would be omitted.
fn omitted_turn_items<'a, T: PartialEq>(
    thread_id: &ThreadId,
    visible: impl Iterator<
        Item = (
            OrchestrationV2ProjectedTurnItemVisibility,
            &'a ThreadId,
            &'a T,
        ),
    >,
    turn_items: &'a [T],
) -> Option<&'a [T]> {
    let mut omitted = 0;
    for (visibility, source_thread_id, item) in visible {
        if !is_local_timeline_row(thread_id, visibility, source_thread_id) {
            continue;
        }
        if turn_items.get(omitted) != Some(item) {
            return None;
        }
        omitted += 1;
    }
    (omitted > 0).then(|| &turn_items[omitted..])
}

/// The turn items to send for a projection whose local visible rows are rebuilt on restore.
fn restored_turn_items<'a, T: Clone>(
    thread_id: &ThreadId,
    visible: impl Iterator<Item = (OrchestrationV2ProjectedTurnItemVisibility, &'a ThreadId, T)>,
    turn_items: &[T],
) -> Vec<T> {
    let local: Vec<T> = visible
        .filter(|(visibility, source_thread_id, _)| {
            is_local_timeline_row(thread_id, *visibility, source_thread_id)
        })
        .map(|(_, _, item)| item)
        .collect();
    if local.is_empty() {
        return turn_items.to_vec();
    }
    local
        .into_iter()
        .chain(turn_items.iter().cloned())
        .collect()
}

/// Server half of the opt-in compact bounded snapshot. Returns None when the projection does not
/// have that exact shape, or when nothing would be omitted.
pub fn omit_local_visible_turn_items(
    projection: &OrchestrationV2ThreadProjection,
) -> Option<OrchestrationV2ThreadProjection> {
    let thread_id = &projection.thread.id;
    let remaining = omitted_turn_items(
        thread_id,
        projection
            .visible_turn_items
            .iter()
            .map(|row| (row.visibility, &row.source_thread_id, &row.item)),
        &projection.turn_items,
    )?
    .to_vec();
    let mut compact = projection.clone();
    compact.turn_items = remaining;
    Some(compact)
}

/// Client half: call on a decoded snapshot that carries the `turnItemsOmitLocalVisible` marker.
pub fn restore_local_visible_turn_items(
    projection: &OrchestrationV2ThreadProjection,
) -> OrchestrationV2ThreadProjection {
    let thread_id = &projection.thread.id;
    let mut restored = projection.clone();
    restored.turn_items = restored_turn_items(
        thread_id,
        projection
            .visible_turn_items
            .iter()
            .map(|row| (row.visibility, &row.source_thread_id, row.item.clone())),
        &projection.turn_items,
    );
    restored
}

/// The usable projection of a decoded bounded snapshot, compact or not.
pub fn bounded_snapshot_projection(
    projection: &OrchestrationV2ThreadProjection,
    turn_items_omit_local_visible: bool,
) -> OrchestrationV2ThreadProjection {
    if turn_items_omit_local_visible {
        restore_local_visible_turn_items(projection)
    } else {
        projection.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OrchestrationV2ProjectedTurnItemVisibility::{Inherited, Local, Synthetic};

    fn thread(name: &str) -> ThreadId {
        ThreadId::new(name).unwrap()
    }

    /// Stands in for a turn item: the algorithm only compares and copies items.
    type Item = &'static str;

    fn omit<'a>(
        child: &ThreadId,
        visible: &'a [(
            OrchestrationV2ProjectedTurnItemVisibility,
            &'a ThreadId,
            Item,
        )],
        turn_items: &'a [Item],
    ) -> Option<Vec<Item>> {
        omitted_turn_items(
            child,
            visible
                .iter()
                .map(|(visibility, source, item)| (*visibility, *source, item)),
            turn_items,
        )
        .map(|rest| rest.to_vec())
    }

    #[test]
    fn omits_only_local_visible_items_and_restores_the_exact_list_and_order() {
        let parent = thread("thread:compact-parent");
        let child = thread("thread:compact-child");
        let visible = [
            (Inherited, &parent, "parent-1"),
            (Inherited, &parent, "parent-2"),
            (Synthetic, &parent, "fork-marker"),
            (Local, &child, "child-1"),
            (Local, &child, "child-2"),
            (Local, &child, "child-3"),
        ];
        let turn_items = ["child-1", "child-2", "child-3", "interrupt-request"];
        // Inherited and synthetic rows never had turn items; the retained request stays.
        assert_eq!(
            omit(&child, &visible, &turn_items),
            Some(vec!["interrupt-request"])
        );
        let compact = ["interrupt-request"];
        assert_eq!(
            restored_turn_items(
                &child,
                visible
                    .iter()
                    .map(|(visibility, source, item)| (*visibility, *source, *item)),
                &compact,
            ),
            turn_items.to_vec()
        );
    }

    #[test]
    fn treats_rows_sourced_from_the_thread_itself_as_local() {
        let child = thread("thread:compact-child");
        let visible = [(Inherited, &child, "own")];
        let turn_items = ["own"];
        assert_eq!(omit(&child, &visible, &turn_items), Some(vec![]));
        assert_eq!(
            restored_turn_items(
                &child,
                visible
                    .iter()
                    .map(|(visibility, source, item)| (*visibility, *source, *item)),
                &[],
            ),
            vec!["own"]
        );
    }

    #[test]
    fn refuses_shapes_it_cannot_restore_exactly() {
        let child = thread("thread:compact-child");
        let visible = [(Local, &child, "a"), (Local, &child, "b")];
        // Different order than the visible rows.
        assert_eq!(omit(&child, &visible, &["b", "a"]), None);
        // A local row without its turn item.
        assert_eq!(omit(&child, &visible, &["a"]), None);
    }

    #[test]
    fn sends_nothing_compact_when_no_local_rows_are_visible() {
        let parent = thread("thread:compact-parent");
        let child = thread("thread:compact-child");
        let visible = [(Inherited, &parent, "parent")];
        assert_eq!(omit(&child, &visible, &["interrupt-request"]), None);
    }

    #[test]
    fn restore_without_local_rows_leaves_the_turn_items_alone() {
        let child = thread("thread:compact-child");
        assert_eq!(
            restored_turn_items::<Item>(&child, std::iter::empty(), &["kept"]),
            vec!["kept"]
        );
    }
}
