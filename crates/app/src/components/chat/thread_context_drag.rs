//! Port of upstream/apps/web/src/components/chat/threadContextDrag.ts
//!
//! Hit-testing and the drop event go through [`ThreadDropSurface`]. The ghost
//! is session state, not a React store. Upstream has no test file; these
//! tests lock the hit-test, highlight, and drop rules in the source.

use turin_contracts::environment::ScopedThreadRef;

pub const THREAD_CONTEXT_DROP_EVENT: &str = "t3-thread-context-drop";
pub const DROP_TARGET_ATTRIBUTE: &str = "data-thread-context-drop";
pub const DROP_OVER_ATTRIBUTE: &str = "data-thread-context-over";

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadContextDragGhost {
    pub x: f64,
    pub y: f64,
    pub title: String,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragPoint {
    pub x: f64,
    pub y: f64,
}

pub trait ThreadDropSurface {
    fn target_at(&self, point: DragPoint) -> Option<u64>;
    fn set_over(&mut self, target: Option<u64>);
    fn dispatch_drop(&mut self, target: u64, threads: &[ScopedThreadRef]);
}

pub struct ThreadContextDrag<S> {
    surface: S,
    ghost: Option<ThreadContextDragGhost>,
    over: Option<u64>,
}

impl<S> ThreadContextDrag<S> {
    pub fn new(surface: S) -> Self {
        Self {
            surface,
            ghost: None,
            over: None,
        }
    }

    pub fn ghost(&self) -> Option<&ThreadContextDragGhost> {
        self.ghost.as_ref()
    }

    pub fn surface(&self) -> &S {
        &self.surface
    }

    pub fn surface_mut(&mut self) -> &mut S {
        &mut self.surface
    }
}

impl<S: ThreadDropSurface> ThreadContextDrag<S> {
    pub fn move_drag(&mut self, point: DragPoint, title: &str, count: usize) {
        let target = self.surface.target_at(point);
        if target != self.over {
            self.surface.set_over(target);
            self.over = target;
        }
        self.set_ghost(Some(ThreadContextDragGhost {
            x: point.x,
            y: point.y,
            title: title.to_owned(),
            count,
        }));
    }

    pub fn end(&mut self) {
        if self.over.is_some() {
            self.surface.set_over(None);
            self.over = None;
        }
        self.set_ghost(None);
    }

    /// True when a composer accepted the drop.
    pub fn drop_threads(&mut self, point: DragPoint, threads: &[ScopedThreadRef]) -> bool {
        let Some(target) = self.surface.target_at(point) else {
            return false;
        };
        if threads.is_empty() {
            return false;
        }
        self.surface.dispatch_drop(target, threads);
        true
    }

    fn set_ghost(&mut self, next: Option<ThreadContextDragGhost>) {
        if self.ghost.is_none() && next.is_none() {
            return;
        }
        self.ghost = next;
    }
}

#[cfg(test)]
#[path = "thread_context_drag_tests.rs"]
mod tests;
