//! Port of upstream/apps/web/src/composer-rich-text-doc.ts (document model
//! and serializer; the ProseMirror schema/extension glue is replaced by
//! [`super::editor_state`]).
//!
//! The stored prompt stays markdown (`**bold**`, `@file` chips as canonical
//! links). The document holds styled text plus inline atom chips, so this
//! module translates both ways and maps cursor offsets between the three
//! coordinate spaces the composer speaks:
//!
//! - flat document offsets (styled markers excluded, chips count 1, one
//!   position per block boundary),
//! - collapsed cursor offsets (markers literal, chips count 1: the coordinate
//!   the draft store and mention detection use),
//! - markdown offsets (markers literal, chips expand to their source).
//!
//! Upstream models blocks as nested ProseMirror nodes. Every block here is one
//! source line (a code fence is one block holding several), so the document is
//! a flat `Vec<Block>`; list nesting stays in each item's `indent` and is
//! derived by [`list_depths`], exactly as upstream's parser derives it. All
//! lengths are UTF-16 code units, like upstream.

use std::sync::OnceLock;

use regex::Regex;

use crate::composer::editor_mentions::{
    ComposerPromptSegment, split_prompt_into_composer_segments,
};
use crate::composer::js_string::{is_js_whitespace_char, utf16_len};
use crate::composer::rich_text::{MARK_NESTING_ORDER, RichTextMark, parse_inline_markdown};

// ── Marks ──────────────────────────────────────────────────────────────────

/// The set of marks on a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Marks(u8);

impl Marks {
    pub const NONE: Marks = Marks(0);

    fn bit(mark: RichTextMark) -> u8 {
        match mark {
            RichTextMark::Bold => 1,
            RichTextMark::Italic => 2,
            RichTextMark::Strike => 4,
            RichTextMark::Code => 8,
        }
    }

    pub fn of(marks: &[RichTextMark]) -> Self {
        Marks(marks.iter().fold(0, |acc, mark| acc | Self::bit(*mark)))
    }

    pub fn has(self, mark: RichTextMark) -> bool {
        self.0 & Self::bit(mark) != 0
    }

    pub fn with(self, mark: RichTextMark) -> Self {
        Marks(self.0 | Self::bit(mark))
    }

    pub fn without(self, mark: RichTextMark) -> Self {
        Marks(self.0 & !Self::bit(mark))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// In `MARK_NESTING_ORDER`.
    pub fn list(self) -> Vec<RichTextMark> {
        MARK_NESTING_ORDER
            .iter()
            .copied()
            .filter(|mark| self.has(*mark))
            .collect()
    }
}

// ── Inline content ─────────────────────────────────────────────────────────

/// Display metadata of a skill chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Atom {
    Mention {
        path: String,
        source: String,
    },
    Skill {
        name: String,
        label: String,
        description: Option<String>,
    },
}

