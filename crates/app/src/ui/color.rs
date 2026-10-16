//! Port of upstream/apps/web/src/lib/color.ts
//!
//! HSV helpers behind the accent colour pickers (`ColorPicker`,
//! `ProviderAccentColorPicker`, `ThemeColorPicker`).

/// `HsvColor`: hue in degrees `[0, 360)`, saturation and value in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HsvColor {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

/// Convert a normalized six-digit sRGB hex color to HSV.
///
/// Like upstream's `Number.parseInt(hex.slice(1), 16)`, the leading character
/// is skipped without being checked; input that does not parse reads as 0
/// (upstream would yield `NaN`, but the callers only pass normalized hex).
pub fn hex_to_hsv(hex: &str) -> HsvColor {
    let digits: String = hex.chars().skip(1).collect();
    let numeric = u32::from_str_radix(&digits, 16).unwrap_or(0);
    let red = f64::from((numeric >> 16) & 255) / 255.0;
    let green = f64::from((numeric >> 8) & 255) / 255.0;
    let blue = f64::from(numeric & 255) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;

    let mut hue = 0.0;
    if delta != 0.0 {
        if max == red {
            // JS `%` is a truncating remainder, as is Rust's.
            hue = ((green - blue) / delta) % 6.0;
        } else if max == green {
            hue = (blue - red) / delta + 2.0;
        } else {
            hue = (red - green) / delta + 4.0;
        }
        hue *= 60.0;
        if hue < 0.0 {
            hue += 360.0;
        }
    }

    HsvColor {
        h: hue,
        s: if max == 0.0 { 0.0 } else { delta / max },
        v: max,
    }
}

/// `Math.round` on a non-negative channel: halves round up.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub fn hsv_to_hex(hue: f64, saturation: f64, value: f64) -> String {
    let normalized_hue = ((hue % 360.0) + 360.0) % 360.0;
    let chroma = value * saturation;
    let x = chroma * (1.0 - (((normalized_hue / 60.0) % 2.0) - 1.0).abs());
    let r#match = value - chroma;
    let (red, green, blue) = if normalized_hue < 60.0 {
        (chroma, x, 0.0)
    } else if normalized_hue < 120.0 {
        (x, chroma, 0.0)
    } else if normalized_hue < 180.0 {
        (0.0, chroma, x)
    } else if normalized_hue < 240.0 {
        (0.0, x, chroma)
    } else if normalized_hue < 300.0 {
        (x, 0.0, chroma)
    } else {
        (chroma, 0.0, x)
    };

    let mut hex = String::from("#");
    for channel in [red, green, blue] {
        hex.push_str(&format!(
            "{:02x}",
            js_round((channel + r#match) * 255.0) as u32
        ));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_hsv_without_changing_the_persisted_color() {
        for hex in [
            "#000000", "#ffffff", "#808080", "#ff0000", "#00ff00", "#0000ff", "#2563eb",
        ] {
            let HsvColor { h, s, v } = hex_to_hsv(hex);
            assert_eq!(hsv_to_hex(h, s, v), hex, "{hex}");
        }
    }

    #[test]
    fn wraps_hue_at_the_slider_boundary_and_across_keyboard_steps() {
        assert_eq!(hsv_to_hex(360.0, 1.0, 1.0), "#ff0000");
        assert_eq!(hsv_to_hex(720.0, 1.0, 1.0), "#ff0000");
        assert_eq!(hsv_to_hex(-60.0, 1.0, 1.0), "#ff00ff");
    }

    #[test]
    fn represents_black_and_greys_without_undefined_saturation() {
        assert_eq!(
            hex_to_hsv("#000000"),
            HsvColor {
                h: 0.0,
                s: 0.0,
                v: 0.0
            }
        );
        assert_eq!(
            hex_to_hsv("#ffffff"),
            HsvColor {
                h: 0.0,
                s: 0.0,
                v: 1.0
            }
        );
    }

    #[test]
    fn negative_hue_wraps_into_the_circle() {
        // Magenta-ish: red is max and green < blue, so the sextant is negative.
        let hsv = hex_to_hsv("#ff0080");
        assert!(hsv.h > 300.0 && hsv.h < 360.0, "{}", hsv.h);
    }
}
