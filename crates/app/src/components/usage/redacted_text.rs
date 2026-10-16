//! Port of upstream/apps/web/src/components/settings/RedactedSensitiveText.tsx
//! (the placeholder; the click-to-reveal button is part of the popover that
//! shows it). Upstream also blurs the placeholder; GPUI has no blur, so the
//! seeded look-alike text stands alone.

const REDACTED_TEXT_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// Same-shape placeholder for `value`: separators stay, other characters
/// become a deterministic pseudo-random letter or digit.
pub fn redacted_placeholder(value: &str) -> String {
    let mut state: u32 = 0x811c9dc5;
    for unit in value.encode_utf16() {
        state ^= u32::from(unit);
        state = state.wrapping_mul(0x01000193);
    }
    let mut next_char = || {
        state = (state ^ (state >> 13)).wrapping_mul(0x85ebca6b);
        state = (state ^ (state >> 16)).wrapping_mul(0xc2b2ae35);
        // `Math.abs` of the signed 32-bit value.
        let magnitude = (state as i32).unsigned_abs() as usize;
        REDACTED_TEXT_ALPHABET[magnitude % REDACTED_TEXT_ALPHABET.len()] as char
    };
    value
        .chars()
        .map(|c| match c {
            '@' | '.' | '-' | '_' => c,
            _ => next_char(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_separators_and_length() {
        let value = "jackson@samwisepsm.com";
        let redacted = redacted_placeholder(value);
        assert_eq!(redacted.chars().count(), value.chars().count());
        for (original, masked) in value.chars().zip(redacted.chars()) {
            if matches!(original, '@' | '.' | '-' | '_') {
                assert_eq!(original, masked);
            } else {
                assert!(REDACTED_TEXT_ALPHABET.contains(&(masked as u8)), "{masked}");
            }
        }
    }

    #[test]
    fn is_stable_per_value_and_differs_across_values() {
        assert_eq!(
            redacted_placeholder("a@b.co"),
            redacted_placeholder("a@b.co")
        );
        assert_ne!(
            redacted_placeholder("a@b.co"),
            redacted_placeholder("c@d.co")
        );
    }
}