impl Atom {
    /// `readAtomSource`.
    pub fn source(&self) -> String {
        match self {
            Atom::Mention { source, .. } => source.clone(),
            Atom::Skill { name, .. } if !name.is_empty() => format!("${name}"),
            Atom::Skill { .. } => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineContent {
    Text(String),
    Atom(Atom),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineNode {
    pub content: InlineContent,
    pub marks: Marks,
}

impl InlineNode {
    pub fn text(text: impl Into<String>, marks: Marks) -> Self {
        Self {
            content: InlineContent::Text(text.into()),
            marks,
        }
    }

    pub fn atom(atom: Atom, marks: Marks) -> Self {
        Self {
            content: InlineContent::Atom(atom),
            marks,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self.content, InlineContent::Text(_))
    }

    /// Document length: UTF-16 units of text, 1 for a chip.
    pub fn doc_len(&self) -> usize {
        match &self.content {
            InlineContent::Text(text) => utf16_len(text),
            InlineContent::Atom(_) => 1,
        }
    }
}

/// Inline content of one block, kept normalised: no empty text nodes and no
/// two adjacent text nodes with equal marks.
pub type Inline = Vec<InlineNode>;

pub fn inline_len(inline: &[InlineNode]) -> usize {
    inline.iter().map(InlineNode::doc_len).sum()
}

pub fn normalize_inline(inline: Inline) -> Inline {
    let mut out: Inline = Vec::with_capacity(inline.len());
    for node in inline {
        if let InlineContent::Text(text) = &node.content {
            if text.is_empty() {
                continue;
            }
            if let Some(last) = out.last_mut()
                && let InlineContent::Text(previous) = &mut last.content
                && last.marks == node.marks
            {
                previous.push_str(text);
                continue;
            }
        }
        out.push(node);
    }
    out
}

// ── Blocks ─────────────────────────────────────────────────────────────────

/// `TaskLinePrefix` / the list line grammar. `indent` is the exact leading
/// whitespace of the source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemPrefix {
    Task {
        indent: String,
        checked: bool,
        marker_space: String,
        content_space: String,
    },
    Bullet {
        indent: String,
        marker: String,
        space: String,
    },
    Ordered {
        indent: String,
        marker: String,
        space: String,
    },
}

impl ItemPrefix {
    pub fn indent(&self) -> &str {
        match self {
            Self::Task { indent, .. }
            | Self::Bullet { indent, .. }
            | Self::Ordered { indent, .. } => indent,
        }
    }

    pub fn set_indent(&mut self, value: String) {
        match self {
            Self::Task { indent, .. }
            | Self::Bullet { indent, .. }
            | Self::Ordered { indent, .. } => {
                *indent = value;
            }
        }
    }

    /// Items of one kind and marker family belong to one list; a change starts
    /// a sibling list (`listKey`).
    pub fn list_key(&self) -> String {
        match self {
            Self::Task { .. } => "task".to_owned(),
            Self::Ordered { marker, .. } => {
                format!("ordered:{}", marker.chars().last().unwrap_or('.'))
            }
            Self::Bullet { marker, .. } => format!("bullet:{marker}"),
        }
    }

    /// `listItemPrefix`: the literal prefix an item serializes to. Empty items
    /// keep their exact spacing.
    pub fn text(&self, empty: bool) -> String {
        match self {
            Self::Task {
                indent,
                checked,
                marker_space,
                content_space,
            } => {
                let content_space = if content_space.is_empty() && !empty {
                    " "
                } else {
                    content_space
                };
                format!(
                    "{indent}-{marker_space}[{}]{content_space}",
                    if *checked { "x" } else { " " }
                )
            }
            Self::Bullet {
                indent,
                marker,
                space,
            }
            | Self::Ordered {
                indent,
                marker,
                space,
            } => {
                let marker = if marker.is_empty() { "-" } else { marker };
                // A bare `-` keeps its missing space only while the item is
                // empty: once it has text, `-text` would not be a list line.
                let space = if space.is_empty() && !empty {
                    " "
                } else {
                    space
                };
                format!("{indent}{marker}{space}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// The fence's info string, exactly as written after the fence run.
    pub language: String,
    pub fence: String,
    /// Closing newline and fence, or empty when the fence was never closed.
    pub close: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inline),
    Heading {
        level: u8,
        space: String,
        content: Inline,
    },
    /// One quote line behind its exact `>` prefix.
    Quote {
        prefix: String,
        content: Inline,
    },
    /// A rule keeps the exact line it was written as. It owns no characters.
    Rule {
        source: String,
    },
    Item {
        prefix: ItemPrefix,
        content: Inline,
    },
    Code(CodeBlock),
}

impl Block {
    pub fn inline(&self) -> Option<&Inline> {
        match self {
            Block::Paragraph(content)
            | Block::Heading { content, .. }
            | Block::Quote { content, .. }
            | Block::Item { content, .. } => Some(content),
            Block::Rule { .. } | Block::Code(_) => None,
        }
    }

    pub fn inline_mut(&mut self) -> Option<&mut Inline> {
        match self {
            Block::Paragraph(content)
            | Block::Heading { content, .. }
            | Block::Quote { content, .. }
            | Block::Item { content, .. } => Some(content),
            Block::Rule { .. } | Block::Code(_) => None,
        }
    }

    /// Document length of the block's content.
    pub fn doc_len(&self) -> usize {
        match self {
            Block::Rule { .. } => 0,
            Block::Code(code) => utf16_len(&code.text),
            other => inline_len(other.inline().expect("inline block")),
        }
    }

    pub fn is_textblock(&self) -> bool {
        !matches!(self, Block::Rule { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
}

impl Doc {
    /// The empty document: one empty paragraph.
    pub fn empty() -> Self {
        Self {
            blocks: vec![Block::Paragraph(Vec::new())],
        }
    }
}

// ── Line grammar ───────────────────────────────────────────────────────────

struct Re;
impl Re {
    fn task() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^([ \t]*)-([ \t]+)\[([ xX])\]([ \t]*)").unwrap())
    }
    fn ordered() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^([ \t]*)(\d+[.)])([ \t]+|$)").unwrap())
    }
    fn bullet() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^([ \t]*)([-*+])([ \t]+|$)").unwrap())
    }
    fn fence_open() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^(`{3,}|~{3,})([^\n]*)$").unwrap())
    }
    fn fence_close() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^(`{3,}|~{3,})[ \t]*\r?$").unwrap())
    }
    fn rule() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(r"^(?:-(?:[ \t]*-){2,}|\*(?:[ \t]*\*){2,}|_(?:[ \t]*_){2,})[ \t]*\r?$")
                .unwrap()
        })
    }
    fn heading() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^(#{1,6})([ \t]+)([^\n]*)$").unwrap())
    }
    fn quote() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"^(>[ \t]*)([^\n]*)$").unwrap())
    }
}

/// `parseTaskPrefix`.
fn parse_task_prefix(head: &str) -> Option<(ItemPrefix, usize)> {
    let captures = Re::task().captures(head)?;
    let whole = captures.get(0)?;
    let after = &head[whole.end()..];
    let content_space = captures.get(4).map_or("", |m| m.as_str());
    if !after.is_empty() && content_space.is_empty() {
        return None;
    }
    Some((
        ItemPrefix::Task {
            indent: captures[1].to_owned(),
            checked: captures[3].eq_ignore_ascii_case("x"),
            marker_space: captures[2].to_owned(),
            content_space: content_space.to_owned(),
        },
        whole.end(),
    ))
}

