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

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyElement, AnyView, App, Div, FontWeight, IntoElement, ParentElement as _, Rems, SharedString,
    Styled as _, Window, div, px, relative, rems,
};

use super::button::{Button, ButtonSize, ButtonVariant};
use super::icon::{Icon, IconName};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_2XL, TEXT_SM};
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
