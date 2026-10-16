//! Port of upstream/apps/web/src/composer-rich-text.ts
//!
//! Inline markdown styling for the rich text composer. The stored prompt
//! stays plain markdown (`**bold**`); these helpers parse it into marked
//! spans for the document. Deliberately small: bold, italic, strikethrough
//! and inline code only. Unmatched markers stay literal text so nothing the
//! user typed is ever lost.

use crate::composer::js_string::is_js_whitespace_char;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RichTextMark {
    Bold,
    Italic,
    Strike,
    Code,
}

impl RichTextMark {
    /// `RICH_TEXT_DELIMITERS`.
    pub fn delimiter(self) -> &'static str {
        match self {
            Self::Bold => "**",
            Self::Italic => "*",
            Self::Strike => "~~",
            Self::Code => "`",
        }
    }
}

/// Outermost mark first, so closers mirror openers when nested
/// (`MARK_NESTING_ORDER` in composer-rich-text-doc.ts).
pub const MARK_NESTING_ORDER: [RichTextMark; 4] = [
    RichTextMark::Strike,
    RichTextMark::Bold,
    RichTextMark::Italic,
    RichTextMark::Code,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextSpan {
    pub text: String,
    pub marks: Vec<RichTextMark>,
}

fn push_span(spans: &mut Vec<RichTextSpan>, text: &str, marks: &[RichTextMark]) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.marks == marks
    {
        last.text.push_str(text);
        return;
    }
    spans.push(RichTextSpan {
        text: text.to_owned(),
        marks: marks.to_vec(),
    });
}

struct Frame {
    delimiter: String,
    mark: RichTextMark,
    spans: Vec<RichTextSpan>,
}

fn current<'a>(
    stack: &'a mut [Frame],
    root: &'a mut Vec<RichTextSpan>,
) -> &'a mut Vec<RichTextSpan> {
    match stack.last_mut() {
        Some(frame) => &mut frame.spans,
        None => root,
    }
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn slice(chars: &[char], start: usize, end: usize) -> String {
    chars[start.min(chars.len())..end.min(chars.len())]
        .iter()
        .collect()
}

fn starts_with_at(chars: &[char], index: usize, needle: &str) -> bool {
    for (at, c) in (index..).zip(needle.chars()) {
        if chars.get(at) != Some(&c) {
            return false;
        }
    }
    true
}

/// Parse the supported inline styles, leaving unmatched and escaped markers
/// literal.
pub fn parse_inline_markdown(text: &str) -> Vec<RichTextSpan> {
    let chars: Vec<char> = text.chars().collect();
    let mut root: Vec<RichTextSpan> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            let piece = slice(&chars, index, index + 2);
            push_span(current(&mut stack, &mut root), &piece, &[]);
            index += 2;
            continue;
        }
        if ch == '`' {
            // Multi-backtick code stays literal; a single-backtick span owns
            // its contents.
            let mut run_len = 0;
            while chars.get(index + run_len) == Some(&'`') {
                run_len += 1;
            }
            let run: String = "`".repeat(run_len);
            let close = (index + run_len..chars.len()).find(|&at| starts_with_at(&chars, at, &run));
            if let Some(close) = close
                && run_len == 1
                && close > index + 1
                && !chars[index..close].contains(&'\n')
                && chars.get(close + 1) != Some(&'`')
            {
                let piece = slice(&chars, index + 1, close);
                push_span(
                    current(&mut stack, &mut root),
                    &piece,
                    &[RichTextMark::Code],
                );
                index = close + 1;
            } else {
                push_span(current(&mut stack, &mut root), &run, &[]);
                index += run_len;
            }
            continue;
        }
        if ch != '*' && ch != '_' && ch != '~' {
            push_span(current(&mut stack, &mut root), &ch.to_string(), &[]);
            index += 1;
            continue;
        }
        let mut end = index;
        while chars.get(end) == Some(&ch) {
            end += 1;
        }
        let before = if index == 0 {
            None
        } else {
            chars.get(index - 1).copied()
        };
        let after = chars.get(end).copied();
        let can_close = before.is_some_and(|b| !is_js_whitespace_char(b))
            && (ch != '_' || !after.is_some_and(is_word_char));
        let can_open = after.is_some_and(|a| !is_js_whitespace_char(a))
            && (ch != '_' || !before.is_some_and(is_word_char));
        while index < end {
            let top_mark = stack.last().map(|frame| frame.mark);
            // Inside italic, a double marker opens bold before closing the
            // single marker. Once bold is active, closing runs unwind both
            // styles.
            let opens_nested_bold = top_mark == Some(RichTextMark::Italic)
                && end - index == 2
                && can_open
                && (ch == '*' || ch == '_')
                && !stack.iter().any(|frame| frame.mark == RichTextMark::Bold);
            let closes = !opens_nested_bold
                && can_close
                && stack.last().is_some_and(|top| {
                    starts_with_at(&chars, index, &top.delimiter)
                        && index + top.delimiter.chars().count() <= end
                });
            if closes {
                let top = stack.pop().expect("closing frame");
                for span in &top.spans {
                    let mut marks = vec![top.mark];
                    marks.extend(span.marks.iter().copied());
                    push_span(current(&mut stack, &mut root), &span.text, &marks);
                }
                index += top.delimiter.chars().count();
            } else if can_open && (ch != '~' || end - index >= 2) {
                let length = if ch == '~' || end - index >= 2 { 2 } else { 1 };
                let mark = if ch == '~' {
                    RichTextMark::Strike
                } else if length == 2 {
                    RichTextMark::Bold
                } else {
                    RichTextMark::Italic
                };
                // A mark cannot nest inside itself, including alternate
                // delimiters. This bounds the stack to the supported styles
                // for arbitrary input.
                if stack.iter().any(|frame| frame.mark == mark) {
                    let piece = slice(&chars, index, end);
                    push_span(current(&mut stack, &mut root), &piece, &[]);
                    index = end;
                    continue;
                }
                stack.push(Frame {
                    delimiter: ch.to_string().repeat(length),
                    mark,
                    spans: Vec::new(),
                });
                index += length;
            } else {
                let piece = slice(&chars, index, end);
                push_span(current(&mut stack, &mut root), &piece, &[]);
                index = end;
            }
        }
    }
    while let Some(frame) = stack.pop() {
        push_span(current(&mut stack, &mut root), &frame.delimiter, &[]);
        for span in &frame.spans {
            push_span(current(&mut stack, &mut root), &span.text, &span.marks);
        }
    }
    root
}

