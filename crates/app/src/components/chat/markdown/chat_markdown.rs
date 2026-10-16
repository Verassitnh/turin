//! Port of upstream/apps/web/src/components/ChatMarkdown.tsx and the
//! `.chat-markdown` rules in upstream/apps/web/src/index.css.
//!
//! Block geometry follows the stylesheet: `p, ul, ol, blockquote, pre,
//! .chat-markdown-table-container { margin: 0.65rem 0 }`, headings
//! `margin: 1.25rem 0 0.5rem`, with CSS margin collapsing between siblings
//! and through margin-transparent containers (list items, quotes). Inline
//! content goes through [`InlineFlow`], which reproduces the browser's line
//! boxes and wrapping. Parsed documents are cached by text, highlighted code
//! by content, so streaming and scrolling never re-parse unchanged messages.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px, relative, rems,
};

use super::code_highlight::{cached_highlight, highlight_code, plain_highlight};
use super::inline_flow::{FlowSpan, FlowSpanStyle, FlowText, InlineFlow, SpanChrome, WhiteSpace};
use super::markdown_links::{
    file_link_label, inline_code_file_path_candidate, is_markdown_file_link_label,
    parse_markdown_file_link,
};
use super::markdown_pipeline::{
    AlertKind, Align, Block, Inlines, LinkAutolink, ListItem, MarkdownDocument, Marks,
    parse_chat_markdown,
};
use crate::components::chat::chat_media;
use crate::components::chat::mermaid_diagram::{MERMAID_UNAVAILABLE, mermaid_diagram};
use crate::components::chat::message_copy_button::{CODE_COPY_TIMEOUT_MS, MessageCopyButton};
use crate::components::chat::timeline::assistant_citations::{
    assistant_citation_label, parse_assistant_citation_href,
};
use crate::composer::views::context_chip::ContextChipKind;
use crate::composer::views::skill_inline_text::{
    SKILL_CHIP_ICON, SkillSegment, split_skill_segments,
};
use crate::theme::color::{MixSpace, color, color_mix, with_alpha};
use crate::theme::tokens::{RADIUS_LG, tw};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};
use turin_shared::inline_skills::InlineSkill;

const MAX_DOCUMENT_CACHE_ENTRIES: usize = 2000;

#[derive(Default)]
struct DocumentCache {
    entries: HashMap<DocumentKey, Arc<MarkdownDocument>>,
    order: VecDeque<DocumentKey>,
}

/// (text hash, text length, line breaks, extra plugin hash).
type DocumentKey = (u64, usize, bool, u64);

/// `extraRemarkPlugins`: a pass over the parsed document, keyed so the cache tells two apart.
#[derive(Clone)]
pub struct ExtraRemarkPlugin {
    pub key: SharedString,
    pub apply: Arc<dyn Fn(&MarkdownDocument) -> MarkdownDocument>,
}

thread_local! {
    static DOCUMENTS: RefCell<DocumentCache> = RefCell::default();
}

/// The parsed document for `text`, cached across frames and rows.
// Every renderer here goes through `cached_document_with`; kept for callers without a plugin.
#[allow(dead_code)]
pub fn cached_document(text: &str, line_breaks: bool) -> Arc<MarkdownDocument> {
    cached_document_with(text, line_breaks, None)
}

/// [`cached_document`] with an extra remark plugin applied after parsing.
pub fn cached_document_with(
    text: &str,
    line_breaks: bool,
    plugin: Option<&ExtraRemarkPlugin>,
) -> Arc<MarkdownDocument> {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let plugin_hash = plugin.map_or(0, |plugin| {
        let mut hasher = DefaultHasher::new();
        plugin.key.hash(&mut hasher);
        hasher.finish()
    });
    let key = (hasher.finish(), text.len(), line_breaks, plugin_hash);
    if let Some(doc) = DOCUMENTS.with(|cache| cache.borrow().entries.get(&key).cloned()) {
        return doc;
    }
    let parsed = parse_chat_markdown(text, line_breaks);
    let doc = Arc::new(match plugin {
        Some(plugin) => (plugin.apply)(&parsed),
        None => parsed,
    });
    DOCUMENTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.entries.len() >= MAX_DOCUMENT_CACHE_ENTRIES
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(key);
        cache.entries.insert(key, doc.clone());
    });
    doc
}

/// `<ChatMarkdown>`.
#[derive(IntoElement)]
pub struct ChatMarkdown {
    id: ElementId,
    text: SharedString,
    line_breaks: bool,
    is_streaming: bool,
    /// `className="text-foreground"` (user messages, reasoning) instead of the
    /// default `text-foreground/80%`.
    full_foreground: bool,
    document_order: u64,
    extra_remark_plugin: Option<ExtraRemarkPlugin>,
    /// `$skill` tokens of these skills read as chips (`renderSkillInlineMarkdownChildren`).
    skills: Vec<InlineSkill>,
}

impl ChatMarkdown {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            line_breaks: false,
            is_streaming: false,
            full_foreground: false,
            document_order: 0,
            extra_remark_plugin: None,
            skills: Vec::new(),
        }
    }

    /// `extraRemarkPlugins`.
    pub fn extra_remark_plugin(mut self, plugin: ExtraRemarkPlugin) -> Self {
        self.extra_remark_plugin = Some(plugin);
        self
    }

    pub fn skills(mut self, skills: Vec<InlineSkill>) -> Self {
        self.skills = skills;
        self
    }

    pub fn line_breaks(mut self, line_breaks: bool) -> Self {
        self.line_breaks = line_breaks;
        self
    }

    pub fn streaming(mut self, streaming: bool) -> Self {
        self.is_streaming = streaming;
        self
    }

    pub fn full_foreground(mut self, full: bool) -> Self {
        self.full_foreground = full;
        self
    }

    /// Base selection order: leaves take `order + n`.
    pub fn document_order(mut self, order: u64) -> Self {
        self.document_order = order;
        self
    }
}

/// Rendered block plus the margins it exposes for collapsing.
struct Rendered {
    element: AnyElement,
    margin_top: Pixels,
    margin_bottom: Pixels,
}

