//! An inline formatting context for chat markdown: the part of the browser
//! upstream relies on that GPUI's `StyledText` cannot express.
//!
//! Upstream's `.chat-markdown` paragraphs mix font sizes on one line (inline
//! `code` is `0.75rem` mono inside `text-sm` prose), draw padded, bordered
//! boxes around inline code that repeat on every line fragment
//! (`box-decoration-break: clone`), and wrap with
//! `overflow-wrap: anywhere; word-break: break-word`: break at spaces, and
//! break inside a word only when the word alone is wider than the line.
//! Code blocks use `white-space: pre` (scroll) or `pre-wrap` (wrap).
//!
//! [`InlineFlow`] shapes each styled span once with the window text system
//! (whose line cache makes reshaping across frames cheap), breaks the spans
//! into line fragments, and paints them with CSS line-box metrics: each span
//! contributes its half-leading around the baseline and the block's font is a
//! strut. It also takes part in the window text selection (gpui-base's
//! [`TextSelectionHandle`]) as a rich participant: it hit-tests its own
//! layout, paints its own selection and answers copy with the selected text.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::{TextSelectionCoverage, TextSelectionHandle, TextSelectionRegistration};
use gpui_kit::{
    App, AvailableSpace, Bounds, Corners, CursorStyle, Edges, Element, ElementId, FontStyle,
    FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Size, StrikethroughStyle, Style, TextAlign, TextRun, Window, font,
    point, px, size, transparent_black,
};

/// The box upstream draws around inline `code` (`.chat-markdown :not(pre) > code`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanChrome {
    pub background: Hsla,
    pub border: Option<Hsla>,
    pub radius: Pixels,
    pub pad_x: Pixels,
    pub pad_y: Pixels,
    /// Extra room before the text for a leading icon (file chips).
    pub leading: Pixels,
    /// `(icon, color, size, offset)` painted in the leading room, `offset`
    /// after the box's padding.
    pub icon: Option<(gpui_kit::assets::IconName, Hsla, Pixels, Pixels)>,
}

/// Resolved style for one span of [`FlowText`].
#[derive(Clone, Debug, PartialEq)]
pub struct FlowSpanStyle {
    pub family: SharedString,
    pub weight: FontWeight,
    pub italic: bool,
    pub size: Pixels,
    /// `line-height` of the span as a multiple of its font size.
    pub line_height: f32,
    pub color: Hsla,
    pub strikethrough: bool,
    pub chrome: Option<SpanChrome>,
    /// Index into [`FlowText::links`].
    pub link: Option<usize>,
    /// Syntax-highlight color override for code tokens.
    pub background: Option<Hsla>,
    /// Never break inside this span (`inline-flex` chips).
    pub atomic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowSpan {
    /// UTF-8 bytes of [`FlowText::text`] this span covers.
    pub len: usize,
    pub style: FlowSpanStyle,
}

/// Text plus spans covering it end to end. `\n` is a forced line break.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowText {
    pub text: SharedString,
    pub spans: Vec<FlowSpan>,
    pub links: Vec<SharedString>,
}

/// CSS `white-space` for the flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WhiteSpace {
    /// `normal` with `overflow-wrap: anywhere`: the builder has already
    /// collapsed whitespace; wrap at spaces, split words that cannot fit.
    Normal,
    /// `normal` with `overflow-wrap: normal` (table cells): min-content is
    /// the longest word; words only split when they overflow anyway.
    NormalWords,
    /// `pre-wrap; overflow-wrap: anywhere` (wrapped code blocks).
    PreWrap,
    /// `pre`: never wrap (scrolling code blocks).
    Pre,
}

#[derive(Clone, Debug)]
struct Fragment {
    span_ix: usize,
    /// Byte range in `FlowText::text`.
    range: Range<usize>,
    /// Left edge of the glyphs (chrome padding sits before it).
    x: Pixels,
    width: Pixels,
    line: ShapedLine,
    ascent: Pixels,
    descent: Pixels,
    chrome_left: bool,
    chrome_right: bool,
    /// The fragment begins its span (where a leading icon goes).
    starts_span: bool,
}

#[derive(Clone, Debug)]
struct FlowLine {
    top: Pixels,
    height: Pixels,
    baseline: Pixels,
    fragments: Vec<Fragment>,
    /// Byte range the line covers, including hung trailing whitespace.
    range: Range<usize>,
    width: Pixels,
}

#[derive(Clone, Debug, Default)]
struct FlowLayout {
    wrap_width: Option<Pixels>,
    lines: Vec<FlowLine>,
    size: Size<Pixels>,
    min_content_width: Pixels,
}

/// One shaped run of a span between forced breaks.
struct Segment {
    span_ix: usize,
    range: Range<usize>,
    line: ShapedLine,
}