/// `parseListPrefix`: the same grammar the literal continuation uses, so plain
/// and rich mode agree on what a list line is. Returns the prefix and its byte
/// length.
pub fn parse_list_prefix(line: &str) -> Option<(ItemPrefix, usize)> {
    if let Some(task) = parse_task_prefix(line) {
        return Some(task);
    }
    if let Some(captures) = Re::ordered().captures(line) {
        return Some((
            ItemPrefix::Ordered {
                indent: captures[1].to_owned(),
                marker: captures[2].to_owned(),
                space: captures[3].to_owned(),
            },
            captures.get(0)?.end(),
        ));
    }
    if let Some(captures) = Re::bullet().captures(line) {
        return Some((
            ItemPrefix::Bullet {
                indent: captures[1].to_owned(),
                marker: captures[2].to_owned(),
                space: captures[3].to_owned(),
            },
            captures.get(0)?.end(),
        ));
    }
    None
}

/// `parseOpeningFence`: three or more backticks or tildes at the start of a
/// line, followed by an info string. A backtick fence cannot carry a backtick
/// in its info string. Indented fences stay paragraphs.
pub fn parse_opening_fence(line: &str) -> Option<(String, String)> {
    let captures = Re::fence_open().captures(line)?;
    let fence = captures[1].to_owned();
    let language = captures[2].to_owned();
    if fence.starts_with('`') && language.contains('`') {
        return None;
    }
    Some((fence, language))
}

/// `isClosingFence`: matches the opening run's character and is at least as
/// long.
pub fn is_closing_fence(line: &str, fence: &str) -> bool {
    let Some(captures) = Re::fence_close().captures(line) else {
        return false;
    };
    let run = &captures[1];
    run.chars().next() == fence.chars().next() && run.chars().count() >= fence.chars().count()
}

// ── Markdown → document ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    /// Inline marks (rich text). Off: every marker stays a literal character.
    pub styling: bool,
    /// Block syntax (lists, fences, quotes, rules, headings). Only with styling.
    pub blocks: bool,
    /// Editor answers verbatim: one paragraph per line, no chips.
    pub literal_text: bool,
    /// Keep this many leading characters as plain text (a paste continuing a
    /// path query).
    pub literal_length: usize,
}

impl BuildOptions {
    pub fn rich() -> Self {
        Self {
            styling: true,
            blocks: true,
            literal_text: false,
            literal_length: 0,
        }
    }

    pub fn plain() -> Self {
        Self {
            styling: false,
            blocks: true,
            literal_text: false,
            literal_length: 0,
        }
    }
}

fn atom_for_segment(
    segment: &ComposerPromptSegment,
    skill_label_for: &dyn Fn(&str) -> SkillMeta,
) -> Atom {
    match segment {
        ComposerPromptSegment::Mention { path, source } => Atom::Mention {
            path: path.clone(),
            source: source.clone(),
        },
        ComposerPromptSegment::Skill { name, .. } => {
            let meta = skill_label_for(name);
            Atom::Skill {
                name: name.clone(),
                label: meta.label,
                description: meta.description,
            }
        }
        ComposerPromptSegment::Text { .. } => unreachable!("text is not an atom"),
    }
}

enum Entry {
    Code(CodeBlock),
    Rule(String),
    Heading {
        level: u8,
        space: String,
        inline: Inline,
    },
    Quote {
        prefix: String,
        inline: Inline,
    },
    Line {
        list: Option<ItemPrefix>,
        inline: Inline,
    },
}

