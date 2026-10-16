//! Port of upstream/apps/web/src/components/ui/dialog.tsx (+ dialog-styles.ts)
//!
//! The modal is gpui-component's dialog host (window-level overlay, focus
//! trap, Escape), restyled to `dialog-glass rounded-2xl border` with no
//! padding of its own: the content view lays out its own
//! `DialogHeader` / `DialogPanel` / `DialogFooter`, so the header and footer
//! stay put while the panel scrolls. Differences from upstream: no backdrop
//! blur (GPUI has none, so the popup fill is opaque), no scale/opacity
//! transition, and the popup sits a tenth of the way down rather than
//! centered (its height is not known when it opens).

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyElement, AnyView, App, ClickEvent, Div, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Rems, SharedString, Styled as _, Window,
    div, px, relative, rems,
};

use super::button::{Button, ButtonSize, ButtonVariant};
use super::icon::{Icon, IconName};
use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM, TEXT_XL};
use crate::theme::{ActiveAppTheme, AppTheme};

/// `max-w-lg`: the default popup width.
pub const DIALOG_DEFAULT_MAX_WIDTH: Rems = Rems(32.);

#[derive(Clone, Copy, Debug)]
pub struct DialogOptions {
    pub max_width: Rems,
    /// Escape and a click on the backdrop close it. The wizard disables both
    /// (`disablePointerDismissal`, `event.cancel()`).
    pub dismissable: bool,
    /// The top-right close button (`showCloseButton`).
    pub show_close_button: bool,
}

impl Default for DialogOptions {
    fn default() -> Self {
        Self {
            max_width: DIALOG_DEFAULT_MAX_WIDTH,
            dismissable: true,
            show_close_button: true,
        }
    }
}

/// Opens `content` as a modal. The content view draws the popup interior
/// (see [`dialog_popup`]).
pub fn open_dialog(
    window: &mut Window,
    cx: &mut App,
    options: DialogOptions,
    content: impl Into<AnyView>,
) {
    let content: AnyView = content.into();
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.app_theme().clone();
        let width = options.max_width.to_pixels(window.rem_size());
        dialog
            .w(width)
            .p_0()
            .bg(theme.background)
            .border_color(dialog_border(&theme))
            .rounded(RADIUS_2XL)
            .shadow(tokens::shadow_dialog(theme.is_dark()))
            .close_button(false)
            .keyboard(options.dismissable)
            .overlay_closable(options.dismissable)
            .child(content.clone())
    });
}

/// `border-color: contrast-foreground 10%` (dark: white 8%).
pub fn dialog_border(theme: &AppTheme) -> gpui_kit::Hsla {
    if theme.is_dark() {
        with_alpha(tokens::tw::WHITE, 0.08).into()
    } else {
        with_alpha(theme.foreground, 0.10).into()
    }
}

/// The popup interior: a column bounded to the window so the panel can scroll.
pub fn dialog_popup(window: &Window, show_close_button: bool) -> Div {
    let max_height = window.viewport_size().height * 0.8;
    let mut popup = div()
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .max_h(max_height);
    if show_close_button {
        popup = popup.child(dialog_close_button());
    }
    popup
}

/// `absolute end-2 top-2` ghost icon button.
pub fn dialog_close_button() -> AnyElement {
    div()
        .absolute()
        .right_2()
        .top_2()
        .child(
            Button::new("dialog-close")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Icon)
                .icon(IconName::Close)
                .on_click(|_, window, cx| window.close_dialog(cx)),
        )
        .into_any_element()
}

