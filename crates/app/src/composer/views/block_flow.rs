//! Builds the [`FlowText`] the prompt editor paints for one block, plus the
//! map between document offsets (UTF-16 units, chips count 1) and the flow's
//! UTF-8 byte offsets.
//!
//! Styling follows `.composer-tiptap` (index.css) and `ContextChip`:
//! inline code is a `0.92em` mono span on a 12% muted tint, chips are
//! `rounded-[0.5em]` accent pills, and the style markers (`**`, `*`, `~~`,
//! `` ` ``) of the styled text the caret touches show as dim, non-editable
//! text (`.composer-rich-marker`).

use gpui_kit::{FontWeight, Hsla, Pixels, SharedString, px};

use crate::components::chat::markdown::inline_flow::{
    FlowSpan, FlowSpanStyle, FlowText, SpanChrome,
};
use crate::composer::js_string::utf16_len;
use crate::composer::rich_doc::{Atom, DocPos, Inline, InlineContent, InlineNode, Marks};
use crate::composer::rich_text::RichTextMark;
use crate::theme::AppTheme;
use crate::theme::color::{MixSpace, color, color_mix, with_alpha};
use crate::ui::IconName;

/// What the painter needs from the environment.
pub struct FlowEnv<'a> {
    pub theme: &'a AppTheme,
    pub rem: Pixels,
    /// Prompt font size.
    pub size: Pixels,
    pub family: SharedString,
    pub mono: SharedString,
    pub sans: SharedString,
}

/// Which marker stop the caret takes at a styled edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretSide {
    /// Inside the styled text before the edge (types with its marks).
    InsideBefore,
    /// Between the markers (types plain).
    Plain,
    /// Inside the styled text after the edge.
    InsideAfter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegKind {
    Text,
    Marker,
    Atom,
}