#[derive(Clone)]
struct Fonts {
    sans: SharedString,
    mono: SharedString,
    code_size: Pixels,
}

/// Everything the block renderers share for one document.
struct Md<'a> {
    theme: &'a AppTheme,
    fonts: Fonts,
    rem: Pixels,
    id: ElementId,
    leaf: Cell<u64>,
    order: u64,
    streaming: bool,
    selection: Hsla,
    skills: &'a [InlineSkill],
}

/// Inherited text properties (`color`, `font-size`, `line-height`).
#[derive(Clone, Copy)]
struct Inherit {
    color: Hsla,
    size: Pixels,
    line_height: f32,
    weight: FontWeight,
    /// Inside a tight list item: paragraphs are unwrapped (no `<p>` margins).
    tight: bool,
    /// `ul` and `ol` nesting depth for marker styles.
    ul_depth: u32,
    ol_depth: u32,
}

impl Md<'_> {
    fn rem(&self, value: f32) -> Pixels {
        self.rem * value
    }

    fn next_leaf(&self) -> (ElementId, u64) {
        let n = self.leaf.get();
        self.leaf.set(n + 1);
        (
            ElementId::NamedInteger("md-leaf".into(), n),
            self.order.saturating_add(n),
        )
    }

    fn block_margin(&self) -> Pixels {
        self.rem(0.65)
    }

    /// FlowText for inline content. Path-shaped inline code and links to
    /// files become file chips (`MarkdownFileLink`), labelled with the
    /// file name the way `fileLinkLabel` does.
    fn flow_text(&self, inlines: &Inlines, inherit: Inherit) -> FlowText {
        let theme = self.theme;
        let mut text = String::with_capacity(inlines.text.len());
        let mut spans = Vec::with_capacity(inlines.runs.len());
        let mut links: Vec<SharedString> = inlines
            .links
            .iter()
            .map(|link| SharedString::from(link.clone()))
            .collect();
        let runs: Vec<_> = inlines.run_ranges().collect();
        let mut ix = 0;
        while ix < runs.len() {
            let (range, marks) = runs[ix].clone();
            let run_text = &inlines.text[range.clone()];
            if marks.code
                && marks.link.is_none()
                && let Some(path) = inline_code_file_path_candidate(run_text)
            {
                links.push(path.clone().into());
                self.push_chip(
                    &mut text,
                    &mut spans,
                    &file_link_label(&path),
                    links.len() - 1,
                    inherit,
                );
                ix += 1;
                continue;
            }
            if let Some(link) = marks.link
                && (ix == 0 || runs[ix - 1].1.link != Some(link))
                && let Some(citation) = inlines
                    .links
                    .get(link)
                    .and_then(|url| parse_assistant_citation_href(url))
            {
                let group_end = runs[ix..]
                    .iter()
                    .position(|(_, m)| m.link != Some(link))
                    .map_or(runs.len(), |offset| ix + offset);
                self.push_citation_chip(
                    &mut text,
                    &mut spans,
                    &assistant_citation_label(&citation),
                    link,
                    inherit,
                );
                ix = group_end;
                continue;
            }
            if let Some(link) = marks.link
                && (ix == 0 || runs[ix - 1].1.link != Some(link))
                && let Some(path) = inlines
                    .links
                    .get(link)
                    .and_then(|url| parse_markdown_file_link(url))
            {
                let group_end = runs[ix..]
                    .iter()
                    .position(|(_, m)| m.link != Some(link))
                    .map_or(runs.len(), |offset| ix + offset);
                let label: String = runs[ix..group_end]
                    .iter()
                    .map(|(r, _)| &inlines.text[r.clone()])
                    .collect();
                if is_markdown_file_link_label(&label, &path) {
                    self.push_chip(
                        &mut text,
                        &mut spans,
                        &file_link_label(&path),
                        link,
                        inherit,
                    );
                    ix = group_end;
                    continue;
                }
            }
            // `renderSkillInlineMarkdownChildren` leaves code and links alone.
            if !self.skills.is_empty() && !marks.code && marks.link.is_none() {
                let segments = split_skill_segments(run_text, self.skills);
                if segments
                    .iter()
                    .any(|s| matches!(s, SkillSegment::Skill { .. }))
                {
                    for segment in segments {
                        match segment {
                            SkillSegment::Text(piece) => {
                                text.push_str(&piece);
                                spans.push(FlowSpan {
                                    len: piece.len(),
                                    style: self.span_style(marks, inherit, theme),
                                });
                            }
                            SkillSegment::Skill { label, .. } => {
                                self.push_skill_chip(&mut text, &mut spans, &label, inherit);
                            }
                        }
                    }
                    ix += 1;
                    continue;
                }
            }
            text.push_str(run_text);
            let mut style = self.span_style(marks, inherit, theme);
            let autolink = marks.link.and_then(|link| {
                inlines
                    .autolinks
                    .iter()
                    .find(|(index, _)| *index == link)
                    .map(|(_, kind)| *kind)
            });
            // `pullRequestAutolink === "commit" && "font-mono"`.
            if autolink == Some(LinkAutolink::PullRequestCommit) {
                style.family = self.fonts.mono.clone();
            }
            // `MarkdownLinkFavicon` leads an external link (first run only):
            // `ms-[0.25em] me-[0.2em] size-[14px]`, GitHub's mark or a globe.
            // `faviconHost && hasText && !isPullRequestAutolink`.
            if let Some(link) = marks.link
                && autolink.is_none()
                && (ix == 0 || runs[ix - 1].1.link != Some(link))
                && let Some(host) = inlines
                    .links
                    .get(link)
                    .and_then(|url| external_link_host(url))
            {
                let icon = if host == "github.com" || host.ends_with(".github.com") {
                    IconName::Github
                } else {
                    IconName::Globe
                };
                style.chrome = Some(SpanChrome {
                    background: gpui_kit::transparent_black(),
                    border: None,
                    radius: px(0.),
                    pad_x: px(0.),
                    pad_y: px(0.),
                    leading: inherit.size * 0.45 + px(14.),
                    icon: Some((icon, style.color, px(14.), inherit.size * 0.25)),
                });
            }
            spans.push(FlowSpan {
                len: range.len(),
                style,
            });
            ix += 1;
        }
        FlowText {
            text: text.into(),
            spans,
            links,
        }
    }

    /// `ContextChip kind="mention"` + `FileTagChipContent`: `h-[1.41em]
    /// rounded-[0.5em] border px-[0.5em] gap-[0.33em] font-medium
    /// text-[0.86em]`, icon `size-[1.17em]`.
    fn push_chip(
        &self,
        text: &mut String,
        spans: &mut Vec<FlowSpan>,
        label: &str,
        link: usize,
        inherit: Inherit,
    ) {
        let theme = self.theme;
        let accent: Hsla = ContextChipKind::Mention
            .accent()
            .expect("mention chips carry an accent");
        let size = inherit.size * 0.86;
        let foreground: Hsla = color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into();
        text.push_str(label);
        spans.push(FlowSpan {
            len: label.len(),
            style: FlowSpanStyle {
                family: self.fonts.sans.clone(),
                weight: FontWeight::MEDIUM,
                italic: false,
                size,
                line_height: 1.,
                color: foreground,
                strikethrough: false,
                chrome: Some(SpanChrome {
                    background: with_alpha(accent, 0.11).into(),
                    border: Some(color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into()),
                    radius: size * 0.5,
                    pad_x: size * 0.5,
                    pad_y: size * 0.1,
                    leading: size * 1.5,
                    icon: Some((IconName::FileText, foreground, size * 1.17, px(0.))),
                }),
                link: Some(link),
                background: None,
                atomic: true,
            },
        });
    }

    fn span_style(&self, marks: Marks, inherit: Inherit, theme: &AppTheme) -> FlowSpanStyle {
        let mut style = FlowSpanStyle {
            family: self.fonts.sans.clone(),
            weight: inherit.weight,
            italic: marks.emphasis,
            size: inherit.size,
            line_height: inherit.line_height,
            color: inherit.color,
            strikethrough: marks.delete,
            chrome: None,
            link: marks.link,
            background: None,
            atomic: false,
        };
        if marks.strong {
            // Preflight `font-weight: bolder`.
            style.weight = if inherit.weight.0 >= 600. {
                FontWeight::BLACK
            } else {
                FontWeight::BOLD
            };
        }
        if marks.code {
            // `.chat-markdown :not(pre) > code`.
            style.family = self.fonts.mono.clone();
            style.size = self.rem(0.75);
            style.color = theme.foreground;
            style.chrome = Some(SpanChrome {
                background: theme.muted,
                border: Some(theme.border),
                radius: self.rem(0.375),
                pad_x: self.rem(0.35),
                pad_y: self.rem(0.1),
                leading: px(0.),
                icon: None,
            });
        }
        if marks.link.is_some() {
            style.color = theme.info_foreground;
        }
        if marks.footnote_ref {
            style.size = self.rem(0.6875);
            style.weight = FontWeight::SEMIBOLD;
            style.color = theme.info_foreground;
        }
        style
    }

    fn inline_flow(
        &self,
        inlines: &Inlines,
        inherit: Inherit,
        white_space: WhiteSpace,
    ) -> InlineFlow {
        let (id, order) = self.next_leaf();
        InlineFlow::new(id, self.flow_text(inlines, inherit), white_space)
            .strut(self.fonts.sans.clone(), inherit.size, inherit.line_height)
            .document_order(order)
            .selection_color(self.selection)
            .on_link(Rc::new(|url, _, cx| {
                if let Some(citation) = parse_assistant_citation_href(url) {
                    chat_media::open_quote(citation, cx);
                } else {
                    cx.open_url(url);
                }
            }))
    }

    /// Stacks rendered siblings with collapsed margins. Returns the stack and
    /// the outer margins it exposes (first child's top, last child's bottom).
    fn stack(&self, children: Vec<Rendered>) -> Rendered {
        let mut margin_top = px(0.);
        let mut margin_bottom = px(0.);
        let count = children.len();
        let mut column = div().flex().flex_col().w_full().min_w_0();
        let mut previous_bottom: Option<Pixels> = None;
        for (ix, child) in children.into_iter().enumerate() {
            if ix == 0 {
                margin_top = child.margin_top;
            }
            let gap = previous_bottom.map(|bottom| bottom.max(child.margin_top));
            column = column.child(
                div()
                    .when_some(gap, |this, gap| this.pt(gap))
                    .child(child.element),
            );
            previous_bottom = Some(child.margin_bottom);
            if ix + 1 == count {
                margin_bottom = child.margin_bottom;
            }
        }
        Rendered {
            element: column.into_any_element(),
            margin_top,
            margin_bottom,
        }
    }

    fn blocks(
        &self,
        blocks: &[Block],
        inherit: Inherit,
        window: &mut Window,
        cx: &mut App,
    ) -> Rendered {
        let rendered = blocks
            .iter()
            .map(|block| self.block(block, inherit, window, cx))
            .collect();
        self.stack(rendered)
    }

    fn block(
        &self,
        block: &Block,
        inherit: Inherit,
        window: &mut Window,
        cx: &mut App,
    ) -> Rendered {
        let margin = self.block_margin();
        match block {
            Block::Paragraph(inlines) => {
                let margin = if inherit.tight { px(0.) } else { margin };
                Rendered {
                    element: div()
                        .w_full()
                        .min_w_0()
                        .child(self.inline_flow(inlines, inherit, WhiteSpace::Normal))
                        .into_any_element(),
                    margin_top: margin,
                    margin_bottom: margin,
                }
            }
            Block::Heading { level, content } => {
                let size = match level {
                    1 => self.rem(1.25),
                    2 => self.rem(1.125),
                    3 => self.rem(1.0),
                    _ => self.rem(0.875),
                };
                let color = if *level >= 6 {
                    self.theme.muted_foreground
                } else {
                    self.theme.foreground
                };
                let heading = Inherit {
                    color,
                    size,
                    line_height: 1.3,
                    weight: FontWeight::SEMIBOLD,
                    ..inherit
                };
                Rendered {
                    element: div()
                        .w_full()
                        .min_w_0()
                        .child(self.inline_flow(content, heading, WhiteSpace::Normal))
                        .into_any_element(),
                    margin_top: self.rem(1.25),
                    margin_bottom: self.rem(0.5),
                }
            }
            Block::ThematicBreak => Rendered {
                // Preflight: `hr { height: 0; border-top-width: 1px }`, no margin.
                element: div()
                    .w_full()
                    .h(px(1.))
                    .bg(self.theme.border)
                    .into_any_element(),
                margin_top: px(0.),
                margin_bottom: px(0.),
            },
            Block::Blockquote(children) => {
                let quote = Inherit {
                    color: self.theme.muted_foreground,
                    tight: false,
                    ..inherit
                };
                let inner = self.blocks(children, quote, window, cx);
                Rendered {
                    element: div()
                        .w_full()
                        .min_w_0()
                        .border_l_2()
                        .border_color(self.theme.border)
                        .pl(self.rem(0.8))
                        .child(inner.element)
                        .into_any_element(),
                    margin_top: margin.max(inner.margin_top),
                    margin_bottom: margin.max(inner.margin_bottom),
                }
            }
            Block::Alert { kind, children } => self.alert(*kind, children, inherit, window, cx),
            Block::List {
                ordered,
                start,
                items,
                ..
            } => self.list(*ordered, *start, items, inherit, window, cx),
            Block::Code {
                lang, meta, code, ..
            } => self.code_block(lang.as_deref(), meta.as_deref(), code, window, cx),
            Block::Table {
                align,
                header,
                rows,
            } => self.table(align, header, rows, inherit, window, cx),
            Block::Html(html) => {
                let inlines = Inlines {
                    text: html.trim_end().to_string(),
                    runs: vec![super::markdown_pipeline::InlineRun {
                        len: html.trim_end().len(),
                        marks: Marks::default(),
                    }],
                    ..Default::default()
                };
                Rendered {
                    element: div()
                        .w_full()
                        .child(self.inline_flow(&inlines, inherit, WhiteSpace::PreWrap))
                        .into_any_element(),
                    margin_top: margin,
                    margin_bottom: margin,
                }
            }
            Block::Footnotes(definitions) => {
                // `section[data-footnotes]`: mt 1.25rem, top border, muted, text-xs.
                let small = Inherit {
                    color: self.theme.muted_foreground,
                    size: self.rem(0.75),
                    tight: true,
                    ..inherit
                };
                let items: Vec<ListItem> = definitions
                    .iter()
                    .map(|(_, blocks)| ListItem {
                        checked: None,
                        spread: false,
                        children: blocks.clone(),
                    })
                    .collect();
                let list = self.list(true, None, &items, small, window, cx);
                Rendered {
                    element: div()
                        .w_full()
                        .border_t_1()
                        .border_color(self.theme.border)
                        .pt(self.rem(0.75))
                        .child(list.element)
                        .into_any_element(),
                    margin_top: self.rem(1.25),
                    margin_bottom: px(0.),
                }
            }
        }
    }

    /// The blockquote renderer's `GITHUB_ALERT_PRESENTATIONS` branch.
    fn alert(
        &self,
        kind: AlertKind,
        children: &[Block],
        inherit: Inherit,
        window: &mut Window,
        cx: &mut App,
    ) -> Rendered {
        let dark = self.theme.is_dark();
        let (label, icon, border, title): (&str, IconName, u32, u32) = match kind {
            AlertKind::Note => (
                "Note",
                IconName::Info,
                0x2b7fff,
                if dark { 0x51a2ff } else { 0x155dfc },
            ),
            AlertKind::Tip => (
                "Tip",
                IconName::Lightbulb,
                0x00bc7d,
                if dark { 0x00d492 } else { 0x009966 },
            ),
            AlertKind::Important => (
                "Important",
                IconName::MessageSquareWarning,
                0xad46ff,
                if dark { 0xc27aff } else { 0x9810fa },
            ),
            AlertKind::Warning => (
                "Warning",
                IconName::TriangleAlert,
                0xfe9a00,
                if dark { 0xfe9a00 } else { 0xe17100 },
            ),
            AlertKind::Caution => (
                "Caution",
                IconName::OctagonAlert,
                0xfb2c36,
                if dark { 0xff6467 } else { 0xe7000b },
            ),
        };
        let title_color: Hsla = crate::theme::color::hex(title).into();
        let border_color: Hsla = with_alpha(crate::theme::color::hex(border), 0.7).into();
        let body = self.blocks(
            children,
            Inherit {
                tight: false,
                ..inherit
            },
            window,
            cx,
        );
        let margin = self.block_margin();
        // The title is a `<p>`: it takes the paragraph margins, which collapse
        // through the alert's `my-1` on top and against the body below.
        let title_row = div()
            .flex()
            .items_center()
            .gap(self.rem(0.375))
            .h(inherit.size * inherit.line_height)
            .text_size(inherit.size)
            .font_weight(FontWeight::MEDIUM)
            .text_color(title_color)
            .child(Icon::new(icon).size(rems(0.875)).color(title_color))
            .child(label.to_string());
        let gap = margin.max(body.margin_top);
        Rendered {
            element: div()
                .w_full()
                .min_w_0()
                .border_l_2()
                .border_color(border_color)
                .pl(self.rem(0.75))
                .child(title_row)
                .when(!children.is_empty(), |this| {
                    this.child(div().pt(gap).child(body.element))
                })
                .into_any_element(),
            margin_top: self.rem(0.25).max(margin),
            margin_bottom: self.rem(0.25).max(body.margin_bottom),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list(
        &self,
        ordered: bool,
        start: Option<u32>,
        items: &[ListItem],
        inherit: Inherit,
        window: &mut Window,
        cx: &mut App,
    ) -> Rendered {
        let margin = self.block_margin();
        // `--list-gutter`: 1.25rem, widened for multi-character ordered markers.
        let first = start.unwrap_or(1);
        let gutter = if ordered {
            let last = first + (items.len().max(1) as u32 - 1);
            let width = first.to_string().len().max(last.to_string().len());
            if width <= 1 {
                self.rem(1.25)
            } else {
                // `${markerWidth + 2}ch`: one `ch` is the width of "0".
                self.zero_width(inherit.size, window) * (width as f32 + 2.)
            }
        } else {
            self.rem(1.25)
        };
        let nested = Inherit {
            ul_depth: inherit.ul_depth + u32::from(!ordered),
            ol_depth: inherit.ol_depth + u32::from(ordered),
            ..inherit
        };
        let line = inherit.size * inherit.line_height;
        let mut rendered = Vec::with_capacity(items.len());
        for (ix, item) in items.iter().enumerate() {
            let tight = !item.spread;
            let content = self.blocks(&item.children, Inherit { tight, ..nested }, window, cx);
            let marker_text = if ordered {
                list_marker_ordered(first as u64 + ix as u64, nested.ol_depth)
            } else {
                list_marker_bullet(nested.ul_depth).to_string()
            };
            let item_el = if let Some(checked) = item.checked {
                // GitHub task list: the checkbox replaces the marker, pulled
                // into the gutter with `margin-left: calc(-1 * gutter)`.
                div()
                    .relative()
                    .flex()
                    .items_start()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(line)
                            .flex()
                            .items_center()
                            .ml(-gutter)
                            .mr(inherit.size * 0.35)
                            .child(self.checkbox(checked)),
                    )
                    .child(div().flex_1().min_w_0().child(content.element))
            } else {
                div()
                    .relative()
                    .w_full()
                    .min_w_0()
                    .child(
                        // `list-style-position: outside`: the marker (with its
                        // trailing space) ends where the content starts.
                        div()
                            .absolute()
                            .top_0()
                            .left(-gutter)
                            .w(gutter)
                            .h(line)
                            .flex()
                            .justify_end()
                            .items_center()
                            .pr(inherit.size * 0.28)
                            .text_size(inherit.size)
                            .line_height(relative(inherit.line_height))
                            .text_color(inherit.color)
                            .font_family(self.fonts.sans.clone())
                            .child(marker_text),
                    )
                    .child(content.element)
            };
            // `li + li { margin-top: 0.25rem }`; li is margin-transparent.
            let own_top = if ix == 0 { px(0.) } else { self.rem(0.25) };
            rendered.push(Rendered {
                element: item_el.into_any_element(),
                margin_top: own_top.max(content.margin_top),
                margin_bottom: content.margin_bottom,
            });
        }
        let stack = self.stack(rendered);
        Rendered {
            element: div()
                .w_full()
                .min_w_0()
                .pl(gutter)
                .child(stack.element)
                .into_any_element(),
            margin_top: margin.max(stack.margin_top),
            margin_bottom: margin.max(stack.margin_bottom),
        }
    }

    fn zero_width(&self, size: Pixels, window: &Window) -> Pixels {
        let line = window.text_system().shape_line(
            "0".into(),
            size,
            &[gpui_kit::TextRun {
                len: 1,
                font: gpui_kit::font(self.fonts.sans.clone()),
                color: self.theme.foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        line.width()
    }

    /// A read-only checkbox (`input[type=checkbox]` with the platform look).
    fn checkbox(&self, checked: bool) -> AnyElement {
        let size = px(13.);
        let accent = self.theme.info;
        div()
            .size(size)
            .rounded(px(3.))
            .border_1()
            .flex()
            .items_center()
            .justify_center()
            .when(checked, |this| this.bg(accent).border_color(accent))
            .when(!checked, |this| {
                this.bg(self.theme.background)
                    .border_color(self.theme.input)
            })
            .when(checked, |this| {
                this.child(
                    Icon::new(IconName::Check)
                        .size(rems(0.625))
                        .color(gpui_kit::white()),
                )
            })
            .opacity(0.7)
            .into_any_element()
    }

    /// `MarkdownCodeBlock` / `MarkdownCodeBlockFrame`.
    fn code_block(
        &self,
        lang: Option<&str>,
        meta: Option<&str>,
        code: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Rendered {
        let theme = self.theme;
        let dark = theme.is_dark();
        let language = lang.unwrap_or("text").to_string();
        let fence_title = meta.and_then(extract_fence_title);
        let (id, order) = self.next_leaf();
        let state_id =
            ElementId::NamedChild(Arc::new(self.id.clone()), format!("code-{order}").into());
        let wrapped_state = window.use_keyed_state(state_id.clone(), cx, |_, _| CodeBlockState {
            wrapped: true,
            show_code: false,
            pending: None,
            highlighted: None,
        });
        let wrapped = wrapped_state.read(cx).wrapped;

        // Shiki keeps a trailing newline's empty line out of the output.
        let code_text = code.strip_suffix('\n').unwrap_or(code);
        let mut hasher = DefaultHasher::new();
        code_text.hash(&mut hasher);
        let code_hash = hasher.finish();
        let ready = {
            let state = wrapped_state.read(cx);
            state.highlighted.as_ref().and_then(|(hash, code)| {
                (*hash == code_hash && accept_highlight(state.pending, code_hash))
                    .then(|| code.clone())
            })
        };
        let highlighted =
            if let Some(hit) = ready.or_else(|| cached_highlight(code_text, &language, dark)) {
                hit
            } else if wrapped_state.read(cx).pending == Some(code_hash) {
                plain_highlight(code_text, dark)
            } else {
                let code_owned = code_text.to_string();
                let language_owned = language.clone();
                let state = wrapped_state.clone();
                wrapped_state.update(cx, |state, _| state.pending = Some(code_hash));
                cx.spawn(async move |cx| {
                    let highlighted = cx
                        .background_executor()
                        .spawn(async move { highlight_code(&code_owned, &language_owned, dark) })
                        .await;
                    state.update(cx, |state, cx| {
                        if accept_highlight(state.pending, code_hash) {
                            state.highlighted = Some((code_hash, highlighted));
                            cx.notify();
                        }
                    });
                })
                .detach();
                plain_highlight(code_text, dark)
            };
        let mermaid = language.eq_ignore_ascii_case("mermaid");
        let show_diagram = mermaid
            && !self.streaming
            && !wrapped_state.read(cx).show_code
            && !code_text.trim().is_empty();
        let code_size = self.fonts.code_size;
        let mut spans = Vec::new();
        let mut cursor = 0usize;
        let base = FlowSpanStyle {
            family: self.fonts.mono.clone(),
            weight: FontWeight::NORMAL,
            italic: false,
            size: code_size,
            line_height: 1.375,
            color: highlighted.foreground,
            strikethrough: false,
            chrome: None,
            link: None,
            background: None,
            atomic: false,
        };
        for (range, color) in &highlighted.tokens {
            if range.start < cursor || range.end > code_text.len() {
                continue;
            }
            if range.start > cursor {
                spans.push(FlowSpan {
                    len: range.start - cursor,
                    style: base.clone(),
                });
            }
            spans.push(FlowSpan {
                len: range.end - range.start,
                style: FlowSpanStyle {
                    color: *color,
                    ..base.clone()
                },
            });
            cursor = range.end;
        }
        if cursor < code_text.len() {
            spans.push(FlowSpan {
                len: code_text.len() - cursor,
                style: base.clone(),
            });
        }
        let content = FlowText {
            text: expand_tabs(code_text).into(),
            spans: retarget_spans_for_tabs(code_text, spans),
            links: Vec::new(),
        };
        let flow = InlineFlow::new(
            id,
            content,
            if wrapped {
                WhiteSpace::PreWrap
            } else {
                WhiteSpace::Pre
            },
        )
        .strut(self.fonts.mono.clone(), code_size, 1.375)
        .document_order(order)
        .selection_color(self.selection);

        // `bg-secondary border-border/70`, dark `bg-input/32 border-transparent`.
        let (bg, border) = if dark {
            (
                with_alpha(theme.input, 0.32).into(),
                gpui_kit::transparent_black(),
            )
        } else {
            (theme.secondary, with_alpha(theme.border, 0.7).into())
        };
        let header_color: Hsla = with_alpha(theme.foreground, 0.72).into();
        let copy_text: SharedString = code.to_string().into();
        let wrap_label = if wrapped {
            "Disable line wrap"
        } else {
            "Wrap lines"
        };
        let toggle_state = wrapped_state.clone();
        let diagram_state = wrapped_state.clone();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(self.rem(0.5))
            .pt(self.rem(0.375))
            .pr(self.rem(0.375))
            .pl(self.rem(0.75))
            .text_color(header_color)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(self.rem(0.375))
                    .font_family(self.fonts.mono.clone())
                    .text_size(self.rem(0.6875))
                    .line_height(rems(1.))
                    .child(div().truncate().child(fence_title.unwrap_or(language))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.rem(0.125))
                    .when(mermaid && !self.streaming, |this| {
                        let label = if show_diagram {
                            "Show code"
                        } else {
                            "Show diagram"
                        };
                        let icon = if show_diagram {
                            IconName::Code
                        } else {
                            IconName::Workflow
                        };
                        this.child(
                            Button::new(ElementId::NamedChild(
                                Arc::new(state_id.clone()),
                                "diagram".into(),
                            ))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconXs)
                            .tooltip(Tooltip::new(label))
                            .child(Icon::new(icon).size(rems(0.75)))
                            .on_click(move |_, _, cx| {
                                diagram_state.update(cx, |state, cx| {
                                    state.show_code = !state.show_code;
                                    cx.notify();
                                });
                            }),
                        )
                    })
                    .child(
                        Button::new(ElementId::NamedChild(
                            Arc::new(state_id.clone()),
                            "wrap".into(),
                        ))
                        .variant(if wrapped {
                            ButtonVariant::Secondary
                        } else {
                            ButtonVariant::GhostMuted
                        })
                        .size(ButtonSize::IconXs)
                        .pressed(false)
                        .tooltip(Tooltip::new(wrap_label))
                        .child(Icon::new(IconName::TextWrap).size(rems(0.75)))
                        .on_click(move |_, _, cx| {
                            toggle_state.update(cx, |state, cx| {
                                state.wrapped = !state.wrapped;
                                cx.notify();
                            });
                        }),
                    )
                    .child(
                        MessageCopyButton::new(
                            ElementId::NamedChild(Arc::new(state_id.clone()), "copy".into()),
                            copy_text,
                        )
                        .label("Copy code")
                        .size(ButtonSize::IconXs)
                        .ghost()
                        .timeout_ms(CODE_COPY_TIMEOUT_MS),
                    ),
            );
        let body = div()
            .id(ElementId::NamedChild(Arc::new(state_id), "scroll".into()))
            .w_full()
            .px(self.rem(0.9))
            .py(self.rem(0.8))
            .when(!wrapped && !show_diagram, |this| this.overflow_x_scroll())
            .child(if show_diagram {
                mermaid_diagram(code_text, MERMAID_UNAVAILABLE, cx)
            } else {
                div()
                    .when(!wrapped, |this| this.flex_none())
                    .child(flow)
                    .into_any_element()
            });
        let margin = self.block_margin();
        Rendered {
            element: div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(border)
                .bg(bg)
                .child(header)
                .child(body)
                .into_any_element(),
            margin_top: margin,
            margin_bottom: margin,
        }
    }

    /// `MarkdownTable` and the `.chat-markdown table` rules.
    #[allow(clippy::too_many_arguments)]
    fn table(
        &self,
        align: &[Align],
        header: &[Inlines],
        rows: &[Vec<Inlines>],
        inherit: Inherit,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Rendered {
        let theme = self.theme;
        let text = Inherit {
            size: self.rem(0.75),
            tight: true,
            ..inherit
        };
        let head = Inherit {
            weight: FontWeight::SEMIBOLD,
            ..text
        };
        let row_border: Hsla = with_alpha(theme.border, 0.6).into();
        let cols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        let max_cell = self.rem(24.);
        let mut grid = div().flex().flex_col().min_w_full();
        let cell = |md: &Self, inlines: &Inlines, style: Inherit, col: usize, heading: bool| {
            let justify = match align.get(col).copied().unwrap_or(Align::None) {
                Align::Right => gpui_kit::TextAlign::Right,
                Align::Center => gpui_kit::TextAlign::Center,
                _ => gpui_kit::TextAlign::Left,
            };
            let _ = justify;
            div()
                .flex_1()
                .min_w(px(0.))
                .max_w(max_cell)
                .px(md.rem(0.75))
                .py(if heading { md.rem(0.55) } else { md.rem(0.45) })
                .child(md.inline_flow(
                    inlines,
                    style,
                    if heading {
                        WhiteSpace::Pre
                    } else {
                        WhiteSpace::NormalWords
                    },
                ))
        };
        let mut header_row = div().flex().w_full().border_b_1().border_color(row_border);
        for col in 0..cols {
            let inlines = header.get(col).cloned().unwrap_or_default();
            header_row = header_row.child(cell(self, &inlines, head, col, true));
        }
        grid = grid.child(header_row);
        for row in rows {
            let mut row_el = div().flex().w_full().border_b_1().border_color(row_border);
            for col in 0..cols {
                let inlines = row.get(col).cloned().unwrap_or_default();
                row_el = row_el.child(cell(self, &inlines, text, col, false));
            }
            grid = grid.child(row_el);
        }
        let markdown_copy: SharedString = serialize_table_markdown(header, rows).into();
        let table_id = ElementId::NamedChild(
            Arc::new(self.id.clone()),
            format!("table-{}", self.leaf.get()).into(),
        );
        let footer = div()
            .mt(self.rem(0.125))
            .flex()
            .items_center()
            .justify_between()
            .child(
                Button::new(ElementId::NamedChild(
                    Arc::new(table_id.clone()),
                    "expand".into(),
                ))
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::IconXs)
                .tooltip(Tooltip::new("Collapse table cells"))
                .child(Icon::new(IconName::Minimize2).size(rems(0.75))),
            )
            .child(
                MessageCopyButton::new(
                    ElementId::NamedChild(Arc::new(table_id.clone()), "copy".into()),
                    markdown_copy,
                )
                .label("Copy table")
                .size(ButtonSize::IconXs)
                .ghost()
                .timeout_ms(CODE_COPY_TIMEOUT_MS),
            );
        let margin = self.block_margin();
        Rendered {
            element: div()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .id(ElementId::NamedChild(Arc::new(table_id), "scroll".into()))
                        .w_full()
                        .overflow_x_scroll()
                        .child(grid),
                )
                .child(footer)
                .into_any_element(),
            margin_top: margin,
            margin_bottom: margin,
        }
    }
}

struct CodeBlockState {
    wrapped: bool,
    show_code: bool,
    pending: Option<u64>,
    highlighted: Option<(u64, Arc<super::code_highlight::HighlightedCode>)>,
}

/// A highlight job is current only while its hash is still the one in flight.
fn accept_highlight(pending: Option<u64>, finished: u64) -> bool {
    pending == Some(finished)
}

impl Md<'_> {
    fn push_citation_chip(
        &self,
        text: &mut String,
        spans: &mut Vec<FlowSpan>,
        label: &str,
        link: usize,
        inherit: Inherit,
    ) {
        let theme = self.theme;
        let accent: Hsla = ContextChipKind::Citation
            .accent()
            .expect("citation chips carry an accent");
        let size = inherit.size * 0.86;
        let foreground: Hsla = color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into();
        text.push_str(label);
        spans.push(FlowSpan {
            len: label.len(),
            style: FlowSpanStyle {
                family: self.fonts.sans.clone(),
                weight: FontWeight::MEDIUM,
                italic: false,
                size,
                line_height: 1.,
                color: foreground,
                strikethrough: false,
                chrome: Some(SpanChrome {
                    background: with_alpha(accent, 0.11).into(),
                    border: Some(color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into()),
                    radius: size * 0.5,
                    pad_x: size * 0.5,
                    pad_y: size * 0.1,
                    leading: size * 1.5,
                    icon: Some((IconName::Quote, foreground, size * 1.17, px(0.))),
                }),
                link: Some(link),
                background: None,
                atomic: true,
            },
        });
    }
}

