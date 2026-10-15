//! Port of the non-color design tokens in upstream/apps/web/src/index.css
//! (`:root` geometry, `@theme inline` text sizes, radii and shadows) plus the
//! Tailwind v4 defaults those utilities resolve to.
//!
//! Lengths are in rems where upstream uses rems, so they scale with the
//! interface font size exactly like the web app's root `font-size` does
//! (GPUI's rem size is the theme font size). Pixel values stay pixels.

use gpui_kit::{BoxShadow, Hsla, Pixels, Rems, point, px};

use super::color::hex;

// ── Geometry (`:root` in index.css) ─────────────────────────────────────────

/// `--radius: 0.625rem`.
pub const RADIUS: Rems = Rems(0.625);
/// `--radius-sm: calc(var(--radius) - 4px)` at the default 16px rem.
pub const RADIUS_SM: Pixels = px(6.);
/// `--radius-md: calc(var(--radius) - 2px)`.
pub const RADIUS_MD: Pixels = px(8.);
/// `--radius-lg: var(--radius)`.
pub const RADIUS_LG: Pixels = px(10.);
/// `--radius-xl: calc(var(--radius) + 4px)`.
pub const RADIUS_XL: Pixels = px(14.);
/// `--radius-2xl: calc(var(--radius) + 8px)`.
pub const RADIUS_2XL: Pixels = px(18.);
/// `--radius-3xl: calc(var(--radius) + 12px)`.
pub const RADIUS_3XL: Pixels = px(22.);
/// Tailwind `rounded` / `rounded-sm` keyword (0.25rem), used by Kbd.
pub const RADIUS_DEFAULT: Rems = Rems(0.25);

/// Compact UI geometry. Keep these values semantic so sidebar, palette,
/// tooltip, and toolbar controls cannot quietly drift apart.
/// `--control-radius: 0.5rem`.
pub const CONTROL_RADIUS: Rems = Rems(0.5);
/// `--sidebar-content-inset: 0.5rem`.
pub const SIDEBAR_CONTENT_INSET: Rems = Rems(0.5);
/// `--sidebar-control-gap: 0.5rem`.
pub const SIDEBAR_CONTROL_GAP: Rems = Rems(0.5);
/// `--sidebar-row-content-inset: 0.625rem`.
pub const SIDEBAR_ROW_CONTENT_INSET: Rems = Rems(0.625);
/// `--command-shell-inset: 0.5rem`.
pub const COMMAND_SHELL_INSET: Rems = Rems(0.5);
/// `--command-content-inset: 1rem`.
pub const COMMAND_CONTENT_INSET: Rems = Rems(1.0);
/// `--floating-content-inset: 0.75rem`.
pub const FLOATING_CONTENT_INSET: Rems = Rems(0.75);
/// `--app-scrollbar-width: 6px`.
pub const APP_SCROLLBAR_WIDTH: Pixels = px(6.);
/// `--workspace-topbar-height: 52px`.
pub const WORKSPACE_TOPBAR_HEIGHT: Pixels = px(52.);
/// `--workspace-controls-left: … + 0.75rem`.
pub const WORKSPACE_CONTROLS_INSET: Rems = Rems(0.75);
/// `--workspace-titlebar-control-size: 1.75rem`.
pub const WORKSPACE_TITLEBAR_CONTROL_SIZE: Rems = Rems(1.75);
/// `--workspace-titlebar-control-gap: 0.75rem`.
pub const WORKSPACE_TITLEBAR_CONTROL_GAP: Rems = Rems(0.75);
/// `--workspace-titlebar-scroll-fade-height: 1.5rem`.
pub const WORKSPACE_TITLEBAR_SCROLL_FADE_HEIGHT: Rems = Rems(1.5);
/// `--chat-content-max-width: 46rem`.
pub const CHAT_CONTENT_MAX_WIDTH: Rems = Rems(46.);
/// `--thread-details-panel-width: 17.5rem`.
pub const THREAD_DETAILS_PANEL_WIDTH: Rems = Rems(17.5);
/// `--workspace-gutter` (desktop `sm` value: 1.25rem).
pub const WORKSPACE_GUTTER: Rems = Rems(1.25);

