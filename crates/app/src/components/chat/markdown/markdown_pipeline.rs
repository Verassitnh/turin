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

use std::collections::HashMap;
use std::ops::Range;

use markdown::mdast::{self, AlignKind, Node, ReferenceKind};
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

/// What an autolink plugin made a link from (`data-pull-request-autolink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAutolink {
    PullRequestReference,
    PullRequestCommit,
}

/// Collapsed inline content: text, runs covering it, and link targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inlines {
    pub text: String,
    pub runs: Vec<InlineRun>,
    pub links: Vec<String>,
    /// Links an extra remark plugin created: `(index into links, what it linked)`.
    pub autolinks: Vec<(usize, LinkAutolink)>,
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
    let Ok(root) = markdown::to_mdast(text, &parse_options()) else {
        return MarkdownDocument {
            blocks: vec![Block::Paragraph(plain_inlines(text))],
        };
    };
    let children = root.children().cloned().unwrap_or_default();
    let definitions = link_definitions(&children, text);
    let mut cx = Lowering {
        source: text,
        line_breaks,
        footnotes: Vec::new(),
        definitions: &definitions,
    };
    let mut blocks = cx.blocks(&children, None);
    if !cx.footnotes.is_empty() {
        blocks.push(Block::Footnotes(std::mem::take(&mut cx.footnotes)));
    }
    MarkdownDocument { blocks }
}

fn parse_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs {
            // remark-gfm, without the math/MDX/frontmatter extensions upstream
            // does not load.
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    }
}

/// `shouldPreserveAssistantLineBreaks`.
pub fn should_preserve_assistant_line_breaks(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("★ Insight")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace) || rest.starts_with('─'))
    })
}

fn plain_inlines(text: &str) -> Inlines {
    let definitions = LinkDefinitions::new();
    let mut builder = InlineBuilder::new(false, "", &definitions);
    builder.push_text(text, Marks::default());
    builder.finish()
}

/// `INLINE_PARSE_PREFIX` (markdownListIndentation.ts).
const INLINE_PARSE_PREFIX: &str = "t3-markdown-inline-prefix:";

struct Lowering<'a> {
    source: &'a str,
    line_breaks: bool,
    footnotes: Vec<(String, Vec<Block>)>,
    definitions: &'a LinkDefinitions,
}

/// Resolved link definition URLs by identifier. A definition may sit anywhere
/// in the document, and the first one for an identifier wins.
type LinkDefinitions = HashMap<String, String>;

fn link_definitions(nodes: &[Node], source: &str) -> LinkDefinitions {
    let mut definitions = HashMap::new();
    collect_link_definitions(nodes, source, &mut definitions);
    definitions
}

