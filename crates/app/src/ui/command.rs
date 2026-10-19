//! Port of upstream/apps/web/src/components/ui/command.tsx (and the dialog
//! surface it sits on, ui/dialog-styles.ts).
//!
//! The command palette's chrome: the `dialog-glass` popup and its backdrop,
//! the input row, group headings, result rows, the shortcut label and the
//! footer with its keyboard hints. Behaviour (filtering, highlight, scrolling)
//! belongs to the palette; Base UI's Autocomplete has no counterpart here.
//! GPUI has no backdrop blur, so the glass layers are translucent fills only.

use crate::theme::color::solid_over;
use gpui_kit::{
    AnyElement, App, Div, FontWeight, Hsla, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, px, rems,
};

use super::icon::{Icon, IconName};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    self, COMMAND_CONTENT_INSET, COMMAND_SHELL_INSET, RADIUS_2XL, RADIUS_SM, TEXT_SM, TEXT_XS, tw,
};
use crate::theme::{ActiveAppTheme, AppTheme};

/// Dialog scrim: a flat dark overlay (no blur, no glass).
pub fn dialog_backdrop(theme: &AppTheme) -> Hsla {
    with_alpha(tw::BLACK, if theme.is_dark() { 0.6 } else { 0.4 }).into()
}

/// Opaque popup surface (user request: no translucent surfaces).
pub fn dialog_glass_background(theme: &AppTheme) -> Hsla {
    theme.background
}

/// `dialog-glass` border: `--contrast-foreground` at 10%, white at 8% in dark.
pub fn dialog_glass_border(theme: &AppTheme) -> Hsla {
    theme.border
}

/// `CommandDialogPopup`: `dialog-glass rounded-2xl border`, `max-w-xl`
/// (36rem), `max-h-105` (26.25rem), `overflow-hidden`.
pub fn command_dialog_popup(theme: &AppTheme) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .w_full()
        .min_w_0()
        .max_w(rems(36.))
        .max_h(rems(26.25))
        .overflow_hidden()
        .rounded(RADIUS_2XL)
        .border_1()
        .border_color(dialog_glass_border(theme))
        .bg(dialog_glass_background(theme))
        .text_color(theme.foreground)
        .shadow(tokens::shadow_dialog(theme.is_dark()))
}

/// `CommandInput`'s wrapper: `px-[var(--command-shell-inset)] py-1.5`, the
/// input itself `h-8.5` with `border-transparent bg-transparent`, a
/// `size-4` search icon (`translate-x-0.5`, `text-icon-muted`) and `ps-8`.
pub fn command_input_row(
    start_addon: Option<AnyElement>,
    input: impl IntoElement,
    accessory: Option<AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.app_theme();
    let addon = start_addon.unwrap_or_else(|| {
        div()
            .ml(px(2.))
            .child(
                Icon::new(IconName::Search)
                    .size(rems(1.))
                    .color(theme.icon_muted),
            )
            .into_any_element()
    });
    div()
        .relative()
        .px(COMMAND_SHELL_INSET)
        .py(rems(0.375))
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .w_full()
                .h(rems(2.125))
                .text_color(theme.foreground)
                .child(
                    // `start-px ps-[calc(--spacing(2)+0.0625rem)]`, opacity-80.
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(1.))
                        .flex()
                        .items_center()
                        .pl(rems(0.5625))
                        .opacity(0.8)
                        .child(addon),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(rems(COMMAND_SHELL_INSET.0 + 1.5))
                        .text_size(TEXT_SM.size)
                        .child(input),
                ),
        )
        .children(accessory)
}

/// `CommandPanel`: `relative min-h-0 overflow-hidden rounded-t-xl`.
pub fn command_panel() -> Div {
    div().relative().min_h_0().overflow_hidden()
}

/// Group heading (`GROUP_LABEL_CLASS_NAME`): `px-2 py-1.5 font-medium
/// text-muted-foreground text-xs`.
pub fn command_list_heading(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_2()
        .py(rems(0.375))
        .font_weight(FontWeight::MEDIUM)
        .text_size(TEXT_XS.size)
        .text_color(cx.app_theme().muted_foreground)
        .child(label.into())
}

/// `CommandItem` row frame: `flex min-h-7 items-center gap-2 rounded-sm px-2
/// py-1.5 text-sm`; the highlighted (`active`) row takes `bg-accent
/// text-accent-foreground`.
pub fn command_item_frame(active: bool, disabled: bool, cx: &App) -> Div {
    let theme = cx.app_theme();
    let mut frame = div()
        .flex()
        .min_h(rems(1.75))
        .items_center()
        .gap_2()
        .rounded(RADIUS_SM)
        .px_2()
        .py(rems(0.375))
        .text_size(TEXT_SM.size)
        .text_color(theme.foreground);
    if active {
        frame = frame.bg(theme.accent).text_color(theme.accent_foreground);
    }
    if disabled {
        frame = frame.opacity(0.64);
    }
    frame
}

/// `CommandShortcut`: `ms-auto font-medium font-sans text-secondary-label
/// text-xs tracking-widest`.
#[derive(IntoElement)]
pub struct CommandShortcut(SharedString);

impl CommandShortcut {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self(label.into())
    }
}

impl RenderOnce for CommandShortcut {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        div()
            .ml_auto()
            .flex_shrink_0()
            .font_family(theme.fonts.sans.clone())
            .font_weight(FontWeight::MEDIUM)
            .text_size(TEXT_XS.size)
            .text_color(theme.secondary_label)
            .child(self.0)
    }
}

/// `CommandFooter`: `flex items-center justify-between gap-2
/// bg-foreground/[0.025] px-[var(--command-content-inset)] py-2.5
/// font-medium text-sm text-muted-foreground`, with its kbd chips on
/// `bg-foreground/[0.08] text-foreground`.
pub fn command_footer(cx: &App) -> Div {
    let theme = cx.app_theme();
    div()
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px(COMMAND_CONTENT_INSET)
        .py(rems(0.625))
        .bg(solid_over(theme.foreground, 0.025, theme.background))
        .font_family(theme.fonts.sans.clone())
        .font_weight(FontWeight::MEDIUM)
        .text_size(TEXT_SM.size)
        .text_color(theme.muted_foreground)
}

/// One footer hint: `<KbdGroup><Kbd>…</Kbd><span>label</span></KbdGroup>`,
/// with the footer's kbd restyle (`bg-foreground/[0.08] text-foreground`).
pub fn command_footer_hint(keys: Vec<FooterKey>, label: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.app_theme();
    let mut group = div().flex().items_center().gap_1();
    for key in keys {
        group = group.child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .gap_1()
                .h(rems(1.25))
                .min_w(rems(1.25))
                .px_1()
                .rounded(tokens::RADIUS_DEFAULT)
                .bg(solid_over(theme.foreground, 0.08, theme.background))
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground)
                .child(match key {
                    FooterKey::Text(text) => div().child(text).into_any_element(),
                    FooterKey::Icon(name) => Icon::new(name)
                        .size(rems(0.75))
                        .color(theme.foreground)
                        .into_any_element(),
                }),
        );
    }
    group.child(label.into())
}

pub enum FooterKey {
    Text(SharedString),
    Icon(IconName),
}
