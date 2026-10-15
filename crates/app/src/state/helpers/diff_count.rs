//! Port of upstream/packages/client-runtime/src/diffCount.ts

/// Formats a diff line count with the viewer's digit grouping, so `83065` reads `83,065`.
///
/// Upstream uses `Intl.NumberFormat()` with the viewer's locale. No locale data is available
/// here, so this groups with commas in threes, which is what `en-US` does.
pub fn format_diff_count(count: i64) -> String {
    let digits = count.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if count < 0 {
        grouped.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::format_diff_count;

    #[test]
    fn groups_digits_in_threes() {
        assert_eq!(format_diff_count(0), "0");
        assert_eq!(format_diff_count(999), "999");
        assert_eq!(format_diff_count(1000), "1,000");
        assert_eq!(format_diff_count(83065), "83,065");
        assert_eq!(format_diff_count(-1234567), "-1,234,567");
    }
}
