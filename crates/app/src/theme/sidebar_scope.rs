//! Port of the `[data-app-sidebar]` token scope in
//! upstream/apps/web/src/index.css: the sidebar container redefines the
//! semantic tokens (`--foreground`, `--muted-foreground`, `--accent`,
//! `--border`, …) and recomputes `--contrast-*` from them, so anything
//! rendered inside the sidebar (`text-muted-foreground`, `border-border`,
//! menus, hover cards) resolves to the sidebar's values.
//!
//! - Standard look (no `data-theme-id`): the literal zinc / near-black set
//!   from the `[data-app-sidebar]` block (index.css:1201-1240).
//! - Themed palettes: `html[data-theme-id] [data-app-sidebar]` maps the
//!   scope back onto the palette roles and tints the container border from
//!   the sidebar foreground (index.css:1491-1523).

use gpui_kit::{Hsla, Rgba};

use super::appearance_contrast::AppearanceContrast;
use super::color::{MixSpace, TRANSPARENT, color, color_mix, hex, over, with_alpha};
use super::theme_palettes::ThemeColors;
use super::tokens::tw;

/// Resolved tokens inside the sidebar. Foregrounds and lines hold the
/// `--contrast-*` value, as `AppTheme` does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidebarScope {
    pub background: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub sidebar: Hsla,
    pub sidebar_foreground: Hsla,
    pub sidebar_muted_foreground: Hsla,
    pub sidebar_control_surface: Hsla,
    pub sidebar_row_hover: Hsla,
    pub sidebar_row_active: Hsla,
    pub sidebar_row_selected: Hsla,
    pub sidebar_border: Hsla,
    /// The container's own `border-color` (its right edge).
    pub container_border: Hsla,
}

/// The raw (pre-contrast) scope values.
struct Raw {
    background: Rgba,
    foreground: Rgba,
    card: Rgba,
    card_foreground: Rgba,
    accent: Rgba,
    accent_foreground: Rgba,
    muted: Rgba,
    muted_foreground: Rgba,
    border: Rgba,
    input: Rgba,
    sidebar: Rgba,
    sidebar_foreground: Rgba,
    sidebar_muted_foreground: Rgba,
    sidebar_control_surface: Rgba,
    sidebar_row_hover: Rgba,
    sidebar_row_active: Rgba,
    sidebar_row_selected: Rgba,
    sidebar_border: Rgba,
}

fn rgba(value: &str) -> Rgba {
    color(value).into()
}

/// `[data-app-sidebar]` for the standard look. Light repeats the root zinc
/// hierarchy; dark is the near-black set. The row surfaces are upstream's
/// flattened capture (`T3_CODE_*_THEME_COLORS`), which equals
/// `color-mix(--contrast-foreground N%, transparent)` over the sidebar.
fn standard(palette: &ThemeColors, dark: bool) -> Raw {
    if dark {
        let foreground = hex(0xf1f3f7);
        Raw {
            background: hex(0x000000),
            foreground,
            card: hex(0x000000),
            card_foreground: foreground,
            accent: hex(0x191a1d),
            accent_foreground: hex(0xf7f9ff),
            muted: hex(0x0a0a0a),
            muted_foreground: hex(0xa3a3a3),
            border: with_alpha(tw::WHITE, 0.08),
            input: with_alpha(tw::WHITE, 0.18),
            // --sidebar: var(--card)
            sidebar: hex(0x000000),
            sidebar_foreground: foreground,
            sidebar_muted_foreground: hex(0xa3a3a3),
            // --sidebar-control-surface: var(--muted)
            sidebar_control_surface: hex(0x0a0a0a),
            sidebar_row_hover: rgba(&palette.sidebar_row_hover),
            sidebar_row_active: rgba(&palette.sidebar_row_active),
            sidebar_row_selected: rgba(&palette.sidebar_row_selected),
            sidebar_border: with_alpha(tw::WHITE, 0.08),
        }
    } else {
        Raw {
            background: tw::ZINC_25,
            foreground: tw::ZINC_800,
            card: tw::WHITE,
            card_foreground: tw::ZINC_800,
            accent: tw::ZINC_100,
            accent_foreground: tw::ZINC_900,
            muted: tw::ZINC_50,
            muted_foreground: tw::ZINC_500,
            border: tw::ZINC_200,
            input: tw::ZINC_300,
            sidebar: tw::ZINC_50,
            sidebar_foreground: tw::ZINC_800,
            sidebar_muted_foreground: tw::ZINC_500,
            sidebar_control_surface: tw::ZINC_100,
            sidebar_row_hover: tw::ZINC_25,
            sidebar_row_active: tw::WHITE,
            sidebar_row_selected: tw::WHITE,
            sidebar_border: tw::ZINC_200,
        }
    }
}

