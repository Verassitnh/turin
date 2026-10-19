//! Port of upstream/apps/web/src/components/pullRequest/PullRequestGuideTab.tsx and the part of
//! guide/useReviewGuideWriter.ts the view needs: a maintainer's tour of the pull request, stops
//! through the diff in reading order with the calls only a person can make. Generated on request
//! and stored, so opening the tab is free.
//!
//! The run is the source's: `generate_guide` starts it and the snapshot's `guide` carries
//! `run: Running | Failed`. Starting one hands it to the [`GuideRunStore`], which the
//! [`GuideRunCoordinator`](super::guide_run_coordinator) polls and announces, so the reader can
//! leave; the tab also says which guide is on screen, which needs no announcement.
//!
//! A write the source refuses is the snapshot's `guide_start_error`: the tab stops waiting and the
//! panel says "Could not start a review guide".
//!
//! Not ported: the header menu's rewrite entry (the panel header, S1) — [`GuideTab::write`] is its
//! seam.
//!
//! Port of upstream/apps/web/src/components/pullRequest/guide/useReviewGuideWriter.ts.

use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div, rems,
};
use turin_contracts::pull_request_review_guide::PullRequestReviewGuideRun;

use super::detail_context::DetailContext;
use super::guide_run_store::{GuideRunStore, GuideRunWatch, guide_run_key};
use super::guide_view::{FinishHandler, GuideBanner, GuideTour};
use super::presentation_view::Spinner;
use super::view_model::Loadable;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

pub struct GuideTab {
    pub ctx: DetailContext,
    /// Whether this is the tab on screen, which is when J and K step through the tour.
    active: bool,
    /// A write was asked for and the source has not said "running" yet.
    starting: bool,
    /// What the snapshot said about the run when the write was asked for.
    baseline: String,
    tour: Option<Entity<GuideTour>>,
    on_finish: Option<FinishHandler>,
    _subscriptions: Vec<Subscription>,
}

impl GuideTab {
    pub fn new(ctx: DetailContext, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&ctx.store, |this, _, cx| {
            this.watch_run_seen_elsewhere(cx);
            cx.notify();
        });
        let this = Self {
            ctx,
            active: true,
            starting: false,
            baseline: String::new(),
            tour: None,
            on_finish: None,
            _subscriptions: vec![subscription],
        };
        this.set_shown(true, cx);
        this
    }

    fn run_key(&self) -> String {
        guide_run_key(&self.ctx.environment_id, &self.ctx.reference)
    }

    /// `setShown`: the guide on screen needs no notice when it lands.
    fn set_shown(&self, shown: bool, cx: &mut Context<Self>) {
        let key = self.run_key();
        let runs = GuideRunStore::global(cx);
        runs.update(cx, |runs, cx| {
            if shown {
                runs.set_shown(Some(key), cx);
            } else if runs.shown_key() == Some(key.as_str()) {
                runs.set_shown(None, cx);
            }
        });
    }

    /// `watchRun`: the baseline the coordinator compares the run's end against.
    fn watch_run(&self, cx: &mut Context<Self>) {
        let (previous_guide_at, previous_failed_at, title) = {
            let snapshot = self.ctx.snapshot(cx);
            let result = snapshot.and_then(|snapshot| snapshot.guide.ready());
            (
                result
                    .and_then(|result| result.guide.as_ref())
                    .map(|guide| guide.generated_at.clone()),
                result.and_then(|result| match &result.run {
                    Some(PullRequestReviewGuideRun::Failed { failed_at, .. }) => {
                        Some(failed_at.clone())
                    }
                    _ => None,
                }),
                snapshot
                    .and_then(|snapshot| snapshot.detail.ready())
                    .map(|detail| detail.title.as_str().to_owned()),
            )
        };
        let watch = GuideRunWatch {
            environment_id: self.ctx.environment_id.clone(),
            reference: self.ctx.reference.clone(),
            title,
            previous_guide_at,
            previous_failed_at,
            since: crate::timestamp_format::now_ms(),
        };
        GuideRunStore::global(cx).update(cx, |runs, cx| runs.watch(watch, cx));
    }

    /// A run started elsewhere, or before a reload, is watched as soon as it is seen.
    fn watch_run_seen_elsewhere(&self, cx: &mut Context<Self>) {
        let running = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.guide.ready())
            .is_some_and(|result| {
                matches!(result.run, Some(PullRequestReviewGuideRun::Running { .. }))
            });
        if running {
            self.watch_run(cx);
        }
    }

    /// The last stop's "Finish review": the panel opens the composer on the review.
    pub fn set_on_finish(&mut self, handler: Option<FinishHandler>, cx: &mut Context<Self>) {
        self.on_finish = handler;
        cx.notify();
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            self.set_shown(active, cx);
            cx.notify();
        }
    }

    /// `write()`: starts (or restarts) the run. Nothing happens while one is already running.
    pub fn write(&mut self, cx: &mut Context<Self>) {
        if self.is_running(cx) {
            return;
        }
        self.starting = true;
        self.watch_run(cx);
        self.baseline = run_signature(
            self.ctx
                .snapshot(cx)
                .and_then(|snapshot| snapshot.guide.ready()),
        );
        if let Some(source) = self.ctx.source(cx) {
            source.generate_guide(&self.ctx.reference, cx);
        }
        cx.notify();
    }

    fn is_running(&self, cx: &App) -> bool {
        self.starting
            || self
                .ctx
                .snapshot(cx)
                .and_then(|snapshot| snapshot.guide.ready())
                .is_some_and(|result| {
                    matches!(result.run, Some(PullRequestReviewGuideRun::Running { .. }))
                })
    }
}

