//! Port of upstream/apps/web/src/lib/color.ts and
//! upstream/apps/web/src/lib/color.test.ts.
//!
//! The color picker's HSV math: normalized six-digit sRGB hex colors to and
//! from hue (degrees), saturation and value (0 to 1).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HsvColor {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

/// `hexToHsv`: a normalized six-digit sRGB hex color to HSV.
pub fn hex_to_hsv(hex: &str) -> HsvColor {
    let numeric = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    let red = ((numeric >> 16) & 255) as f32 / 255.;
    let green = ((numeric >> 8) & 255) as f32 / 255.;
    let blue = (numeric & 255) as f32 / 255.;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;

    let mut hue = 0.;
    if delta != 0. {
        hue = if max == red {
            ((green - blue) / delta) % 6.
        } else if max == green {
            (blue - red) / delta + 2.
        } else {
            (red - green) / delta + 4.
        };
        hue *= 60.;
        if hue < 0. {
            hue += 360.;
        }
    }
    HsvColor {
        h: hue,
        s: if max == 0. { 0. } else { delta / max },
        v: max,
    }
}

/// `hsvToHex`: hue wraps, so `360` and `720` are red and `-60` is magenta.
pub fn hsv_to_hex(hue: f32, saturation: f32, value: f32) -> String {
    let normalized_hue = ((hue % 360.) + 360.) % 360.;
    let chroma = value * saturation;
    let x = chroma * (1. - ((normalized_hue / 60.) % 2. - 1.).abs());
    let m = value - chroma;
    let (red, green, blue) = if normalized_hue < 60. {
        (chroma, x, 0.)
    } else if normalized_hue < 120. {
        (x, chroma, 0.)
    } else if normalized_hue < 180. {
        (0., chroma, x)
    } else if normalized_hue < 240. {
        (0., x, chroma)
    } else if normalized_hue < 300. {
        (x, 0., chroma)
    } else {
        (chroma, 0., x)
    };
    let channel = |value: f32| ((value + m) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(red),
        channel(green),
        channel(blue)
    )
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
        assert_eq!(hsv_to_hex(360., 1., 1.), "#ff0000");
        assert_eq!(hsv_to_hex(720., 1., 1.), "#ff0000");
        assert_eq!(hsv_to_hex(-60., 1., 1.), "#ff00ff");
    }

    #[test]
    fn represents_black_and_greys_without_undefined_saturation() {
        assert_eq!(
            hex_to_hsv("#000000"),
            HsvColor {
                h: 0.,
                s: 0.,
                v: 0.
            }
        );
        assert_eq!(
            hex_to_hsv("#ffffff"),
            HsvColor {
                h: 0.,
                s: 0.,
                v: 1.
            }
        );
    }
}