fn shape_segments(content: &FlowText, window: &Window) -> Vec<Segment> {
    let text = content.text.as_ref();
    let mut segments = Vec::new();
    let mut offset = 0;
    for (span_ix, span) in content.spans.iter().enumerate() {
        let end = (offset + span.len).min(text.len());
        let mut start = offset;
        while start < end {
            let stop = text[start..end]
                .find('\n')
                .map(|ix| start + ix)
                .unwrap_or(end);
            if stop > start {
                let piece: SharedString = text[start..stop].to_string().into();
                let style = &span.style;
                let mut run_font = font(style.family.clone());
                run_font.weight = style.weight;
                if style.italic {
                    run_font.style = FontStyle::Italic;
                }
                let run = TextRun {
                    len: piece.len(),
                    font: run_font,
                    color: style.color,
                    background_color: style.background,
                    underline: None,
                    strikethrough: style.strikethrough.then_some(StrikethroughStyle {
                        thickness: px(1.),
                        color: Some(style.color),
                    }),
                };
                let line = window
                    .text_system()
                    .shape_line(piece, style.size, &[run], None);
                segments.push(Segment {
                    span_ix,
                    range: start..stop,
                    line,
                });
            }
            if stop < end {
                // The newline itself is a zero-width segment boundary.
                start = stop + 1;
            } else {
                start = stop;
            }
        }
        offset = end;
    }
    segments
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// Byte offsets where a line may break (the start of the next atom).
fn break_opportunities(text: &str, white_space: WhiteSpace) -> Vec<usize> {
    let mut out = Vec::new();
    if white_space == WhiteSpace::Pre {
        return out;
    }
    let mut prev: Option<char> = None;
    for (ix, c) in text.char_indices() {
        if let Some(p) = prev {
            let after_space = (p == ' ' || p == '\t') && c != ' ' && c != '\t';
            let cjk = (is_cjk(p) || is_cjk(c)) && !c.is_whitespace() && !p.is_whitespace();
            if (after_space || cjk) && c != '\n' && p != '\n' {
                out.push(ix);
            }
        }
        prev = Some(c);
    }
    out
}

/// Grapheme-ish split points for `overflow-wrap: anywhere` (char boundaries,
/// skipping combining marks and joiners so clusters stay whole).
fn char_split_points(text: &str, range: Range<usize>) -> Vec<usize> {
    let mut out = Vec::new();
    for (ix, c) in text[range.clone()].char_indices() {
        if ix == 0 {
            continue;
        }
        let combining = matches!(c as u32, 0x0300..=0x036F | 0x200D | 0xFE0F | 0x1F3FB..=0x1F3FF);
        let after_joiner = text[range.start..range.start + ix].ends_with('\u{200D}');
        if !combining && !after_joiner {
            out.push(range.start + ix);
        }
    }
    out
}

struct Measurer<'a> {
    content: &'a FlowText,
    segments: &'a [Segment],
}

impl Measurer<'_> {
    /// Advance width of `[start, end)` (no chrome padding).
    fn width(&self, start: usize, end: usize) -> Pixels {
        let mut total = px(0.);
        for seg in self.segments {
            if seg.range.end <= start || seg.range.start >= end {
                continue;
            }
            let a = start.max(seg.range.start) - seg.range.start;
            let b = end.min(seg.range.end) - seg.range.start;
            total += seg.line.x_for_index(b) - seg.line.x_for_index(a);
        }
        total
    }

    fn span_at(&self, ix: usize) -> Option<(usize, Range<usize>)> {
        let mut offset = 0;
        for (span_ix, span) in self.content.spans.iter().enumerate() {
            let end = offset + span.len;
            if ix >= offset && ix < end {
                return Some((span_ix, offset..end));
            }
            offset = end;
        }
        None
    }

    /// Chrome padding an atom `[start, end)` adds when it opens or closes a
    /// chrome span (or, with `clone`, when a line breaks inside one).
    fn chrome_pad(&self, start: usize, end: usize, line_start: bool) -> Pixels {
        let mut pad = px(0.);
        if let Some((span_ix, range)) = self.span_at(start)
            && let Some(chrome) = self.content.spans[span_ix].style.chrome
            && (range.start == start || line_start)
        {
            let leading = if range.start == start {
                chrome.leading
            } else {
                px(0.)
            };
            pad += chrome.pad_x + leading + chrome.border.map_or(px(0.), |_| px(1.));
        }
        if end > start
            && let Some((span_ix, _)) = self.span_at(end - 1)
            && let Some(chrome) = self.content.spans[span_ix].style.chrome
        {
            pad += chrome.pad_x + chrome.border.map_or(px(0.), |_| px(1.));
        }
        // Chrome spans fully inside the atom contribute both sides.
        let mut offset = 0;
        for span in &self.content.spans {
            let span_end = offset + span.len;
            if let Some(chrome) = span.style.chrome
                && offset > start
                && span_end < end
            {
                pad +=
                    (chrome.pad_x + chrome.border.map_or(px(0.), |_| px(1.))) * 2. + chrome.leading;
            }
            offset = span_end;
        }
        pad
    }
}

fn trim_trailing_ws(text: &str, start: usize, end: usize) -> usize {
    let trimmed = text[start..end].trim_end_matches([' ', '\t']);
    start + trimmed.len()
}

/// Atoms between break opportunities and forced breaks.
#[cfg(test)]
fn atoms(text: &str, white_space: WhiteSpace) -> Vec<(usize, usize, bool)> {
    atoms_with(text, break_opportunities(text, white_space))
}