fn collect_link_definitions(nodes: &[Node], source: &str, out: &mut LinkDefinitions) {
    for node in nodes {
        if let Node::Definition(definition) = node {
            // The same Windows-path rule as inline destinations, applied to the
            // definition's authored destination.
            let url = match definition
                .position
                .as_ref()
                .and_then(|position| authored_definition_destination(source, position))
            {
                Some(authored) => keep_windows_path_destination(authored, &definition.url),
                None => definition.url.clone(),
            };
            out.entry(definition.identifier.clone())
                .or_insert_with(|| normalize_link_url(&url));
        }
        if let Some(children) = node.children() {
            collect_link_definitions(children, source, out);
        }
    }
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
                out.extend(self.recovered_blocks(code.value.trim()));
                continue;
            }
            if let Some(block) = self.block(node) {
                out.push(block);
            }
        }
        out
    }

    /// `parseRecoveredMarkdown`: the text is parsed behind a prefix, which
    /// forces block-looking input (`# Heading`, `- nested`, a fence) into a
    /// paragraph while GFM inline syntax still parses. Root children after the
    /// first paragraph are kept as blocks.
    fn recovered_blocks(&mut self, value: &str) -> Vec<Block> {
        let source = format!("{INLINE_PARSE_PREFIX}{value}");
        let Ok(root) = markdown::to_mdast(&source, &parse_options()) else {
            return vec![Block::Paragraph(plain_inlines(value))];
        };
        let mut children = root.children().cloned().unwrap_or_default();
        let stripped = if let Some(Node::Paragraph(paragraph)) = children.first_mut()
            && let Some(Node::Text(text)) = paragraph.children.first_mut()
            && let Some(rest) = text.value.strip_prefix(INLINE_PARSE_PREFIX)
        {
            let rest = rest.to_string();
            if rest.is_empty() {
                paragraph.children.remove(0);
            } else {
                text.value = rest;
            }
            true
        } else {
            false
        };
        if !stripped {
            return vec![Block::Paragraph(plain_inlines(value))];
        }
        // The recovered text is its own parse, so its references resolve against
        // its own definitions.
        let definitions = link_definitions(&children, &source);
        let mut inner = Lowering {
            source: &source,
            line_breaks: self.line_breaks,
            footnotes: std::mem::take(&mut self.footnotes),
            definitions: &definitions,
        };
        let blocks = inner.blocks(&children, None);
        self.footnotes = inner.footnotes;
        blocks
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
        let mut builder = InlineBuilder::new(self.line_breaks, self.source, self.definitions);
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
struct InlineBuilder<'a> {
    line_breaks: bool,
    /// The document source, for authored link destinations.
    source: &'a str,
    definitions: &'a LinkDefinitions,
    text: String,
    runs: Vec<InlineRun>,
    links: Vec<String>,
    images: Vec<(usize, String, String)>,
}

impl<'a> InlineBuilder<'a> {
    fn new(line_breaks: bool, source: &'a str, definitions: &'a LinkDefinitions) -> Self {
        Self {
            line_breaks,
            source,
            definitions,
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

    /// The link's destination, with [`keep_windows_path_destination`] applied.
    fn link_destination(&self, link: &mdast::Link) -> String {
        let authored = link
            .position
            .as_ref()
            .and_then(|position| authored_destination(self.source, position, &link.children));
        match authored {
            Some(authored) => keep_windows_path_destination(authored, &link.url),
            None => link.url.clone(),
        }
    }

    fn push_link(&mut self, url: String, children: &[Node], marks: Marks) {
        let ix = self.links.len();
        self.links.push(url);
        for child in children {
            self.node(
                child,
                Marks {
                    link: Some(ix),
                    ..marks
                },
            );
        }
    }

    fn push_image(&mut self, url: String, alt: &str, marks: Marks) {
        self.images.push((self.text.len(), alt.to_string(), url));
        if !alt.is_empty() {
            self.push_text(alt, marks);
        }
    }

    /// `revert` in mdast-util-to-hast: a reference without a definition is
    /// its source text, brackets and label included.
    fn revert_reference(
        &mut self,
        children: &[Node],
        kind: ReferenceKind,
        label: Option<&str>,
        identifier: &str,
        marks: Marks,
    ) {
        self.push_text("[", marks);
        for child in children {
            self.node(child, marks);
        }
        self.push_text(&reference_suffix(kind, label, identifier), marks);
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
                let url = normalize_link_url(&self.link_destination(link));
                self.push_link(url, &link.children, marks);
            }
            Node::LinkReference(reference) => {
                match self.definitions.get(&reference.identifier).cloned() {
                    Some(url) => self.push_link(url, &reference.children, marks),
                    None => self.revert_reference(
                        &reference.children,
                        reference.reference_kind,
                        reference.label.as_deref(),
                        &reference.identifier,
                        marks,
                    ),
                }
            }
            Node::Image(image) => self.push_image(image.url.clone(), &image.alt, marks),
            Node::ImageReference(image) => match self.definitions.get(&image.identifier).cloned() {
                Some(url) => self.push_image(url, &image.alt, marks),
                None => {
                    let suffix = reference_suffix(
                        image.reference_kind,
                        image.label.as_deref(),
                        &image.identifier,
                    );
                    self.push_text(&format!("![{}{suffix}", image.alt), marks);
                }
            },
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
            autolinks: Vec::new(),
            images: self.images,
        }
    }
}

