//! Port of `formatDuration` from upstream/packages/shared/src/orchestrationTiming.ts

pub fn format_duration(duration_ms: f64) -> String {
    if !duration_ms.is_finite() || duration_ms < 0.0 {
        return "0ms".to_string();
    }
    // `duration_ms` is non-negative from here on, where `f64::round` (half away
    // from zero) agrees with JS `Math.round` (half towards +infinity).
    if duration_ms < 1_000.0 {
        return format!("{}ms", 1_f64.max(duration_ms.round()));
    }
    if duration_ms < 10_000.0 {
        let tenths = (duration_ms / 100.0).round() / 10.0;
        return if tenths >= 10.0 {
            "10s".to_string()
        } else {
            format!("{tenths:.1}s")
        };
    }
    if duration_ms < 60_000.0 {
        return format!("{}s", (duration_ms / 1_000.0).round());
    }
    let total_seconds = (duration_ms / 1_000.0).round();
    let hours = (total_seconds / 3_600.0).floor();
    let minutes = ((total_seconds % 3_600.0) / 60.0).floor();
    let seconds = total_seconds % 60.0;
    let mut parts: Vec<String> = Vec::new();
    if hours > 0.0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0.0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0.0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        for (duration_ms, expected) in [
            (0.0, "1ms"),
            (250.0, "250ms"),
            (1_500.0, "1.5s"),
            (9_950.0, "10s"),
            (22_000.0, "22s"),
            (60_000.0, "1m"),
            (65_000.0, "1m 5s"),
            (119_500.0, "2m"),
            (3_599_499.0, "59m 59s"),
            (3_599_500.0, "1h"),
            (3_661_000.0, "1h 1m 1s"),
            (90_061_000.0, "25h 1m 1s"),
        ] {
            assert_eq!(format_duration(duration_ms), expected, "{duration_ms}");
        }
        for invalid in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(format_duration(invalid), "0ms");
        }
    }
}
