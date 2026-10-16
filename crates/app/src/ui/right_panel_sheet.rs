//! Port of upstream/apps/web/src/components/RightPanelSheet.tsx
//!
//! Right sheet, no close button, width `min(42vw, 28rem)` and at least 20rem.
//! Below 760px the width is `min(88vw, 24rem)` with no minimum. Turin always
//! draws a 52px titlebar, so the panel starts under it (upstream only does
//! that under the Windows `wco` caption overlay).

use gpui_kit::{AnyElement, App, IntoElement, ParentElement, Pixels, RenderOnce, Window, px, rems};

use super::sheet::Sheet;
use crate::theme::tokens::WORKSPACE_TOPBAR_HEIGHT;

type Dismiss = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

pub fn right_panel_width(viewport: Pixels, rem: Pixels) -> Pixels {
    let narrow = viewport < px(760.);
    if narrow {
        let cap = rems(24.).to_pixels(rem);
        let preferred = viewport * 0.88_f32;
        if preferred < cap { preferred } else { cap }
    } else {
        let cap = rems(28.).to_pixels(rem);
        let floor = rems(20.).to_pixels(rem);
        let preferred = viewport * 0.42_f32;
        let width = if preferred < cap { preferred } else { cap };
        if width < floor { floor } else { width }
    }
}

#[derive(IntoElement)]
pub struct RightPanelSheet {
    open: bool,
    children: Vec<AnyElement>,
    on_close: Dismiss,
}

impl RightPanelSheet {
    pub fn new(on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self {
            open: false,
            children: Vec::new(),
            on_close: std::rc::Rc::new(on_close),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
}

impl ParentElement for RightPanelSheet {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for RightPanelSheet {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        if !self.open {
            return div_empty();
        }
        let width = right_panel_width(window.viewport_size().width, window.rem_size());
        let on_close = self.on_close.clone();
        Sheet::new("right-panel-sheet", move |window, cx| on_close(window, cx))
            .show_close_button(false)
            .width(width)
            .top_inset(WORKSPACE_TOPBAR_HEIGHT)
            .children(self.children)
            .into_any_element()
    }
}

fn div_empty() -> gpui_kit::AnyElement {
    gpui_kit::div().into_any_element()
}
