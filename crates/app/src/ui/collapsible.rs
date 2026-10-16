//! Port of upstream/apps/web/src/components/ui/collapsible.tsx
//!
//! The panel is `overflow-hidden` and, when `animate` is set, travels its
//! height over 200ms (`transition-[height] duration-200`, Base UI's
//! `data-starting-style:h-0` / `data-ending-style:h-0`, ease from the CSS
//! default). The panel measures its own content like Base UI's
//! `--collapsible-panel-height`: the open height is the content's natural
//! height, so after the travel settles the open panel is auto-height again.
//!
//! gpui-component's `Collapsible` springs open on its own curve, so this is
//! the upstream 200ms ease instead.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, IntoElement, ParentElement,
    RenderOnce, Styled, Window, div, px,
};

use super::animated_size::{HEIGHT_TRANSITION_MS, css_ease_out};

#[derive(Default)]
struct PanelState {
    open: bool,
    initialized: bool,
    measured: f32,
    animating: bool,
    epoch: u64,
}

#[derive(IntoElement)]
pub struct Collapsible {
    id: ElementId,
    open: bool,
    animate: bool,
    trigger: Option<AnyElement>,
    children: Vec<AnyElement>,
}

impl Collapsible {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            open: false,
            animate: true,
            trigger: None,
            children: Vec::new(),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// `animate = false` skips the height travel (reduced motion).
    pub fn animate(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }

    pub fn trigger(mut self, trigger: impl IntoElement) -> Self {
        self.trigger = Some(trigger.into_any_element());
        self
    }
}

impl ParentElement for Collapsible {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Collapsible {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| PanelState::default());
        let toggled = {
            let state = state.read(cx);
            state.initialized && state.open != self.open
        };
        let open = self.open;
        let animate = self.animate;
        if toggled || !state.read(cx).initialized {
            state.update(cx, |state, _| {
                state.initialized = true;
                state.open = open;
                if toggled {
                    state.epoch += 1;
                    state.animating = animate;
                }
            });
        }
        if toggled && animate {
            let epoch = state.read(cx).epoch;
            let state = state.downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(HEIGHT_TRANSITION_MS))
                    .await;
                state
                    .update(cx, |state, cx| {
                        if state.epoch == epoch && state.animating {
                            state.animating = false;
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .detach();
        }
        let (measured, animating, epoch) = {
            let state = state.read(cx);
            (state.measured, state.animating, state.epoch)
        };
        let measure = state.clone();
        let mut panel = div()
            .overflow_hidden()
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(content) = bounds.first() else {
                    return;
                };
                let height = f32::from(content.size.height);
                if (measure.read(cx).measured - height).abs() > f32::EPSILON {
                    measure.update(cx, |state, cx| {
                        state.measured = height;
                        if state.animating {
                            cx.notify();
                        }
                    });
                }
            })
            .child(div().children(self.children));
        let panel = if animating {
            let (from, to) = if open { (0., measured) } else { (measured, 0.) };
            panel
                .with_animation(
                    ElementId::NamedInteger(format!("{:?}-panel", self.id).into(), epoch),
                    Animation::new(Duration::from_millis(HEIGHT_TRANSITION_MS))
                        .with_easing(css_ease_out),
                    move |el, delta| el.h(px(from + (to - from) * delta)),
                )
                .into_any_element()
        } else {
            if !open {
                panel = panel.h(px(0.));
            }
            panel.into_any_element()
        };
        div().flex().flex_col().children(self.trigger).child(panel)
    }
}