/// `keepWindowsPathDestination`: a link destination that is a Windows path keeps
/// its authored text. CommonMark reads the `\.` in `C:\me\.t3\shot.png` as an
/// escape, so the decoded URL loses backslashes. A destination with `&` keeps the
/// decoded URL, since it may hold character references.
fn keep_windows_path_destination(authored: &str, decoded: &str) -> String {
    if is_windows_absolute_path(authored) && !authored.contains('&') {
        authored.to_string()
    } else {
        decoded.to_string()
    }
}

/// `isWindowsAbsolutePath` (path.ts): a UNC path or a drive path.
fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_none_or(|&b| b == b'/' || b == b'\\');
    drive || value.starts_with("\\\\")
}

/// The destination as written between `](` and the closing `)`, or between `<`
/// and `>` (the source `sliceSerialize` gives for `resourceDestinationString`).
fn authored_destination<'s>(
    source: &'s str,
    position: &markdown::unist::Position,
    label: &[Node],
) -> Option<&'s str> {
    // The label ends at its last child, or right after the opening bracket.
    let label_end = label
        .last()
        .and_then(Node::position)
        .map_or(position.start.offset + 1, |end| end.end.offset);
    let link_source = source.get(label_end..position.end.offset)?;
    let open = link_source.find("](")? + 2;
    destination_at(&link_source[open..])
}

/// The destination of a definition, as written after its `]:`.
fn authored_definition_destination<'s>(
    source: &'s str,
    position: &markdown::unist::Position,
) -> Option<&'s str> {
    let definition_source = source.get(position.start.offset..position.end.offset)?;
    let open = definition_source.find("]:")? + 2;
    destination_at(&definition_source[open..])
}

/// A destination at the start of `rest` (after optional white space): either
/// `<...>` or a bare run that ends at white space or an unmatched `)`.
fn destination_at(rest: &str) -> Option<&str> {
    let rest = rest.trim_start_matches([' ', '\t', '\n', '\r']);
    if let Some(inner) = rest.strip_prefix('<') {
        return inner.split_once('>').map(|(dest, _)| dest);
    }
    // Balanced parentheses may appear in a bare destination; whitespace or an
    // unmatched `)` ends it.
    let mut depth = 0usize;
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            c if c.is_whitespace() => break,
            _ => {}
        }
        end = i + c.len_utf8();
    }
    Some(&rest[..end])
}