#[cfg(test)]
mod tests {
    use super::RichTextMark::*;
    use super::*;

    fn span(text: &str, marks: &[RichTextMark]) -> RichTextSpan {
        RichTextSpan {
            text: text.to_owned(),
            marks: marks.to_vec(),
        }
    }

    #[test]
    fn parses_bold_markers_into_styled_spans() {
        assert_eq!(
            parse_inline_markdown("hello **bold** world"),
            vec![
                span("hello ", &[]),
                span("bold", &[Bold]),
                span(" world", &[])
            ]
        );
    }

    #[test]
    fn preserves_unmatched_markers_escaped_markers_and_identifiers() {
        for text in [
            "plain text",
            "snake_case",
            "unmatched **",
            "** spaced **",
            "\\*literal\\*",
        ] {
            assert_eq!(parse_inline_markdown(text), vec![span(text, &[])]);
        }
    }

    #[test]
    fn renders_triple_markers_and_nested_styles() {
        assert_eq!(
            parse_inline_markdown("***both***"),
            vec![span("both", &[Bold, Italic])]
        );
        assert_eq!(
            parse_inline_markdown("*a **b** c*"),
            vec![
                span("a ", &[Italic]),
                span("b", &[Italic, Bold]),
                span(" c", &[Italic])
            ]
        );
        assert_eq!(
            parse_inline_markdown("**a `code` c**"),
            vec![
                span("a ", &[Bold]),
                span("code", &[Bold, Code]),
                span(" c", &[Bold])
            ]
        );
    }

    #[test]
    fn preserves_crossing_italic_and_bold_spans() {
        assert_eq!(
            parse_inline_markdown("*a**b*****c**"),
            vec![
                span("a", &[Italic]),
                span("b", &[Italic, Bold]),
                span("c", &[Bold])
            ]
        );
    }

    #[test]
    fn keeps_repeated_and_alternate_delimiters_literal_inside_an_active_mark() {
        let repeated = "*".repeat(10_000);
        assert_eq!(
            parse_inline_markdown(&format!("{repeated}text{repeated}")),
            vec![
                span(&format!("{}text", &repeated[2..]), &[Bold]),
                span(&repeated[2..], &[])
            ]
        );
        let alternate = "__".repeat(5_000);
        assert_eq!(
            parse_inline_markdown(&format!("**~~{alternate}text{alternate}~~**")),
            vec![span(
                &format!("{alternate}text{alternate}"),
                &[Bold, Strike]
            )]
        );
    }

    #[test]
    fn keeps_code_span_contents_literal() {
        assert_eq!(
            parse_inline_markdown("`**not bold**`"),
            vec![span("**not bold**", &[Code])]
        );
    }
}
