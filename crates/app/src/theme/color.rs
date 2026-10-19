//! Color math shared by the theme modules: the oklab conversions from
//! upstream/packages/shared/src/themePreview.ts (`parseOklab`, `oklabToHex`;
//! the rest of that file is `theme_preview.rs`), extended with the CSS `color-mix()` the web
//! stylesheet (upstream/apps/web/src/index.css) leans on for derived tokens.
//!
//! Upstream lets the browser resolve `oklch(...)`, `color-mix(...)` and alpha
//! colors; GPUI wants concrete [`Rgba`], so everything resolves here.

use gpui_kit::{Hsla, Rgba};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Upstream rounds to a byte here (it writes hex); we keep the float so
/// mixes chain without accumulating rounding.
fn linear_to_srgb(value: f32) -> f32 {
    let converted = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    converted.clamp(0.0, 1.0)
}

pub fn rgba_to_oklab(color: Rgba) -> Oklab {
    let red = srgb_to_linear(color.r);
    let green = srgb_to_linear(color.g);
    let blue = srgb_to_linear(color.b);
    let l_root = (0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_99 * blue).cbrt();
    let m_root = (0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue).cbrt();
    let s_root = (0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue).cbrt();
    Oklab {
        l: 0.210_454_26 * l_root + 0.793_617_8 * m_root - 0.004_072_047 * s_root,
        a: 1.977_998_5 * l_root - 2.428_592_2 * m_root + 0.450_593_7 * s_root,
        b: 0.025_904_037 * l_root + 0.782_771_77 * m_root - 0.808_675_77 * s_root,
    }
}