/// The text after a reference's `[text]`: `]` for a shortcut, `][]` for a
/// collapsed reference, `][label]` for a full one (`label || identifier`).
fn reference_suffix(kind: ReferenceKind, label: Option<&str>, identifier: &str) -> String {
    match kind {
        ReferenceKind::Shortcut => "]".to_string(),
        ReferenceKind::Collapsed => "][]".to_string(),
        ReferenceKind::Full => {
            let label = label
                .filter(|label| !label.is_empty())
                .unwrap_or(identifier);
            format!("][{label}]")
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
    fn over_indented_list_block_syntax_is_inline_prose() {
        for (text, expected) in [
            ("-      # Heading", "# Heading"),
            ("-      - nested", "- nested"),
            ("-      ```", "```"),
        ] {
            let doc = parse_chat_markdown(text, false);
            match &doc.blocks[0] {
                Block::List { items, .. } => match &items[0].children[0] {
                    Block::Paragraph(inlines) => assert_eq!(inlines.text, expected, "{text}"),
                    other => panic!("{text}: {other:?}"),
                },
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[test]
    fn over_indented_list_text_keeps_gfm_inline_syntax() {
        let doc = parse_chat_markdown("-      **bold** `code`", false);
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{doc:?}");
        };
        let Block::Paragraph(inlines) = &items[0].children[0] else {
            panic!("{doc:?}");
        };
        assert_eq!(inlines.text, "bold code");
        assert!(
            inlines
                .run_ranges()
                .any(|(range, marks)| marks.strong && &inlines.text[range] == "bold")
        );
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

    #[test]
    fn windows_path_destinations_keep_their_backslashes() {
        // `\.` is a CommonMark escape; the authored Windows path must win.
        let doc = parse_chat_markdown(r"[x](C:\me\.t3\shot.png)", false);
        assert_eq!(
            para(&doc).links,
            vec!["file:///C:/me/.t3/shot.png".to_string()]
        );
        let doc = parse_chat_markdown(r"[x](<C:\me\.t3\shot.png>)", false);
        assert_eq!(
            para(&doc).links,
            vec!["file:///C:/me/.t3/shot.png".to_string()]
        );
    }

    #[test]
    fn windows_path_destinations_with_ampersands_keep_decoding() {
        // A destination with `&` may hold character references, so it stays decoded.
        let doc = parse_chat_markdown(r"[x](C:\a&amp;b\c.txt)", false);
        assert_eq!(para(&doc).links, vec!["file:///C:/a&b/c.txt".to_string()]);
    }

    #[test]
    fn relative_destinations_still_decode_escapes() {
        let doc = parse_chat_markdown(r"[x](docs/a\.md)", false);
        assert_eq!(para(&doc).links, vec!["docs/a.md".to_string()]);
    }

    #[test]
    fn full_collapsed_and_shortcut_references_resolve_to_their_definitions() {
        let doc = parse_chat_markdown(
            "[docs][d], [again][], and [third]\n\n[d]: https://x.dev\n[again]: https://y.dev\n[third]: https://z.dev",
            false,
        );
        let inlines = para(&doc);
        assert_eq!(inlines.text, "docs, again, and third");
        assert_eq!(
            inlines.links,
            vec![
                "https://x.dev".to_string(),
                "https://y.dev".to_string(),
                "https://z.dev".to_string(),
            ]
        );
    }

    #[test]
    fn reference_labels_match_case_and_white_space() {
        let doc = parse_chat_markdown(
            "[Docs  Page][DOCS page]\n\n[docs page]: https://x.dev",
            false,
        );
        assert_eq!(para(&doc).links, vec!["https://x.dev".to_string()]);
    }

    #[test]
    fn unresolved_references_keep_their_source_text() {
        let doc = parse_chat_markdown("[a][missing] [b][] [WIP] fix", false);
        let inlines = para(&doc);
        assert_eq!(inlines.text, "[a][missing] [b][] [WIP] fix");
        assert!(inlines.links.is_empty());
    }

    #[test]
    fn first_definition_of_an_identifier_wins() {
        let doc = parse_chat_markdown(
            "[x][a]\n\n[a]: https://one.dev\n[a]: https://two.dev",
            false,
        );
        assert_eq!(para(&doc).links, vec!["https://one.dev".to_string()]);
    }

    #[test]
    fn definitions_inside_containers_resolve_across_the_document() {
        let doc = parse_chat_markdown("see [a]\n\n> [a]: https://x.dev", false);
        assert_eq!(para(&doc).links, vec!["https://x.dev".to_string()]);
    }

    #[test]
    fn reference_definitions_keep_windows_path_destinations() {
        // The definition's `\.` is a CommonMark escape; the authored path must win.
        let doc = parse_chat_markdown("[x][shot]\n\n[shot]: C:\\me\\.t3\\shot.png", false);
        assert_eq!(
            para(&doc).links,
            vec!["file:///C:/me/.t3/shot.png".to_string()]
        );
    }

    #[test]
    fn reference_images_resolve_or_keep_their_source_text() {
        let doc = parse_chat_markdown("![alt][pic]\n\n[pic]: https://img.dev/a.png", false);
        let inlines = para(&doc);
        assert_eq!(inlines.text, "alt");
        assert_eq!(
            inlines.images,
            vec![(0, "alt".to_string(), "https://img.dev/a.png".to_string())]
        );
        let doc = parse_chat_markdown("![alt][missing]", false);
        assert_eq!(para(&doc).text, "![alt][missing]");
        assert!(para(&doc).images.is_empty());
    }
}
