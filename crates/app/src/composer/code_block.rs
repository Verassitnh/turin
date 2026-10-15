//! Port of upstream/apps/web/src/composer-code-block.ts (the string rules;
//! the editor commands built on them live in `editor_state`).

/// Spaces rather than a tab: the fence round-trips through Markdown on its way
/// to the agent, and a literal tab there renders at whatever width the reader
/// happens to use. Two matches how this repo indents its own source.
pub const CODE_BLOCK_INDENT: &str = "  ";

/// The leading run of spaces and tabs on a line, which Enter carries forward.
pub fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, c)| *c != ' ' && *c != '\t')
        .map_or(line.len(), |(at, _)| at);
    &line[..end]
}

/// Removes one indent's worth of leading whitespace, tolerating lines that
/// were indented by hand with an odd number of spaces or with tabs. A line
/// with no leading whitespace is returned unchanged, so outdenting bottoms out
/// quietly rather than eating the first character of code.
pub fn outdent_line(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix(CODE_BLOCK_INDENT) {
        return rest;
    }
    if let Some(rest) = line.strip_prefix('\t') {
        return rest;
    }
    line.strip_prefix(' ').unwrap_or(line)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentDirection {
    In,
    Out,
}

/// Applies Tab/Shift+Tab to every line a selection touches, whole lines at a
/// time.
pub fn indent_lines(lines: &[&str], direction: IndentDirection) -> Vec<String> {
    lines
        .iter()
        .map(|line| match direction {
            // Indenting a blank line would leave trailing whitespace behind on
            // a line the user cannot see the caret move on, so leave it be.
            IndentDirection::In if line.is_empty() => String::new(),
            IndentDirection::In => format!("{CODE_BLOCK_INDENT}{line}"),
            IndentDirection::Out => outdent_line(line).to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_whitespace_reads_the_indent_a_new_line_should_inherit() {
        assert_eq!(leading_whitespace("    nested()"), "    ");
        assert_eq!(leading_whitespace("\tnested()"), "\t");
        assert_eq!(leading_whitespace("flush()"), "");
        assert_eq!(leading_whitespace("  a  b"), "  ");
    }

    #[test]
    fn outdent_line_removes_a_full_indent_or_falls_back() {
        assert_eq!(outdent_line("    deep"), "  deep");
        assert_eq!(outdent_line(" odd"), "odd");
        assert_eq!(outdent_line("\ttabbed"), "tabbed");
        assert_eq!(outdent_line("flush()"), "flush()");
    }

    #[test]
    fn indent_lines_indents_outdents_and_leaves_blank_lines_alone() {
        assert_eq!(indent_lines(&["a", "b"], IndentDirection::In), ["  a", "  b"]);
        assert_eq!(indent_lines(&["a", "", "b"], IndentDirection::In), ["  a", "", "  b"]);
        assert_eq!(
            indent_lines(&["    a", "  b", "c"], IndentDirection::Out),
            ["  a", "b", "c"]
        );
    }
}