pub fn oklab_to_rgba(color: Oklab, alpha: f32) -> Rgba {
    let l_prime = color.l + 0.396_337_78 * color.a + 0.215_803_76 * color.b;
    let m_prime = color.l - 0.105_561_346 * color.a - 0.063_854_17 * color.b;
    let s_prime = color.l - 0.089_484_18 * color.a - 1.291_485_5 * color.b;
    let l = l_prime.powi(3);
    let m = m_prime.powi(3);
    let s = s_prime.powi(3);
    Rgba {
        r: linear_to_srgb(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        g: linear_to_srgb(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        b: linear_to_srgb(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
        a: alpha,
    }
}

fn parse_hex(value: &str) -> Option<Rgba> {
    let hex = value.strip_prefix('#')?;
    let byte = |index: usize| -> Option<f32> {
        u8::from_str_radix(hex.get(index..index + 2)?, 16)
            .ok()
            .map(|v| v as f32 / 255.0)
    };
    match hex.len() {
        6 => Some(Rgba {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: 1.0,
        }),
        8 => Some(Rgba {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: byte(6)?,
        }),
        _ => None,
    }
}

/// `oklch(L C H)` with optional `/ alpha`; mirrors upstream's `OKLCH_PATTERN`
/// (lightness as a 0–1 number, hue in degrees).
fn parse_oklch(value: &str) -> Option<Rgba> {
    let inner = value.strip_prefix("oklch(")?.strip_suffix(')')?;
    let (channels, alpha) = match inner.split_once('/') {
        Some((channels, alpha)) => (channels, Some(alpha.trim())),
        None => (inner, None),
    };
    let mut parts = channels.split_whitespace();
    let lightness = parse_number_or_percent(parts.next()?)?;
    let chroma: f32 = parts.next()?.parse().ok()?;
    let hue = parts.next()?.parse::<f32>().ok()?.to_radians();
    let alpha = match alpha {
        Some(alpha) => parse_number_or_percent(alpha)?,
        None => 1.0,
    };
    Some(oklab_to_rgba(
        Oklab {
            l: lightness,
            a: chroma * hue.cos(),
            b: chroma * hue.sin(),
        },
        alpha,
    ))
}

fn parse_number_or_percent(value: &str) -> Option<f32> {
    match value.strip_suffix('%') {
        Some(percent) => percent.parse::<f32>().ok().map(|v| v / 100.0),
        None => value.parse().ok(),
    }
}

/// Parses the color strings theme files use: `#rrggbb`, `#rrggbbaa`, `oklch(...)`.
pub fn parse_color(value: &str) -> Option<Rgba> {
    let value = value.trim();
    parse_hex(value).or_else(|| parse_oklch(value))
}

/// Compile-time palette strings are known-good (see the palette test), so a
/// bad one is a bug: fall back to magenta to make it visible, never panic.
pub fn color(value: &str) -> Hsla {
    parse_color(value)
        .unwrap_or(Rgba {
            r: 1.0,
            g: 0.0,
            b: 1.0,
            a: 1.0,
        })
        .into()
}

/// `#rrggbb` from 0xRRGGBB, for the Tailwind palette constants.
pub const fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

pub const TRANSPARENT: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

/// Interpolation space for [`color_mix`], as in `color-mix(in <space>, …)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixSpace {
    Srgb,
    Oklab,
}

/// CSS `color-mix(in <space>, a <weight>, b)`: `weight` is `a`'s share
/// (0–1). Alpha is interpolated and channels premultiplied, as CSS does, so
/// mixing with `transparent` yields `a` at reduced opacity.
pub fn color_mix(space: MixSpace, a: impl Into<Rgba>, weight: f32, b: impl Into<Rgba>) -> Rgba {
    let (a, b) = (a.into(), b.into());
    let weight = weight.clamp(0.0, 1.0);
    let other = 1.0 - weight;
    let alpha = a.a * weight + b.a * other;
    if alpha <= f32::EPSILON {
        return TRANSPARENT;
    }
    let premix = |x: f32, y: f32| (x * a.a * weight + y * b.a * other) / alpha;
    match space {
        MixSpace::Srgb => Rgba {
            r: premix(a.r, b.r),
            g: premix(a.g, b.g),
            b: premix(a.b, b.b),
            a: alpha,
        },
        MixSpace::Oklab => {
            let (la, lb) = (rgba_to_oklab(a), rgba_to_oklab(b));
            oklab_to_rgba(
                Oklab {
                    l: premix(la.l, lb.l),
                    a: premix(la.a, lb.a),
                    b: premix(la.b, lb.b),
                },
                alpha,
            )
        }
    }
}

/// `color-mix(in srgb, color <weight>, transparent)`, i.e. Tailwind's
/// `bg-x/NN` opacity modifier.
pub fn with_alpha(color: impl Into<Rgba>, alpha: f32) -> Rgba {
    let mut color = color.into();
    color.a *= alpha.clamp(0.0, 1.0);
    color
}

/// `color` at `fraction` flattened over an opaque `base`: the result has no
/// transparency, so the surface never shows what is behind it.
pub fn solid_over(
    color: impl Into<Rgba>,
    fraction: f32,
    base: impl Into<Rgba>,
) -> gpui_kit::Hsla {
    color_mix(MixSpace::Srgb, color, fraction, base).into()
}

/// Upstream `oklabToHex`: opaque `#rrggbb`. Alpha, when present, is appended
/// so gpui-component's theme parser keeps it.
pub fn to_hex(color: impl Into<Rgba>) -> String {
    let color = color.into();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if color.a >= 1.0 {
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(color.r),
            byte(color.g),
            byte(color.b)
        )
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            byte(color.r),
            byte(color.g),
            byte(color.b),
            byte(color.a)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(to_hex(parse_color("#1b4ed8").unwrap()), "#1b4ed8");
        assert_eq!(to_hex(parse_color("#ffffff80").unwrap()), "#ffffff80");
    }

    #[test]
    fn oklch_matches_upstream_flattened_tokens() {
        // index.css `--primary: oklch(0.488 0.217 264)` was flattened to
        // `#1b4ed8` in T3_CODE_LIGHT_THEME_COLORS.
        let primary = parse_color("oklch(0.488 0.217 264)").unwrap();
        let expected = parse_color("#1b4ed8").unwrap();
        for (x, y) in [
            (primary.r, expected.r),
            (primary.g, expected.g),
            (primary.b, expected.b),
        ] {
            assert!(
                (x - y).abs() < 2.0 / 255.0,
                "{} vs {}",
                to_hex(primary),
                "#1b4ed8"
            );
        }
    }

    #[test]
    fn mixing_with_transparent_only_scales_alpha() {
        let mixed = color_mix(MixSpace::Srgb, hex(0x336699), 0.4, TRANSPARENT);
        assert_eq!(to_hex(Rgba { a: 1.0, ..mixed }), "#336699");
        assert!((mixed.a - 0.4).abs() < 1e-6);
    }

    #[test]
    fn oklab_mix_endpoints_are_identity() {
        let a = hex(0x27272a);
        let b = hex(0xfcfcfc);
        assert_eq!(to_hex(color_mix(MixSpace::Oklab, a, 1.0, b)), "#27272a");
        assert_eq!(to_hex(color_mix(MixSpace::Oklab, a, 0.0, b)), "#fcfcfc");
    }
}