fn atoms_with(text: &str, breaks: Vec<usize>) -> Vec<(usize, usize, bool)> {
    // (start, end, forced_break_after)
    let mut out = Vec::new();
    let mut start = 0;
    let mut bi = 0;
    let bytes = text.as_bytes();
    let mut ix = 0;
    while ix < text.len() {
        if bytes[ix] == b'\n' {
            out.push((start, ix, true));
            start = ix + 1;
            ix += 1;
            while bi < breaks.len() && breaks[bi] <= start {
                bi += 1;
            }
            continue;
        }
        if bi < breaks.len() && breaks[bi] == ix && ix > start {
            out.push((start, ix, false));
            start = ix;
            bi += 1;
        }
        ix += 1;
    }
    out.push((start, text.len(), false));
    out
}

fn compute_layout(
    content: &FlowText,
    segments: &[Segment],
    white_space: WhiteSpace,
    wrap_width: Option<Pixels>,
    strut: (Pixels, Pixels, Pixels),
) -> FlowLayout {
    let text = content.text.as_ref();
    let m = Measurer { content, segments };
    let max_w = if white_space == WhiteSpace::Pre {
        None
    } else {
        wrap_width
    };

    // 1. Place atoms into line byte ranges.
    let mut line_ranges: Vec<Range<usize>> = Vec::new();
    let mut line_start = 0usize;
    let mut line_x = px(0.);
    let mut line_has_content = false;
    let mut min_content = px(0.);
    let mut breaks = break_opportunities(text, white_space);
    let mut offset = 0;
    for span in &content.spans {
        let end = offset + span.len;
        if span.style.atomic {
            breaks.retain(|&b| b <= offset || b >= end);
        }
        offset = end;
    }
    for (a_start, a_end, forced) in atoms_with(text, breaks) {
        let content_end = trim_trailing_ws(text, a_start, a_end);
        let mut start = a_start;
        loop {
            let w =
                m.width(start, content_end) + m.chrome_pad(start, content_end, !line_has_content);
            let word_min = match white_space {
                WhiteSpace::NormalWords | WhiteSpace::Pre => w,
                _ => char_split_points(text, start..content_end.max(start))
                    .first()
                    .map_or(w, |&first| m.width(start, first)),
            };
            min_content = min_content.max(word_min);
            let fits = max_w.is_none_or(|max| line_x + w <= max + px(0.5));
            if fits || !line_has_content && max_w.is_none() {
                line_x += w + m.width(content_end, a_end);
                line_has_content |= content_end > start;
                break;
            }
            if line_has_content {
                // Break before this atom.
                line_ranges.push(line_start..start);
                line_start = start;
                line_x = px(0.);
                line_has_content = false;
                continue;
            }
            // Alone and too wide: split it anywhere (`overflow-wrap: anywhere`).
            let max = max_w.unwrap_or(px(f32::MAX));
            let mut split = None;
            for point_ix in char_split_points(text, start..content_end) {
                let w = m.width(start, point_ix) + m.chrome_pad(start, point_ix, true);
                if w <= max {
                    split = Some(point_ix);
                } else {
                    break;
                }
            }
            let split = split.unwrap_or_else(|| {
                // Not even one character fits: take one anyway.
                char_split_points(text, start..content_end)
                    .first()
                    .copied()
                    .unwrap_or(content_end)
            });
            if split >= content_end {
                line_x += w + m.width(content_end, a_end);
                line_has_content = true;
                break;
            }
            line_ranges.push(line_start..split);
            line_start = split;
            start = split;
            line_x = px(0.);
            line_has_content = false;
        }
        if forced {
            line_ranges.push(line_start..a_end);
            line_start = a_end + 1;
            line_x = px(0.);
            line_has_content = false;
        }
    }
    if line_start <= text.len() {
        line_ranges.push(line_start..text.len());
    }

    let mut span_starts = Vec::with_capacity(content.spans.len());
    let mut span_offset = 0;
    for span in &content.spans {
        span_starts.push(span_offset);
        span_offset += span.len;
    }

    // 2. Build fragments and line boxes.
    let (strut_ascent, strut_descent, strut_line_height) = strut;
    let mut lines = Vec::with_capacity(line_ranges.len());
    let mut top = px(0.);
    let mut max_width = px(0.);
    for range in line_ranges {
        let mut fragments: Vec<Fragment> = Vec::new();
        let mut x = px(0.);
        let content_end = trim_trailing_ws(text, range.start, range.end.max(range.start));
        for seg in segments {
            if seg.range.end <= range.start || seg.range.start >= range.end {
                continue;
            }
            let a = range.start.max(seg.range.start);
            let b = range.end.min(seg.range.end);
            if a >= b {
                continue;
            }
            let style = &content.spans[seg.span_ix].style;
            let mut cursor = seg.line.cursor();
            if a > seg.range.start {
                cursor.take_until(a - seg.range.start);
            }
            let piece = cursor.take_until(b - seg.range.start);
            let pad = style
                .chrome
                .map_or(px(0.), |c| c.pad_x + c.border.map_or(px(0.), |_| px(1.)));
            // A chrome span continuing from the previous fragment on this line
            // shares its box.
            let continues = fragments
                .last()
                .is_some_and(|last: &Fragment| last.span_ix == seg.span_ix);
            let starts_span = a == span_starts[seg.span_ix];
            if !continues {
                x += pad;
                if starts_span {
                    x += style.chrome.map_or(px(0.), |c| c.leading);
                }
            }
            let width = piece.width();
            if let Some(last) = fragments.last_mut()
                && last.span_ix != seg.span_ix
                && last.chrome_right
            {
                // Closed box: the pad was already added when it ended.
            }
            let ascent = seg.line.ascent;
            let descent = seg.line.descent;
            fragments.push(Fragment {
                span_ix: seg.span_ix,
                range: a..b,
                x,
                width,
                line: piece,
                ascent,
                descent,
                chrome_left: style.chrome.is_some() && !continues,
                starts_span,
                chrome_right: false,
            });
            x += width;
            let next_same_span = segments.iter().any(|other| {
                other.span_ix == seg.span_ix
                    && other.range.start == seg.range.end
                    && other.range.start < range.end
            });
            if style.chrome.is_some() && !next_same_span {
                if let Some(last) = fragments.last_mut() {
                    last.chrome_right = true;
                }
                x += pad;
            }
        }
        // Line box: strut plus every fragment's half-leading box.
        let strut_half = (strut_line_height - (strut_ascent + strut_descent)) / 2.;
        let mut above = strut_half + strut_ascent;
        let mut below = strut_half + strut_descent;
        for frag in &fragments {
            let style = &content.spans[frag.span_ix].style;
            let lh = style.size * style.line_height;
            let half = (lh - (frag.ascent + frag.descent)) / 2.;
            above = above.max(half + frag.ascent);
            below = below.max(half + frag.descent);
        }
        let height = above + below;
        let content_width = if content_end > range.start {
            fragments
                .iter()
                .filter(|f| f.range.start < content_end)
                .map(|f| {
                    let end = f.range.end.min(content_end);
                    let w = f.line.x_for_index(end - f.range.start);
                    let pad = if f.chrome_right {
                        content.spans[f.span_ix]
                            .style
                            .chrome
                            .map_or(px(0.), |c| c.pad_x + c.border.map_or(px(0.), |_| px(1.)))
                    } else {
                        px(0.)
                    };
                    f.x + w + pad
                })
                .fold(px(0.), Pixels::max)
        } else {
            px(0.)
        };
        max_width = max_width.max(content_width);
        lines.push(FlowLine {
            top,
            height,
            baseline: above,
            fragments,
            range,
            width: content_width,
        });
        top += height;
    }

    FlowLayout {
        wrap_width,
        size: size(max_width, top),
        lines,
        min_content_width: min_content,
    }
}

