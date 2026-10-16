//! Small shared pieces of the Usage page chrome: the rich tooltip, inline
//! text buttons, checkbox menu rows and figure dimming.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Context, CursorStyle, Div, ElementId,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    SharedString, Stateful, Styled as _, Window, canvas, div, px, rems,
};

use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName};

/// A tooltip with arbitrary rows (`TooltipPopup` holding a column of spans):
/// `rounded-md border bg-popover text-xs shadow-md/5 px-2 py-1`.
struct RichTooltip {
    rows: Vec<(SharedString, bool)>,
}

impl Render for RichTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme();
        // Offset from the pointer, as the plain tooltip does.
        div().pl(px(4.)).pt(px(4.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .max_w(rems(20.))
                .rounded(RADIUS_MD)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow(tokens::shadow_md(0.05))
                .px_2()
                .py_1()
                .text_size(TEXT_XS.size)
                .font_family(theme.fonts.sans.clone())
                .text_color(theme.popover_foreground)
                .children(self.rows.iter().map(|(text, primary)| {
                    div()
                        .text_color(if *primary {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(text.clone())
                })),
        )
    }
}

/// `(text, primary)` rows: primary rows read `text-foreground`, the rest muted.
pub fn rich_tooltip(
    rows: Vec<(SharedString, bool)>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| RichTooltip { rows: rows.clone() }).into()
}

/// `inlineButtonVariants`, tone `default`/`muted`: `inline-flex items-center
/// gap-0.5 font-medium hover:underline`. Add children and `.on_click`.
pub fn inline_button(id: impl Into<ElementId>, muted: bool, theme: &AppTheme) -> Stateful<Div> {
    let hover = theme.foreground;
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(2.))
        .font_weight(FontWeight::MEDIUM)
        .cursor(CursorStyle::PointingHand)
        .text_color(if muted {
            theme.muted_foreground
        } else {
            theme.foreground
        })
        .hover(move |style| {
            let style = style.underline();
            if muted {
                style.text_color(hover)
            } else {
                style
            }
        })
}

/// `MenuCheckboxItem` (default variant): `grid-cols-[1rem_minmax(0,1fr)] gap-2
/// min-h-7 rounded-sm py-1 ps-2 pe-4 text-sm`, the check in the first column.
pub fn menu_checkbox_item(
    id: impl Into<ElementId>,
    checked: bool,
    child: impl IntoElement,
    theme: &AppTheme,
) -> Stateful<Div> {
    let accent = theme.accent;
    let accent_fg = theme.accent_foreground;
    div()
        .id(id)
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .gap_2()
        .rounded(px(6.))
        .py_1()
        .pl_2()
        .pr_4()
        .cursor(CursorStyle::PointingHand)
        .text_size(TEXT_SM.size)
        .text_color(theme.foreground)
        .hover(move |style| style.bg(accent).text_color(accent_fg))
        .child(div().size(rems(1.)).flex_shrink_0().when(checked, |this| {
            this.child(Icon::new(IconName::Check).size(rems(1.)))
        }))
        .child(div().min_w_0().flex_1().child(child))
}

/// `MenuSeparator`: `mx-2 my-1 h-px bg-border`.
pub fn menu_separator(theme: &AppTheme) -> Div {
    div().mx_2().my_1().h(px(1.)).bg(theme.border)
}

/// `figureClass(loading)`: a figure still coming in is muted to 40%.
pub fn figure_opacity(loading: bool) -> f32 {
    if loading { 0.4 } else { 1. }
}

/// Captures an element's bounds each frame, for anchoring a popup to it.
#[derive(Clone, Default)]
pub struct BoundsProbe(Rc<Cell<Bounds<Pixels>>>);

impl BoundsProbe {
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.0.get()
    }

    /// An absolutely positioned canvas filling its (relative) parent.
    pub fn element(&self) -> AnyElement {
        let cell = self.0.clone();
        canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
            .absolute()
            .size_full()
            .into_any_element()
    }
}

/// A theme color at `alpha` as `Hsla`.
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}

/// The compact text field (`Input size="compact"`): `h-7 rounded-md border
/// border-input bg-background text-xs`, dark `bg-input/32`, optionally `mono`
/// with tabular digits. The text is gpui-component's input with its own
/// chrome off, so the wrapper supplies the border.
pub fn compact_input(
    state: &gpui_kit::Entity<gpui_kit::component::input::InputState>,
    mono: bool,
    invalid: bool,
    disabled: bool,
    theme: &AppTheme,
) -> Div {
    let border = if invalid {
        with_alpha(theme.destructive, 0.36)
    } else {
        theme.input.into()
    };
    div()
        .flex()
        .w_full()
        .min_w_0()
        .h(px(28.))
        .items_center()
        .rounded(tokens::RADIUS_MD)
        .border_1()
        .border_color(border)
        .bg(if theme.is_dark() {
            with_alpha(theme.input, 0.32)
        } else {
            theme.background.into()
        })
        .shadow(tokens::shadow_xs(0.05))
        .text_size(TEXT_XS.size)
        .text_color(theme.foreground)
        .when(disabled, |this| this.opacity(0.64))
        .when(mono, |this| this.font_family(theme.fonts.mono.clone()))
        .child(
            gpui_kit::component::input::Input::new(state)
                .appearance(false)
                .disabled(disabled),
        )
}

/// `MenuItem`: `min-h-7 rounded-sm px-2 py-1 text-sm`, no check column.
pub fn menu_item(
    id: impl Into<ElementId>,
    child: impl IntoElement,
    theme: &AppTheme,
) -> Stateful<Div> {
    let accent = theme.accent;
    let accent_fg = theme.accent_foreground;
    div()
        .id(id)
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .rounded(px(6.))
        .py_1()
        .px_2()
        .cursor(CursorStyle::PointingHand)
        .text_size(TEXT_SM.size)
        .text_color(theme.foreground)
        .hover(move |style| style.bg(accent).text_color(accent_fg))
        .child(child)
}
