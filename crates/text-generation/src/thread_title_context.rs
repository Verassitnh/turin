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