impl FlowLayout {
    /// Byte index nearest to `position` (relative to the flow's origin).
    fn index_for_position(&self, position: Point<Pixels>, text_len: usize) -> usize {
        let Some(first) = self.lines.first() else {
            return 0;
        };
        if position.y < first.top {
            return 0;
        }
        let line = self
            .lines
            .iter()
            .find(|line| position.y < line.top + line.height)
            .unwrap_or_else(|| self.lines.last().unwrap_or(first));
        if position.y >= line.top + line.height {
            return text_len;
        }
        let mut best = line.range.start;
        for frag in &line.fragments {
            if position.x < frag.x {
                return best.max(frag.range.start).min(frag.range.start);
            }
            if position.x <= frag.x + frag.width {
                return frag.range.start + frag.line.closest_index_for_x(position.x - frag.x);
            }
            best = frag.range.end;
        }
        best.min(line.range.end)
    }

    /// x position of byte `ix` on `line`.
    fn x_for_index(line: &FlowLine, ix: usize) -> Pixels {
        for frag in &line.fragments {
            if ix <= frag.range.end && ix >= frag.range.start {
                return frag.x + frag.line.x_for_index(ix - frag.range.start);
            }
            if ix < frag.range.start {
                return frag.x;
            }
        }
        line.fragments
            .last()
            .map_or(px(0.), |frag| frag.x + frag.width)
    }
}

pub type LinkHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// A highlighted byte range painted behind the text (the composer's own
/// selection, which does not take part in the window text selection).
#[derive(Clone, Debug)]
pub struct FlowHighlight {
    pub range: Range<usize>,
    pub color: Hsla,
    /// The selection continues past the end of the text: cover the line end too.
    pub through_line_end: bool,
}

/// A caret painted over the text at a byte index.
#[derive(Clone, Copy, Debug)]
pub struct FlowCaret {
    pub index: usize,
    pub color: Hsla,
    pub width: Pixels,
}

/// Read access to the last laid-out geometry of an [`InlineFlow`], for editors
/// that hit-test and move carets themselves. Window coordinates throughout.
#[derive(Clone, Default)]
pub struct FlowProbe(Rc<RefCell<FlowProbeState>>);

#[derive(Default)]
struct FlowProbeState {
    cache: Option<Rc<RefCell<FlowCache>>>,
    bounds: Bounds<Pixels>,
}

