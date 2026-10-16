//! Behavior of upstream/apps/web/src/components/chat/threadContextDrag.ts.
//! Upstream ships no test file.

use turin_contracts::base_schemas::{EnvironmentId, ThreadId};
use turin_contracts::environment::ScopedThreadRef;

use super::*;

struct FakeSurface {
    hit: Option<u64>,
    over: Option<u64>,
    drops: Vec<(u64, Vec<ScopedThreadRef>)>,
}

impl ThreadDropSurface for FakeSurface {
    fn target_at(&self, _point: DragPoint) -> Option<u64> {
        self.hit
    }
    fn set_over(&mut self, target: Option<u64>) {
        self.over = target;
    }
    fn dispatch_drop(&mut self, target: u64, threads: &[ScopedThreadRef]) {
        self.drops.push((target, threads.to_vec()));
    }
}

fn point(x: f64, y: f64) -> DragPoint {
    DragPoint { x, y }
}

fn thread(id: &str) -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: EnvironmentId::new("environment-1").unwrap(),
        thread_id: ThreadId::new(id).unwrap(),
    }
}

fn drag(hit: Option<u64>) -> ThreadContextDrag<FakeSurface> {
    ThreadContextDrag::new(FakeSurface {
        hit,
        over: None,
        drops: Vec::new(),
    })
}

#[test]
fn tracks_the_ghost_and_highlights_the_target_under_the_pointer() {
    let mut drag = drag(Some(7));
    drag.move_drag(point(3.0, 4.0), "Fix login", 2);
    assert_eq!(
        drag.ghost(),
        Some(&ThreadContextDragGhost {
            x: 3.0,
            y: 4.0,
            title: "Fix login".into(),
            count: 2,
        })
    );
    assert_eq!(drag.surface().over, Some(7));
}

#[test]
fn clears_the_previous_highlight_when_the_pointer_changes_target() {
    let mut drag = drag(Some(7));
    drag.move_drag(point(1.0, 1.0), "Fix login", 1);
    drag.surface_mut().hit = None;
    drag.move_drag(point(9.0, 9.0), "Fix login", 1);
    assert_eq!(drag.surface().over, None);
}

#[test]
fn end_clears_the_ghost_and_the_highlight() {
    let mut drag = drag(Some(7));
    drag.move_drag(point(1.0, 1.0), "Fix login", 1);
    drag.end();
    assert_eq!(THREAD_CONTEXT_DROP_EVENT, "t3-thread-context-drop");
    assert_eq!(DROP_TARGET_ATTRIBUTE, "data-thread-context-drop");
    assert_eq!(DROP_OVER_ATTRIBUTE, "data-thread-context-over");
    assert!(drag.ghost().is_none());
    assert_eq!(drag.surface().over, None);
}

#[test]
fn drop_hands_the_threads_to_the_composer_target() {
    let mut drag = drag(Some(7));
    let threads = vec![thread("thread-1")];
    assert!(drag.drop_threads(point(1.0, 1.0), &threads));
    assert_eq!(drag.surface().drops, [(7, threads)]);
}

#[test]
fn drop_is_ignored_without_a_target_or_without_threads() {
    let mut missing = drag(None);
    assert!(!missing.drop_threads(point(1.0, 1.0), &[thread("thread-1")]));
    let mut empty = drag(Some(7));
    assert!(!empty.drop_threads(point(1.0, 1.0), &[]));
    assert!(empty.surface().drops.is_empty());
}