impl Md<'_> {
    /// `ContextChip kind="skill"` + `SkillChipIcon` + `ContextChipLabel`.
    fn push_skill_chip(
        &self,
        text: &mut String,
        spans: &mut Vec<FlowSpan>,
        label: &str,
        inherit: Inherit,
    ) {
        let theme = self.theme;
        let accent: Hsla = ContextChipKind::Skill
            .accent()
            .expect("skill chips carry an accent");
        let size = inherit.size * 0.86;
        let foreground: Hsla = color_mix(MixSpace::Oklab, accent, 0.22, theme.foreground).into();
        text.push_str(label);
        spans.push(FlowSpan {
            len: label.len(),
            style: FlowSpanStyle {
                family: self.fonts.sans.clone(),
                weight: FontWeight::MEDIUM,
                italic: false,
                size,
                line_height: 1.,
                color: foreground,
                strikethrough: false,
                chrome: Some(SpanChrome {
                    background: with_alpha(accent, 0.11).into(),
                    border: Some(color_mix(MixSpace::Oklab, accent, 0.34, theme.border).into()),
                    radius: size * 0.5,
                    pad_x: size * 0.5,
                    pad_y: size * 0.1,
                    leading: size * 1.5,
                    icon: Some((SKILL_CHIP_ICON, foreground, size * 1.17, px(0.))),
                }),
                link: None,
                background: None,
                atomic: true,
            },
        });
    }
}