impl FlowProbe {
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().bounds
    }

    fn with_layout<R>(&self, f: impl FnOnce(&FlowLayout, Bounds<Pixels>, usize) -> R) -> Option<R> {
        let state = self.0.borrow();
        let cache = state.cache.as_ref()?.borrow();
        let layout = cache.layout.as_ref()?;
        let len = cache.content.as_ref().map_or(0, |c| c.text.len());
        Some(f(layout, state.bounds, len))
    }

    /// Byte index nearest to a window point.
    pub fn index_for_point(&self, position: Point<Pixels>) -> Option<usize> {
        self.with_layout(|layout, bounds, len| {
            layout.index_for_position(position - bounds.origin, len)
        })
    }

    pub fn line_count(&self) -> usize {
        self.with_layout(|layout, _, _| layout.lines.len().max(1))
            .unwrap_or(1)
    }

    /// The visual line holding byte `index`; a wrap point belongs to the later
    /// line, like a browser caret.
    pub fn line_for_index(&self, index: usize) -> usize {
        self.with_layout(|layout, _, len| {
            layout
                .lines
                .iter()
                .position(|line| {
                    index < line.range.end || (index == len && index <= line.range.end)
                })
                .unwrap_or(layout.lines.len().saturating_sub(1))
        })
        .unwrap_or(0)
    }

    /// Visible byte range of a line (without hung trailing whitespace).
    pub fn line_visible_range(&self, line: usize) -> Range<usize> {
        self.with_layout(|layout, _, len| {
            layout.lines.get(line).map_or(0..len, |line| {
                let end = line
                    .fragments
                    .last()
                    .map_or(line.range.start, |f| f.range.end);
                line.range.start..end.max(line.range.start)
            })
        })
        .unwrap_or(0..0)
    }

    /// Byte index on `line` nearest to window x.
    pub fn index_for_x_on_line(&self, line: usize, x: Pixels) -> usize {
        self.with_layout(|layout, bounds, len| {
            let Some(target) = layout.lines.get(line) else {
                return len;
            };
            layout.index_for_position(
                point(x - bounds.origin.x, target.top + target.height / 2.),
                len,
            )
        })
        .unwrap_or(0)
    }

    /// The caret rectangle for byte `index` (window coordinates).
    pub fn caret_bounds(&self, index: usize, width: Pixels) -> Option<Bounds<Pixels>> {
        let line = self.line_for_index(index);
        self.with_layout(|layout, bounds, _| {
            let target = layout.lines.get(line)?;
            let x = FlowLayout::x_for_index(target, index);
            Some(Bounds::new(
                bounds.origin + point(x, target.top),
                size(width, target.height),
            ))
        })
        .flatten()
    }
}

/// Shared between the measure callback and prepaint/paint.
#[derive(Default)]
pub struct FlowCache {
    content: Option<FlowText>,
    white_space: Option<WhiteSpace>,
    segments: Vec<Segment>,
    layout: Option<FlowLayout>,
    rem: Pixels,
}

struct FlowRetained {
    cache: Rc<RefCell<FlowCache>>,
    selection: Option<TextSelectionHandle>,
    /// Text the last paint selected, for copy.
    selected: Rc<RefCell<String>>,
    /// Shared with the mouse handlers, which run outside paint.
    links: Rc<RefCell<LinkInteraction>>,
}

#[derive(Default)]
struct LinkInteraction {
    hovered: Option<usize>,
    pressed_at: Option<Point<Pixels>>,
}

/// Inline flow element. See the module docs.
pub struct InlineFlow {
    id: ElementId,
    content: FlowText,
    white_space: WhiteSpace,
    /// Strut font: the block's own font size and line height factor.
    strut_size: Pixels,
    strut_line_height: f32,
    strut_family: SharedString,
    selectable: bool,
    document_order: u64,
    selection_color: Hsla,
    on_link: Option<LinkHandler>,
    highlights: Vec<FlowHighlight>,
    caret: Option<FlowCaret>,
    probe: Option<FlowProbe>,
}

impl InlineFlow {
    pub fn new(id: impl Into<ElementId>, content: FlowText, white_space: WhiteSpace) -> Self {
        Self {
            id: id.into(),
            content,
            white_space,
            strut_size: px(14.),
            strut_line_height: 1.625,
            strut_family: SharedString::default(),
            selectable: true,
            document_order: 0,
            selection_color: transparent_black(),
            on_link: None,
            highlights: Vec::new(),
            caret: None,
            probe: None,
        }
    }

    /// Whether the flow takes part in the window text selection (default true).
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Editor hooks: highlighted ranges, a caret, and a probe for hit testing.
    pub fn highlights(mut self, highlights: Vec<FlowHighlight>) -> Self {
        self.highlights = highlights;
        self
    }

    pub fn caret(mut self, caret: Option<FlowCaret>) -> Self {
        self.caret = caret;
        self
    }