#[derive(Debug, Clone)]
pub struct Seg {
    pub kind: SegKind,
    pub doc_start: usize,
    pub doc_len: usize,
    pub byte_start: usize,
    pub byte_len: usize,
    /// The segment's text (for UTF-16 ↔ byte conversion inside text).
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct OffsetMap {
    pub segs: Vec<Seg>,
    pub doc_len: usize,
    pub byte_len: usize,
}

fn byte_for_units(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (at, c) in text.char_indices() {
        if seen >= units {
            return at;
        }
        seen += c.len_utf16();
    }
    text.len()
}

impl OffsetMap {
    /// Byte offset of document offset `doc`, resolving ambiguous marker
    /// boundaries with `side`.
    pub fn doc_to_byte(&self, doc: usize, side: CaretSide) -> usize {
        let mut candidates: Vec<usize> = Vec::new();
        for seg in &self.segs {
            match seg.kind {
                SegKind::Text => {
                    if doc >= seg.doc_start && doc <= seg.doc_start + seg.doc_len {
                        candidates
                            .push(seg.byte_start + byte_for_units(&seg.text, doc - seg.doc_start));
                    }
                }
                SegKind::Atom => {
                    if doc == seg.doc_start {
                        candidates.push(seg.byte_start);
                    } else if doc == seg.doc_start + seg.doc_len {
                        candidates.push(seg.byte_start + seg.byte_len);
                    }
                }
                SegKind::Marker => {
                    if doc == seg.doc_start {
                        candidates.push(seg.byte_start);
                        candidates.push(seg.byte_start + seg.byte_len);
                    }
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        match candidates.len() {
            0 => self.byte_len,
            1 => candidates[0],
            len => match side {
                CaretSide::InsideBefore => candidates[0],
                CaretSide::Plain => candidates[len / 2],
                CaretSide::InsideAfter => candidates[len - 1],
            },
        }
    }

    /// Document offset for a byte offset (a click or vertical move).
    pub fn byte_to_doc(&self, byte: usize) -> usize {
        for seg in &self.segs {
            let end = seg.byte_start + seg.byte_len;
            if byte < seg.byte_start || byte > end {
                continue;
            }
            return match seg.kind {
                SegKind::Text => {
                    let within = &seg.text[..(byte - seg.byte_start).min(seg.text.len())];
                    seg.doc_start + utf16_len(within)
                }
                SegKind::Marker => seg.doc_start,
                SegKind::Atom => {
                    if byte - seg.byte_start <= seg.byte_len / 2 {
                        seg.doc_start
                    } else {
                        seg.doc_start + seg.doc_len
                    }
                }
            };
        }
        self.doc_len
    }

    /// Byte range of the chip (or marker run) a document offset touches.
    pub fn byte_range_of_doc_range(
        &self,
        from: usize,
        to: usize,
        side_from: CaretSide,
        side_to: CaretSide,
    ) -> std::ops::Range<usize> {
        self.doc_to_byte(from, side_from)..self.doc_to_byte(to, side_to)
    }
}

pub struct BlockFlow {
    pub flow: FlowText,
    pub map: OffsetMap,
}

/// The accent a chip kind carries (`ContextChip` kinds).
fn chip_accent(atom: &Atom) -> Hsla {
    match atom {
        Atom::Mention { .. } => color("oklch(0.62 0.11 215)"),
        Atom::Skill { .. } => color("oklch(0.62 0.16 322)"),
    }
}

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(at) => &trimmed[at + 1..],
        None => trimmed,
    }
}

fn chip_label(atom: &Atom) -> String {
    match atom {
        Atom::Mention { path, .. } => {
            let name = basename(path);
            if name.is_empty() {
                path.clone()
            } else {
                name.to_owned()
            }
        }
        Atom::Skill { label, name, .. } => {
            if label.is_empty() {
                name.clone()
            } else {
                label.clone()
            }
        }
    }
}

/// Typography of the block the inline content sits in.
#[derive(Clone, Copy)]
pub struct BlockTypography {
    pub size: Pixels,
    pub line_height: f32,
    pub weight: FontWeight,
    pub color: Hsla,
}

/// Which marks' markers to show (the styled text the selection touches).
pub struct MarkerReveal<'a> {
    pub caret: Option<DocPos>,
    pub selection: Option<(DocPos, DocPos)>,
    pub block: usize,
    /// The marks typed text would take at the caret.
    pub current_marks: Marks,
    pub enabled: bool,
    pub _marker: std::marker::PhantomData<&'a ()>,
}

struct Run {
    mark: RichTextMark,
    first: usize,
    last: usize,
    doc_start: usize,
    doc_end: usize,
}

fn mark_runs(inline: &Inline) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut doc = 0usize;
    for mark in [
        RichTextMark::Strike,
        RichTextMark::Bold,
        RichTextMark::Italic,
        RichTextMark::Code,
    ] {
        let mut open: Option<Run> = None;
        let mut offset = 0usize;
        doc = 0;
        for (index, node) in inline.iter().enumerate() {
            let len = node.doc_len();
            if node.marks.has(mark) {
                match &mut open {
                    Some(run) => {
                        run.last = index;
                        run.doc_end = offset + len;
                    }
                    None => {
                        open = Some(Run {
                            mark,
                            first: index,
                            last: index,
                            doc_start: offset,
                            doc_end: offset + len,
                        });
                    }
                }
            } else if let Some(run) = open.take() {
                runs.push(run);
            }
            offset += len;
            doc = offset;
        }
        if let Some(run) = open.take() {
            runs.push(run);
        }
    }
    let _ = doc;
    runs
}

fn run_is_active(run: &Run, reveal: &MarkerReveal<'_>) -> bool {
    if !reveal.enabled {
        return false;
    }
    if let Some((from, to)) = reveal.selection
        && from != to
    {
        // Any overlap of the selection with the run.
        let (start, end) = if from.block == to.block && from.block == reveal.block {
            (from.offset, to.offset)
        } else if reveal.block < from.block || reveal.block > to.block {
            return false;
        } else {
            let start = if reveal.block == from.block {
                from.offset
            } else {
                0
            };
            let end = if reveal.block == to.block {
                to.offset
            } else {
                usize::MAX
            };
            (start, end)
        };
        return start < run.doc_end && end > run.doc_start;
    }
    let Some(caret) = reveal.caret.filter(|c| c.block == reveal.block) else {
        return false;
    };
    if caret.offset > run.doc_start && caret.offset < run.doc_end {
        return true;
    }
    if caret.offset == run.doc_start || caret.offset == run.doc_end {
        // At an edge, only when the caret types with this mark.
        return reveal.current_marks.has(run.mark);
    }
    false
}

