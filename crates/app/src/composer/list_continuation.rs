//! Port of upstream/apps/web/src/composer-list-continuation.ts
//!
//! List continuation and indentation as plain string edits over the stored
//! prompt (offsets are UTF-16 code units, like upstream). The rich editor
//! applies the same rules to its document; this is the literal-text path and
//! the shared marker counter.

use std::sync::OnceLock;

use regex::Regex;

use crate::composer::editor_mentions::split_prompt_into_composer_segments;
use crate::composer::js_string::utf16_len;

/// Offsets into the prompt (expanded, UTF-16 units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerListEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

enum ListMarker {
    Ordered {
        indent: String,
        number_text: String,
        delimiter: char,
    },
    Task {
        indent: String,
    },
    Bullet {
        indent: String,
        bullet: String,
    },
}

fn parse_list_marker(line: &str) -> Option<(ListMarker, usize)> {
    static ORDERED: OnceLock<Regex> = OnceLock::new();
    static TASK: OnceLock<Regex> = OnceLock::new();
    static BULLET: OnceLock<Regex> = OnceLock::new();
    // `[0-9]`, not `\d`: Rust's `\d` matches Unicode digits, JS's does not.
    let ordered = ORDERED.get_or_init(|| Regex::new(r"^([0-9]+)([.)])([ \t]+|\s*$)").unwrap());
    let task = TASK.get_or_init(|| Regex::new(r"^-\s\[[ xX]\]([ \t]+|\s*$)").unwrap());
    let bullet = BULLET.get_or_init(|| Regex::new(r"^([-*+])([ \t]+|\s*$)").unwrap());

    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent = line[..indent_len].to_owned();
    let rest = &line[indent_len..];
    let indent_units = utf16_len(&indent);
    if let Some(captures) = ordered.captures(rest) {
        return Some((
            ListMarker::Ordered {
                indent,
                number_text: captures[1].to_owned(),
                delimiter: if &captures[2] == ")" { ')' } else { '.' },
            },
            indent_units + utf16_len(&captures[0]),
        ));
    }
    if let Some(captures) = task.captures(rest) {
        return Some((
            ListMarker::Task { indent },
            indent_units + utf16_len(&captures[0]),
        ));
    }
    if let Some(captures) = bullet.captures(rest) {
        return Some((
            ListMarker::Bullet {
                indent,
                bullet: captures[1].to_owned(),
            },
            indent_units + utf16_len(&captures[0]),
        ));
    }
    None
}

/// The ordered marker that follows `3.` or `09)`: one higher, same delimiter,
/// zero padding kept, and a number too large to count left as typed.
pub fn next_ordered_marker_text(marker: &str) -> String {
    let delimiter = marker.chars().last().unwrap_or('.');
    let number_text = &marker[..marker.len() - delimiter.len_utf8()];
    // JS `Number.isSafeInteger`: at most 2^53 - 1.
    let next = match number_text.parse::<u64>() {
        Ok(number) if number < (1u64 << 53) - 1 => {
            let next = (number + 1).to_string();
            format!("{next:0>width$}", width = number_text.len())
        }
        _ => number_text.to_owned(),
    };
    format!("{next}{delimiter}")
}

fn next_marker_text(marker: &ListMarker) -> String {
    match marker {
        ListMarker::Ordered {
            indent,
            number_text,
            delimiter,
        } => format!(
            "{indent}{} ",
            next_ordered_marker_text(&format!("{number_text}{delimiter}"))
        ),
        ListMarker::Task { indent } => format!("{indent}- [ ] "),
        ListMarker::Bullet { indent, bullet } => format!("{indent}{bullet} "),
    }
}

/// True when splitting at the caret would cut an inline chip in two.
fn is_inside_inline_token(value: &str, cursor: usize) -> bool {
    let mut offset = 0;
    for segment in split_prompt_into_composer_segments(value) {
        let end = offset + segment.source_len();
        if segment.is_inline_token() && cursor > offset && cursor < end {
            return true;
        }
        offset = end;
    }
    false
}

struct Line {
    start: usize,
    end: usize,
    text: String,
}

fn current_line(units: &[u16], cursor: usize) -> Line {
    let start = units[..cursor]
        .iter()
        .rposition(|u| *u == 0x0a)
        .map_or(0, |at| at + 1);
    let end = units[cursor..]
        .iter()
        .position(|u| *u == 0x0a)
        .map_or(units.len(), |at| cursor + at);
    Line {
        start,
        end,
        text: String::from_utf16_lossy(&units[start..end]),
    }
}

