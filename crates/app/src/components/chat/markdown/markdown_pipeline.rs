//! Port of upstream/packages/shared/src/markdownPipeline.ts,
//! markdownGithubAlerts.ts and markdownListIndentation.ts.
//!
//! Upstream runs remark (`remark-gfm`, `remark-breaks` when `lineBreaks`)
//! plus its own plugins, then rehype, then React components. Turin parses
//! with markdown-rs (the Rust port of micromark/mdast, so the GFM AST is the
//! same one remark builds) and lowers the mdast into [`MarkdownDocument`], a
//! render-ready block tree whose inline content is already whitespace-collapsed
//! the way CSS `white-space: normal` would show it. The document is
//! theme-independent and cached per message text, so a re-render never
//! re-parses.
//!
//! Plugins ported: `remarkGithubAlerts` (`[!NOTE]` … blockquotes),
//! `remarkNormalizeListItemIndentation` (over-indented same-line list text is
//! prose, not an indented code block), `remarkBreaks` (`line_breaks`),
//! `remarkPreserveCodeMeta` (fence meta kept on [`Block::Code`]) and the
//! Windows-drive link normalisation. Raw HTML is shown as literal text, which
//! is what upstream's sanitizer leaves of anything outside its allowlist.

use std::ops::Range;

use markdown::mdast::{self, AlignKind, Node};
use markdown::{Constructs, ParseOptions};

/// `GITHUB_ALERT_PRESENTATIONS` keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    fn parse(marker: &str) -> Option<Self> {
        match marker.to_ascii_lowercase().as_str() {
            "note" => Some(Self::Note),
            "tip" => Some(Self::Tip),
            "important" => Some(Self::Important),
            "warning" => Some(Self::Warning),
            "caution" => Some(Self::Caution),
            _ => None,
        }
    }
}

/// Inline marks active over a run of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Marks {
    pub strong: bool,
    pub emphasis: bool,
    pub delete: bool,
    pub code: bool,
    /// Index into [`Inlines::links`].
    pub link: Option<usize>,
    /// Footnote reference (`[^1]`), rendered as a small label.
    pub footnote_ref: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineRun {
    pub len: usize,
    pub marks: Marks,
}

/// Collapsed inline content: text, runs covering it, and link targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inlines {
    pub text: String,
    pub runs: Vec<InlineRun>,
    pub links: Vec<String>,
    /// Images that appear inline: `(byte offset, alt, url)`.
    pub images: Vec<(usize, String, String)>,
}

impl Inlines {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }

    /// Byte ranges of each run.
    pub fn run_ranges(&self) -> impl Iterator<Item = (Range<usize>, Marks)> + '_ {
        let mut offset = 0;
        self.runs.iter().map(move |run| {
            let range = offset..offset + run.len;
            offset += run.len;
            (range, run.marks)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub children: Vec<Block>,
    /// mdast `spread`: loose items keep paragraph margins.
    pub spread: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Right,
    Center,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inlines),
    Heading {
        level: u8,
        content: Inlines,
    },
    ThematicBreak,
    Blockquote(Vec<Block>),
    Alert {
        kind: AlertKind,
        children: Vec<Block>,
    },
    List {
        ordered: bool,
        start: Option<u32>,
        spread: bool,
        items: Vec<ListItem>,
    },
    Code {
        lang: Option<String>,
        meta: Option<String>,
        code: String,
        /// A fence that has not been closed yet (streaming).
        closed: bool,
    },
    Table {
        align: Vec<Align>,
        header: Vec<Inlines>,
        rows: Vec<Vec<Inlines>>,
    },
    /// Raw HTML, shown as literal text.
    Html(String),
    /// `section[data-footnotes]`.
    Footnotes(Vec<(String, Vec<Block>)>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownDocument {
    pub blocks: Vec<Block>,
}

/// `CHAT_MARKDOWN_REMARK_PLUGINS` / `_WITH_BREAKS`.
pub fn parse_chat_markdown(text: &str, line_breaks: bool) -> MarkdownDocument {
    let options = ParseOptions {
        constructs: Constructs {
            // remark-gfm, without the math/MDX/frontmatter extensions upstream
            // does not load.
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    };
    let Ok(root) = markdown::to_mdast(text, &options) else {
        return MarkdownDocument {
            blocks: vec![Block::Paragraph(plain_inlines(text))],
        };
    };
    let mut cx = Lowering {
        source: text,
        line_breaks,
        footnotes: Vec::new(),
    };
    let children = root.children().cloned().unwrap_or_default();
    let mut blocks = cx.blocks(&children, None);
    if !cx.footnotes.is_empty() {
        blocks.push(Block::Footnotes(std::mem::take(&mut cx.footnotes)));
    }
    MarkdownDocument { blocks }
}

/// `shouldPreserveAssistantLineBreaks`.
pub fn should_preserve_assistant_line_breaks(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("★ Insight")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('─'))
    })
}