    pub fn probe(mut self, probe: FlowProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    pub fn strut(mut self, family: SharedString, size: Pixels, line_height: f32) -> Self {
        self.strut_family = family;
        self.strut_size = size;
        self.strut_line_height = line_height;
        self
    }

    pub fn document_order(mut self, order: u64) -> Self {
        self.document_order = order;
        self
    }

    pub fn selection_color(mut self, color: Hsla) -> Self {
        self.selection_color = color;
        self
    }

    /// Handles a completed link click. Without one, the flow opens the URL.
    pub fn on_link(mut self, handler: LinkHandler) -> Self {
        self.on_link = Some(handler);
        self
    }

    fn strut_metrics(&self, window: &Window) -> (Pixels, Pixels, Pixels) {
        let line = window.text_system().shape_line(
            "M".into(),
            self.strut_size,
            &[TextRun {
                len: 1,
                font: font(self.strut_family.clone()),
                color: transparent_black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        (
            line.ascent,
            line.descent,
            self.strut_size * self.strut_line_height,
        )
    }
}

impl IntoElement for InlineFlow {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct FlowPrepaint {
    hitbox: Hitbox,
    link_hitboxes: Vec<(usize, Bounds<Pixels>)>,
}

impl Element for InlineFlow {
    type RequestLayoutState = Rc<RefCell<FlowCache>>;
    type PrepaintState = Option<FlowPrepaint>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let cache = window.with_element_state(
            global_id.expect("InlineFlow has an id"),
            |retained: Option<FlowRetained>, _| {
                let retained = retained.unwrap_or_else(|| FlowRetained {
                    cache: Rc::default(),
                    selection: None,
                    selected: Rc::default(),
                    links: Rc::default(),
                });
                (retained.cache.clone(), retained)
            },
        );
        {
            let mut c = cache.borrow_mut();
            let rem = window.rem_size();
            if c.content.as_ref() != Some(&self.content)
                || c.white_space != Some(self.white_space)
                || c.rem != rem
            {
                c.segments = shape_segments(&self.content, window);
                c.content = Some(self.content.clone());
                c.white_space = Some(self.white_space);
                c.layout = None;
                c.rem = rem;
            }
        }
        let strut = self.strut_metrics(window);
        let white_space = self.white_space;
        let measure_cache = cache.clone();
        let _ = cx;
        let layout_id =
            window.request_measured_layout(Style::default(), move |known, available, _, _| {
                let mut c = measure_cache.borrow_mut();
                let wrap_width = known.width.or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(px(0.)),
                    AvailableSpace::MaxContent => None,
                });
                let Some(content) = c.content.clone() else {
                    return Size::default();
                };
                let needs = c
                    .layout
                    .as_ref()
                    .is_none_or(|layout| layout.wrap_width != wrap_width);
                if needs {
                    let layout =
                        compute_layout(&content, &c.segments, white_space, wrap_width, strut);
                    c.layout = Some(layout);
                }
                let layout = c.layout.as_ref().map(|l| (l.size, l.min_content_width));
                let (flow_size, min_content) = layout.unwrap_or_default();
                let width = match available.width {
                    AvailableSpace::MinContent => min_content,
                    _ => known.width.unwrap_or(flow_size.width),
                };
                size(width.ceil(), flow_size.height.ceil())
            });
        (layout_id, cache)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        cache: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let strut = self.strut_metrics(window);
        {
            let mut c = cache.borrow_mut();
            let needs = c
                .layout
                .as_ref()
                .is_none_or(|layout| layout.wrap_width != Some(bounds.size.width));
            if needs && let Some(content) = c.content.clone() {
                let layout = compute_layout(
                    &content,
                    &c.segments,
                    self.white_space,
                    Some(bounds.size.width),
                    strut,
                );
                c.layout = Some(layout);
            }
        }
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        if let Some(probe) = &self.probe {
            let mut state = probe.0.borrow_mut();
            state.cache = Some(cache.clone());
            state.bounds = bounds;
        }
        let c = cache.borrow();
        let layout = c.layout.as_ref()?;
        let mut link_hitboxes = Vec::new();
        let mut text_bounds = Vec::with_capacity(layout.lines.len());
        for line in &layout.lines {
            text_bounds.push(Bounds::new(
                bounds.origin + point(px(0.), line.top),
                size(line.width.max(px(1.)), line.height),
            ));
            for frag in &line.fragments {
                if let Some(link) = self.content.spans[frag.span_ix].style.link {
                    link_hitboxes.push((
                        link,
                        Bounds::new(
                            bounds.origin + point(frag.x, line.top),
                            size(frag.width, line.height),
                        ),
                    ));
                }
            }
        }
        drop(c);

        if self.selectable {
            let global_id = global_id.expect("InlineFlow has an id");
            let order = self.document_order;
            let handle =
                window.with_element_state(global_id, |retained: Option<FlowRetained>, _| {
                    let mut retained = retained.unwrap_or_else(|| FlowRetained {
                        cache: cache.clone(),
                        selection: None,
                        selected: Rc::default(),
                        links: Rc::default(),
                    });
                    let handle = retained
                        .selection
                        .get_or_insert_with(|| {
                            let handle = TextSelectionHandle::new("", cx);
                            let selected = retained.selected.clone();
                            handle.copy_with(move |_| selected.borrow().clone(), cx);
                            handle
                        })
                        .clone();
                    (handle, retained)
                });
            let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(order)
                .with_text_bounds(text_bounds)
                .with_rendered_element(&handle, window, cx);
            handle.register(registration, window, cx);
        }

        Some(FlowPrepaint {
            hitbox,
            link_hitboxes,
        })
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        cache: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(prepaint) = prepaint.as_ref() else {
            return;
        };
        let global_id = global_id.expect("InlineFlow has an id");
        let (handle, selected_text, interaction) =
            window.with_element_state(global_id, |retained: Option<FlowRetained>, _| {
                let retained = retained.unwrap_or_else(|| FlowRetained {
                    cache: cache.clone(),
                    selection: None,
                    selected: Rc::default(),
                    links: Rc::default(),
                });
                (
                    (
                        retained.selection.clone(),
                        retained.selected.clone(),
                        retained.links.clone(),
                    ),
                    retained,
                )
            });

        let hovered_link = interaction.borrow().hovered;
        let c = cache.borrow();
        let Some(layout) = c.layout.as_ref() else {
            return;
        };
        let text = self.content.text.as_ref();

        // Selection range from the window selection snapshot.
        let mut selected_range: Option<Range<usize>> = None;
        if let Some(handle) = handle.as_ref()
            && let Some(snapshot) = handle.snapshot(cx)
        {
            let own = handle.entity_id();
            let local = |p: Point<Pixels>| layout.index_for_position(p - bounds.origin, text.len());
            let points = snapshot.window_points();
            let anchor_mine = snapshot.anchor().entity_id() == Some(own);
            let cursor_mine = snapshot.cursor().entity_id() == Some(own);
            use TextSelectionCoverage as Cov;
            selected_range = match (snapshot.coverage(), points) {
                (Cov::Full, _) => Some(0..text.len()),
                (Cov::FromStart, Some(points)) => {
                    let p = if cursor_mine {
                        points.cursor()
                    } else {
                        points.anchor()
                    };
                    Some(0..local(p))
                }
                (Cov::ToEnd, Some(points)) => {
                    let p = if cursor_mine {
                        points.cursor()
                    } else {
                        points.anchor()
                    };
                    Some(local(p)..text.len())
                }
                (Cov::Bounded, Some(points)) if anchor_mine && cursor_mine => {
                    let a = local(points.anchor());
                    let b = local(points.cursor());
                    Some(a.min(b)..a.max(b))
                }
                _ => None,
            };
        }
        let selected_string = selected_range
            .clone()
            .filter(|r| r.start < r.end)
            .and_then(|r| text.get(r))
            .unwrap_or("")
            .to_string();
        if *selected_text.borrow() != selected_string {
            *selected_text.borrow_mut() = selected_string;
        }

        // Selection highlight.
        if let Some(range) = selected_range.filter(|r| r.start < r.end) {
            for line in &layout.lines {
                let line_end = line.range.end;
                if line_end < range.start || line.range.start > range.end {
                    continue;
                }
                let start = range.start.max(line.range.start);
                let end = range.end.min(line_end);
                let x0 = FlowLayout::x_for_index(line, start);
                let mut x1 = FlowLayout::x_for_index(line, end);
                if range.end > line_end {
                    // Selection continues: cover the hung newline/space.
                    x1 = x1.max(line.width) + px(4.);
                }
                if x1 > x0 {
                    window.paint_quad(PaintQuad {
                        bounds: Bounds::new(
                            bounds.origin + point(x0, line.top),
                            size(x1 - x0, line.height),
                        ),
                        corner_radii: Corners::default(),
                        background: self.selection_color.into(),
                        border_widths: Edges::default(),
                        border_color: transparent_black(),
                        border_style: Default::default(),
                    });
                }
            }
        }

        // Highlights set by an editor.
        for highlight in &self.highlights {
            for line in &layout.lines {
                let line_end = line
                    .fragments
                    .last()
                    .map_or(line.range.start, |f| f.range.end);
                if line_end < highlight.range.start || line.range.start > highlight.range.end {
                    continue;
                }
                let start = highlight.range.start.max(line.range.start);
                let end = highlight.range.end.min(line_end);
                let x0 = FlowLayout::x_for_index(line, start);
                let mut x1 = FlowLayout::x_for_index(line, end);
                if highlight.through_line_end && highlight.range.end >= line_end {
                    x1 = x1.max(line.width) + px(4.);
                }
                if x1 > x0 {
                    window.paint_quad(PaintQuad {
                        bounds: Bounds::new(
                            bounds.origin + point(x0, line.top),
                            size(x1 - x0, line.height),
                        ),
                        corner_radii: Corners::default(),
                        background: highlight.color.into(),
                        border_widths: Edges::default(),
                        border_color: transparent_black(),
                        border_style: Default::default(),
                    });
                }
            }
        }

        // Chrome boxes, then text.
        for line in &layout.lines {
            let baseline_y = bounds.origin.y + line.top + line.baseline;
            let mut open_box: Option<(Pixels, usize)> = None;
            let mut open_icon = false;
            for frag in &line.fragments {
                let style = &self.content.spans[frag.span_ix].style;
                if let Some(chrome) = style.chrome {
                    let border = chrome.border.map_or(px(0.), |_| px(1.));
                    if frag.chrome_left || open_box.is_none() {
                        let leading = if frag.starts_span {
                            chrome.leading
                        } else {
                            px(0.)
                        };
                        open_box = Some((frag.x - chrome.pad_x - border - leading, frag.span_ix));
                        open_icon = frag.starts_span;
                    }
                    if frag.chrome_right || frag.range.end >= line.range.end {
                        let (left, _) = open_box.take().unwrap_or((frag.x, frag.span_ix));
                        let right = frag.x + frag.width + chrome.pad_x + border;
                        let top = baseline_y - frag.ascent - chrome.pad_y - border;
                        let bottom = baseline_y + frag.descent + chrome.pad_y + border;
                        window.paint_quad(PaintQuad {
                            bounds: Bounds::from_corners(
                                point(bounds.origin.x + left, top),
                                point(bounds.origin.x + right, bottom),
                            ),
                            corner_radii: Corners::all(chrome.radius),
                            background: chrome.background.into(),
                            border_widths: Edges::all(border),
                            border_color: chrome.border.unwrap_or(transparent_black()),
                            border_style: Default::default(),
                        });
                        if let Some((icon, color, icon_size, offset)) =
                            chrome.icon.filter(|_| open_icon)
                        {
                            let x = bounds.origin.x + left + border + chrome.pad_x + offset;
                            let y = (top + bottom) / 2. - icon_size / 2.;
                            let _ = window.paint_svg(
                                Bounds::new(point(x, y), size(icon_size, icon_size)),
                                icon.path(),
                                None,
                                gpui_kit::TransformationMatrix::unit(),
                                color,
                                cx,
                            );
                        }
                    }
                } else {
                    open_box = None;
                }
            }
            for frag in &line.fragments {
                let origin = point(bounds.origin.x + frag.x, baseline_y - frag.ascent);
                let _ = frag.line.paint(
                    origin,
                    frag.ascent + frag.descent,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                // Hovered links get upstream's dotted underline
                // (`radial-gradient(circle, currentcolor 0.75px, …)` every 4px).
                if hovered_link.is_some() && style_link(&self.content, frag.span_ix) == hovered_link
                {
                    let color = self.content.spans[frag.span_ix].style.color;
                    let y = bounds.origin.y + line.top + line.height - px(2.);
                    let mut x = bounds.origin.x + frag.x;
                    let end = x + frag.width;
                    while x + px(1.5) <= end {
                        window.paint_quad(PaintQuad {
                            bounds: Bounds::new(point(x + px(1.25), y), size(px(1.5), px(1.5))),
                            corner_radii: Corners::all(px(0.75)),
                            background: color.into(),
                            border_widths: Edges::default(),
                            border_color: transparent_black(),
                            border_style: Default::default(),
                        });
                        x += px(4.);
                    }
                }
            }
        }
        if let Some(caret) = self.caret {
            let line_ix = layout
                .lines
                .iter()
                .position(|line| {
                    caret.index < line.range.end
                        || (caret.index == text.len() && caret.index <= line.range.end)
                })
                .unwrap_or(layout.lines.len().saturating_sub(1));
            if let Some(line) = layout.lines.get(line_ix) {
                let x = FlowLayout::x_for_index(line, caret.index);
                window.paint_quad(PaintQuad {
                    bounds: Bounds::new(
                        bounds.origin + point(x, line.top),
                        size(caret.width, line.height),
                    ),
                    corner_radii: Corners::default(),
                    background: caret.color.into(),
                    border_widths: Edges::default(),
                    border_color: transparent_black(),
                    border_style: Default::default(),
                });
            }
        }
        drop(c);

        // Links: pointer cursor, hover underline, click to open.
        if !prepaint.link_hitboxes.is_empty() {
            let hovered = prepaint
                .link_hitboxes
                .iter()
                .find(|(_, b)| b.contains(&window.mouse_position()))
                .map(|(link, _)| *link);
            if hovered.is_some() && prepaint.hitbox.is_hovered(window) {
                window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
            }
            let links = prepaint.link_hitboxes.clone();
            let hitbox = prepaint.hitbox.clone();
            let state = interaction.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                if !phase.bubble() {
                    return;
                }
                let now = links
                    .iter()
                    .find(|(_, b)| b.contains(&event.position) && hitbox.is_hovered(window))
                    .map(|(link, _)| *link);
                if now != state.borrow().hovered {
                    state.borrow_mut().hovered = now;
                    window.refresh();
                }
            });
            let links = prepaint.link_hitboxes.clone();
            let state = interaction.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, _| {
                if phase.bubble()
                    && event.button == MouseButton::Left
                    && links.iter().any(|(_, b)| b.contains(&event.position))
                {
                    state.borrow_mut().pressed_at = Some(event.position);
                }
            });
            let links = prepaint.link_hitboxes.clone();
            let urls = self.content.links.clone();
            let on_link = self.on_link.clone();
            let state = interaction.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if !phase.bubble() || event.button != MouseButton::Left {
                    return;
                }
                let pressed = state.borrow_mut().pressed_at.take();
                let Some(pressed) = pressed else {
                    return;
                };
                let moved =
                    (event.position.x - pressed.x).abs() + (event.position.y - pressed.y).abs();
                if moved > px(4.) {
                    return;
                }
                if let Some((link, _)) = links.iter().find(|(_, b)| b.contains(&event.position))
                    && let Some(url) = urls.get(*link)
                {
                    match on_link.as_ref() {
                        Some(handler) => handler(url, window, cx),
                        None => cx.open_url(url),
                    }
                }
            });
        }
    }
}

fn style_link(content: &FlowText, span_ix: usize) -> Option<usize> {
    content.spans.get(span_ix).and_then(|s| s.style.link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaks_after_spaces_and_between_cjk() {
        assert_eq!(
            break_opportunities("a b  c", WhiteSpace::Normal),
            vec![2, 5]
        );
        assert_eq!(break_opportunities("你好", WhiteSpace::Normal), vec![3]);
        assert!(break_opportunities("a b", WhiteSpace::Pre).is_empty());
    }

    #[test]
    fn atoms_split_on_forced_breaks() {
        assert_eq!(
            atoms("ab cd\nef", WhiteSpace::Normal),
            vec![(0, 3, false), (3, 5, true), (6, 8, false)]
        );
    }
}
