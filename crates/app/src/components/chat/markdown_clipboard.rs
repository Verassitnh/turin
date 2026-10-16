//! Port of upstream/apps/web/src/markdown-clipboard.ts over Turin's markdown document
//! (`markdown::markdown_pipeline::Block`), in place of the rendered DOM.
//!
//! Upstream converts a DOM selection inside rendered chat markdown back into markdown
//! source, so highlight-and-copy keeps formatting. Turin renders from the lowered
//! document, so each DOM rule has a document-level equivalent here:
//! - a tag's markdown is the block or inline it lowered from;
//! - `data-markdown-copy` is the copy an inline link carries: a composer context
//!   reference or an assistant citation, from the href;
//! - `pre` ancestry is the `CodeText` fragment: a selection inside a code block
//!   copies its text as is.
//!
//! The `text/html` flavor is not produced; GPUI's clipboard carries text only.
//! PORTING.md Deviations lists every DOM rule that has no document equivalent.

use turin_contracts::base_schemas::{is_js_whitespace, js_trim};

use super::markdown::markdown_pipeline::{Align, Block, Inlines, ListItem, Marks};
use super::timeline::assistant_citations::{
    parse_assistant_citation_href, serialize_assistant_citation,
};
use turin_shared::composer_context_references::parse_composer_context_href;