fn plain_inlines(text: &str) -> Inlines {
    let mut builder = InlineBuilder::new(false);
    builder.push_text(text, Marks::default());
    builder.finish()
}

struct Lowering<'a> {
    source: &'a str,
    line_breaks: bool,
    footnotes: Vec<(String, Vec<Block>)>,
}

/// `GITHUB_ALERT_MARKER`: `[!NOTE]` on its own line.
fn github_alert_marker(text: &str) -> Option<(AlertKind, usize, bool)> {
    let rest = text.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = AlertKind::parse(&rest[..close])?;
    let after = &rest[close + 1..];
    let consumed = 2 + close + 1;
    if after.is_empty() {
        Some((kind, consumed, false))
    } else if let Some(stripped) = after.strip_prefix("\r\n") {
        let _ = stripped;
        Some((kind, consumed + 2, true))
    } else if after.starts_with('\n') {
        Some((kind, consumed + 1, true))
    } else {
        None
    }
}

impl Lowering<'_> {
    fn blocks(&mut self, nodes: &[Node], parent: Option<&Node>) -> Vec<Block> {
        let mut out = Vec::new();
        for node in nodes {
            // remarkNormalizeListItemIndentation: an indented code block that
            // starts on its list marker's line is accidental alignment.
            if let (Node::Code(code), Some(Node::ListItem(item))) = (node, parent)
                && code.value.starts_with([' ', '\t'])
                && let (Some(code_pos), Some(item_pos)) = (&code.position, &item.position)
                && code_pos.start.line == item_pos.start.line
                && !matches!(
                    self.source.as_bytes().get(code_pos.start.offset),
                    Some(b'`' | b'~')
                )
            {
                let recovered = parse_chat_markdown(code.value.trim(), self.line_breaks);
                out.extend(recovered.blocks);
                continue;
            }
            if let Some(block) = self.block(node) {
                out.push(block);
            }
        }
        out
    }

    fn block(&mut self, node: &Node) -> Option<Block> {
        Some(match node {
            Node::Paragraph(p) => {
                let inlines = self.inlines(&p.children);
                if inlines.is_empty() && inlines.images.is_empty() {
                    return None;
                }
                Block::Paragraph(inlines)
            }
            Node::Heading(h) => Block::Heading {
                level: h.depth,
                content: self.inlines(&h.children),
            },
            Node::ThematicBreak(_) => Block::ThematicBreak,
            Node::Blockquote(q) => self.blockquote(q),
            Node::List(list) => Block::List {
                ordered: list.ordered,
                start: list.start,
                spread: list.spread,
                items: list
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        Node::ListItem(item) => Some(ListItem {
                            checked: item.checked,
                            spread: item.spread,
                            children: self.blocks(&item.children, Some(child)),
                        }),
                        _ => None,
                    })
                    .collect(),
            },
            Node::Code(code) => {
                let closed = code
                    .position
                    .as_ref()
                    .is_none_or(|pos| is_closed_code_fence(self.source, pos));
                Block::Code {
                    lang: code.lang.clone().map(|lang| {
                        // Shiki doesn't bundle a gitignore grammar; ini is a close match (#685)
                        if lang == "gitignore" {
                            "ini".into()
                        } else {
                            lang
                        }
                    }),
                    meta: code
                        .meta
                        .as_ref()
                        .map(|meta| meta.trim().to_string())
                        .filter(|meta| !meta.is_empty()),
                    code: code.value.clone(),
                    closed,
                }
            }
            Node::Table(table) => self.table(table),
            Node::Html(html) => Block::Html(html.value.clone()),
            Node::FootnoteDefinition(def) => {
                let children = self.blocks(&def.children, None);
                self.footnotes.push((def.identifier.clone(), children));
                return None;
            }
            Node::Definition(_) => return None,
            Node::Math(math) => Block::Code {
                lang: Some("math".into()),
                meta: None,
                code: math.value.clone(),
                closed: true,
            },
            // Stray inline nodes at block level read as a paragraph.
            other => {
                let inlines = self.inlines(std::slice::from_ref(other));
                if inlines.is_empty() {
                    return None;
                }
                Block::Paragraph(inlines)
            }
        })
    }

    /// `remarkGithubAlerts` + the `blockquote` renderer.
    fn blockquote(&mut self, quote: &mdast::Blockquote) -> Block {
        let mut children = quote.children.clone();
        let mut alert = None;
        if let Some(Node::Paragraph(paragraph)) = children.first_mut()
            && let Some(Node::Text(text)) = paragraph.children.first_mut()
            && let Some((kind, consumed, ends_line)) = github_alert_marker(&text.value)
        {
            let remainder = text.value[consumed..].to_string();
            if !remainder.is_empty() {
                // The quote continues on the next line inside the same text node.
                text.value = remainder;
                alert = Some(kind);
            } else if ends_line || paragraph.children.len() == 1 {
                // The marker was the whole text node.
                paragraph.children.remove(0);
                if paragraph.children.is_empty() {
                    children.remove(0);
                }
                alert = Some(kind);
            }
            // Something else shares the marker's line: an ordinary quote.
        }
        let blocks = self.blocks(&children, None);
        match alert {
            Some(kind) => Block::Alert {
                kind,
                children: blocks,
            },
            None => Block::Blockquote(blocks),
        }
    }

    fn table(&mut self, table: &mdast::Table) -> Block {
        let mut rows = table.children.iter().filter_map(|row| match row {
            Node::TableRow(row) => Some(
                row.children
                    .iter()
                    .map(|cell| match cell {
                        Node::TableCell(cell) => self.inlines_owned(&cell.children),
                        _ => Inlines::default(),
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        });
        let header = rows.next().unwrap_or_default();
        let rows = rows.collect();
        Block::Table {
            align: table
                .align
                .iter()
                .map(|align| match align {
                    AlignKind::Left => Align::Left,
                    AlignKind::Right => Align::Right,
                    AlignKind::Center => Align::Center,
                    AlignKind::None => Align::None,
                })
                .collect(),
            header,
            rows,
        }
    }

    fn inlines_owned(&self, nodes: &[Node]) -> Inlines {
        let mut builder = InlineBuilder::new(self.line_breaks);
        for node in nodes {
            builder.node(node, Marks::default());
        }
        builder.finish()
    }

    fn inlines(&self, nodes: &[Node]) -> Inlines {
        self.inlines_owned(nodes)
    }
}

/// `isClosedCodeFence`: the fence's closing marker exists in the source.
fn is_closed_code_fence(source: &str, position: &markdown::unist::Position) -> bool {
    let Some(slice) = source.get(position.start.offset..position.end.offset) else {
        return true;
    };
    let trimmed = slice.trim_start();
    let fence_char = match trimmed.chars().next() {
        Some(c @ ('`' | '~')) => c,
        // Indented code has no fence to close.
        _ => return true,
    };
    let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();
    let mut lines = slice.lines();
    lines.next();
    lines.any(|line| {
        let line = line.trim();
        line.len() >= fence_len && line.chars().all(|c| c == fence_char)
    })
}

/// Builds collapsed inline text the way CSS `white-space: normal` renders it:
/// runs of spaces, tabs and soft line breaks become one space, and spaces
/// next to a forced break disappear.
struct InlineBuilder {
    line_breaks: bool,
    text: String,
    runs: Vec<InlineRun>,
    links: Vec<String>,
    images: Vec<(usize, String, String)>,
}

impl InlineBuilder {
    fn new(line_breaks: bool) -> Self {
        Self {
            line_breaks,
            text: String::new(),
            runs: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
        }
    }

    fn push_raw(&mut self, s: &str, marks: Marks) {
        if s.is_empty() {
            return;
        }
        self.text.push_str(s);
        match self.runs.last_mut() {
            Some(last) if last.marks == marks => last.len += s.len(),
            _ => self.runs.push(InlineRun {
                len: s.len(),
                marks,
            }),
        }
    }

    fn last_char(&self) -> Option<char> {
        self.text.chars().next_back()
    }

    fn push_text(&mut self, s: &str, marks: Marks) {
        let mut chunk = String::with_capacity(s.len());
        let mut pending_space = false;
        for c in s.chars() {
            if c == '\n' && self.line_breaks {
                // remark-breaks: every soft line break is a <br>.
                if !chunk.is_empty() {
                    self.push_raw(&chunk, marks);
                    chunk.clear();
                }
                pending_space = false;
                self.hard_break(marks);
                continue;
            }
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                pending_space = true;
                continue;
            }
            if pending_space {
                let prev = chunk.chars().next_back().or_else(|| self.last_char());
                if prev.is_some_and(|p| p != ' ' && p != '\n') {
                    chunk.push(' ');
                }
                pending_space = false;
            }
            chunk.push(c);
        }
        if pending_space {
            let prev = chunk.chars().next_back().or_else(|| self.last_char());
            if prev.is_some_and(|p| p != ' ' && p != '\n') {
                chunk.push(' ');
            }
        }
        self.push_raw(&chunk, marks);
    }

    fn hard_break(&mut self, marks: Marks) {
        // Spaces before a forced break are not rendered.
        while self.text.ends_with(' ') {
            self.text.pop();
            if let Some(last) = self.runs.last_mut() {
                last.len -= 1;
                if last.len == 0 {
                    self.runs.pop();
                }
            }
        }
        self.push_raw("\n", marks);
    }

    fn node(&mut self, node: &Node, marks: Marks) {
        match node {
            Node::Text(text) => self.push_text(&text.value, marks),
            Node::Strong(n) => {
                for child in &n.children {
                    self.node(
                        child,
                        Marks {
                            strong: true,
                            ..marks
                        },
                    );
                }
            }
            Node::Emphasis(n) => {
                for child in &n.children {
                    self.node(
                        child,
                        Marks {
                            emphasis: true,
                            ..marks
                        },
                    );
                }
            }
            Node::Delete(n) => {
                for child in &n.children {
                    self.node(
                        child,
                        Marks {
                            delete: true,
                            ..marks
                        },
                    );
                }
            }
            Node::InlineCode(code) => self.push_text(
                &code.value,
                Marks {
                    code: true,
                    ..marks
                },
            ),
            Node::InlineMath(math) => self.push_text(
                &math.value,
                Marks {
                    code: true,
                    ..marks
                },
            ),
            Node::Break(_) => self.hard_break(marks),
            Node::Link(link) => {
                let ix = self.links.len();
                self.links.push(normalize_link_url(&link.url));
                for child in &link.children {
                    self.node(
                        child,
                        Marks {
                            link: Some(ix),
                            ..marks
                        },
                    );
                }
            }
            Node::LinkReference(reference) => {
                // Unresolved references render as their text.
                for child in &reference.children {
                    self.node(child, marks);
                }
            }
            Node::Image(image) => {
                self.images
                    .push((self.text.len(), image.alt.clone(), image.url.clone()));
                if !image.alt.is_empty() {
                    self.push_text(&image.alt, marks);
                }
            }
            Node::ImageReference(image) => self.push_text(&image.alt, marks),
            Node::FootnoteReference(reference) => self.push_raw(
                &reference
                    .label
                    .clone()
                    .unwrap_or_else(|| reference.identifier.clone()),
                Marks {
                    footnote_ref: true,
                    ..marks
                },
            ),
            Node::Html(html) => self.push_text(&html.value, marks),
            other => {
                if let Some(children) = other.children() {
                    for child in children {
                        self.node(child, marks);
                    }
                }
            }
        }
    }

    fn finish(mut self) -> Inlines {
        // Leading and trailing white space of a block is not rendered.
        while self.text.ends_with([' ', '\n']) {
            self.text.pop();
            if let Some(last) = self.runs.last_mut() {
                last.len -= 1;
                if last.len == 0 {
                    self.runs.pop();
                }
            }
        }
        let leading = self.text.len() - self.text.trim_start_matches(' ').len();
        if leading > 0 {
            self.text.drain(..leading);
            let mut remove = leading;
            while remove > 0 && !self.runs.is_empty() {
                let take = remove.min(self.runs[0].len);
                self.runs[0].len -= take;
                remove -= take;
                if self.runs[0].len == 0 {
                    self.runs.remove(0);
                }
            }
            for image in &mut self.images {
                image.0 = image.0.saturating_sub(leading);
            }
        }
        Inlines {
            text: self.text,
            runs: self.runs,
            links: self.links,
            images: self.images,
        }
    }
}

/// `remarkNormalizeLinksAndTagInlineCode`: Windows drive links become `file:` URLs.
fn normalize_link_url(url: &str) -> String {
    let bytes = url.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return format!("file:///{}", url.replace('\\', "/"));
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(doc: &MarkdownDocument) -> &Inlines {
        match &doc.blocks[0] {
            Block::Paragraph(inlines) => inlines,
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn collapses_soft_breaks_and_spaces() {
        let doc = parse_chat_markdown("one   two\nthree", false);
        assert_eq!(para(&doc).text, "one two three");
    }

    #[test]
    fn line_breaks_mode_keeps_newlines() {
        let doc = parse_chat_markdown("one \ntwo", true);
        assert_eq!(para(&doc).text, "one\ntwo");
    }

    #[test]
    fn marks_inline_code_and_links() {
        let doc = parse_chat_markdown("see `a  b` and [docs](https://x.dev)", false);
        let inlines = para(&doc);
        assert_eq!(inlines.text, "see a b and docs");
        let marks: Vec<_> = inlines.run_ranges().collect();
        assert!(
            marks
                .iter()
                .any(|(r, m)| m.code && &inlines.text[r.clone()] == "a b")
        );
        assert_eq!(inlines.links, vec!["https://x.dev".to_string()]);
    }

    #[test]
    fn github_alert_marker_on_its_own_line() {
        let doc = parse_chat_markdown("> [!WARNING]\n> Careful", false);
        match &doc.blocks[0] {
            Block::Alert { kind, children } => {
                assert_eq!(*kind, AlertKind::Warning);
                assert_eq!(children.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        let doc = parse_chat_markdown("> [!NOTE] aside", false);
        assert!(matches!(doc.blocks[0], Block::Blockquote(_)));
    }

    #[test]
    fn over_indented_list_text_is_prose() {
        let doc = parse_chat_markdown("-       text", false);
        match &doc.blocks[0] {
            Block::List { items, .. } => {
                assert!(matches!(items[0].children[0], Block::Paragraph(_)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unclosed_fence_is_streaming() {
        let doc = parse_chat_markdown("```rust\nfn main() {}", false);
        assert!(matches!(doc.blocks[0], Block::Code { closed: false, .. }));
        let doc = parse_chat_markdown("```rust\nfn main() {}\n```", false);
        assert!(matches!(doc.blocks[0], Block::Code { closed: true, .. }));
    }

    #[test]
    fn insight_blocks_preserve_line_breaks() {
        assert!(!should_preserve_assistant_line_breaks("`★ Insight ─────`"));
        assert!(should_preserve_assistant_line_breaks("★ Insight ─────"));
        assert!(!should_preserve_assistant_line_breaks("Insight"));
    }

    #[test]
    fn windows_drive_links_become_file_urls() {
        assert_eq!(normalize_link_url("C:\\a\\b.txt"), "file:///C:/a/b.txt");
    }
}
