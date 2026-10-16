//! Port of upstream/apps/web/src/components/ui/switch.tsx
//!
//! Desktop thumb sizes: default `--spacing(4)` (16px), `sm` `--spacing(3.5)`
//! (14px). Track is `thumb + 2px` tall and `thumb * 2 - 2px` wide, padded 2px.
//! Checked track is `bg-primary`; unchecked and mixed are `bg-input`. The
//! mixed thumb sits in the center at 70% opacity. Disabled is `opacity-64`.
//!
//! Not ported: the focus ring, the active thumb stretch (`scale-x-110`), and
//! the 200ms color transition. gpui-component's `Switch` uses its own size
//! ladder, so this is drawn from primitives.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, CursorStyle, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, RenderOnce, StatefulInteractiveElement, Styled, Window, div, px, rems,
};

use crate::components::drag_region::no_drag;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SwitchSize {
    #[default]
    Default,
    Sm,
}

#[derive(Clone, Copy, Debug)]
struct SwitchMetrics {
    track: Pixels,
    track_width: Pixels,
    knob: Pixels,
    travel: Pixels,
}

fn metrics(size: SwitchSize, rem: Pixels) -> SwitchMetrics {
    let thumb = match size {
        SwitchSize::Default => rems(1.).to_pixels(rem),
        SwitchSize::Sm => rems(0.875).to_pixels(rem),
    };
    SwitchMetrics {
        track: thumb + px(2.),
        track_width: thumb + thumb - px(2.),
        knob: thumb - px(2.),
        travel: thumb - px(4.),
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    mixed: bool,
    disabled: bool,
    size: SwitchSize,
    on_click: Option<ClickHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            mixed: false,
            disabled: false,
            size: SwitchSize::Default,
            on_click: None,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// macOS mixed state: thumb centered on the muted track.
    pub fn mixed(mut self, mixed: bool) -> Self {
        self.mixed = mixed;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn size(mut self, size: SwitchSize) -> Self {
        self.size = size;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let metrics = metrics(self.size, window.rem_size());
        let track: Hsla = if self.checked && !self.mixed {
            theme.primary
        } else {
            theme.input
        };
        let offset = if self.mixed {
            metrics.travel * 0.5_f32
        } else if self.checked {
            metrics.travel
        } else {
            px(0.)
        };
        let interactive = !self.disabled;
        let mut root = div()
            .id(self.id)
            .on_mouse_down(MouseButton::Left, no_drag)
            .flex()
            .flex_shrink_0()
            .items_center()
            .h(metrics.track)
            .w(metrics.track_width)
            .p(px(2.))
            .rounded_full()
            .bg(track)
            .when(!interactive, |this| this.opacity(0.64))
            .when(interactive, |this| this.cursor(CursorStyle::PointingHand))
            .child(
                div()
                    .size(metrics.knob)
                    .ml(offset)
                    .rounded_full()
                    .bg(theme.background)
                    .shadow(tokens::shadow_sm(0.05))
                    .when(self.mixed, |this| this.opacity(0.7)),
            );
        if let Some(on_click) = self.on_click.filter(|_| interactive) {
            root = root.on_click(move |event, window, cx| on_click(event, window, cx));
        }
        root
    }
}
