//! Projects [`AppTheme`] onto gpui-component's `ThemeConfig`, so inputs,
//! menus, popovers, scrollbars and resize handles from the component library
//! wear the upstream palette. No upstream counterpart: on the web every
//! primitive reads the CSS variables directly.
//!
//! Hover/active shades follow the upstream utilities (`hover:bg-primary/90`,
//! `hover:bg-accent`, …) rather than gpui-component's own derivations.

use std::rc::Rc;

use gpui_kit::component::{ThemeConfig, ThemeConfigColors, ThemeMode};
use gpui_kit::{Hsla, SharedString};

use super::AppTheme;
use super::color::{MixSpace, TRANSPARENT, color_mix, to_hex, with_alpha};
use super::theme_palettes::MOBILE_DEFAULT_THEME_ID;
use super::tokens::tw;

fn s(color: Hsla) -> Option<SharedString> {
    Some(to_hex(color).into())
}

/// `bg-x/NN`: the color at a fraction of its opacity.
fn alpha(color: Hsla, fraction: f32) -> Hsla {
    with_alpha(color, fraction).into()
}

/// `color-mix(in srgb, a w, b)`.
fn mix(a: Hsla, weight: f32, b: Hsla) -> Hsla {
    color_mix(MixSpace::Srgb, a, weight, b).into()
}

/// Assigns `field = value` pairs: the config's palette fields are private,
/// so a struct literal cannot be used.
macro_rules! set {
    ($colors:ident, $($field:ident = $value:expr),* $(,)?) => {
        $($colors.$field = $value;)*
    };
}

pub fn theme_config(theme: &AppTheme) -> Rc<ThemeConfig> {
    let dark = theme.is_dark();
    let hover_90 = |color: Hsla| mix(color, 0.9, theme.background);
    let mut colors = ThemeConfigColors::default();
    set!(
        colors,
        background = s(theme.background),
        foreground = s(theme.foreground),
        border = s(theme.border),
        input = s(theme.input),
        ring = s(theme.ring),
        muted = s(theme.muted),
        muted_foreground = s(theme.muted_foreground),
        accent = s(theme.accent),
        accent_foreground = s(theme.accent_foreground),
        popover = s(theme.popover),
        popover_foreground = s(theme.popover_foreground),
        primary = s(theme.primary),
        primary_foreground = s(theme.primary_foreground),
        primary_hover = s(hover_90(theme.primary)),
        primary_active = s(hover_90(theme.primary)),
        secondary = s(theme.secondary),
        secondary_foreground = s(theme.secondary_foreground),
        secondary_hover = s(theme.accent),
        secondary_active = s(theme.accent),
        danger = s(theme.destructive),
        danger_foreground = s(theme.destructive_foreground),
        danger_hover = s(hover_90(theme.destructive)),
        danger_active = s(hover_90(theme.destructive)),
        warning = s(theme.warning),
        warning_foreground = s(theme.warning_foreground),
        warning_hover = s(hover_90(theme.warning)),
        warning_active = s(hover_90(theme.warning)),
        success = s(theme.success),
        success_foreground = s(theme.success_foreground),
        success_hover = s(hover_90(theme.success)),
        success_active = s(hover_90(theme.success)),
        info = s(theme.info),
        info_foreground = s(theme.info_foreground),
        info_hover = s(hover_90(theme.info)),
        info_active = s(hover_90(theme.info)),
        // Outline buttons: `bg-popover`, `hover:bg-accent/50`; dark `bg-input/32`.
        button = s(if dark {
            alpha(theme.input, 0.32)
        } else {
            theme.popover
        }),
        button_foreground = s(theme.foreground),
        button_hover = s(if dark {
            alpha(theme.input, 0.64)
        } else {
            alpha(theme.accent, 0.5)
        }),
        button_active = s(theme.accent),
        button_primary = s(theme.primary),
        button_primary_foreground = s(theme.primary_foreground),
        button_primary_hover = s(hover_90(theme.primary)),
        button_primary_active = s(hover_90(theme.primary)),
        button_secondary = s(theme.secondary),
        button_secondary_foreground = s(theme.secondary_foreground),
        button_secondary_hover = s(mix(theme.secondary, 0.9, theme.background)),
        button_secondary_active = s(mix(theme.secondary, 0.8, theme.background)),
        button_danger = s(theme.destructive),
        button_danger_foreground = s(tw::WHITE.into()),
        button_danger_hover = s(hover_90(theme.destructive)),
        button_danger_active = s(hover_90(theme.destructive)),
        caret = s(theme.foreground),
        selection = s(alpha(theme.theme_accent, if dark { 0.35 } else { 0.25 })),
        link = s(theme.primary),
        link_hover = s(theme.primary),
        link_active = s(theme.primary),
        list = s(theme.background),
        list_hover = s(theme.accent),
        list_active = s(theme.accent),
        list_active_border = s(theme.ring),
        list_even = s(theme.background),
        list_head = s(theme.background),
        sidebar = s(theme.sidebar),
        sidebar_foreground = s(theme.sidebar_foreground),
        sidebar_border = s(theme.sidebar_border),
        sidebar_accent = s(theme.sidebar_row_active),
        sidebar_accent_foreground = s(theme.sidebar_foreground),
        sidebar_primary = s(theme.primary),
        sidebar_primary_foreground = s(theme.primary_foreground),
        title_bar = s(theme.app_chrome_background),
        title_bar_border = s(theme.toolbar_border),
        tab_bar = s(theme.app_chrome_background),
        tab = s(TRANSPARENT.into()),
        tab_active = s(theme.accent),
        tab_foreground = s(theme.muted_foreground),
        tab_active_foreground = s(theme.foreground),
        tab_bar_segmented = s(theme.muted),
        scrollbar = s(TRANSPARENT.into()),
        scrollbar_thumb = s(theme.scrollbar_thumb),
        scrollbar_thumb_hover = s(theme.scrollbar_thumb_hover),
        switch = s(theme.input),
        switch_thumb = s(tw::WHITE.into()),
        slider_bar = s(theme.primary),
        slider_thumb = s(theme.primary_foreground),
        progress_bar = s(theme.primary),
        skeleton = s(theme.muted),
        drag_border = s(theme.ring),
        drop_target = s(alpha(theme.theme_accent, 0.12)),
        overlay = s(alpha(theme.background, if dark { 0.64 } else { 0.60 })),
        window_border = s(theme.border),
        status_bar = s(theme.app_chrome_background),
        status_bar_border = s(theme.border),
        group_box = s(theme.card),
        group_box_foreground = s(theme.card_foreground),
        table = s(theme.background),
        table_head = s(theme.background),
        table_head_foreground = s(theme.muted_foreground),
        table_hover = s(theme.accent),
        table_active = s(theme.accent),
        table_active_border = s(theme.ring),
        table_row_border = s(theme.border),
    );
    Rc::new(ThemeConfig {
        is_default: false,
        name: format!(
            "{} {}",
            theme.theme_id.unwrap_or(MOBILE_DEFAULT_THEME_ID),
            if dark { "dark" } else { "light" }
        )
        .into(),
        mode: if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        font_size: Some(theme.fonts.size_interface),
        font_family: Some(theme.fonts.sans.clone().into()),
        mono_font_family: Some(theme.fonts.mono.clone().into()),
        mono_font_size: Some(theme.fonts.size_code),
        // `--radius-md` drives gpui-component's general radius (inputs,
        // menus); `--radius-lg` its large surfaces (dialogs, popovers).
        radius: Some(8),
        radius_lg: Some(10),
        shadow: Some(true),
        colors,
        highlight: None,
    })
}