// ── Typography (`@theme inline` + Tailwind v4 defaults) ─────────────────────

/// A Tailwind text size: font size and line height, both in rems.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSize {
    pub size: Rems,
    pub line_height: Rems,
}

const fn text(size: f32, line_height: f32) -> TextSize {
    TextSize {
        size: Rems(size),
        line_height: Rems(line_height),
    }
}

/// 7px, provider icon overlays.
pub const TEXT_5XS: TextSize = text(0.4375, 0.4375);
/// 8px, provider icon overlays.
pub const TEXT_4XS: TextSize = text(0.5, 0.5);
/// 10px / 14px. Dense UI: metadata rows, badges, counters.
pub const TEXT_3XS: TextSize = text(0.625, 0.875);
/// 11px / 16px. Dense UI: keyboard hints and diagnostics tables.
pub const TEXT_2XS: TextSize = text(0.6875, 1.0);
/// 12px / 16px.
pub const TEXT_XS: TextSize = text(0.75, 1.0);
/// 14px / 20px.
pub const TEXT_SM: TextSize = text(0.875, 1.25);
/// 16px / 24px.
pub const TEXT_BASE: TextSize = text(1.0, 1.5);
/// 18px / 28px.
pub const TEXT_LG: TextSize = text(1.125, 1.75);
/// 20px / 28px.
pub const TEXT_XL: TextSize = text(1.25, 1.75);

/// Tailwind `leading-snug`.
pub const LEADING_SNUG: f32 = 1.375;
/// Tailwind `leading-relaxed`.
pub const LEADING_RELAXED: f32 = 1.625;

// ── Tailwind v4 palette entries the stylesheet references directly ──────────

pub mod tw {
    use super::hex;
    use gpui_kit::Rgba;

    pub const WHITE: Rgba = hex(0xffffff);
    pub const BLACK: Rgba = hex(0x000000);
    /// `--color-zinc-25: oklch(99.2% 0 0)` (app-specific).
    pub const ZINC_25: Rgba = hex(0xfcfcfc);
    pub const ZINC_50: Rgba = hex(0xfafafa);
    pub const ZINC_100: Rgba = hex(0xf4f4f5);
    pub const ZINC_200: Rgba = hex(0xe4e4e7);
    pub const ZINC_300: Rgba = hex(0xd4d4d8);
    pub const ZINC_500: Rgba = hex(0x71717b);
    pub const ZINC_800: Rgba = hex(0x27272a);
    pub const ZINC_900: Rgba = hex(0x18181b);
    pub const NEUTRAL_100: Rgba = hex(0xf5f5f5);
    pub const NEUTRAL_950: Rgba = hex(0x0a0a0a);
    pub const BLUE_400: Rgba = hex(0x51a2ff);
    pub const BLUE_500: Rgba = hex(0x2b7fff);
    pub const BLUE_700: Rgba = hex(0x1447e6);
    pub const EMERALD_400: Rgba = hex(0x00d492);
    pub const EMERALD_500: Rgba = hex(0x00bc7d);
    pub const EMERALD_700: Rgba = hex(0x007a55);
    pub const RED_400: Rgba = hex(0xff6467);
    pub const RED_600: Rgba = hex(0xe7000b);
    pub const EMERALD_600: Rgba = hex(0x009966);
}

// ── Shadows ─────────────────────────────────────────────────────────────────

fn shadow(y: f32, blur: f32, spread: f32, color: Hsla) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    }
}

fn black(alpha: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: alpha,
    }
}

fn white(alpha: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 1.,
        a: alpha,
    }
}

/// Tailwind `shadow-xs` (`0 1px 2px 0 rgb(0 0 0 / 5%)`), with an alpha
/// override for the `shadow-xs/5` style modifiers.
pub fn shadow_xs(alpha: f32) -> Vec<BoxShadow> {
    vec![shadow(1., 2., 0., black(alpha))]
}

/// Tailwind `shadow-sm`.
pub fn shadow_sm(alpha: f32) -> Vec<BoxShadow> {
    vec![
        shadow(1., 3., 0., black(alpha)),
        shadow(1., 2., -1., black(alpha)),
    ]
}

