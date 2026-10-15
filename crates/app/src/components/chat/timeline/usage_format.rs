//! Port of upstream/packages/shared/src/usageFormat.ts
//!
//! Only `formatTokens` (and the `trim` / `INTEGER` helpers it uses) is ported;
//! the usage page's currency, percent, day and time-zone formatting live with
//! the usage page.

/// Compacts a token count to three significant figures with a unit suffix, so
/// columns of numbers line up at a glance (`19.9B`, `76.7M`, `804K`).
pub fn format_tokens(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1e12 {
        return format!("{}T", trim(value / 1e12));
    }
    if abs >= 1e9 {
        return format!("{}B", trim(value / 1e9));
    }
    if abs >= 1e6 {
        return format!("{}M", trim(value / 1e6));
    }
    if abs >= 1e3 {
        return format!("{}K", trim(value / 1e3));
    }
    format_integer(js_math_round(value))
}

fn trim(value: f64) -> String {
    let abs = value.abs();
    let digits = if abs >= 100.0 {
        0
    } else if abs >= 10.0 {
        1
    } else {
        2
    };
    let fixed = js_to_fixed(value, digits);
    // `.replace(/\.0+$/, "")`: only an all-zero fraction goes, so `1.50` stays.
    match fixed.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() && fraction.bytes().all(|b| b == b'0') => {
            whole.to_string()
        }
        _ => fixed,
    }
}

/// JS `Math.round`: halves round towards +infinity (`f64::round` rounds them
/// away from zero), and a result of zero keeps the input's sign (`-0.4` → `-0`).
fn js_math_round(value: f64) -> f64 {
    let floor = value.floor();
    // `value - floor` is the exact fractional part for every finite double.
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    if rounded == 0.0 {
        0_f64.copysign(value)
    } else {
        rounded
    }
}

/// JS `Number.prototype.toFixed` for the 0–2 digits `trim` asks for.
///
/// Both JS and Rust round the exact binary value, but on an exact tie JS picks
/// the larger magnitude (`(1.125).toFixed(2) === "1.13"`) where Rust's
/// formatter rounds half to even (`"1.12"`). A tie at `digits` decimals is a
/// value whose magnitude times `2^(digits + 1)` is an odd integer, which is
/// exact to test because scaling by a power of two is exact.
fn js_to_fixed(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let abs = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    let tie_scale = f64::from(1_u32 << (digits + 1));
    let scaled = abs * tie_scale;
    let is_tie = scaled.fract() == 0.0 && scaled % 2.0 == 1.0 && scaled < 2_f64.powi(53);
    if !is_tie {
        // JS writes the sign for any value below zero, even one that rounds to
        // zero (`(-0.001).toFixed(2)` is "-0.00"), but not for -0 itself.
        return format!("{sign}{abs:.digits$}");
    }
    // abs * 10^digits = scaled * 5^digits / 2; round the half up.
    let five_pow = 5_u64.pow(digits as u32);
    let scaled_int = scaled as u64;
    let rounded = (scaled_int * five_pow).div_ceil(2);
    let ten_pow = 10_u64.pow(digits as u32);
    if digits == 0 {
        format!("{sign}{rounded}")
    } else {
        format!(
            "{sign}{}.{:0width$}",
            rounded / ten_pow,
            rounded % ten_pow,
            width = digits
        )
    }
}

/// `new Intl.NumberFormat("en-US").format(n)` for an already-rounded `n`.
fn format_integer(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "∞" } else { "-∞" }.to_string();
    }
    let digits = format!("{:.0}", value.abs());
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value.is_sign_negative() {
        grouped.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // usageFormat.test.ts has no formatTokens cases. These pin the doc
    // comment's examples and edge cases, with expectations taken from running
    // upstream's implementation under Node.
    #[test]
    fn format_tokens_matches_upstream() {
        for (value, expected) in [
            (0.0, "0"),
            (999.0, "999"),
            (999.4, "999"),
            (999.5, "1,000"),
            (-0.4, "-0"),
            (-0.5, "-0"),
            (-2.5, "-2"),
            (0.5, "1"),
            (1.5, "2"),
            (1_000.0, "1K"),
            (1_500.0, "1.50K"),
            (12_345.0, "12.3K"),
            (804_000.0, "804K"),
            (999_950.0, "1000K"),
            (1_000_000.0, "1M"),
            (1_125_000.0, "1.13M"),
            (1_234_567.0, "1.23M"),
            (-1_234_567.0, "-1.23M"),
            (1_500_000.0, "1.50M"),
            (76_700_000.0, "76.7M"),
            (19_900_000_000.0, "19.9B"),
            (2.5e15, "2500T"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "InfinityT"),
            (f64::NEG_INFINITY, "-InfinityT"),
        ] {
            assert_eq!(format_tokens(value), expected, "{value}");
        }
    }
}
