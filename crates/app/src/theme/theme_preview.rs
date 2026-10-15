//! Port of upstream/packages/shared/src/themePreview.ts
//!
//! Preview-orb data for the Settings → Appearance theme picker. The oklab
//! math (`parseOklab`, `oklabToHex`) lives in [`super::color`].

use super::color::{Oklab, oklab_to_rgba, parse_color, rgba_to_oklab, to_hex};
use super::theme_palettes::ThemeAppearance;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemePreviewColors {
    pub canvas: &'static str,
    pub accent: &'static str,
    pub message_action: &'static str,
}

/// The standard T3 Code artwork is not a built-in theme, so its preview colors live here.
pub fn standard_theme_preview_colors(appearance: ThemeAppearance) -> ThemePreviewColors {
    match appearance {
        ThemeAppearance::Light => ThemePreviewColors {
            canvas: "#fcfcfc",
            accent: "#f4f4f5",
            message_action: "#4f46e5",
        },
        ThemeAppearance::Dark => ThemePreviewColors {
            canvas: "#0a0a0a",
            accent: "#1c1c1f",
            message_action: "#8b9cff",
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemePreviewAccentSpec {
    pub center: (f32, f32),
    pub middle_offset: f32,
    pub middle_opacity: f32,
    pub end_offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemePreviewActionSpec {
    pub center: (f32, f32),
    pub start_opacity: f32,
    pub end_offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemePreviewRenderSpec {
    pub base_target: &'static str,
    pub base_weight: f32,
    pub accent: ThemePreviewAccentSpec,
    pub action: ThemePreviewActionSpec,
    pub scale: f32,
    pub blur_at_56_px: f32,
}

/// Shared geometry and falloff for the web and native theme preview orbs.
pub fn theme_preview_render_spec(appearance: ThemeAppearance) -> ThemePreviewRenderSpec {
    match appearance {
        ThemeAppearance::Light => ThemePreviewRenderSpec {
            base_target: "#ffffff",
            base_weight: 0.8,
            accent: ThemePreviewAccentSpec {
                center: (0.72, 0.22),
                middle_offset: 0.28,
                middle_opacity: 0.72,
                end_offset: 0.58,
            },
            action: ThemePreviewActionSpec {
                center: (0.18, 0.82),
                start_opacity: 0.45,
                end_offset: 0.55,
            },
            scale: 1.1,
            blur_at_56_px: 3.,
        },
        ThemeAppearance::Dark => ThemePreviewRenderSpec {
            base_target: "#09090b",
            base_weight: 0.8,
            accent: ThemePreviewAccentSpec {
                center: (0.28, 0.78),
                middle_offset: 0.28,
                middle_opacity: 0.62,
                end_offset: 0.58,
            },
            action: ThemePreviewActionSpec {
                center: (0.82, 0.18),
                start_opacity: 0.45,
                end_offset: 0.55,
            },
            scale: 1.1,
            blur_at_56_px: 3.,
        },
    }
}

pub fn mix_theme_preview_base(colors: &ThemePreviewColors, mode: ThemeAppearance) -> String {
    let spec = theme_preview_render_spec(mode);
    let (Some(canvas), Some(target)) = (parse_color(colors.canvas), parse_color(spec.base_target))
    else {
        return colors.canvas.to_string();
    };
    let (canvas, target) = (rgba_to_oklab(canvas), rgba_to_oklab(target));
    let target_weight = 1. - spec.base_weight;
    to_hex(oklab_to_rgba(
        Oklab {
            l: canvas.l * spec.base_weight + target.l * target_weight,
            a: canvas.a * spec.base_weight + target.a * target_weight,
            b: canvas.b * spec.base_weight + target.b * target_weight,
        },
        1.,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_mix_moves_the_canvas_toward_the_target() {
        let light = mix_theme_preview_base(
            &standard_theme_preview_colors(ThemeAppearance::Light),
            ThemeAppearance::Light,
        );
        // #fcfcfc 80% toward white stays near-white and lighter than the canvas.
        assert!(light.as_str() >= "#fcfcfc" && light.as_str() <= "#ffffff");
        let unparsable = ThemePreviewColors {
            canvas: "nope",
            accent: "#000000",
            message_action: "#000000",
        };
        assert_eq!(
            mix_theme_preview_base(&unparsable, ThemeAppearance::Dark),
            "nope"
        );
    }
}
