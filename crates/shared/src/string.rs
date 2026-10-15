//! Port of upstream/packages/shared/src/String.ts

use crate::js_string::{slice_utf16, utf16_len};
use turin_contracts::base_schemas::js_trim;

/// Trims, then keeps at most `max_length` UTF-16 code units and appends `"..."`.
///
/// Upstream defaults `maxLength` to 50; Rust callers pass it explicitly. A cut
/// inside a surrogate pair drops the whole pair (a `&str` cannot hold the lone
/// surrogate JS would keep).
pub fn truncate(text: &str, max_length: usize) -> String {
    let trimmed = js_trim(text);
    if utf16_len(trimmed) <= max_length {
        return trimmed.to_owned();
    }
    format!("{}...", slice_utf16(trimmed, 0, max_length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_trimmed_and_kept() {
        assert_eq!(truncate("   hi   ", 50), "hi");
    }

    #[test]
    fn long_text_is_cut_with_ellipsis() {
        let truncated = truncate(&"x".repeat(60), 50);
        assert_eq!(truncated.len(), 53);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn cut_inside_surrogate_pair_drops_the_pair() {
        let truncated = truncate(&"😀".repeat(27), 50);
        assert_eq!(truncated.chars().count(), 25 + 3);
    }
}
