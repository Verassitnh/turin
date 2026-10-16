//! Port of upstream/apps/web/src/components/ui/collapsible.tsx
//!
//! The panel is `overflow-hidden` and, when `animate` is set, travels its
//! height over 200ms. Without a measured height the open panel is auto and
//! the closed panel is hidden. Pass [`super::animated_size::HeightState`] via
//! [`Collapsible::height`] to drive the same clip the CSS panel uses.
//!
//! gpui-component's `Collapsible` springs open on its own curve, so this is
//! the upstream 200ms ease instead.

use gpui_kit::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div};

use super::animated_size::{HEIGHT_TRANSITION_MS, HeightState};

#[derive(IntoElement)]
pub struct Collapsible {
    open: bool,
    animate: bool,
    height: Option<HeightState>,
    trigger: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl Collapsible {
    pub fn new() -> Self {
        Self {
            open: false,
            animate: true,
            height: None,
            trigger: None,
            children: Vec::new(),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn animate(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }

    pub fn height(mut self, height: HeightState) -> Self {
        self.height = Some(height);
        self
    }

    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.trigger = Some(trigger.into_any_element());
        self
    }
}

impl Default for Collapsible {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Collapsible {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Collapsible {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut panel = div().overflow_hidden();
        if let Some(height) = self.height.filter(|_| self.animate)
            && let Some(px_height) = height.height
        {
            panel = panel.h(gpui_kit::px(px_height as f32));
        }
        if !self.open {
            panel = panel.h(gpui_kit::px(0.));
        }
        let _ = HEIGHT_TRANSITION_MS;
        div()
            .flex()
            .flex_col()
            .children(self.trigger)
            .child(panel.children(self.children))
    }
}
