//! The diff surface's colors: `@pierre/diffs`' stylesheet (`dist/style.js`,
//! the `--diffs-*` custom properties) fed with the app's code tokens by
//! `DIFF_SURFACE_THEME_UNSAFE_CSS` (upstream/apps/web/src/lib/diffRendering.ts)
//! and `DIFF_VIEW_UNSAFE_CSS` (components/diffs/StyledDiffCodeView.tsx).
//!
//! CSS resolves these formulas live (`color-mix()`, `light-dark()`); GPUI
//! needs concrete colors, so [`DiffColors::resolve`] evaluates them once per
//! theme. `color-mix(in lab, …)` is CIE Lab, mixed here with [`lab_mix`]
//! (the theme module only has sRGB and Oklab).
//!
//! Port of upstream/apps/web/src/components/diffs/StyledDiffCodeView.tsx (overrides).

use gpui_kit::{Hsla, Rgba};

use crate::theme::AppTheme;
use crate::theme::color::{MixSpace, color_mix, hex};

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    let converted = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    };
    converted.clamp(0., 1.)
}

const WHITE_X: f32 = 0.950_47;
const WHITE_Z: f32 = 1.088_83;

fn lab_f(t: f32) -> f32 {
    const DELTA: f32 = 6. / 29.;
    if t > DELTA * DELTA * DELTA {
        t.cbrt()
    } else {
        t / (3. * DELTA * DELTA) + 4. / 29.
    }
}

fn lab_f_inv(t: f32) -> f32 {
    const DELTA: f32 = 6. / 29.;
    if t > DELTA {
        t * t * t
    } else {
        3. * DELTA * DELTA * (t - 4. / 29.)
    }
}

fn to_lab(color: Rgba) -> [f32; 3] {
    let (r, g, b) = (
        srgb_to_linear(color.r),
        srgb_to_linear(color.g),
        srgb_to_linear(color.b),
    );
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    let (fx, fy, fz) = (lab_f(x / WHITE_X), lab_f(y), lab_f(z / WHITE_Z));
    [116. * fy - 16., 500. * (fx - fy), 200. * (fy - fz)]
}

fn from_lab(lab: [f32; 3], alpha: f32) -> Rgba {
    let fy = (lab[0] + 16.) / 116.;
    let (fx, fz) = (fy + lab[1] / 500., fy - lab[2] / 200.);
    let (x, y, z) = (
        WHITE_X * lab_f_inv(fx),
        lab_f_inv(fy),
        WHITE_Z * lab_f_inv(fz),
    );
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266 * x + 1.876_010_8 * y + 0.041_556 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    Rgba {
        r: linear_to_srgb(r),
        g: linear_to_srgb(g),
        b: linear_to_srgb(b),
        a: alpha,
    }
}

/// `color-mix(in lab, a <weight>, b)`; `weight` is `a`'s share (0–1).
pub fn lab_mix(a: impl Into<Rgba>, weight: f32, b: impl Into<Rgba>) -> Rgba {
    let (a, b) = (a.into(), b.into());
    let (la, lb) = (to_lab(a), to_lab(b));
    let other = 1. - weight;
    from_lab(
        [
            la[0] * weight + lb[0] * other,
            la[1] * weight + lb[1] * other,
            la[2] * weight + lb[2] * other,
        ],
        a.a * weight + b.a * other,
    )
}

fn srgb_mix(a: impl Into<Rgba>, weight: f32, b: impl Into<Rgba>) -> Rgba {
    color_mix(MixSpace::Srgb, a, weight, b)
}

/// Resolved colors of one diff theme (light or dark).
#[derive(Clone, Copy, Debug)]
pub struct DiffColors {
    pub dark: bool,
    /// `--diffs-bg` / `--code-background`.
    pub bg: Hsla,
    /// `--diffs-fg` / `--code-foreground`.
    pub fg: Hsla,
    /// `--diffs-fg-number`: line numbers on context rows.
    pub fg_number: Hsla,
    /// `--diffs-bg-context` (annotations, scrollbar thumbs).
    pub bg_context: Hsla,
    /// `--diffs-bg-context-gutter`: gutter cells beside annotations/buffers.
    pub bg_context_gutter: Hsla,
    /// `--diffs-bg-buffer`: the hatch lines of empty split cells.
    pub bg_buffer: Hsla,
    /// Hunk separator pill (`--diffs-bg-separator` as the app sets it).
    pub bg_separator: Hsla,
    /// Separator pill hover (`color-mix(code-background 92%, code-foreground)`).
    pub bg_separator_hover: Hsla,
    /// Separator text and button icons: `color-mix(fg 50%, bg)`.
    pub fg_separator: Hsla,
    pub addition_base: Hsla,
    pub deletion_base: Hsla,
    pub modified_base: Hsla,
    /// Changed-line backgrounds (`data-line-type=change-*` `[data-line]`).
    pub line_bg_addition: Hsla,
    pub line_bg_deletion: Hsla,
    /// Changed-line gutter backgrounds (`[data-column-number]`).
    pub number_bg_addition: Hsla,
    pub number_bg_deletion: Hsla,
    /// Word-diff emphasis (`--diffs-bg-*-emphasis`).
    pub emphasis_addition: Hsla,
    pub emphasis_deletion: Hsla,
    /// Selected-line backgrounds (`DIFF_VIEW_UNSAFE_CSS`).
    pub selected_line_bg: Hsla,
    pub selected_number_bg: Hsla,
    /// Header hover edge cue: `color-mix(fg 24%, transparent)`.
    pub header_hover_edge: Hsla,
}

