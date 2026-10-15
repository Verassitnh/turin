//! Port of upstream/apps/web/src/hooks/useResizableWidth.ts (the clamp; the
//! owning view holds the width and persistence lands with settings).

/// `clamp`: non-finite widths fall back to the default, then the width is
/// held inside `[min_width, max_width]`.
pub fn clamp_resizable_width(
    value: f32,
    default_width: f32,
    min_width: f32,
    max_width: f32,
) -> f32 {
    if !value.is_finite() {
        return default_width;
    }
    min_width.max(max_width.min(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_for_non_finite_and_clamps() {
        assert_eq!(clamp_resizable_width(f32::NAN, 540., 360., 800.), 540.);
        assert_eq!(clamp_resizable_width(100., 540., 360., 800.), 360.);
        assert_eq!(clamp_resizable_width(900., 540., 360., 800.), 800.);
    }
}
