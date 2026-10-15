//! Port of `limitTitleMessage` from upstream/packages/provider-core/src/server/threadTitleContext.ts.
//!
//! Only this function is here. The rest of that file (`formatThreadTitleContext`) is
//! the thread-title context builder that T13b ports on its own branch.

use turin_provider_core::js::{take_utf16, utf16_len};

const TRUNCATED: &str = "\n[Content truncated]\n";

/// Keeps the head and the tail of a message that is over `budget` UTF-16 units.
/// The tail is where the latest instructions are, so both ends survive.
pub fn limit_title_message(text: &str, budget: usize) -> String {
    if utf16_len(text) <= budget {
        return text.to_owned();
    }
    if budget <= utf16_len(TRUNCATED) {
        return String::new();
    }
    let available = budget - utf16_len(TRUNCATED);
    let head = available.div_ceil(2);
    let tail = available - head;
    let tail_text = if tail > 0 { last_utf16(text, tail) } else { "" };
    format!("{}{TRUNCATED}{tail_text}", take_utf16(text, head))
}

/// `text.slice(-units)` in UTF-16 units.
fn last_utf16(text: &str, units: usize) -> &str {
    let mut skip = utf16_len(text).saturating_sub(units);
    for (byte, c) in text.char_indices() {
        if skip == 0 {
            return &text[byte..];
        }
        skip = skip.saturating_sub(c.len_utf16());
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_budget_gives_an_empty_message() {
        assert_eq!(limit_title_message(&"x".repeat(100), 0), "");
    }

    #[test]
    fn a_message_within_budget_is_returned_unchanged() {
        assert_eq!(limit_title_message("short message", 13), "short message");
        assert_eq!(limit_title_message("short message", 100), "short message");
    }

    #[test]
    fn the_result_never_exceeds_a_small_budget() {
        let text = "x".repeat(100);
        for budget in 1..40 {
            let limited = limit_title_message(&text, budget);
            assert!(
                utf16_len(&limited) <= budget,
                "budget {budget} gave {} units",
                utf16_len(&limited)
            );
        }
    }

    #[test]
    fn a_long_message_keeps_its_head_and_its_tail_around_the_marker() {
        // Budget 29: 29 - 21 (marker) = 8 units, head 4 ("abcd"), tail 4 ("wxyz").
        let text = format!("{}wxyz", "abcdefghijklmnopqrstuvwxyz");
        let limited = limit_title_message(&text, 29);
        assert_eq!(limited, format!("abcd{TRUNCATED}wxyz"));
        assert_eq!(utf16_len(&limited), 29);
    }

    #[test]
    fn an_odd_available_budget_gives_the_extra_unit_to_the_head() {
        // Budget 26: available 5, head 3, tail 2.
        let limited = limit_title_message("0123456789", 26);
        assert_eq!(limited, "0123456789");
        let limited = limit_title_message(&"0123456789".repeat(3), 26);
        assert_eq!(limited, format!("012{TRUNCATED}89"));
    }
}