/// `buildTiptapContent`, as blocks.
pub fn build_blocks(
    value: &str,
    skill_label_for: &dyn Fn(&str) -> SkillMeta,
    options: BuildOptions,
) -> Vec<Block> {
    // Editor answers are verbatim text, including Markdown and context-token sources.
    if options.literal_text {
        return value
            .split('\n')
            .map(|line| {
                Block::Paragraph(if line.is_empty() {
                    Vec::new()
                } else {
                    vec![InlineNode::text(line, Marks::NONE)]
                })
            })
            .collect();
    }
    let styling = options.styling;
    let block_syntax = styling && options.blocks;

    // Hide token source from the markdown parser, then restore the atoms with
    // the marks of their surrounding text. Choose a sentinel absent from input.
    let mut sentinel = '\u{FFFC}';
    let mut code_point = 0xE000u32;
    while value.contains(sentinel) {
        sentinel = char::from_u32(code_point).unwrap_or('\u{FFFC}');
        code_point += 1;
    }
    let literal: String = value.chars().take(options.literal_length).collect();
    let rest: String = value.chars().skip(options.literal_length).collect();
    let mut atoms: Vec<Atom> = Vec::new();
    let mut atom_sources: Vec<String> = Vec::new();
    let mut text = String::new();
    for segment in split_prompt_into_composer_segments(&rest) {
        match &segment {
            ComposerPromptSegment::Text { text: piece } => text.push_str(piece),
            other => {
                atoms.push(atom_for_segment(other, skill_label_for));
                atom_sources.push(other.source().to_owned());
                text.push(sentinel);
            }
        }
    }
    let literal_prefix = literal;
    let mut atom_index = 0usize;

    let build_inline = |content: &str, atom_index: &mut usize| -> Inline {
        let spans = if styling {
            parse_inline_markdown(content)
        } else {
            vec![crate::composer::rich_text::RichTextSpan {
                text: content.to_owned(),
                marks: Vec::new(),
            }]
        };
        let mut inline = Vec::new();
        for span in spans {
            let marks = Marks::of(&span.marks);
            for (index, piece) in span.text.split(sentinel).enumerate() {
                if index > 0
                    && let Some(atom) = atoms.get(*atom_index)
                {
                    *atom_index += 1;
                    inline.push(InlineNode::atom(atom.clone(), marks));
                }
                if !piece.is_empty() {
                    inline.push(InlineNode::text(piece, marks));
                }
            }
        }
        normalize_inline(inline)
    };

    let source_lines: Vec<&str> = text.split('\n').collect();
    let mut entries: Vec<Entry> = Vec::new();

    let restore_sources = |line: &str, atom_index: &mut usize| -> String {
        let mut joined = String::new();
        for (index, piece) in line.split(sentinel).enumerate() {
            if index > 0 {
                *atom_index += 1;
                if let Some(source) = atom_sources.get(*atom_index - 1) {
                    joined.push_str(source);
                }
            }
            joined.push_str(piece);
        }
        joined
    };

    let mut index = 0usize;
    while index < source_lines.len() {
        let line = source_lines[index];
        // The rest of the literal's line continues the text before the caret,
        // so it cannot open a block.
        if index == 0 && !literal_prefix.is_empty() {
            let mut inline = vec![InlineNode::text(literal_prefix.clone(), Marks::NONE)];
            inline.extend(build_inline(line, &mut atom_index));
            entries.push(Entry::Line {
                list: None,
                inline: normalize_inline(inline),
            });
            index += 1;
            continue;
        }
        let opening = if block_syntax {
            parse_opening_fence(line)
        } else {
            None
        };
        let Some((fence, info)) = opening else {
            // A thematic break outranks a list: `- - -` and `* * *` are rules,
            // and `***` on its own line is a rule rather than an empty bold span.
            if block_syntax && Re::rule().is_match(line) {
                entries.push(Entry::Rule(line.to_owned()));
                index += 1;
                continue;
            }
            if block_syntax && let Some(captures) = Re::heading().captures(line) {
                entries.push(Entry::Heading {
                    level: captures[1].len() as u8,
                    space: captures[2].to_owned(),
                    inline: build_inline(&captures[3], &mut atom_index),
                });
                index += 1;
                continue;
            }
            if block_syntax && let Some(captures) = Re::quote().captures(line) {
                entries.push(Entry::Quote {
                    prefix: captures[1].to_owned(),
                    inline: build_inline(&captures[2], &mut atom_index),
                });
                index += 1;
                continue;
            }
            let parsed = if block_syntax {
                parse_list_prefix(line)
            } else {
                None
            };
            let content = parsed.as_ref().map_or(line, |(_, len)| &line[*len..]);
            entries.push(Entry::Line {
                list: parsed.map(|(prefix, _)| prefix),
                inline: build_inline(content, &mut atom_index),
            });
            index += 1;
            continue;
        };
        // The info string went through the sentinel pass like every line, so a
        // token in it is put back as source here, in order, before the body.
        let language = restore_sources(&info, &mut atom_index);
        let mut body: Vec<String> = Vec::new();
        let mut cursor = index + 1;
        let mut close = String::new();
        while cursor < source_lines.len() {
            let candidate = source_lines[cursor];
            if is_closing_fence(candidate, &fence) {
                close = format!("\n{candidate}");
                break;
            }
            body.push(restore_sources(candidate, &mut atom_index));
            cursor += 1;
        }
        // An unclosed fence runs to the end of the prompt, which is what the
        // user is looking at while they are still typing the block.
        let closed = cursor < source_lines.len();
        entries.push(Entry::Code(CodeBlock {
            language,
            fence,
            close,
            text: body.join("\n"),
        }));
        index = if closed {
            cursor + 1
        } else {
            source_lines.len()
        };
    }

    // Pass 2: upstream groups consecutive list lines into (possibly nested)
    // lists by indent prefix. Items are flattened in source order here and
    // keep their exact `indent`, so the grouping is [`list_depths`] on demand
    // and serializing a flat item list reproduces the source byte for byte.
    entries
        .into_iter()
        .map(|entry| match entry {
            Entry::Quote { prefix, inline } => Block::Quote {
                prefix,
                content: inline,
            },
            Entry::Code(code) => Block::Code(code),
            Entry::Rule(source) => Block::Rule { source },
            Entry::Heading {
                level,
                space,
                inline,
            } => Block::Heading {
                level,
                space,
                content: inline,
            },
            Entry::Line { list: None, inline } => Block::Paragraph(inline),
            Entry::Line {
                list: Some(prefix),
                inline,
            } => Block::Item {
                prefix,
                content: inline,
            },
        })
        .collect()
}

