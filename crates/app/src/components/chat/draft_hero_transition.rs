//! Port of upstream/apps/web/src/components/chat/draftHeroTransition.ts
//!
//! The browser `document` / `window` pair is a [`HeroSurface`]. There is no
//! process-global document: a [`DraftHeroSession`] owns the in-flight mobile
//! composer transition the route handoff waits on.

use std::sync::atomic::{AtomicU64, Ordering};

use futures::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;

pub const DRAFT_HERO_TRANSITION_ANIMATION_ID: &str = "t3-draft-hero-transition";
pub const DRAFT_HERO_TRANSITION_EASING: &str = "cubic-bezier(0.4, 0, 0.2, 1)";
pub const MOBILE_COMPOSER_VIEW_TRANSITION_NAME: &str = "t3-mobile-composer";
pub const MOBILE_DRAFT_HEADLINE_VIEW_TRANSITION_NAME: &str = "t3-mobile-draft-headline";
const MOBILE_COMPOSER_TRANSITION_DURATION_PROPERTY: &str = "--mobile-composer-transition-duration";

/// A view-transition `finished` promise. `Err` is a cancellation. Shared so
/// the morph and the route handoff can both wait on it.
pub type SettledTransition = Shared<BoxFuture<'static, Result<(), ()>>>;

#[derive(Clone)]
pub struct HeroAnimation {
    pub id: String,
    pub finished: SettledTransition,
}

/// The document and window the transition consults. Tests supply a fake.
pub trait HeroSurface {
    fn has_document(&self) -> bool;
    fn has_window(&self) -> bool;
    /// `None` when `document.getAnimations` is missing.
    fn animations(&self) -> Option<Vec<HeroAnimation>>;
    fn mobile_viewport(&self) -> bool;
    fn prefers_reduced_motion(&self) -> bool;
    fn can_start_view_transition(&self) -> bool;
    fn set_duration(&mut self, property: &str, value: &str);
    fn remove_duration(&mut self, property: &str);
    fn set_route_transition(&mut self, active: bool);
    fn start_view_transition(&mut self, update: &mut dyn FnMut()) -> Result<SettledTransition, ()>;
}

pub struct TransitionOptions {
    pub active: bool,
    pub duration_ms: u64,
}

struct ActiveTransition {
    id: u64,
    finished: SettledTransition,
}

pub struct DraftHeroSession<S> {
    surface: Mutex<S>,
    active: Mutex<Option<ActiveTransition>>,
    next_id: AtomicU64,
}

impl<S> DraftHeroSession<S> {
    pub fn new(surface: S) -> Self {
        Self {
            surface: Mutex::new(surface),
            active: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn surface(&self) -> parking_lot::MutexGuard<'_, S> {
        self.surface.lock()
    }
}

impl<S: HeroSurface> DraftHeroSession<S> {
    pub async fn wait_for_draft_hero_transition(&self) {
        let active = self
            .active
            .lock()
            .as_ref()
            .map(|active| active.finished.clone());
        let animations = {
            let surface = self.surface.lock();
            if surface.has_document() {
                surface.animations()
            } else {
                None
            }
        };
        let Some(animations) = animations else {
            if let Some(active) = active {
                let _ = active.await;
            }
            return;
        };
        let mut waits = Vec::new();
        if let Some(active) = active {
            waits.push(
                async move {
                    let _ = active.await;
                }
                .boxed(),
            );
        }
        for animation in animations {
            if animation.id != DRAFT_HERO_TRANSITION_ANIMATION_ID {
                continue;
            }
            let finished = animation.finished;
            waits.push(
                async move {
                    let _ = finished.await;
                }
                .boxed(),
            );
        }
        futures::future::join_all(waits).await;
    }

    pub async fn run_mobile_composer_transition(
        &self,
        mut update: impl FnMut(),
        options: TransitionOptions,
    ) {
        let plain = {
            let surface = self.surface.lock();
            !surface.has_document()
                || !surface.has_window()
                || !options.active
                || !surface.mobile_viewport()
                || surface.prefers_reduced_motion()
                || !surface.can_start_view_transition()
        };
        if plain {
            update();
            return;
        }

        let mut started = false;
        let mut run_update = || {
            if started {
                return;
            }
            started = true;
            update();
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let finished = {
            let mut surface = self.surface.lock();
            surface.set_duration(
                MOBILE_COMPOSER_TRANSITION_DURATION_PROPERTY,
                &format!("{}ms", options.duration_ms),
            );
            surface.set_route_transition(true);
            surface.start_view_transition(&mut run_update)
        };
        let _cleanup = Cleanup {
            session: self,
            id,
            armed: true,
        };
        match finished {
            Err(()) => run_update(),
            Ok(finished) => {
                *self.active.lock() = Some(ActiveTransition {
                    id,
                    finished: finished.clone(),
                });
                if finished.await.is_err() {
                    run_update();
                }
            }
        }
    }
}

struct Cleanup<'a, S: HeroSurface> {
    session: &'a DraftHeroSession<S>,
    id: u64,
    armed: bool,
}

impl<S: HeroSurface> Drop for Cleanup<'_, S> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut active = self.session.active.lock();
        if active.as_ref().is_some_and(|current| current.id == self.id) {
            *active = None;
        }
        drop(active);
        let mut surface = self.session.surface.lock();
        surface.set_route_transition(false);
        surface.remove_duration(MOBILE_COMPOSER_TRANSITION_DURATION_PROPERTY);
    }
}

#[cfg(test)]
#[path = "draft_hero_transition_tests.rs"]
mod tests;
