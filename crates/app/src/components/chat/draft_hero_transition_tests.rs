//! Port of upstream/apps/web/src/components/chat/draftHeroTransition.test.ts

use std::cell::Cell;
use std::rc::Rc;

use futures::future::FutureExt;
use futures::poll;
use tokio::sync::oneshot;

use super::*;

struct FakeSurface {
    has_document: bool,
    has_window: bool,
    animations: Option<Vec<HeroAnimation>>,
    mobile: bool,
    reduced_motion: bool,
    can_start: bool,
    start_calls: usize,
    finished: Option<SettledTransition>,
    duration_sets: Vec<(String, String)>,
    duration_removes: Vec<String>,
    route: bool,
}

impl Default for FakeSurface {
    fn default() -> Self {
        Self {
            has_document: true,
            has_window: true,
            animations: Some(Vec::new()),
            mobile: true,
            reduced_motion: false,
            can_start: true,
            start_calls: 0,
            finished: None,
            duration_sets: Vec::new(),
            duration_removes: Vec::new(),
            route: false,
        }
    }
}

impl HeroSurface for FakeSurface {
    fn has_document(&self) -> bool {
        self.has_document
    }
    fn has_window(&self) -> bool {
        self.has_window
    }
    fn animations(&self) -> Option<Vec<HeroAnimation>> {
        self.animations.clone()
    }
    fn mobile_viewport(&self) -> bool {
        self.mobile
    }
    fn prefers_reduced_motion(&self) -> bool {
        self.reduced_motion
    }
    fn can_start_view_transition(&self) -> bool {
        self.can_start
    }
    fn set_duration(&mut self, property: &str, value: &str) {
        self.duration_sets
            .push((property.to_owned(), value.to_owned()));
    }
    fn remove_duration(&mut self, property: &str) {
        self.duration_removes.push(property.to_owned());
    }
    fn set_route_transition(&mut self, active: bool) {
        self.route = active;
    }
    fn start_view_transition(&mut self, update: &mut dyn FnMut()) -> Result<SettledTransition, ()> {
        self.start_calls += 1;
        update();
        self.finished.clone().ok_or(())
    }
}

fn pending_finish() -> (oneshot::Sender<Result<(), ()>>, SettledTransition) {
    let (tx, rx) = oneshot::channel();
    let finished = async move { rx.await.unwrap_or(Err(())) }.boxed().shared();
    (tx, finished)
}

fn ready_finish(result: Result<(), ()>) -> SettledTransition {
    async move { result }.boxed().shared()
}

#[tokio::test(flavor = "current_thread")]
async fn waits_for_the_draft_hero_animation_and_ignores_unrelated_ones() {
    let (finish, hero) = pending_finish();
    let (_unrelated_tx, unrelated) = pending_finish();
    let session = DraftHeroSession::new(FakeSurface {
        animations: Some(vec![
            HeroAnimation {
                id: "unrelated-animation".into(),
                finished: unrelated,
            },
            HeroAnimation {
                id: DRAFT_HERO_TRANSITION_ANIMATION_ID.into(),
                finished: hero,
            },
        ]),
        ..FakeSurface::default()
    });

    let wait = session.wait_for_draft_hero_transition();
    tokio::pin!(wait);
    assert!(poll!(&mut wait).is_pending());

    finish.send(Ok(())).unwrap();
    assert!(poll!(&mut wait).is_ready());
}

#[tokio::test(flavor = "current_thread")]
async fn allows_the_handoff_when_an_active_transition_is_cancelled() {
    let session = DraftHeroSession::new(FakeSurface {
        animations: Some(vec![HeroAnimation {
            id: DRAFT_HERO_TRANSITION_ANIMATION_ID.into(),
            finished: ready_finish(Err(())),
        }]),
        ..FakeSurface::default()
    });
    session.wait_for_draft_hero_transition().await;
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_the_route_handoff_waiting_while_the_morph_is_active() {
    let (finish, settled) = pending_finish();
    let calls = Rc::new(Cell::new(0));
    let calls_for_update = Rc::clone(&calls);
    let session = DraftHeroSession::new(FakeSurface {
        animations: Some(Vec::new()),
        finished: Some(settled),
        mobile: true,
        reduced_motion: false,
        ..FakeSurface::default()
    });

    let run = session.run_mobile_composer_transition(
        move || calls_for_update.set(calls_for_update.get() + 1),
        TransitionOptions {
            active: true,
            duration_ms: 240,
        },
    );
    tokio::pin!(run);
    assert!(poll!(&mut run).is_pending());

    let wait = session.wait_for_draft_hero_transition();
    tokio::pin!(wait);
    assert!(poll!(&mut wait).is_pending());

    finish.send(Ok(())).unwrap();
    assert!(poll!(&mut run).is_ready());
    assert!(poll!(&mut wait).is_ready());
    assert_eq!(calls.get(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn uses_a_scoped_view_transition_on_mobile() {
    let calls = Rc::new(Cell::new(0));
    let calls_for_update = Rc::clone(&calls);
    let session = DraftHeroSession::new(FakeSurface {
        finished: Some(ready_finish(Ok(()))),
        mobile: true,
        reduced_motion: false,
        ..FakeSurface::default()
    });

    session
        .run_mobile_composer_transition(
            move || calls_for_update.set(calls_for_update.get() + 1),
            TransitionOptions {
                active: true,
                duration_ms: 360,
            },
        )
        .await;

    let surface = session.surface();
    assert_eq!(surface.start_calls, 1);
    assert_eq!(calls.get(), 1);
    assert_eq!(
        surface.duration_sets,
        [(
            "--mobile-composer-transition-duration".to_owned(),
            "360ms".to_owned()
        )]
    );
    assert_eq!(
        surface.duration_removes,
        ["--mobile-composer-transition-duration".to_owned()]
    );
    assert!(!surface.route);
}

#[tokio::test(flavor = "current_thread")]
async fn updates_without_a_view_transition_when_panel_animations_are_inactive() {
    let calls = Rc::new(Cell::new(0));
    let calls_for_update = Rc::clone(&calls);
    let session = DraftHeroSession::new(FakeSurface {
        mobile: true,
        reduced_motion: true,
        ..FakeSurface::default()
    });
    session
        .run_mobile_composer_transition(
            move || calls_for_update.set(calls_for_update.get() + 1),
            TransitionOptions {
                active: false,
                duration_ms: 360,
            },
        )
        .await;
    assert_eq!(session.surface().start_calls, 0);
    assert_eq!(calls.get(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn updates_without_a_view_transition_when_reduced_motion_is_preferred() {
    let calls = Rc::new(Cell::new(0));
    let calls_for_update = Rc::clone(&calls);
    let session = DraftHeroSession::new(FakeSurface {
        mobile: true,
        reduced_motion: true,
        ..FakeSurface::default()
    });
    session
        .run_mobile_composer_transition(
            move || calls_for_update.set(calls_for_update.get() + 1),
            TransitionOptions {
                active: true,
                duration_ms: 360,
            },
        )
        .await;
    assert_eq!(session.surface().start_calls, 0);
    assert_eq!(calls.get(), 1);
}