/// One piece of a copied selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardFragment<'a> {
    /// A selection over rendered markdown blocks.
    Blocks(&'a [Block]),
    /// A selection inside a code block: its text, copied as is.
    CodeText(&'a str),
}

/// `markdown-clipboard.ts` `chatMarkdownClipboardPayload`, text flavor only. `None`
/// when nothing copies. Pieces are joined with a blank line.
pub fn chat_markdown_clipboard_text(fragments: &[ClipboardFragment<'_>]) -> Option<String> {
    let texts: Vec<String> = fragments
        .iter()
        .map(|fragment| match fragment {
            ClipboardFragment::Blocks(blocks) => serialize_rendered_markdown_fragment(blocks),
            ClipboardFragment::CodeText(text) => (*text).to_owned(),
        })
        .filter(|text| !text.is_empty())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

/// `serializeRenderedMarkdownFragment`: a fragment whose only visible content is one
/// code block copies as plain code; anything else serializes as markdown.
pub fn serialize_rendered_markdown_fragment(blocks: &[Block]) -> String {
    if let Some(code) = sole_code_block(blocks) {
        return strip_one_trailing_newline(code).to_owned();
    }
    tidy_markdown(&serialize_children(blocks))
}

/// `serializeTableElementToMarkdown`.
pub fn serialize_table_to_markdown(
    align: &[Align],
    header: &[Inlines],
    rows: &[Vec<Inlines>],
) -> String {
    serialize_table(align, header, rows).trim().to_owned()
}

/// `serializeTableElementToCsv`: header and body rows, one CSV line each.
pub fn serialize_table_to_csv(header: &[Inlines], rows: &[Vec<Inlines>]) -> String {
    std::iter::once(header)
        .chain(rows.iter().map(Vec::as_slice))
        .filter(|cells| !cells.is_empty())
        .map(|cells| {
            cells
                .iter()
                .map(|cell| csv_cell(&cell.text))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn csv_cell(value: &str) -> String {
    let normalized = collapse_whitespace(value);
    let normalized = js_trim(&normalized);
    if normalized.contains(['"', ',', '\n']) {
        format!("\"{}\"", normalized.replace('"', "\"\""))
    } else {
        normalized.to_owned()
    }
}

/// `serializeTable`: the first row gets the separator line.
fn serialize_table(align: &[Align], header: &[Inlines], rows: &[Vec<Inlines>]) -> String {
    let mut lines = Vec::new();
    if !header.is_empty() {
        lines.push(table_row(header));
        lines.push(table_separator(align, header.len()));
    }
    for row in rows.iter().filter(|row| !row.is_empty()) {
        if header.is_empty() && lines.is_empty() {
            lines.push(table_row(row));
            lines.push(table_separator(align, row.len()));
            continue;
        }
        lines.push(table_row(row));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("{}\n\n", lines.join("\n"))
}

fn table_row(cells: &[Inlines]) -> String {
    let cells: Vec<String> = cells.iter().map(serialize_table_cell).collect();
    format!("| {} |", cells.join(" | "))
}

fn serialize_table_cell(cell: &Inlines) -> String {
    let content = serialize_inlines(cell);
    let content = collapse_newlines_to_spaces(&content);
    js_trim(&content).replace('|', "\\|")
}

/// `tableSeparatorFor`: the marker for each column from its alignment.
fn table_separator(align: &[Align], columns: usize) -> String {
    let markers: Vec<&str> = (0..columns)
        .map(
            |index| match align.get(index).copied().unwrap_or(Align::None) {
                Align::Center => ":---:",
                Align::Right => "---:",
                Align::None | Align::Left => "---",
            },
        )
        .collect();
    format!("| {} |", markers.join(" | "))
}

/// Serializes a sequence of blocks in document order (`serializeChildren`).
fn serialize_children(blocks: &[Block]) -> String {
    blocks.iter().map(serialize_block).collect()
}

fn serialize_block(block: &Block) -> String {
    match block {
        Block::Paragraph(inlines) => format!("{}\n\n", js_trim(&serialize_inlines(inlines))),
        Block::Heading { level, content } => format!(
            "{} {}\n\n",
            "#".repeat(usize::from(*level)),
            js_trim(&serialize_inlines(content))
        ),
        Block::ThematicBreak => "---\n\n".to_owned(),
        Block::Blockquote(children) => serialize_blockquote(children),
        // Alerts are containers with no markdown of their own: a div's rule.
        Block::Alert { children, .. } => div_content(&serialize_children(children)),
        Block::List {
            ordered,
            start,
            items,
            ..
        } => serialize_list(*ordered, *start, items),
        Block::Code { lang, code, .. } => serialize_code_block(lang.as_deref(), code),
        Block::Table {
            align,
            header,
            rows,
        } => serialize_table(align, header, rows),
        // Raw HTML is shown as literal text, so it copies as text.
        Block::Html(text) => format!("{text}\n\n"),
        Block::Footnotes(footnotes) => footnotes
            .iter()
            .map(|(_, children)| serialize_children(children))
            .collect(),
    }
}

/// The DIV rule: content ends on a newline.
fn div_content(content: &str) -> String {
    if !content.is_empty() && !content.ends_with('\n') {
        format!("{content}\n")
    } else {
        content.to_owned()
    }
}

fn serialize_blockquote(children: &[Block]) -> String {
    let content = collapse_blank_runs(&serialize_children(children));
    let content = js_trim(&content);
    if content.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = content
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect();
    format!("{}\n\n", quoted.join("\n"))
}

/// `serializeList`. `start` defaults to 1, and so does 0 (`parseInt(...) || 1`).
fn serialize_list(ordered: bool, start: Option<u32>, items: &[ListItem]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let start = start.filter(|&start| start != 0).unwrap_or(1);
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(index, item)| serialize_list_item(item, ordered, start + index as u32))
        .collect();
    format!("{}\n\n", lines.join("\n"))
}

/// `serializeListItem`: the marker, then the content indented under it.
fn serialize_list_item(item: &ListItem, ordered: bool, number: u32) -> String {
    let task = match item.checked {
        Some(true) => "[x] ",
        Some(false) => "[ ] ",
        None => "",
    };
    let marker = if ordered {
        format!("{number}. {task}")
    } else {
        format!("- {task}")
    };
    // A loose item keeps its paragraph margins; a tight one reads as lines.
    let mut content = if item.spread {
        serialize_children(&item.children)
    } else {
        serialize_tight_children(&item.children)
    };
    content = collapse_blank_runs(&content);
    let mut content = js_trim(&content).to_owned();
    if !item.spread {
        content = collapse_double_newlines(&content);
    }
    let indent = " ".repeat(marker.chars().count());
    let mut lines = content.split('\n');
    let first = lines.next().unwrap_or("");
    let mut out = vec![format!("{marker}{first}")];
    out.extend(lines.map(|line| {
        if line.is_empty() {
            String::new()
        } else {
            format!("{indent}{line}")
        }
    }));
    out.join("\n")
}

/// A tight item's blocks: paragraphs are bare text, and blocks are separated by one
/// newline, as the renderer's inter-block whitespace separates them.
fn serialize_tight_children(children: &[Block]) -> String {
    children
        .iter()
        .map(|child| match child {
            Block::Paragraph(inlines) => js_trim(&serialize_inlines(inlines)).to_owned(),
            other => serialize_block(other),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `serializeCodeBlock`: the fence is longer than any backtick run in the code.
fn serialize_code_block(lang: Option<&str>, code: &str) -> String {
    let code = strip_one_trailing_newline(code);
    let fence = code_fence_for(code);
    let language = lang
        .filter(|language| !language.is_empty() && *language != "text")
        .unwrap_or("");
    format!("{fence}{language}\n{code}\n{fence}\n\n")
}

/// `codeFenceFor`: at least three backticks, and longer than any run of three or more.
fn code_fence_for(code: &str) -> String {
    let longest = longest_backtick_run(code, 3);
    "`".repeat(3.max(longest + 1))
}

/// `wrapInlineCode`: a fence longer than any backtick run, padded when the code starts
/// or ends with a backtick.
fn wrap_inline_code(code: &str) -> String {
    let longest = longest_backtick_run(code, 1);
    let fence = "`".repeat(1.max(longest + usize::from(longest > 0)));
    let pad = if code.starts_with('`') || code.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{code}{pad}{fence}")
}

/// The longest run of backticks at least `min` long (0 when there is none).
fn longest_backtick_run(text: &str, min: usize) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars().chain(std::iter::once(' ')) {
        if character == '`' {
            run += 1;
        } else {
            if run >= min {
                longest = longest.max(run);
            }
            run = 0;
        }
    }
    longest
}

/// `wrapInlineMarker`: whitespace around the content stays outside the marker.
fn wrap_inline_marker(content: &str, marker: &str) -> String {
    let core_start = content
        .char_indices()
        .find(|&(_, c)| !is_js_whitespace(c))
        .map(|(index, _)| index);
    let Some(core_start) = core_start else {
        return content.to_owned();
    };
    let core_end = content
        .char_indices()
        .rev()
        .find(|&(_, c)| !is_js_whitespace(c))
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(core_start);
    format!(
        "{}{marker}{}{marker}{}",
        &content[..core_start],
        &content[core_start..core_end],
        &content[core_end..]
    )
}

/// One run of text, or an image standing in for its alt, in the order it renders.
enum Piece<'a> {
    Text {
        text: &'a str,
        marks: Marks,
    },
    Image {
        alt: &'a str,
        src: &'a str,
        /// Link on the alt run, so an image inside a link stays inside it.
        link: Option<usize>,
    },
}

/// The inline content of a block: marks wrap their text, links wrap their label, and
/// `![alt](src)` replaces the alt span `push_image` left in the text.
fn serialize_inlines(inlines: &Inlines) -> String {
    let mut pieces: Vec<(usize, Piece<'_>)> = Vec::new();
    let mut image_index = 0;
    for (range, marks) in inlines.run_ranges() {
        let mut cursor = range.start;
        while image_index < inlines.images.len() {
            let (offset, alt, src) = &inlines.images[image_index];
            if *offset >= range.end {
                break;
            }
            if *offset < cursor {
                image_index += 1;
                continue;
            }
            if *offset > cursor {
                pieces.push((
                    cursor,
                    Piece::Text {
                        text: &inlines.text[cursor..*offset],
                        marks,
                    },
                ));
            }
            // The alt occupies the bytes `push_image` appended at this offset.
            let alt_end = match inlines.text.get(*offset..*offset + alt.len()) {
                Some(span) if span == alt.as_str() => *offset + alt.len(),
                _ => *offset,
            };
            pieces.push((
                *offset,
                Piece::Image {
                    alt: alt.as_str(),
                    src: src.as_str(),
                    link: marks.link,
                },
            ));
            cursor = alt_end.min(range.end);
            image_index += 1;
        }
        if cursor < range.end {
            pieces.push((
                cursor,
                Piece::Text {
                    text: &inlines.text[cursor..range.end],
                    marks,
                },
            ));
        }
    }

    let mut out = String::new();
    let mut link: Option<usize> = None;
    let mut link_content = String::new();
    for (_, piece) in &pieces {
        let (piece_link, content) = match piece {
            Piece::Text { text, marks } => (marks.link, piece_text(text, *marks)),
            Piece::Image { alt, src, link } => (
                *link,
                if alt.is_empty() || src.is_empty() {
                    String::new()
                } else {
                    format!("![{alt}]({src})")
                },
            ),
        };
        if piece_link != link {
            out.push_str(&flush_link(inlines, link, &link_content));
            link_content.clear();
            link = piece_link;
        }
        link_content.push_str(&content);
    }
    out.push_str(&flush_link(inlines, link, &link_content));
    out
}

/// A text run's markdown: code innermost, then strike, emphasis and strong.
fn piece_text(text: &str, marks: Marks) -> String {
    let mut content = if marks.code && !text.contains('\n') {
        wrap_inline_code(text)
    } else {
        text.to_owned()
    };
    if marks.delete {
        content = wrap_inline_marker(&content, "~~");
    }
    if marks.emphasis {
        content = wrap_inline_marker(&content, "*");
    }
    if marks.strong {
        content = wrap_inline_marker(&content, "**");
    }
    content
}

fn flush_link(inlines: &Inlines, link: Option<usize>, content: &str) -> String {
    match link.and_then(|index| inlines.links.get(index)) {
        Some(href) => anchor(content, href),
        None => content.to_owned(),
    }
}

/// `serializeAnchor`. A context reference or citation copies as its markdown, an
/// http(s) link as `[label](href)`, and anything else as its label.
fn anchor(content: &str, href: &str) -> String {
    if let Some(citation) = parse_assistant_citation_href(href) {
        return serialize_assistant_citation(&citation);
    }
    if parse_composer_context_href(href).is_some() {
        return format!("[{content}]({href})");
    }
    if !is_http_url(href) {
        return content.to_owned();
    }
    let label = js_trim(content);
    if label.is_empty() {
        return String::new();
    }
    if label == href {
        return href.to_owned();
    }
    format!("[{label}]({href})")
}

/// `/^https?:\/\//i`.
fn is_http_url(href: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        href.get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

/// Tracks whether a fragment carries exactly one code block and nothing else a reader
/// would see.
#[derive(Default)]
struct SoleCodeBlockScan<'a> {
    /// The first code block's text, once one is found.
    code: Option<&'a str>,
    /// How many code blocks were found (a second one makes the fragment not sole).
    code_blocks: usize,
    other: bool,
}

impl<'a> SoleCodeBlockScan<'a> {
    fn scan(&mut self, blocks: &'a [Block]) {
        for block in blocks {
            if self.other {
                return;
            }
            match block {
                Block::Paragraph(inlines)
                | Block::Heading {
                    content: inlines, ..
                } => {
                    if !inlines.text.trim().is_empty() || !inlines.images.is_empty() {
                        self.other = true;
                    }
                }
                Block::ThematicBreak => self.other = true,
                Block::Blockquote(children) | Block::Alert { children, .. } => self.scan(children),
                Block::List { items, .. } => {
                    for item in items {
                        // A list item renders its marker, so an item that holds no code
                        // block of its own carries content even when it has no text.
                        let before = self.code_blocks;
                        self.scan(&item.children);
                        if !self.other && self.code_blocks == before {
                            self.other = true;
                        }
                    }
                }
                Block::Code { code, .. } => {
                    if self.code_blocks > 0 {
                        self.other = true;
                    } else {
                        self.code = Some(code.as_str());
                        self.code_blocks = 1;
                    }
                }
                Block::Table { header, rows, .. } => {
                    if header
                        .iter()
                        .chain(rows.iter().flatten())
                        .any(|cell| !cell.text.trim().is_empty())
                    {
                        self.other = true;
                    }
                }
                Block::Html(text) => {
                    if !text.trim().is_empty() {
                        self.other = true;
                    }
                }
                Block::Footnotes(footnotes) => {
                    for (_, children) in footnotes {
                        self.scan(children);
                    }
                }
            }
        }
    }
}

/// `soleCodeBlock`: the code of the only block a fragment shows, if it shows nothing
/// else.
fn sole_code_block(blocks: &[Block]) -> Option<&str> {
    let mut scan = SoleCodeBlockScan::default();
    scan.scan(blocks);
    if scan.other { None } else { scan.code }
}

/// `tidyMarkdown`: trailing spaces and runs of blank lines go, outside fenced code.
fn tidy_markdown(markdown: &str) -> String {
    let mut out = String::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("```") {
        out.push_str(&tidy_prose(&rest[..start]));
        let after_open = start + 3;
        let end = rest[after_open..]
            .find("```")
            .map_or(rest.len(), |close| after_open + close + 3);
        out.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    out.push_str(&tidy_prose(rest));
    js_trim(&out).to_owned()
}

fn tidy_prose(text: &str) -> String {
    collapse_blank_runs(&strip_trailing_spaces_before_newlines(text))
}

fn strip_trailing_spaces_before_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = String::new();
    for character in text.chars() {
        match character {
            ' ' | '\t' => pending.push(character),
            '\n' => {
                pending.clear();
                out.push('\n');
            }
            other => {
                out.push_str(&pending);
                pending.clear();
                out.push(other);
            }
        }
    }
    out.push_str(&pending);
    out
}

/// `replace(/\n{3,}/g, "\n\n")`.
fn collapse_blank_runs(text: &str) -> String {
    collapse_newline_runs(text, 3, "\n\n")
}

/// `replace(/\n{2,}/g, "\n")`.
fn collapse_double_newlines(text: &str) -> String {
    collapse_newline_runs(text, 2, "\n")
}

/// Replaces each run of at least `min` newlines with `replacement`.
fn collapse_newline_runs(text: &str, min: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    let flush = |out: &mut String, run: usize| {
        if run >= min {
            out.push_str(replacement);
        } else {
            out.push_str(&"\n".repeat(run));
        }
    };
    for character in text.chars() {
        if character == '\n' {
            run += 1;
        } else {
            flush(&mut out, run);
            run = 0;
            out.push(character);
        }
    }
    flush(&mut out, run);
    out
}

/// `replace(/\n+/g, " ")` after collapsing spaces: a cell is one line.
fn collapse_newlines_to_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_newlines = false;
    for character in text.chars() {
        if character == '\n' {
            if !in_newlines {
                out.push(' ');
            }
            in_newlines = true;
        } else {
            in_newlines = false;
            out.push(character);
        }
    }
    out
}

/// `replace(/\s+/g, " ")` with JS whitespace.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for character in text.chars() {
        if is_js_whitespace(character) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            in_space = false;
            out.push(character);
        }
    }
    out
}

fn strip_one_trailing_newline(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::super::timeline::assistant_citations::{
        AssistantCitation, format_assistant_citation_href,
    };
    use super::*;

    fn plain(text: &str) -> Inlines {
        inlines(&[(text, Marks::default())])
    }

    fn code(text: &str) -> (&str, Marks) {
        (
            text,
            Marks {
                code: true,
                ..Marks::default()
            },
        )
    }

    fn strong(text: &str) -> (&str, Marks) {
        (
            text,
            Marks {
                strong: true,
                ..Marks::default()
            },
        )
    }

    /// Builds inline content from runs of text and their marks.
    fn inlines(parts: &[(&str, Marks)]) -> Inlines {
        let mut out = Inlines::default();
        for (text, marks) in parts {
            out.text.push_str(text);
            out.runs
                .push(super::super::markdown::markdown_pipeline::InlineRun {
                    len: text.len(),
                    marks: *marks,
                });
        }
        out
    }

    fn paragraph(parts: &[(&str, Marks)]) -> Block {
        Block::Paragraph(inlines(parts))
    }

    fn item(checked: Option<bool>, spread: bool, children: Vec<Block>) -> ListItem {
        ListItem {
            checked,
            children,
            spread,
        }
    }

    fn code_block(code: &str) -> Block {
        Block::Code {
            lang: None,
            meta: None,
            code: code.to_owned(),
            closed: true,
        }
    }

    fn serialize(blocks: &[Block]) -> String {
        serialize_rendered_markdown_fragment(blocks)
    }

    #[test]
    fn copies_an_image_in_place_of_its_alt_and_keeps_one_inside_a_link() {
        use super::super::markdown::markdown_pipeline::parse_chat_markdown;

        let doc = parse_chat_markdown("See ![cat](http://example.com/c.png) now", false);
        assert_eq!(
            serialize(&doc.blocks),
            "See ![cat](http://example.com/c.png) now"
        );
        let doc = parse_chat_markdown(
            "[See ![cat](http://example.com/c.png) now](http://example.com)",
            false,
        );
        assert_eq!(
            serialize(&doc.blocks),
            "[See ![cat](http://example.com/c.png) now](http://example.com)"
        );
    }

    #[test]
    fn wraps_inline_code_in_backticks() {
        let blocks = [paragraph(&[
            ("run ", Marks::default()),
            code("git status"),
            (" first", Marks::default()),
        ])];
        assert_eq!(serialize(&blocks), "run `git status` first");
    }

    #[test]
    fn keeps_a_multi_line_code_selection_plain_instead_of_inline_wrapping_it() {
        let blocks = [paragraph(&[code("first line\nsecond line")])];
        assert_eq!(serialize(&blocks), "first line\nsecond line");
    }

    #[test]
    fn keeps_a_code_selection_inside_a_block_as_its_text() {
        let fragments = [ClipboardFragment::CodeText(
            "git show-ref --verify refs/remotes/origin/opt/deploy/dev",
        )];
        assert_eq!(
            chat_markdown_clipboard_text(&fragments).as_deref(),
            Some("git show-ref --verify refs/remotes/origin/opt/deploy/dev")
        );
    }

    #[test]
    fn wraps_strong_text_and_hoists_its_surrounding_space() {
        let blocks = [paragraph(&[
            (" ", Marks::default()),
            strong("bold"),
            (" ", Marks::default()),
        ])];
        assert_eq!(serialize(&blocks), "**bold**");
        assert_eq!(wrap_inline_marker(" bold ", "**"), " **bold** ");
    }

    #[test]
    fn code_fences_outgrow_backtick_runs_in_the_code() {
        assert_eq!(code_fence_for("a ```b``` c"), "````");
        assert_eq!(wrap_inline_code("a`b"), "``a`b``");
        assert_eq!(wrap_inline_code("`x`"), "`` `x` ``");
    }

    #[test]
    fn keeps_a_tidy_fence_of_three_for_plain_code() {
        let blocks = [code_block("pnpm test")];
        assert_eq!(serialize(&blocks), "pnpm test");
    }

    #[test]
    fn copies_a_context_reference_once_as_its_markdown() {
        let reference = "[Review comment](t3-context://v1/review-comment/review-1)";
        let mut paragraph_inlines = inlines(&[
            ("Fix ", Marks::default()),
            (
                "Review comment",
                Marks {
                    link: Some(0),
                    ..Marks::default()
                },
            ),
            (" please", Marks::default()),
        ]);
        paragraph_inlines
            .links
            .push("t3-context://v1/review-comment/review-1".to_owned());
        assert_eq!(
            serialize(&[Block::Paragraph(paragraph_inlines)]),
            format!("Fix {reference} please")
        );
    }

    #[test]
    fn copies_a_citation_as_its_serialized_quote() {
        let citation = AssistantCitation {
            version: 1,
            environment_id: "environment-one".to_owned(),
            thread_id: "thread-one".to_owned(),
            message_id: "assistant-one".to_owned(),
            text: "A quote".to_owned(),
            comment: None,
            start: 0,
            end: 7,
            prefix: String::new(),
            suffix: String::new(),
        };
        let href = format_assistant_citation_href(&citation);
        let mut paragraph_inlines = inlines(&[
            ("Explain ", Marks::default()),
            (
                "A quote",
                Marks {
                    link: Some(0),
                    ..Marks::default()
                },
            ),
        ]);
        paragraph_inlines.links.push(href.to_owned());
        assert_eq!(
            serialize(&[Block::Paragraph(paragraph_inlines)]),
            format!("Explain {}", serialize_assistant_citation(&citation))
        );
    }

    #[test]
    fn keeps_fences_when_a_bare_list_item_sits_alongside_the_code_block() {
        // serializeListItem emits "- " for an item with no text, so the item is
        // content the plain-code path would drop.
        let blocks = [
            Block::List {
                ordered: false,
                start: None,
                spread: false,
                items: vec![item(None, false, Vec::new())],
            },
            code_block("pnpm test"),
        ];
        assert_eq!(serialize(&blocks), "-\n\n```\npnpm test\n```");
    }

    #[test]
    fn keeps_fences_when_a_checkbox_only_task_item_sits_alongside_the_code_block() {
        let blocks = [
            Block::List {
                ordered: false,
                start: None,
                spread: false,
                items: vec![item(Some(false), false, Vec::new())],
            },
            code_block("pnpm test"),
        ];
        assert_eq!(serialize(&blocks), "- [ ]\n\n```\npnpm test\n```");
    }

    #[test]
    fn still_drops_fences_for_a_code_block_that_is_the_whole_list_item() {
        // The item only wraps the block, so a selection that never left the pre would
        // drop the marker too.
        let blocks = [
            Block::List {
                ordered: false,
                start: None,
                spread: false,
                items: vec![item(None, false, vec![code_block("pnpm test")])],
            },
            Block::Paragraph(Inlines::default()),
        ];
        assert_eq!(serialize(&blocks), "pnpm test");
    }

    #[test]
    fn keeps_fences_when_a_file_chip_sits_alongside_the_code_block() {
        let blocks = [
            code_block("pnpm test"),
            Block::Paragraph(inlines(&[code("src/foo.ts")])),
        ];
        assert_eq!(serialize(&blocks), "```\npnpm test\n```\n\n`src/foo.ts`");
    }

    #[test]
    fn omits_fences_when_a_selection_past_the_last_line_drags_in_the_whole_code_block() {
        // Dragging over the final newline ends the range after the pre, so the fragment
        // carries the block plus the empty head of the next paragraph.
        let blocks = [
            code_block("printf '%s' 'TOKEN' | gh secret set CLOUDFLARE_API_TOKEN"),
            Block::Paragraph(Inlines::default()),
        ];
        assert_eq!(
            serialize(&blocks),
            "printf '%s' 'TOKEN' | gh secret set CLOUDFLARE_API_TOKEN"
        );
    }

    #[test]
    fn still_fences_a_code_block_copied_alongside_prose() {
        let blocks = [
            paragraph(&[("Run this:", Marks::default())]),
            code_block("gh workflow run Deploy --ref main"),
        ];
        assert_eq!(
            serialize(&blocks),
            "Run this:\n\n```\ngh workflow run Deploy --ref main\n```"
        );
    }

    #[test]
    fn list_layouts_keep_their_nesting_and_task_markers() {
        // Each row: the parent item's checkbox state, the child's, and the marker lines.
        let rows = [
            (None, Some(true), "- Parent", "  - [x] Child"),
            (Some(false), Some(true), "- [ ] Parent", "      - [x] Child"),
            (Some(true), Some(false), "- [x] Parent", "      - [ ] Child"),
        ];
        for parent_loose in [false, true] {
            for child_loose in [false, true] {
                for (parent_checked, child_checked, parent_line, child_line) in rows {
                    let child_list = Block::List {
                        ordered: false,
                        start: None,
                        spread: child_loose,
                        items: vec![item(
                            child_checked,
                            child_loose,
                            vec![paragraph(&[("Child", Marks::default())])],
                        )],
                    };
                    let parent = item(
                        parent_checked,
                        parent_loose,
                        vec![paragraph(&[("Parent", Marks::default())]), child_list],
                    );
                    let blocks = [
                        paragraph(&[("Before", Marks::default())]),
                        Block::List {
                            ordered: false,
                            start: None,
                            spread: parent_loose,
                            items: vec![parent],
                        },
                        paragraph(&[("After", Marks::default())]),
                    ];
                    let separator = if parent_loose { "\n\n" } else { "\n" };
                    assert_eq!(
                        serialize(&blocks),
                        format!("Before\n\n{parent_line}{separator}{child_line}\n\nAfter"),
                    );
                }
            }
        }
    }

    #[test]
    fn ordered_lists_number_from_their_start_and_tables_carry_alignment() {
        let blocks = [Block::List {
            ordered: true,
            start: Some(3),
            spread: false,
            items: vec![
                item(None, false, vec![paragraph(&[("a", Marks::default())])]),
                item(None, false, vec![paragraph(&[("b", Marks::default())])]),
            ],
        }];
        assert_eq!(serialize(&blocks), "3. a\n4. b");
        let table = serialize_table_to_markdown(
            &[Align::Center, Align::Right],
            &[plain("h1"), plain("h|2")],
            &[vec![plain("x"), plain("y")]],
        );
        assert_eq!(table, "| h1 | h\\|2 |\n| :---: | ---: |\n| x | y |");
    }

    #[test]
    fn table_csv_quotes_cells_that_need_it() {
        let csv = serialize_table_to_csv(
            &[plain("name"), plain("note")],
            &[vec![plain("a, b"), plain("say \"hi\"\n  now")]],
        );
        assert_eq!(csv, "name,note\n\"a, b\",\"say \"\"hi\"\" now\"");
    }

    #[test]
    fn a_sole_code_block_with_a_second_one_is_not_plain() {
        let blocks = [code_block("one"), code_block("two")];
        assert_eq!(serialize(&blocks), "```\none\n```\n\n```\ntwo\n```");
    }
}