impl DiffColors {
    pub fn resolve(theme: &AppTheme) -> Self {
        let dark = theme.is_dark();
        let bg: Rgba = theme.code_background.into();
        let fg: Rgba = theme.code_foreground.into();
        let success: Rgba = theme.success.into();
        let destructive: Rgba = theme.destructive.into();

        // `--diffs-added/deleted/modified-{light,dark}`.
        let (added, deleted, modified) = if dark {
            (hex(0x5ecc71), hex(0xff6762), hex(0x69b1ff))
        } else {
            (hex(0x0dbe4e), hex(0xff2e3f), hex(0x009fff))
        };

        // `--diffs-bg-{addition,deletion}-override` and the `-number-override`
        // variants: sRGB mixes of the code background with `--diff-*`.
        let (row_share, number_share) = if dark { (0.7, 0.6) } else { (0.5, 0.35) };
        let row_target = |tone: Rgba| srgb_mix(bg, row_share, tone);
        let number_target = |tone: Rgba| srgb_mix(bg, number_share, tone);
        // `[data-background]` rules: lab mix of the surface with the target.
        let (line_mix, number_mix) = if dark { (0.8, 0.85) } else { (0.88, 0.91) };
        let line = |tone: Rgba| lab_mix(bg, line_mix, row_target(tone));
        let number = |tone: Rgba| lab_mix(bg, number_mix, number_target(tone));

        // `--diffs-bg-*-emphasis`: the override mixes 80% bg / 20% tone.
        let emphasis_addition = srgb_mix(bg, 0.8, success);
        let emphasis_deletion = srgb_mix(bg, 0.8, destructive);

        // Selection: `color-mix(in lab, bg 88%, color-mix(bg 50%, modified))`
        // light, `80% / 70%` dark; numbers `91% / 35%` light, `85% / 60%` dark.
        let selected_line_bg = if dark {
            lab_mix(bg, 0.8, srgb_mix(bg, 0.7, modified))
        } else {
            lab_mix(bg, 0.88, srgb_mix(bg, 0.5, modified))
        };
        let selected_number_bg = if dark {
            lab_mix(bg, 0.85, srgb_mix(bg, 0.6, modified))
        } else {
            lab_mix(bg, 0.91, srgb_mix(bg, 0.35, modified))
        };

        Self {
            dark,
            bg: bg.into(),
            fg: fg.into(),
            fg_number: lab_mix(fg, 0.65, bg).into(),
            bg_context: srgb_mix(bg, 0.97, fg).into(),
            bg_context_gutter: lab_mix(srgb_mix(bg, 0.97, fg), if dark { 0.45 } else { 0.9 }, bg)
                .into(),
            bg_buffer: srgb_mix(bg, 0.9, fg).into(),
            bg_separator: srgb_mix(bg, 0.97, fg).into(),
            bg_separator_hover: srgb_mix(bg, 0.92, fg).into(),
            fg_separator: srgb_mix(fg, 0.5, bg).into(),
            addition_base: added.into(),
            deletion_base: deleted.into(),
            modified_base: modified.into(),
            line_bg_addition: line(success).into(),
            line_bg_deletion: line(destructive).into(),
            number_bg_addition: number(success).into(),
            number_bg_deletion: number(destructive).into(),
            emphasis_addition: emphasis_addition.into(),
            emphasis_deletion: emphasis_deletion.into(),
            selected_line_bg: selected_line_bg.into(),
            selected_number_bg: selected_number_bg.into(),
            header_hover_edge: crate::theme::color::with_alpha(fg, 0.24).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_mix_endpoints_and_midpoint() {
        let a = hex(0xff0000);
        let b = hex(0x0000ff);
        let near = |x: Rgba, y: Rgba| {
            (x.r - y.r).abs() < 0.01 && (x.g - y.g).abs() < 0.01 && (x.b - y.b).abs() < 0.01
        };
        assert!(near(lab_mix(a, 1., b), a));
        assert!(near(lab_mix(a, 0., b), b));
        // CIE Lab midpoint of red and blue is a purple, not sRGB's #800080.
        let mid = lab_mix(a, 0.5, b);
        assert!(mid.r > 0.6 && mid.b > 0.5 && mid.g < 0.1, "{mid:?}");
    }

    #[test]
    fn lab_mix_with_white_matches_a_tint() {
        let tint = lab_mix(hex(0xffffff), 0.88, hex(0x00bc7d));
        assert!(tint.g > tint.r && tint.g > 0.9, "{tint:?}");
    }
}