/// `html[data-theme-id] [data-app-sidebar]`: the scope is the palette.
fn themed(p: &ThemeColors) -> Raw {
    Raw {
        background: rgba(&p.canvas),
        foreground: rgba(&p.text),
        card: rgba(&p.surface),
        card_foreground: rgba(&p.text),
        accent: rgba(&p.accent_surface),
        accent_foreground: rgba(&p.accent_surface_foreground),
        muted: rgba(&p.muted),
        muted_foreground: rgba(&p.muted_foreground),
        border: rgba(&p.border),
        input: rgba(&p.input),
        sidebar: rgba(&p.sidebar),
        sidebar_foreground: rgba(&p.sidebar_foreground),
        sidebar_muted_foreground: rgba(&p.sidebar_muted_foreground),
        sidebar_control_surface: rgba(&p.sidebar_control_surface),
        sidebar_row_hover: rgba(&p.sidebar_row_hover),
        sidebar_row_active: rgba(&p.sidebar_row_active),
        sidebar_row_selected: rgba(&p.sidebar_row_selected),
        sidebar_border: rgba(&p.sidebar_border),
    }
}

impl SidebarScope {
    /// `standard`: no `data-theme-id` (the T3 Code palette). `theme_id` picks
    /// out T3 Chat's branded dark divider.
    pub fn resolve(
        palette: &ThemeColors,
        standard_look: bool,
        theme_id: Option<&str>,
        dark: bool,
        contrast: AppearanceContrast,
    ) -> Self {
        let raw = if standard_look {
            standard(palette, dark)
        } else {
            themed(palette)
        };
        let target = if dark { tw::WHITE } else { tw::BLACK };
        // The `:root, [data-app-sidebar]` contrast block, over the scope's
        // own surfaces.
        let fg = |value: Rgba, backdrop: Rgba| -> Hsla {
            contrast.foreground(value, backdrop, target).into()
        };
        let line =
            |value: Rgba, foreground: Rgba| -> Hsla { contrast.border(value, foreground).into() };
        let sidebar_foreground = fg(raw.sidebar_foreground, raw.sidebar);
        let border: Hsla = over(line(raw.border, raw.foreground), raw.background);
        let sidebar_border: Hsla = over(
            line(raw.sidebar_border, raw.sidebar_foreground),
            raw.sidebar,
        );
        let container_border = if standard_look {
            // `border-border` inside the scope.
            border
        } else if dark && theme_id == Some("t3-chat") {
            // T3 Chat keeps its pink divider in dark mode.
            sidebar_border
        } else {
            over(
                color_mix(
                    MixSpace::Srgb,
                    sidebar_foreground,
                    if dark { 0.08 } else { 0.10 },
                    TRANSPARENT,
                ),
                raw.sidebar,
            )
        };
        Self {
            background: raw.background.into(),
            foreground: fg(raw.foreground, raw.background),
            card: raw.card.into(),
            card_foreground: fg(raw.card_foreground, raw.card),
            accent: raw.accent.into(),
            accent_foreground: fg(raw.accent_foreground, raw.accent),
            muted: raw.muted.into(),
            muted_foreground: fg(raw.muted_foreground, raw.background),
            border,
            input: over(line(raw.input, raw.foreground), raw.background),
            sidebar: raw.sidebar.into(),
            sidebar_foreground,
            sidebar_muted_foreground: fg(raw.sidebar_muted_foreground, raw.sidebar),
            sidebar_control_surface: raw.sidebar_control_surface.into(),
            sidebar_row_hover: over(raw.sidebar_row_hover, raw.sidebar),
            sidebar_row_active: over(raw.sidebar_row_active, raw.sidebar),
            sidebar_row_selected: over(raw.sidebar_row_selected, raw.sidebar),
            sidebar_border,
            container_border,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::to_hex;
    use crate::theme::theme_palettes::{OCEAN_THEME, T3_CODE_DARK_THEME_COLORS};

    #[test]
    fn standard_dark_scope_uses_the_sidebar_literals() {
        let scope = SidebarScope::resolve(
            &T3_CODE_DARK_THEME_COLORS,
            true,
            None,
            true,
            AppearanceContrast::default(),
        );
        assert_eq!(to_hex(scope.muted_foreground), "#a3a3a3");
        assert_eq!(to_hex(scope.foreground), "#f1f3f7");
        assert_eq!(to_hex(scope.accent), "#191a1d");
        assert_eq!(to_hex(scope.border), "#141414");
    }

    #[test]
    fn themed_scope_maps_palette_roles_and_tints_the_container() {
        let palette = OCEAN_THEME.colors.clone();
        let scope = SidebarScope::resolve(
            &palette,
            false,
            Some("ocean"),
            false,
            AppearanceContrast::default(),
        );
        assert_eq!(to_hex(scope.foreground), to_hex(color(&palette.text)));
        assert_eq!(
            to_hex(scope.muted_foreground),
            to_hex(color(&palette.muted_foreground))
        );
        assert!((scope.container_border.a - 1.0).abs() < f32::EPSILON);
    }
}