/// `extractFenceTitle`: ```ts title="x.ts" / ```ts src/main.ts
fn extract_fence_title(meta: &str) -> Option<String> {
    for token in meta.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        for key in ["title=", "file=", "filename="] {
            if lower.starts_with(key) {
                let value = &token[key.len()..];
                let value = value.trim_matches(['"', '\'']);
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    // `FENCE_FILENAME_TOKEN_REGEX`: a bare file name token.
    let first = meta.split_whitespace().next()?;
    let valid_start = first
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '@');
    let valid_chars = first
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '.' | '/' | '-'));
    let ext = first.rsplit_once('.').map(|(_, ext)| ext);
    (valid_start
        && valid_chars
        && ext.is_some_and(|ext| !ext.is_empty() && ext.chars().all(char::is_alphanumeric)))
    .then(|| first.to_string())
}

/// Host of an `http(s)` link, for its favicon.
fn external_link_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.split('@').next_back()?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// CSS `tab-size: 8`.
fn expand_tabs(code: &str) -> String {
    if !code.contains('\t') {
        return code.to_string();
    }
    let mut out = String::with_capacity(code.len() + 16);
    let mut column = 0usize;
    for c in code.chars() {
        match c {
            '\t' => {
                let spaces = 8 - (column % 8);
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' => {
                out.push('\n');
                column = 0;
            }
            c => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

/// Re-measures span lengths after [`expand_tabs`].
fn retarget_spans_for_tabs(code: &str, spans: Vec<FlowSpan>) -> Vec<FlowSpan> {
    if !code.contains('\t') {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len());
    let mut offset = 0usize;
    let mut column = 0usize;
    for span in spans {
        let piece = &code[offset..offset + span.len];
        let mut len = 0usize;
        for c in piece.chars() {
            match c {
                '\t' => {
                    let spaces = 8 - (column % 8);
                    len += spaces;
                    column += spaces;
                }
                '\n' => {
                    len += 1;
                    column = 0;
                }
                c => {
                    len += c.len_utf8();
                    column += 1;
                }
            }
        }
        offset += span.len;
        out.push(FlowSpan {
            len,
            style: span.style,
        });
    }
    out
}

/// `serializeTableElementToMarkdown`.
fn serialize_table_markdown(header: &[Inlines], rows: &[Vec<Inlines>]) -> String {
    let cell = |inlines: &Inlines| inlines.text.replace('|', "\\|").replace('\n', " ");
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(&header.iter().map(cell).collect::<Vec<_>>().join(" | "));
    out.push_str(" |\n|");
    out.push_str(&header.iter().map(|_| " --- |").collect::<String>());
    for row in rows {
        out.push_str("\n| ");
        out.push_str(&row.iter().map(cell).collect::<Vec<_>>().join(" | "));
        out.push_str(" |");
    }
    out
}

/// Marker for `ul` at nesting `depth` (1-based): disc, circle, square.
fn list_marker_bullet(depth: u32) -> &'static str {
    match depth {
        0 | 1 => "•",
        2 => "◦",
        _ => "▪",
    }
}

/// Marker for `ol` at nesting `depth`: decimal, lower-alpha, lower-roman.
fn list_marker_ordered(n: u64, depth: u32) -> String {
    match depth {
        0 | 1 => format!("{n}."),
        2 => format!("{}.", lower_alpha(n)),
        _ => format!("{}.", lower_roman(n)),
    }
}

fn lower_alpha(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    out.iter().rev().collect()
}

fn lower_roman(mut n: u64) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let table = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, numeral) in table {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

impl RenderOnce for ChatMarkdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let document = cached_document_with(
            &self.text,
            self.line_breaks,
            self.extra_remark_plugin.as_ref(),
        );
        let rem = window.rem_size();
        let fonts = Fonts {
            sans: theme.fonts.sans.clone().into(),
            mono: theme.fonts.mono.clone().into(),
            code_size: px(theme.fonts.size_code),
        };
        let base_color: Hsla = if self.full_foreground {
            theme.foreground
        } else {
            // `text-foreground/[calc(80%+var(--appearance-contrast-boost)/5)]`.
            with_alpha(theme.foreground, 0.8).into()
        };
        let selection: Hsla = with_alpha(
            theme.theme_accent,
            if theme.is_dark() { 0.35 } else { 0.25 },
        )
        .into();
        let md = Md {
            theme: &theme,
            fonts,
            rem,
            id: self.id.clone(),
            leaf: Cell::new(0),
            order: self.document_order,
            streaming: self.is_streaming,
            selection,
            skills: &self.skills,
        };
        let _ = md.streaming;
        let inherit = Inherit {
            color: base_color,
            size: rem * 0.875,
            line_height: 1.625,
            weight: FontWeight::NORMAL,
            tight: false,
            ul_depth: 0,
            ol_depth: 0,
        };
        let content = md.blocks(&document.blocks, inherit, window, cx);
        let _ = color_mix(MixSpace::Oklab, tw::WHITE, 0.5, tw::BLACK);
        let _ = color("white");
        // `.chat-markdown > :first-child { margin-top: 0 }` and last-child bottom.
        div()
            .id(self.id)
            .w_full()
            .min_w_0()
            .text_size(rem * 0.875)
            .text_color(base_color)
            .child(content.element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_a_stale_highlight_completion() {
        assert!(!accept_highlight(Some(2), 1));
        assert!(accept_highlight(Some(2), 2));
        assert!(!accept_highlight(None, 2));
    }

    #[test]
    fn fence_titles() {
        assert_eq!(extract_fence_title("title=\"x.ts\""), Some("x.ts".into()));
        assert_eq!(
            extract_fence_title("src/main.ts"),
            Some("src/main.ts".into())
        );
        assert_eq!(extract_fence_title("{1,3}"), None);
    }

    #[test]
    fn list_markers() {
        assert_eq!(list_marker_ordered(3, 1), "3.");
        assert_eq!(list_marker_ordered(28, 2), "ab.");
        assert_eq!(list_marker_ordered(4, 3), "iv.");
        assert_eq!(list_marker_bullet(2), "◦");
    }

    #[test]
    fn tabs_expand_to_eight_columns() {
        assert_eq!(expand_tabs("a\tb"), "a       b");
        let spans = vec![FlowSpan {
            len: 3,
            style: FlowSpanStyle {
                family: "x".into(),
                weight: FontWeight::NORMAL,
                italic: false,
                size: px(13.),
                line_height: 1.375,
                color: gpui_kit::black(),
                strikethrough: false,
                chrome: None,
                link: None,
                background: None,
                atomic: false,
            },
        }];
        assert_eq!(retarget_spans_for_tabs("a\tb", spans)[0].len, 9);
    }
}