/// `buildDocJson`: the document, with the empty line the rule extension keeps
/// after a final rule.
pub fn build_doc(
    value: &str,
    skill_label_for: &dyn Fn(&str) -> SkillMeta,
    options: BuildOptions,
) -> Doc {
    let mut blocks = build_blocks(value, skill_label_for, options);
    if matches!(blocks.last(), Some(Block::Rule { .. })) {
        blocks.push(Block::Paragraph(Vec::new()));
    }
    if blocks.is_empty() {
        blocks.push(Block::Paragraph(Vec::new()));
    }
    Doc { blocks }
}

/// Display nesting depth of every block (0 for non-items), derived from the
/// items' `indent` with the same stack rules as the parser.
pub fn list_depths(doc: &Doc) -> Vec<usize> {
    let mut depths = vec![0usize; doc.blocks.len()];
    let mut stack: Vec<String> = Vec::new();
    for (index, block) in doc.blocks.iter().enumerate() {
        let Block::Item { prefix, .. } = block else {
            stack.clear();
            continue;
        };
        let indent = prefix.indent();
        loop {
            let Some(top) = stack.last() else {
                stack.push(indent.to_owned());
                continue;
            };
            if top == indent {
                break;
            }
            if !top.is_empty() && !indent.starts_with(top.as_str()) {
                if stack.len() > 1 {
                    stack.pop();
                    continue;
                }
                *stack.last_mut().expect("top") = indent.to_owned();
                break;
            }
            stack.push(indent.to_owned());
            break;
        }
        depths[index] = stack.len() - 1;
    }
    depths
}

// ── Document → markdown, with offset runs ──────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Text,
    Token,
    Break,
    Prefix,
}

#[derive(Debug, Clone)]
pub struct RichRun {
    pub kind: RunKind,
    /// Flat document offset (atoms count 1, markers excluded).
    pub flat_start: usize,
    pub doc_len: usize,
    /// Collapsed cursor length (markers literal, tokens count 1).
    pub collapsed_len: usize,
    /// Markdown length (tokens expand to their source).
    pub md_len: usize,
    /// Marker layout inside text runs.
    pub open_len: usize,
    pub close_len: usize,
    pub md_start: usize,
    pub collapsed_start: usize,
    pub code_block: bool,
    /// Block the run belongs to (`None` for breaks).
    pub block: Option<usize>,
    /// Offset of the run's content inside its block's content.
    pub block_offset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DocMap {
    pub value: String,
    pub runs: Vec<RichRun>,
    pub doc_length: usize,
    /// Flat offset where each block's content starts.
    pub block_starts: Vec<usize>,
    /// Block that owns each markdown line (diagnostics / tests).
    pub block_count: usize,
}

struct Acc {
    runs: Vec<RichRun>,
    value: String,
    flat: usize,
    collapsed: usize,
    md: usize,
}

impl Acc {
    fn push_break(&mut self) {
        self.runs.push(RichRun {
            kind: RunKind::Break,
            flat_start: self.flat,
            doc_len: 1,
            collapsed_len: 1,
            md_len: 1,
            open_len: 0,
            close_len: 0,
            md_start: self.md,
            collapsed_start: self.collapsed,
            code_block: false,
            block: None,
            block_offset: 0,
        });
        self.value.push('\n');
        self.flat += 1;
        self.collapsed += 1;
        self.md += 1;
    }