fn run_signature(
    result: Option<&turin_contracts::pull_request_review_guide::PullRequestReviewGuideResult>,
) -> String {
    result.map_or_else(String::new, |result| {
        format!(
            "{:?}|{:?}",
            result.run,
            result
                .guide
                .as_ref()
                .map(|guide| guide.generated_at.clone())
        )
    })
}

/// What a failed run says, only while it is about the head the reader is looking at.
pub fn current_failure<'a>(
    run: Option<&'a PullRequestReviewGuideRun>,
    current_head_sha: Option<&str>,
) -> Option<&'a str> {
    match run {
        Some(PullRequestReviewGuideRun::Failed {
            head_sha, detail, ..
        }) if Some(head_sha.as_str()) == current_head_sha => Some(detail.as_str()),
        _ => None,
    }
}

/// Written before the latest push.
pub fn is_guide_stale(guide_head: &str, current_head_sha: Option<&str>) -> bool {
    current_head_sha.is_some_and(|current| current != guide_head)
}

impl Render for GuideTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        // The snapshot also holds whole patches; only what the tab reads is copied.
        let (can_write, can_comment, guide_state) = match self.ctx.snapshot(cx) {
            Some(snapshot) => (
                snapshot.can_write,
                matches!(&snapshot.detail, Loadable::Ready(detail)
                    if detail.capabilities.review.inline_comment && detail.viewer_permissions.comment),
                Some(snapshot.guide.clone()),
            ),
            None => (false, false, None),
        };
        let result = guide_state.as_ref().and_then(Loadable::ready);
        let query_error = guide_state
            .as_ref()
            .and_then(Loadable::error)
            .map(str::to_owned);
        let running_in_snapshot = result.is_some_and(|result| {
            matches!(result.run, Some(PullRequestReviewGuideRun::Running { .. }))
        });
        // The source has taken the run over once the snapshot says anything new about it.
        let start_refused = self
            .ctx
            .snapshot(cx)
            .is_some_and(|snapshot| snapshot.guide_start_error.is_some());
        if self.starting && (start_refused || self.baseline != run_signature(result)) {
            self.starting = false;
        }
        let running = self.starting || running_in_snapshot;
        let current_head = result.and_then(|result| {
            result
                .current_head_sha
                .as_ref()
                .map(|sha| sha.as_str().to_owned())
        });
        let failure = current_failure(
            result.and_then(|result| result.run.as_ref()),
            current_head.as_deref(),
        )
        .map(str::to_owned);
        let write_button = |id: &'static str,
                            label: &'static str,
                            size: ButtonSize,
                            running: bool,
                            cx: &mut Context<Self>| {
            can_write.then(|| {
                Button::new(id)
                    .size(size)
                    .variant(ButtonVariant::Outline)
                    .disabled(running)
                    .child(if running {
                        Spinner::new("guide-write-spinner")
                            .size(rems(0.875))
                            .into_any_element()
                    } else {
                        Icon::new(IconName::Sparkles)
                            .size(rems(0.875))
                            .into_any_element()
                    })
                    .child(if running {
                        "Reading the change..."
                    } else {
                        label
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.write(cx)))
            })
        };

        let guide = result.and_then(|result| result.guide.clone());
        let Some(guide) = guide else {
            // No guide yet.
            if matches!(guide_state, None | Some(Loadable::Pending)) {
                return div()
                    .flex()
                    .min_h(rems(12.))
                    .items_center()
                    .justify_center()
                    .child(Spinner::new("guide-loading"))
                    .into_any_element();
            }
            let description = failure.clone().or(query_error).unwrap_or_else(|| {
                if running {
                    "An agent is reading the change. A large one takes a few minutes. You can leave; you will be told when it is ready.".to_owned()
                } else {
                    "A guide is a tour of the change: stops through the code in reading order, with the calls only a maintainer can make.".to_owned()
                }
            });
            return div()
                .flex()
                .flex_col()
                .min_h(rems(12.))
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .py(rems(2.5))
                .text_center()
                .child(
                    div()
                        .text_size(TEXT_SM.size)
                        .line_height(TEXT_SM.line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(if running {
                            "Writing a review guide"
                        } else {
                            "No review guide yet"
                        }),
                )
                .child(
                    div()
                        .max_w(rems(28.))
                        .text_size(TEXT_XS.size)
                        .line_height(TEXT_XS.line_height)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(description)),
                )
                .children(write_button(
                    "guide-write",
                    if failure.is_none() {
                        "Write guide"
                    } else {
                        "Try again"
                    },
                    ButtonSize::Sm,
                    running,
                    cx,
                ))
                .into_any_element();
        };

        // Only a guide that needs attention says so; a current one is just the tour.
        let stale = is_guide_stale(guide.head_sha.as_str(), current_head.as_deref());
        let banner = (stale || failure.is_some() || running).then(|| GuideBanner {
            running,
            message: if running {
                "Rewriting this guide. You will be told when the new one is ready.".to_owned()
            } else {
                failure
                    .clone()
                    .unwrap_or_else(|| "Written before the latest push.".to_owned())
            },
            can_write,
        });
        let tour = match &self.tour {
            Some(tour) => tour.clone(),
            None => {
                let tour = cx.new(|cx| GuideTour::new(self.ctx.clone(), guide.clone(), window, cx));
                self.tour = Some(tour.clone());
                tour
            }
        };
        let active = self.active;
        let finish = self.on_finish.clone();
        tour.update(cx, |tour, cx| {
            tour.set_guide(&guide, banner, can_comment, active, cx);
            tour.set_finish(finish);
        });
        div().size_full().min_h_0().child(tour).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::TrimmedNonEmptyString;

    fn failed(head: &str) -> PullRequestReviewGuideRun {
        PullRequestReviewGuideRun::Failed {
            head_sha: TrimmedNonEmptyString::new(head).unwrap(),
            failed_at: "2026-10-14T00:00:00.000Z".to_owned(),
            detail: TrimmedNonEmptyString::new("model timed out").unwrap(),
        }
    }

    #[test]
    fn a_failure_shows_only_for_the_head_being_read() {
        assert_eq!(
            current_failure(Some(&failed("abc")), Some("abc")),
            Some("model timed out")
        );
        assert_eq!(current_failure(Some(&failed("abc")), Some("def")), None);
        assert_eq!(current_failure(Some(&failed("abc")), None), None);
        assert_eq!(current_failure(None, Some("abc")), None);
    }

    #[test]
    fn a_guide_is_stale_once_the_head_moves_on() {
        assert!(is_guide_stale("abc", Some("def")));
        assert!(!is_guide_stale("abc", Some("abc")));
        // The host did not report a head: nothing can be said to have moved.
        assert!(!is_guide_stale("abc", None));
    }
}