/// The flow for one inline run (a paragraph, heading, quote line or item).
pub fn build_inline_flow(
    env: &FlowEnv<'_>,
    inline: &Inline,
    typography: BlockTypography,
    reveal: &MarkerReveal<'_>,
) -> BlockFlow {
    let theme = env.theme;
    let runs = mark_runs(inline);
    let active: Vec<&Run> = runs
        .iter()
        .filter(|run| run_is_active(run, reveal))
        .collect();

    let mut text = String::new();
    let mut spans: Vec<FlowSpan> = Vec::new();
    let mut map = OffsetMap::default();
    let mut doc = 0usize;

    let base_style = |marks: Marks| -> FlowSpanStyle {
        let mut style = FlowSpanStyle {
            family: env.family.clone(),
            weight: typography.weight,
            italic: marks.has(RichTextMark::Italic),
            size: typography.size,
            line_height: typography.line_height,
            color: typography.color,
            strikethrough: marks.has(RichTextMark::Strike),
            chrome: None,
            link: None,
            background: None,
            atomic: false,
        };
        if marks.has(RichTextMark::Bold) {
            style.weight = if typography.weight.0 >= 600. {
                FontWeight::BLACK
            } else {
                FontWeight::BOLD
            };
        }
        if marks.has(RichTextMark::Code) {
            style.family = env.mono.clone();
            style.size = typography.size * 0.92;
            style.color = theme.foreground;
            style.chrome = Some(SpanChrome {
                background: with_alpha(theme.muted_foreground, 0.12).into(),
                border: None,
                radius: typography.size * 0.3,
                pad_x: typography.size * 0.25,
                pad_y: typography.size * 0.05,
                leading: px(0.),
                icon: None,
            });
        }
        style
    };

    let push_marker = |marker: &str,
                       text: &mut String,
                       spans: &mut Vec<FlowSpan>,
                       map: &mut OffsetMap,
                       doc: usize| {
        let mut style = base_style(Marks::NONE);
        style.color = with_alpha(theme.muted_foreground, 0.75).into();
        style.weight = FontWeight::NORMAL;
        style.italic = false;
        style.strikethrough = false;
        map.segs.push(Seg {
            kind: SegKind::Marker,
            doc_start: doc,
            doc_len: 0,
            byte_start: text.len(),
            byte_len: marker.len(),
            text: marker.to_owned(),
        });
        text.push_str(marker);
        spans.push(FlowSpan {
            len: marker.len(),
            style,
        });
    };

    for (index, node) in inline.iter().enumerate() {
        // Opening markers: outermost first.
        let mut opens: Vec<&&Run> = active.iter().filter(|run| run.first == index).collect();
        opens.sort_by_key(|run| std::cmp::Reverse(run.doc_end - run.doc_start));
        for run in opens {
            push_marker(run.mark.delimiter(), &mut text, &mut spans, &mut map, doc);
        }
        match &node.content {
            InlineContent::Text(piece) => {
                if !piece.is_empty() {
                    map.segs.push(Seg {
                        kind: SegKind::Text,
                        doc_start: doc,
                        doc_len: utf16_len(piece),
                        byte_start: text.len(),
                        byte_len: piece.len(),
                        text: piece.clone(),
                    });
                    text.push_str(piece);
                    spans.push(FlowSpan {
                        len: piece.len(),
                        style: base_style(node.marks),
                    });
                }
            }
            InlineContent::Atom(atom) => {
                let label = chip_label(atom);
                let accent = chip_accent(atom);
                let chip_size = typography.size * 0.86;
                let foreground: Hsla =
                    color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into();
                let mut style = base_style(node.marks);
                style.family = env.sans.clone();
                style.weight = FontWeight::MEDIUM;
                style.size = chip_size;
                style.line_height = 1.;
                style.color = foreground;
                style.strikethrough = false;
                style.atomic = true;
                let icon = match atom {
                    Atom::Mention { .. } => IconName::FileText,
                    Atom::Skill { .. } => IconName::Package,
                };
                style.chrome = Some(SpanChrome {
                    background: with_alpha(accent, 0.11).into(),
                    border: Some(color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into()),
                    radius: chip_size * 0.5,
                    pad_x: chip_size * 0.5,
                    pad_y: chip_size * 0.1,
                    leading: chip_size * 1.5,
                    icon: Some((icon, foreground, chip_size * 1.17, px(0.))),
                });
                map.segs.push(Seg {
                    kind: SegKind::Atom,
                    doc_start: doc,
                    doc_len: 1,
                    byte_start: text.len(),
                    byte_len: label.len(),
                    text: label.clone(),
                });
                text.push_str(&label);
                spans.push(FlowSpan {
                    len: label.len(),
                    style,
                });
            }
        }
        doc += node.doc_len();
        // Closing markers: innermost first.
        let mut closes: Vec<&&Run> = active.iter().filter(|run| run.last == index).collect();
        closes.sort_by_key(|run| run.doc_end - run.doc_start);
        for run in closes {
            push_marker(run.mark.delimiter(), &mut text, &mut spans, &mut map, doc);
        }
    }
    map.doc_len = doc;
    map.byte_len = text.len();
    BlockFlow {
        flow: FlowText {
            text: text.into(),
            spans,
            links: Vec::new(),
        },
        map,
    }
}