    fn push_prefix(&mut self, prefix: &str, block: usize) {
        let len = utf16_len(prefix);
        self.runs.push(RichRun {
            kind: RunKind::Prefix,
            flat_start: self.flat,
            doc_len: 0,
            collapsed_len: len,
            md_len: len,
            open_len: 0,
            close_len: 0,
            md_start: self.md,
            collapsed_start: self.collapsed,
            code_block: false,
            block: Some(block),
            block_offset: 0,
        });
        self.value.push_str(prefix);
        self.collapsed += len;
        self.md += len;
    }
}

fn has_content_on_both_sides_filter(children: &mut [InlineNode]) {
    // Emphasis cannot open or close next to whitespace. Retain a whitespace
    // mark only when its range has visible content on both sides.
    for mark in MARK_NESTING_ORDER {
        if mark == RichTextMark::Code {
            continue;
        }
        for direction in [1i32, -1] {
            let mut has_content = false;
            let mut index: i32 = if direction == 1 {
                0
            } else {
                children.len() as i32 - 1
            };
            while index >= 0 && (index as usize) < children.len() {
                let child = &mut children[index as usize];
                let is_ws_text = matches!(&child.content, InlineContent::Text(text)
                    if !child.marks.has(RichTextMark::Code)
                        && !text.is_empty()
                        && text.chars().all(is_js_whitespace_char));
                if !child.marks.has(mark) {
                    has_content = false;
                } else if is_ws_text {
                    if !has_content {
                        child.marks = child.marks.without(mark);
                    }
                } else {
                    has_content = true;
                }
                index += direction;
            }
        }
    }
}

fn split_boundary_whitespace(inline: &[InlineNode]) -> Vec<InlineNode> {
    let mut children = Vec::new();
    for child in inline {
        let InlineContent::Text(text) = &child.content else {
            children.push(child.clone());
            continue;
        };
        if child.marks.has(RichTextMark::Code) {
            children.push(child.clone());
            continue;
        }
        // Separate boundary whitespace so delimiters can move past it without
        // changing the document offsets or marks on the visible text.
        let chars: Vec<char> = text.chars().collect();
        let start = chars
            .iter()
            .take_while(|c| is_js_whitespace_char(**c))
            .count();
        let end = chars
            .iter()
            .rposition(|c| !is_js_whitespace_char(*c))
            .map_or(0, |at| at + 1)
            .max(start);
        let mut offset = 0;
        for boundary in [start, end, chars.len()] {
            if boundary > offset {
                children.push(InlineNode::text(
                    chars[offset..boundary].iter().collect::<String>(),
                    child.marks,
                ));
            }
            offset = boundary;
        }
    }
    children
}

/// The marker nesting chosen for each child of an inline run (longer shared
/// marks surround shorter ones).
fn ordered_marks(children: &[InlineNode]) -> Vec<Vec<RichTextMark>> {
    let mut mark_ends: [Option<usize>; 4] = [None; 4];
    let slot = |mark: RichTextMark| match mark {
        RichTextMark::Strike => 0,
        RichTextMark::Bold => 1,
        RichTextMark::Italic => 2,
        RichTextMark::Code => 3,
    };
    let mut ordered: Vec<Vec<RichTextMark>> = vec![Vec::new(); children.len()];
    for index in (0..children.len()).rev() {
        let marks = children[index].marks;
        for mark in MARK_NESTING_ORDER {
            if !marks.has(mark) {
                mark_ends[slot(mark)] = None;
            } else if mark_ends[slot(mark)].is_none() {
                mark_ends[slot(mark)] = Some(index);
            }
        }
        let mut list = marks.list();
        list.sort_by(|a, b| {
            let end_a = mark_ends[slot(*a)].unwrap_or(0);
            let end_b = mark_ends[slot(*b)].unwrap_or(0);
            end_b.cmp(&end_a).then_with(|| {
                let order = |m: &RichTextMark| MARK_NESTING_ORDER.iter().position(|x| x == m);
                order(a).cmp(&order(b))
            })
        });
        ordered[index] = list;
    }
    for index in 1..ordered.len() {
        let marks = ordered[index].clone();
        let mut retained: Vec<RichTextMark> = Vec::new();
        for mark in &ordered[index - 1] {
            if !marks.contains(mark) {
                break;
            }
            retained.push(*mark);
        }
        let mut next = retained.clone();
        next.extend(
            marks
                .iter()
                .filter(|mark| !retained.contains(mark))
                .copied(),
        );
        ordered[index] = next;
    }
    ordered
}

fn common_length(left: &[RichTextMark], right: &[RichTextMark]) -> usize {
    left.iter().zip(right).take_while(|(a, b)| a == b).count()
}

/// `appendInlineRuns`.
fn append_inline_runs(inline: &[InlineNode], block: usize, acc: &mut Acc) {
    let mut children = split_boundary_whitespace(inline);
    has_content_on_both_sides_filter(&mut children);
    let ordered = ordered_marks(&children);
    let mut block_offset = 0usize;
    for (index, child) in children.iter().enumerate() {
        let marks = &ordered[index];
        let previous: &[RichTextMark] = if index == 0 { &[] } else { &ordered[index - 1] };
        let next: &[RichTextMark] = ordered.get(index + 1).map_or(&[], Vec::as_slice);
        let open: String = marks[common_length(marks, previous)..]
            .iter()
            .map(|mark| mark.delimiter())
            .collect();
        let close: String = marks[common_length(marks, next)..]
            .iter()
            .rev()
            .map(|mark| mark.delimiter())
            .collect();
        let (source, doc_len, kind) = match &child.content {
            InlineContent::Text(text) => (text.clone(), utf16_len(text), RunKind::Text),
            InlineContent::Atom(atom) => (atom.source(), 1, RunKind::Token),
        };
        let md_text = format!("{open}{source}{close}");
        let collapsed_len = utf16_len(&open) + doc_len + utf16_len(&close);
        let md_len = utf16_len(&md_text);
        acc.runs.push(RichRun {
            kind,
            flat_start: acc.flat,
            doc_len,
            collapsed_len,
            md_len,
            open_len: utf16_len(&open),
            close_len: utf16_len(&close),
            md_start: acc.md,
            collapsed_start: acc.collapsed,
            code_block: false,
            block: Some(block),
            block_offset,
        });
        block_offset += doc_len;
        acc.value.push_str(&md_text);
        acc.flat += doc_len;
        acc.collapsed += collapsed_len;
        acc.md += md_len;
    }
    // Empty paragraphs have an editable position even though they emit no text.
    if children.is_empty() {
        acc.runs.push(RichRun {
            kind: RunKind::Text,
            flat_start: acc.flat,
            doc_len: 0,
            collapsed_len: 0,
            md_len: 0,
            open_len: 0,
            close_len: 0,
            md_start: acc.md,
            collapsed_start: acc.collapsed,
            code_block: false,
            block: Some(block),
            block_offset: 0,
        });
    }
}

/// `codeBlockSource`: the markdown a code block serializes to, delimiters
/// included. `followed` says another block comes after it in the draft.
pub fn code_block_source(code: &CodeBlock, followed: bool) -> (String, String, String) {
    let mut fence = if code.fence.is_empty() {
        "```".to_owned()
    } else {
        code.fence.clone()
    };
    let mut close = code.close.clone();
    // Code holding a line that would close the fence, such as a pasted fence,
    // is written inside a longer one, so the agent reads all of it as code. A
    // parsed block never holds such a line, so its source stays byte-identical.
    let longest = code
        .text
        .split('\n')
        .filter(|line| is_closing_fence(line, &fence))
        .map(|line| line.chars().take_while(|c| *c == '`' || *c == '~').count())
        .max()
        .unwrap_or(0);
    if longest > 0 {
        let marker = fence.chars().next().unwrap_or('`');
        fence = marker.to_string().repeat(longest + 1);
        if !close.is_empty() {
            close = format!("\n{fence}");
        }
    }
    // An unclosed fence runs to the end of the draft, so with a block after it
    // the fence is written closed, or the next rebuild would read that block as code.
    if close.is_empty() && followed {
        close = format!("\n{fence}");
    }
    let open = format!(
        "{fence}{}{}",
        code.language,
        if code.text.is_empty() { "" } else { "\n" }
    );
    (open, code.text.clone(), close)
}

/// `appendCodeBlockRun`: a fence becomes runs whose open and close lengths are
/// the delimiters, split at each chip source (which spans its characters in
/// the document and one position in the collapsed space).
fn append_code_block_run(code: &CodeBlock, block: usize, followed: bool, acc: &mut Acc) {
    let (open, content, close) = code_block_source(code, followed);
    let pieces: Vec<(usize, usize)> = if content.is_empty() {
        vec![(0, 0)]
    } else {
        split_prompt_into_composer_segments(&content)
            .iter()
            .map(|segment| match segment {
                ComposerPromptSegment::Text { .. } => (segment.source_len(), segment.source_len()),
                _ => (segment.source_len(), 1),
            })
            .collect()
    };
    let mut offset = 0usize;
    let last = pieces.len() - 1;
    for (index, (length, collapsed_piece)) in pieces.into_iter().enumerate() {
        let open_len = if index == 0 { utf16_len(&open) } else { 0 };
        let close_len = if index == last { utf16_len(&close) } else { 0 };
        let collapsed_len = open_len + collapsed_piece + close_len;
        let md_len = open_len + length + close_len;
        acc.runs.push(RichRun {
            kind: RunKind::Text,
            flat_start: acc.flat,
            doc_len: length,
            collapsed_len,
            md_len,
            open_len,
            close_len,
            md_start: acc.md,
            collapsed_start: acc.collapsed,
            code_block: true,
            block: Some(block),
            block_offset: offset,
        });
        offset += length;
        acc.flat += length;
        acc.collapsed += collapsed_len;
        acc.md += md_len;
    }
    acc.value.push_str(&open);
    acc.value.push_str(&content);
    acc.value.push_str(&close);
}

/// `isTrailingLineAfterRule`: the empty line after a rule that ends the
/// document, which writes nothing.
fn is_trailing_line_after_rule(doc: &Doc, index: usize) -> bool {
    index == doc.blocks.len() - 1
        && index > 0
        && matches!(&doc.blocks[index], Block::Paragraph(inline) if inline.is_empty())
        && matches!(doc.blocks[index - 1], Block::Rule { .. })
}

/// `serializeEditorDoc`.
pub fn serialize_doc(doc: &Doc) -> DocMap {
    let mut acc = Acc {
        runs: Vec::new(),
        value: String::new(),
        flat: 0,
        collapsed: 0,
        md: 0,
    };
    let mut block_starts = Vec::with_capacity(doc.blocks.len());
    for (index, block) in doc.blocks.iter().enumerate() {
        if index > 0 && !is_trailing_line_after_rule(doc, index) {
            acc.push_break();
        }
        block_starts.push(acc.flat);
        match block {
            Block::Item { prefix, content } => {
                let text = prefix.text(content.is_empty());
                acc.push_prefix(&text, index);
                append_inline_runs(content, index, &mut acc);
            }
            Block::Code(code) => {
                append_code_block_run(code, index, index + 1 < doc.blocks.len(), &mut acc);
            }
            Block::Quote { prefix, content } => {
                acc.push_prefix(prefix, index);
                append_inline_runs(content, index, &mut acc);
            }
            Block::Heading {
                level,
                space,
                content,
            } => {
                let prefix = format!("{}{space}", "#".repeat(usize::from(*level)));
                acc.push_prefix(&prefix, index);
                append_inline_runs(content, index, &mut acc);
            }
            Block::Rule { source } => {
                let source = if source.is_empty() { "---" } else { source };
                acc.push_prefix(source, index);
            }
            Block::Paragraph(content) => append_inline_runs(content, index, &mut acc),
        }
    }
    DocMap {
        value: acc.value,
        runs: acc.runs,
        doc_length: acc.flat,
        block_starts,
        block_count: doc.blocks.len(),
    }
}

/// `serializeSelection` for a flat range: the stored markdown of the blocks
/// the range touches, cut to the range.
pub fn serialize_range(doc: &Doc, map: &DocMap, from_flat: usize, to_flat: usize) -> String {
    // The markdown between the two offsets, which already carries the item
    // and quote prefixes of the lines it crosses.
    let start = flat_to_markdown(map, from_flat.min(to_flat));
    let end = flat_to_markdown(map, to_flat.max(from_flat));
    let units: Vec<u16> = map.value.encode_utf16().collect();
    let _ = doc;
    String::from_utf16_lossy(&units[start.min(units.len())..end.min(units.len())])
}

fn last_run_end(map: &DocMap, collapsed: bool) -> usize {
    map.runs.last().map_or(0, |last| {
        if collapsed {
            last.collapsed_start + last.collapsed_len
        } else {
            last.md_start + last.md_len
        }
    })
}

/// The end of a run belongs to the next run, which is how the end of
/// `**bold**` lands after its markers. A fence is the exception: its close is
/// a line of its own, so the end of the code stays inside the block.
fn run_owns_offset(run: &RichRun, flat_offset: usize) -> bool {
    let end = run.flat_start + run.doc_len;
    flat_offset < end || (flat_offset == end && run.code_block)
}

pub fn flat_to_collapsed(map: &DocMap, flat_offset: usize) -> usize {
    let bounded = flat_offset.min(map.doc_length);
    for run in &map.runs {
        if run_owns_offset(run, bounded) {
            if matches!(run.kind, RunKind::Text | RunKind::Token) {
                // A chip's source in a fence spans its characters but one position.
                let within = (bounded - run.flat_start)
                    .min(run.collapsed_len - run.open_len - run.close_len);
                return run.collapsed_start + run.open_len + within;
            }
            return run.collapsed_start + (bounded - run.flat_start);
        }
    }
    last_run_end(map, true)
}

pub fn flat_to_markdown(map: &DocMap, flat_offset: usize) -> usize {
    let bounded = flat_offset.min(map.doc_length);
    for run in &map.runs {
        if run_owns_offset(run, bounded) {
            if matches!(run.kind, RunKind::Text | RunKind::Token) {
                return run.md_start + run.open_len + (bounded - run.flat_start);
            }
            return run.md_start + (bounded - run.flat_start);
        }
    }
    last_run_end(map, false)
}

pub fn collapsed_to_flat(map: &DocMap, collapsed_offset: usize) -> usize {
    for run in &map.runs {
        if collapsed_offset < run.collapsed_start + run.collapsed_len {
            // Checkbox prefixes and style markers are shown, never edited:
            // every offset inside them clamps to the adjacent document position.
            match run.kind {
                RunKind::Prefix => return run.flat_start,
                RunKind::Text | RunKind::Token => {
                    let within = collapsed_offset - run.collapsed_start;
                    let content_len = run.collapsed_len - run.open_len - run.close_len;
                    if within <= run.open_len {
                        return run.flat_start;
                    }
                    if within >= run.open_len + content_len {
                        return run.flat_start + run.doc_len;
                    }
                    return run.flat_start + (within - run.open_len);
                }
                RunKind::Break => return run.flat_start + (collapsed_offset - run.collapsed_start),
            }
        }
    }
    map.doc_length
}

// ── Positions ──────────────────────────────────────────────────────────────

/// A caret position: a block and an offset in the block's content (UTF-16
/// units, chips count 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct DocPos {
    pub block: usize,
    pub offset: usize,
}

impl DocPos {
    pub fn new(block: usize, offset: usize) -> Self {
        Self { block, offset }
    }
}

pub fn pos_to_flat(map: &DocMap, pos: DocPos) -> usize {
    let block = pos.block.min(map.block_starts.len().saturating_sub(1));
    map.block_starts.get(block).copied().unwrap_or(0) + pos.offset
}

/// The position a flat offset names. A rule owns no characters, so an offset
/// at a rule belongs to the block after it.
pub fn flat_to_pos(doc: &Doc, map: &DocMap, flat: usize) -> DocPos {
    let flat = flat.min(map.doc_length);
    let mut best = 0usize;
    for (index, start) in map.block_starts.iter().enumerate() {
        if *start <= flat {
            best = index;
        } else {
            break;
        }
    }
    // Prefer a later block that shares the start (rule then trailing line).
    let mut block = best;
    while block + 1 < map.block_starts.len()
        && map.block_starts[block + 1] == map.block_starts[block]
    {
        block += 1;
    }
    let len = doc.blocks[block].doc_len();
    DocPos::new(block, (flat - map.block_starts[block]).min(len))
}

#[cfg(test)]
mod tests;