/// `DialogHeader`: `flex flex-col gap-2 p-6`, `pb-3` when a panel follows.
pub fn dialog_header(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    with_panel: bool,
    theme: &AppTheme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_2()
        .p_6()
        .when_panel(with_panel)
        .child(
            div()
                .text_size(rems(1.25))
                .line_height(relative(1.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(description.map(|description| {
            div()
                .text_size(TEXT_SM.size)
                .line_height(TEXT_SM.line_height)
                .text_color(theme.muted_foreground)
                .child(description)
        }))
}

trait PanelPadding {
    fn when_panel(self, with_panel: bool) -> Self;
}

impl PanelPadding for Div {
    fn when_panel(self, with_panel: bool) -> Self {
        if with_panel { self.pb_3() } else { self }
    }
}

/// `DialogFooter`: `px-6`, `variant="default"` adds `border-t bg-muted/72 py-4`
/// and the popup's bottom radius, `bare` only `py-4`. Actions sit at the end.
pub fn dialog_footer(theme: &AppTheme, bare: bool) -> Div {
    let footer = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_end()
        .gap_2()
        .px_6()
        .py_4();
    if bare {
        footer
    } else {
        footer
            .border_t_1()
            .border_color(theme.border)
            .bg(with_alpha(theme.muted, 0.72))
            .rounded_b(px(17.))
    }
}

/// `DialogPanel`: the scrolling body, `space-y-4 p-6` (`pt-1` after a header,
/// `pb-1` before a bare footer).
pub fn dialog_panel(
    id: &'static str,
    after_header: bool,
    before_bare_footer: bool,
) -> gpui_kit::Stateful<Div> {
    use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};
    let mut panel = div()
        .id(id)
        .flex()
        .flex_col()
        .flex_auto()
        .min_h_0()
        .overflow_y_scroll()
        .gap_4()
        .px_6()
        .pt_6()
        .pb_6();
    if after_header {
        panel = panel.pt_1();
    }
    if before_bare_footer {
        panel = panel.pb_1();
    }
    panel
}

/// Convenience for icon-only close affordances elsewhere in dialogs.
pub fn close_icon() -> Icon {
    Icon::new(IconName::Close)
}

// --- In-window overlay dialogs (git / pull request), from main's dialog.tsx port ---

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// `dialog-backdrop`: `color-mix(in srgb, var(--background) 60%, transparent)`, 64% in dark.
pub fn dialog_backdrop_color(theme: &AppTheme) -> Hsla {
    with_alpha(theme.background, if theme.is_dark() { 0.64 } else { 0.6 }).into()
}

fn dialog_surface(theme: &AppTheme) -> Hsla {
    let glass = with_alpha(theme.background, 0.8);
    color_mix(MixSpace::Srgb, theme.background, 0.8, glass).into()
}

/// Full-window overlay with centred popup (pull request link, thread picker, etc.).
pub fn dialog_overlay(
    id: impl Into<SharedString>,
    max_width: Pixels,
    on_dismiss: Dismiss,
    children: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let dismiss_backdrop = on_dismiss.clone();
    let dismiss_close = on_dismiss;
    let id: SharedString = id.into();
    div()
        .id(SharedString::from(format!("{id}-backdrop")))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .bg(dialog_backdrop_color(theme))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            dismiss_backdrop(window, cx)
        })
        .child(
            div()
                .id(id)
                .relative()
                .flex()
                .flex_col()
                .min_h_0()
                .w_full()
                .min_w_0()
                .max_w(max_width)
                .max_h(relative(1.))
                .rounded(RADIUS_2XL)
                .border_1()
                .border_color(dialog_border(theme))
                .bg(dialog_surface(theme))
                .text_color(theme.popover_foreground)
                .shadow(tokens::shadow_dialog(theme.is_dark()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(children)
                .child(
                    div().absolute().right(rems(0.5)).top(rems(0.5)).child(
                        Button::new("dialog-close")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Icon)
                            .icon(IconName::X)
                            .on_click(move |_: &ClickEvent, window, cx| dismiss_close(window, cx)),
                    ),
                ),
        )
        .into_any_element()
}

/// Empty header shell for overlay dialogs (`DialogHeader` without title helpers).
pub fn overlay_dialog_header(has_panel: bool) -> Div {
    let header = div().flex().flex_col().gap_2().p(rems(1.5));
    if has_panel {
        header.pb(rems(0.75))
    } else {
        header
    }
}

/// `DialogTitle`: `font-semibold text-xl leading-none`.
pub fn dialog_title(text: impl Into<SharedString>) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(TEXT_XL.size)
        .line_height(relative(1.))
        .child(text.into())
}

/// `DialogDescription`: `text-muted-foreground text-sm`.
pub fn dialog_description(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(cx.app_theme().muted_foreground)
        .child(text.into())
}

/// Scroll-free body section for overlay dialogs.
pub fn overlay_dialog_panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .px(rems(1.5))
        .pb(rems(1.5))
        .pt(px(4.))
}

/// Bordered footer for overlay dialogs.
pub fn overlay_dialog_footer(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        .flex()
        .justify_end()
        .gap_2()
        .px(rems(1.5))
        .py_4()
        .border_t_1()
        .border_color(theme.border)
        .bg(with_alpha(theme.muted, 0.72))
        .rounded_b(px(17.))
}