/// Plain flow for text with a single style (code lines, quote prefixes…).
pub fn plain_flow(text: &str, style: FlowSpanStyle) -> BlockFlow {
    let mut map = OffsetMap::default();
    map.segs.push(Seg {
        kind: SegKind::Text,
        doc_start: 0,
        doc_len: utf16_len(text),
        byte_start: 0,
        byte_len: text.len(),
        text: text.to_owned(),
    });
    map.doc_len = utf16_len(text);
    map.byte_len = text.len();
    let spans = if text.is_empty() {
        Vec::new()
    } else {
        vec![FlowSpan {
            len: text.len(),
            style,
        }]
    };
    BlockFlow {
        flow: FlowText {
            text: text.to_owned().into(),
            spans,
            links: Vec::new(),
        },
        map,
    }
}

/// Inline nodes as markdown-free preview text (menu rows etc.).
pub fn inline_plain_text(inline: &[InlineNode]) -> String {
    inline
        .iter()
        .map(|node| match &node.content {
            InlineContent::Text(t) => t.clone(),
            InlineContent::Atom(atom) => atom.source(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_seg(doc_start: usize, text: &str, byte_start: usize) -> Seg {
        Seg {
            kind: SegKind::Text,
            doc_start,
            doc_len: utf16_len(text),
            byte_start,
            byte_len: text.len(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn maps_offsets_through_text_and_chips() {
        let map = OffsetMap {
            segs: vec![
                text_seg(0, "ab😀", 0),
                Seg {
                    kind: SegKind::Atom,
                    doc_start: 4,
                    doc_len: 1,
                    byte_start: 6,
                    byte_len: 8,
                    text: "README.md".into(),
                },
                text_seg(5, "cd", 14),
            ],
            doc_len: 7,
            byte_len: 16,
        };
        assert_eq!(map.doc_to_byte(2, CaretSide::Plain), 2);
        assert_eq!(map.doc_to_byte(4, CaretSide::Plain), 6);
        assert_eq!(map.doc_to_byte(5, CaretSide::Plain), 14);
        assert_eq!(map.doc_to_byte(7, CaretSide::Plain), 16);
        assert_eq!(map.byte_to_doc(2), 2);
        assert_eq!(map.byte_to_doc(6), 4);
        assert_eq!(map.byte_to_doc(9), 4);
        assert_eq!(map.byte_to_doc(12), 5);
        assert_eq!(map.byte_to_doc(15), 6);
    }

    #[test]
    fn styled_edges_offer_three_caret_stops() {
        let marker = |doc, byte| Seg {
            kind: SegKind::Marker,
            doc_start: doc,
            doc_len: 0,
            byte_start: byte,
            byte_len: 2,
            text: "**".into(),
        };
        let map = OffsetMap {
            segs: vec![
                text_seg(0, "ab", 0),
                marker(2, 2),
                marker(2, 4),
                text_seg(2, "cd", 6),
            ],
            doc_len: 4,
            byte_len: 8,
        };
        assert_eq!(map.doc_to_byte(2, CaretSide::InsideBefore), 2);
        assert_eq!(map.doc_to_byte(2, CaretSide::Plain), 4);
        assert_eq!(map.doc_to_byte(2, CaretSide::InsideAfter), 6);
        // A click on a marker lands on the edge.
        assert_eq!(map.byte_to_doc(3), 2);
    }
}