/// Enter on a list item line: continue the list, or exit it when the item is
/// empty. Returns `None` for non-list lines, carets inside the marker, and
/// carets inside an inline chip: all fall through to a plain newline.
pub fn list_continuation_for_enter(value: &str, cursor: usize) -> Option<ComposerListEdit> {
    let units: Vec<u16> = value.encode_utf16().collect();
    if cursor > units.len() {
        return None;
    }
    let line = current_line(&units, cursor);
    let (marker, marker_len) = parse_list_marker(&line.text)?;
    let marker_end = line.start + marker_len;
    if cursor < marker_end {
        return None;
    }
    if is_inside_inline_token(value, cursor) {
        return None;
    }
    let rest = String::from_utf16_lossy(&units[marker_end.min(line.end)..line.end]);
    if turin_contracts::base_schemas::js_trim(&rest).is_empty() {
        // Empty item: Enter exits the list by removing the marker.
        return Some(ComposerListEdit {
            start: line.start,
            end: cursor.max(marker_end),
            replacement: String::new(),
        });
    }
    Some(ComposerListEdit {
        start: cursor,
        end: cursor,
        replacement: format!("\n{}", next_marker_text(&marker)),
    })
}

/// Tab on a list item line: indent by two spaces. Ranged selections, non-list
/// lines, and carets inside an inline chip fall through.
pub fn list_indent_for_tab(value: &str, start: usize, end: usize) -> Option<ComposerListEdit> {
    let units: Vec<u16> = value.encode_utf16().collect();
    if start != end || start > units.len() {
        return None;
    }
    let line = current_line(&units, start);
    parse_list_marker(&line.text)?;
    if is_inside_inline_token(value, start) {
        return None;
    }
    Some(ComposerListEdit {
        start: line.start,
        end: line.start,
        replacement: "  ".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(value: &str, edit: &ComposerListEdit) -> String {
        let units: Vec<u16> = value.encode_utf16().collect();
        format!(
            "{}{}{}",
            String::from_utf16_lossy(&units[..edit.start]),
            edit.replacement,
            String::from_utf16_lossy(&units[edit.end..])
        )
    }

    fn enter(value: &str, cursor: usize) -> String {
        apply(
            value,
            &list_continuation_for_enter(value, cursor).expect("edit"),
        )
    }

    #[test]
    fn continues_ordered_lists_with_the_next_number() {
        assert_eq!(enter("1. foo", 6), "1. foo\n2. ");
    }

    #[test]
    fn increments_multi_digit_and_paren_markers() {
        assert_eq!(enter("12. foo", 7), "12. foo\n13. ");
        assert_eq!(enter("3) foo", 6), "3) foo\n4) ");
    }

    #[test]
    fn continues_bullets_and_keeps_indentation() {
        assert_eq!(enter("- foo", 5), "- foo\n- ");
        assert_eq!(enter("  * foo", 7), "  * foo\n  * ");
    }

    #[test]
    fn continues_tasks_unchecked() {
        assert_eq!(enter("- [x] done", 10), "- [x] done\n- [ ] ");
    }

    #[test]
    fn splits_mid_line_items() {
        assert_eq!(enter("1. foobar", 5), "1. fo\n2. obar");
    }

    #[test]
    fn exits_the_list_on_an_empty_item() {
        assert_eq!(enter("1. foo\n2. ", 10), "1. foo\n");
        assert_eq!(enter("- ", 2), "");
    }

    #[test]
    fn ignores_non_list_lines_and_carets_inside_the_marker() {
        assert_eq!(list_continuation_for_enter("plain text", 5), None);
        assert_eq!(list_continuation_for_enter("1.foo no space", 3), None);
        assert_eq!(list_continuation_for_enter("-foo", 2), None);
        assert_eq!(list_continuation_for_enter("1. foo", 1), None);
    }

    #[test]
    fn refuses_to_split_a_supplementary_currency_skill_chip() {
        let value = "1. \u{11FDD}review go";
        let cursor = utf16_len(&value[..value.find(" go").unwrap()]) - 1;
        assert_eq!(list_continuation_for_enter(value, cursor), None);
    }

    #[test]
    fn refuses_to_split_an_inline_chip() {
        let value = "1. @README.md go";
        let cursor = value.find("README").unwrap() + 2;
        assert_eq!(list_continuation_for_enter(value, cursor), None);
    }

    #[test]
    fn ordered_markers_take_ascii_digits_only() {
        // ARABIC-INDIC DIGIT THREE is a Unicode `\d` but not a JS one.
        assert_eq!(list_continuation_for_enter("\u{0663}. foo", 6), None);
    }

    #[test]
    fn indents_list_items_on_tab() {
        assert_eq!(
            apply("- foo", &list_indent_for_tab("- foo", 2, 2).unwrap()),
            "  - foo"
        );
        assert_eq!(list_indent_for_tab("plain", 2, 2), None);
        assert_eq!(list_indent_for_tab("- foo", 1, 3), None);
    }

    #[test]
    fn next_ordered_marker_text_counts_up() {
        for (marker, expected) in [
            ("1.", "2."),
            ("1)", "2)"),
            ("09.", "10."),
            ("001)", "002)"),
            ("99.", "100."),
            ("99999999999999999999.", "99999999999999999999."),
        ] {
            assert_eq!(next_ordered_marker_text(marker), expected);
        }
    }
}