/// Tailwind `shadow-md`.
pub fn shadow_md(alpha: f32) -> Vec<BoxShadow> {
    vec![
        shadow(4., 6., -1., black(alpha)),
        shadow(2., 4., -2., black(alpha)),
    ]
}

/// Tailwind `shadow-lg`.
pub fn shadow_lg(alpha: f32) -> Vec<BoxShadow> {
    vec![
        shadow(10., 15., -3., black(alpha)),
        shadow(4., 6., -4., black(alpha)),
    ]
}

/// Tailwind `shadow-xl` (`0 20px 25px -5px`, `0 8px 10px -6px`).
pub fn shadow_xl(alpha: f32) -> Vec<BoxShadow> {
    vec![
        shadow(20., 25., -5., black(alpha)),
        shadow(8., 10., -6., black(alpha)),
    ]
}

/// `--shadow-composer` / `--shadow-composer-dark`: the composer's elevation,
/// a soft drop pulled in under the surface.
pub fn shadow_composer(dark: bool) -> Vec<BoxShadow> {
    if dark {
        vec![shadow(14., 32., -18., black(0.75))]
    } else {
        vec![shadow(12., 28., -18., black(0.40))]
    }
}

/// Menus and popovers: `shadow-[0_16px_40px_-18px_rgb(0_0_0/55%)]`, dark
/// `0_18px_44px_-18px_rgb(0_0_0/80%)`.
pub fn shadow_popover(dark: bool) -> Vec<BoxShadow> {
    if dark {
        vec![shadow(18., 44., -18., black(0.80))]
    } else {
        vec![shadow(16., 40., -18., black(0.55))]
    }
}

/// `dialog-glass`: `0 24px 64px -24px rgb(0 0 0 / 65%)`, dark adds an inset
/// highlight and a deeper drop.
pub fn shadow_dialog(dark: bool) -> Vec<BoxShadow> {
    if dark {
        let mut highlight = shadow(1., 0., 0., white(0.04));
        highlight.inset = true;
        vec![highlight, shadow(24., 72., -20., black(0.90))]
    } else {
        vec![shadow(24., 64., -24., black(0.65))]
    }
}

/// `--inset-shadow-control-highlight: 0 1px rgb(255 255 255 / 16%)`, the
/// top edge sheen on solid controls.
pub fn inset_control_highlight() -> BoxShadow {
    let mut highlight = shadow(1., 0., 0., white(0.16));
    highlight.inset = true;
    highlight
}

/// The 1px lip on outline controls and popups: upstream's
/// `before:shadow-[0_1px_--theme(--color-black/4%)]` (light) and
/// `dark:before:shadow-[0_-1px_--theme(--color-white/6%)]` are *outer*
/// shadows of an `inset-0` `::before`, so they paint over the bottom border
/// in light mode and the top border in dark mode. GPUI paints shadows under
/// borders, so the lip is drawn as a 1px overlay line ([`control_lip_overlay`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlLip {
    pub color: Hsla,
    /// `true`: over the top border (dark); `false`: over the bottom border.
    pub top: bool,
}

pub fn control_lip(dark: bool) -> ControlLip {
    if dark {
        ControlLip {
            color: white(0.06),
            top: true,
        }
    } else {
        ControlLip {
            color: black(0.04),
            top: false,
        }
    }
}

/// An absolutely positioned 1px line over the given border edge. Children
/// paint after their parent's border, so it lands above it. Inset by the
/// corner radius so it stays on the straight part of the edge.
pub fn control_lip_overlay(lip: ControlLip, radius: Pixels) -> gpui_kit::Div {
    use gpui_kit::Styled as _;
    let line = gpui_kit::div()
        .absolute()
        .left(radius)
        .right(radius)
        .h(px(1.))
        .bg(lip.color);
    if lip.top {
        line.top(px(-1.))
    } else {
        line.bottom(px(-1.))
    }
}

/// Rems at the default interface size, for code that needs a pixel figure
/// (e.g. resizable panel limits) before a window exists.
pub fn rem_px(value: Rems) -> Pixels {
    px(value.0 * super::appearance_fonts::DEFAULT_INTERFACE_FONT_SIZE as f32)
}
