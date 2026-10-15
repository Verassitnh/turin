//! Port of upstream/apps/web/src/appearanceContrast.ts and the `--contrast-*`
//! token formulas in upstream/apps/web/src/index.css.
//!
//! Upstream writes three CSS variables and lets `color-mix()` recompute every
//! contrast token. Here the same formulas run once when the theme resolves.

use gpui_kit::Rgba;

use super::color::{MixSpace, TRANSPARENT, color_mix};

/// Upstream contracts/settings.ts.
pub const MIN_APPEARANCE_CONTRAST: u32 = 50;
pub const MAX_APPEARANCE_CONTRAST: u32 = 200;
pub const DEFAULT_APPEARANCE_CONTRAST: u32 = 100;

/// The three knobs `applyAppearanceContrast` writes, as 0–1 fractions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppearanceContrast {
    /// `--appearance-contrast-base`: `min(contrast, 100)%`.
    pub base: f32,
    /// `--appearance-contrast-boost`: `max(contrast - 100, 0)%`.
    pub boost: f32,
    /// `--appearance-contrast-border-boost`: a quarter of the boost.
    pub border_boost: f32,
}

impl AppearanceContrast {
    /// Takes a `u32` and clamps it to `50..=200`. Upstream's `AppearanceContrast`
    /// is a `Schema.Int` range-checked at decode (contracts/settings.ts), so
    /// settings decode must reject out-of-range values before they reach here.
    /// Schema-valid inputs behave identically.
    pub fn new(contrast: u32) -> Self {
        let contrast = contrast.clamp(MIN_APPEARANCE_CONTRAST, MAX_APPEARANCE_CONTRAST) as f32;
        let boost = (contrast - 100.).max(0.);
        Self {
            base: contrast.min(100.) / 100.,
            boost: boost / 100.,
            border_boost: boost / 4. / 100.,
        }
    }

    /// Text tokens: `color-mix(in oklab, color-mix(in oklab, fg base, backdrop),
    /// target boost)`, where the target is black in light mode and white in dark.
    pub fn foreground(&self, color: Rgba, backdrop: Rgba, target: Rgba) -> Rgba {
        let based = color_mix(MixSpace::Oklab, color, self.base, backdrop);
        color_mix(MixSpace::Oklab, target, self.boost, based)
    }

    /// Hairlines: `color-mix(in srgb, color-mix(in srgb, border base,
    /// transparent), foreground border-boost)`.
    pub fn border(&self, color: Rgba, foreground: Rgba) -> Rgba {
        let based = color_mix(MixSpace::Srgb, color, self.base, TRANSPARENT);
        color_mix(MixSpace::Srgb, foreground, self.border_boost, based)
    }
}

impl Default for AppearanceContrast {
    fn default() -> Self {
        Self::new(DEFAULT_APPEARANCE_CONTRAST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::{hex, to_hex};

    fn knobs(contrast: u32) -> (f32, f32, f32) {
        let c = AppearanceContrast::new(contrast);
        // Upstream writes percentages; compare at that precision.
        let pct = |v: f32| (v * 10000.).round() / 100.;
        (pct(c.base), pct(c.boost), pct(c.border_boost))
    }

    // describe("applyAppearanceContrast")

    #[test]
    fn boosts_semantic_contrast_above_the_default() {
        assert_eq!(knobs(135), (100., 35., 8.75));
    }

    #[test]
    fn supports_the_maximum_contrast_boost() {
        assert_eq!(knobs(200), (100., 100., 25.));
    }

    #[test]
    fn softens_semantic_contrast_below_the_default() {
        assert_eq!(knobs(70), (70., 0., 0.));
    }

    #[test]
    fn disables_contrast_mixing_at_the_default() {
        assert_eq!(knobs(100), (100., 0., 0.));
    }

    // Turin: the resolved color-mix (no upstream counterpart).

    #[test]
    fn default_contrast_is_identity() {
        let contrast = AppearanceContrast::default();
        let fg = hex(0x27272a);
        let out = contrast.foreground(fg, hex(0xfcfcfc), hex(0x000000));
        assert_eq!(to_hex(out), "#27272a");
        assert_eq!(to_hex(contrast.border(hex(0xe4e4e7), fg)), "#e4e4e7");
    }
}
