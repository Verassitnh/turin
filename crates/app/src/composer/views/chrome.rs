//! Small building blocks shared by the composer views: the toolbar control
//! look (ComposerControl.tsx), the floating glass layer the pickers and menus
//! open in, and a bounds tracker for anchoring them to their triggers.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    Anchor, AnyElement, App, Bounds, CursorStyle, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Stateful, Styled as _, anchored,
    canvas, deferred, div, point, px, rems,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::tooltip::dropdown_glass_border;
use crate::ui::{Icon, IconName};

/// A trigger's last painted bounds.
pub type BoundsCell = Rc<Cell<Bounds<Pixels>>>;

/// An invisible element that records the bounds of the box it is placed in.
pub fn track_bounds(cell: &BoundsCell) -> impl IntoElement {
    let cell = cell.clone();
    canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().size_full()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ControlSize {
    Sm,
    Xs,
}

/// `composerControlClassName`.
pub fn composer_control(id: impl Into<gpui_kit::ElementId>, size: ControlSize, pressed: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.app_theme();
    let base = div()
        .id(id)
        .relative()
        .flex()
        .h(rems(1.75))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .whitespace_nowrap()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(with_alpha(tokens::tw::BLACK, 0.))
        .cursor(CursorStyle::PointingHand);
    let base = match size {
        ControlSize::Sm => base
            .gap(rems(0.375))
            .px(rems(0.625))
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.secondary_label)
            .hover(|style| style.bg(theme.accent).text_color(theme.foreground)),
        ControlSize::Xs => base
            .gap(rems(0.25))
            .px(rems(0.4375))
            .text_size(TEXT_XS.size)
            .text_color(with_alpha(theme.muted_foreground, 0.7))
            .hover(|style| style.bg(theme.accent).text_color(with_alpha(theme.foreground, 0.8))),
    };
    if pressed { base.bg(theme.accent).text_color(theme.accent_foreground) } else { base }
}

/// `ComposerControlChevron`.
pub fn control_chevron(size: ControlSize, cx: &App) -> impl IntoElement {
    let theme = cx.app_theme();
    match size {
        ControlSize::Sm => div().mx(px(-2.)).child(Icon::new(IconName::ChevronDown).size(rems(0.875)).color(theme.icon_muted)),
        ControlSize::Xs => div().ml_0().mr(px(-4.)).opacity(0.5).child(Icon::new(IconName::ChevronDown).size(rems(0.75)).color(theme.muted_foreground)),
    }
}

/// `ComposerControlSeparator`: a 1px vertical rule, `mx-0.5 h-4`.
pub fn control_separator(size: ControlSize, cx: &App) -> impl IntoElement {
    let theme = cx.app_theme();
    div()
        .mx(px(2.))
        .w(px(1.))
        .h(if size == ControlSize::Xs { px(14.) } else { px(16.) })
        .flex_shrink_0()
        .bg(theme.border)
}

/// The dropdown-glass surface menus and pickers paint.
pub fn glass_surface(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        // GPUI has no backdrop blur, so the glass fill would show the composer
        // text through it; paint it opaque.
        .bg(theme.popover)
        .border_1()
        .border_color(dropdown_glass_border(theme))
        .rounded(RADIUS_LG)
        .shadow(tokens::shadow_popover(theme.is_dark()))
        .text_color(theme.popover_foreground)
        .overflow_hidden()
}

/// Places `content` against a trigger: above or below it, aligned to its
/// start or end edge, kept inside the window.
pub fn floating(trigger: Bounds<Pixels>, above: bool, align_end: bool, content: impl IntoElement) -> AnyElement {
    let (anchor, x) = match (above, align_end) {
        (true, false) => (Anchor::BottomLeft, trigger.left()),
        (true, true) => (Anchor::BottomRight, trigger.right()),
        (false, false) => (Anchor::TopLeft, trigger.left()),
        (false, true) => (Anchor::TopRight, trigger.right()),
    };
    let y = if above { trigger.top() - px(6.) } else { trigger.bottom() + px(6.) };
    deferred(
        anchored()
            .position(point(x, y))
            .anchor(anchor)
            .snap_to_window_with_margin(px(8.))
            .child(div().occlude().child(content)),
    )
    .with_priority(2)
    .into_any_element()
}

/// A row in a menu list: `rounded-sm px-2 py-1 text-sm`, highlighted rows on `bg-accent`.
pub fn menu_row(id: impl Into<gpui_kit::ElementId>, highlighted: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.app_theme();
    let row = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .min_h(rems(2.))
        .px_2()
        .py_1()
        .rounded(px(6.))
        .text_size(TEXT_SM.size)
        .cursor(CursorStyle::PointingHand)
        .hover(|style| style.bg(theme.accent).text_color(theme.accent_foreground));
    if highlighted { row.bg(theme.accent).text_color(theme.accent_foreground) } else { row }
}

pub fn muted_label(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_size(TEXT_XS.size).text_color(cx.app_theme().muted_foreground).child(text.into())
}

pub fn dim(color: Hsla, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}
