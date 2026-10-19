//! Port of upstream/apps/web/src/components/ui/sheet.tsx
//!
//! A docked panel over `bg-background/60` (no blur). Right and left are
//! `max-w-md` (28rem) and `w-[calc(100%-3rem)]`. Top and bottom are full width
//! with a hairline. Inset adds `p-4` and `rounded-2xl`. The close control is
//! the ghost icon button at `end-2 top-2`.
//!
//! Not ported: the 200ms translate (no element transforms). `transitionDurationMs`
//! is accepted so callers can request an instant open; the panel still appears
//! in place.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, MouseButton, ParentElement,
    Pixels, RenderOnce, SharedString, Styled, Window, div, px, rems,
};

use super::{Button, ButtonSize, ButtonVariant, IconName};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, tw};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SheetSide {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SheetVariant {
    #[default]
    Default,
    Inset,
}

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

pub fn sheet_width(viewport: Pixels, rem: Pixels) -> Pixels {
    let max = rems(28.).to_pixels(rem);
    let available = viewport - rems(3.).to_pixels(rem);
    if available < max { available } else { max }
}

#[derive(IntoElement)]
pub struct Sheet {
    id: SharedString,
    side: SheetSide,
    variant: SheetVariant,
    show_close_button: bool,
    width: Option<Pixels>,
    top_inset: Option<Pixels>,
    on_close: Dismiss,
    children: Vec<AnyElement>,
}

impl Sheet {
    pub fn new(
        id: impl Into<SharedString>,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            side: SheetSide::Right,
            variant: SheetVariant::Default,
            show_close_button: true,
            width: None,
            top_inset: None,
            on_close: Rc::new(on_close),
            children: Vec::new(),
        }
    }

    pub fn side(mut self, side: SheetSide) -> Self {
        self.side = side;
        self
    }

    pub fn variant(mut self, variant: SheetVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn show_close_button(mut self, show: bool) -> Self {
        self.show_close_button = show;
        self
    }

    /// Overrides the side sheet's `max-w-md` width (the right-panel sheet).
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Drops the panel below the workspace top bar.
    pub fn top_inset(mut self, inset: Pixels) -> Self {
        self.top_inset = Some(inset);
        self
    }
}

impl ParentElement for Sheet {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Sheet {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let rem = window.rem_size();
        let viewport = window.viewport_size();
        let side = self.side;
        let inset = self.variant == SheetVariant::Inset;
        let horizontal = matches!(side, SheetSide::Left | SheetSide::Right);
        let width = self
            .width
            .unwrap_or_else(|| sheet_width(viewport.width, rem));
        let top_inset = self.top_inset.unwrap_or(px(0.));
        let close = self.on_close.clone();
        let dismiss = self.on_close;
        let mut popup = div()
            .id(self.id.clone())
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow(tokens::shadow_lg(0.05))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(self.children);
        popup = match side {
            SheetSide::Right => popup
                .border_l_1()
                .border_color(theme.border)
                .w(width)
                .h_full(),
            SheetSide::Left => popup
                .border_r_1()
                .border_color(theme.border)
                .w(width)
                .h_full(),
            SheetSide::Top => popup.border_b_1().border_color(theme.border).w_full(),
            SheetSide::Bottom => popup.border_t_1().border_color(theme.border).w_full(),
        };
        if inset {
            popup = popup
                .rounded(RADIUS_2XL)
                .border_1()
                .border_color(theme.border);
        }
        if self.show_close_button {
            let id = SharedString::from(format!("{}-close", self.id));
            popup = popup.child(
                div().absolute().right(rems(0.5)).top(rems(0.5)).child(
                    Button::new(id)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Icon)
                        .icon(IconName::X)
                        .on_click(move |_: &ClickEvent, window, cx| close(window, cx)),
                ),
            );
        }
        let mut layer = div()
            .id(SharedString::from(format!("{}-backdrop", self.id)))
            .absolute()
            .top(top_inset)
            .left_0()
            .right_0()
            .bottom_0()
            .bg(with_alpha(
                tw::BLACK,
                if theme.is_dark() { 0.6 } else { 0.4 },
            ))
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx));
        if inset {
            layer = layer.p_4();
        }
        layer = match side {
            SheetSide::Right => layer.flex().justify_end(),
            SheetSide::Left => layer.flex().justify_start(),
            SheetSide::Top => layer.flex().flex_col().justify_start(),
            SheetSide::Bottom => layer.flex().flex_col().justify_end(),
        };
        let _ = horizontal;
        layer.child(popup)
    }
}

pub fn sheet_header(has_panel: bool) -> gpui_kit::Div {
    let header = div().flex().flex_col().gap_2().p(rems(1.5));
    if has_panel {
        header.pb(rems(0.75))
    } else {
        header
    }
}

pub fn sheet_title(text: impl Into<SharedString>) -> gpui_kit::Div {
    div()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_size(tokens::TEXT_XL.size)
        .line_height(gpui_kit::relative(1.))
        .child(text.into())
}

pub fn sheet_description(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    div()
        .text_size(tokens::TEXT_SM.size)
        .line_height(tokens::TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}
