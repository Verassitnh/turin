//! Connects the composer's PageUp/PageDown keys to the messages timeline list.

use std::time::Instant;

use gpui_kit::{Context, Pixels, px};

use super::messages_timeline::MessagesTimeline;
use super::page_scroll_controller::{PageScrollController, PageScrollKey, PageScrollMetrics};

const COMPOSER_SCROLL_PADDING_BOTTOM_PX: f64 = 24.0;

/// Shared between [`MessagesTimeline`] and the composer for one open thread.
pub struct TimelinePageScrollSession {
    controller: PageScrollController,
    needs_notify: bool,
    clock: Instant,
}

impl Default for TimelinePageScrollSession {
    fn default() -> Self {
        Self {
            controller: PageScrollController::default(),
            needs_notify: false,
            clock: Instant::now(),
        }
    }
}

impl TimelinePageScrollSession {
    fn now_ms(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }

    pub fn handle_key_down(&mut self, key: PageScrollKey, metrics: PageScrollMetrics) -> bool {
        let now = self.now_ms();
        let started = self.controller.handle_key_down(
            key,
            now,
            metrics,
            COMPOSER_SCROLL_PADDING_BOTTOM_PX,
        );
        self.needs_notify = self.controller.needs_frame();
        started
    }

    pub fn handle_key_up(&mut self, key: &str) {
        self.controller.handle_key_up(key);
        self.needs_notify = self.controller.needs_frame();
    }

    pub fn release_active_key(&mut self) {
        self.controller.release_active_key();
        self.needs_notify = self.controller.needs_frame();
    }

    pub fn needs_frame(&self) -> bool {
        self.controller.needs_frame()
    }

    pub fn take_notify(&mut self) -> bool {
        let notify = self.needs_notify;
        self.needs_notify = false;
        notify
    }

    /// Returns the scroll delta to apply to the timeline list, if any.
    pub fn frame(&mut self, metrics: PageScrollMetrics) -> Option<Pixels> {
        let now = self.now_ms();
        let next = self.controller.frame(now, metrics, COMPOSER_SCROLL_PADDING_BOTTOM_PX)?;
        let delta = next - metrics.scroll_top;
        self.needs_notify = self.controller.needs_frame();
        Some(px(delta as f32))
    }
}

impl MessagesTimeline {
    pub fn page_scroll_metrics(&self, _window: &gpui_kit::Window) -> PageScrollMetrics {
        let viewport = self.list.viewport_bounds();
        let client_height = f64::from(viewport.size.height);
        let scroll_top = f64::from(self.list.logical_scroll_top());
        let scroll_height = scroll_top + client_height;
        PageScrollMetrics {
            bounding_height: client_height,
            client_height,
            scroll_height: scroll_height.max(client_height),
            scroll_top,
        }
    }

    pub fn apply_page_scroll_delta(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        if f32::from(delta) == 0. {
            return;
        }
        self.list.scroll_by(delta);
        cx.notify();
    }
}
