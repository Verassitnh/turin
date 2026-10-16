//! `remark-parse` + `CHAT_MARKDOWN_REMARK_PLUGINS` + `remark-rehype` for thread find, over
//! markdown-rs (the Rust port of micromark/mdast).
//!
//! Plugins and what they do here, in upstream order:
//! - `remarkGfm`: markdown-rs' GFM constructs.
//! - `remarkKeepWindowsPathDestinations`: a link, image or definition destination that is a
//!   Windows path keeps its authored text ([`authored_destination`]).
//! - `remarkGithubAlerts`: [`crate::markdown_github_alerts`] drops the `[!NOTE]` marker line.
//! - `remarkNormalizeListItemIndentation`: [`crate::markdown_list_indentation`].
//! - `remarkCodexDirectives`: done on the source before parsing (see `thread_find_text`), which
//!   leaves artifact template cards as headings holding a placeholder token, turned into
//!   `div[data-codex-artifact-template]` here.
//! - `remarkBreaks` (when `line_breaks`): [`newline_to_break`].
//! - `remarkNormalizeLinksAndTagInlineCode`: Windows drive links become `file:` URLs and inline
//!   code outside a link gets `dataInlineCode`.
//!
//! `remark-rehype` (`mdast-util-to-hast`) follows the upstream handlers exactly, including the
//! `"\n"` text nodes `wrap` inserts between blocks: thread find segments include the space they
//! become.

use std::collections::HashMap;

use markdown::mdast::{self, Node, ReferenceKind};
use markdown::unist::Position;
use markdown::{Constructs, ParseOptions};

use super::hast::{Element, Hast};
use crate::codex_artifact_templates::CodexArtifactTemplate;
use crate::js_string::encode_uri_component;

pub(super) fn parse_options() -> ParseOptions {
    ParseOptions {
        constructs: Constructs {
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    }
}

/// An artifact template placeholder: a heading holding this text stands for the card.
pub(super) fn template_token(index: usize) -> String {
    format!("\u{E000}t3-artifact-template-{index}\u{E001}")
}

// ---------------------------------------------------------------------------
// mdast transforms
// ---------------------------------------------------------------------------

/// `remarkBreaks`: every line ending in text becomes a `break`.
pub(super) fn newline_to_break(node: &mut Node) {
    let Some(children) = node.children_mut() else {
        return;
    };
    let old = std::mem::take(children);
    let mut next = Vec::with_capacity(old.len());
    for mut child in old {
        match child {
            Node::Text(text) if text.value.contains(['\n', '\r']) => {
                let value = text.value;
                let mut start = 0;
                let mut index = 0;
                let bytes = value.as_bytes();
                let push_text = |next: &mut Vec<Node>, piece: &str| {
                    if !piece.is_empty() {
                        next.push(Node::Text(mdast::Text {
                            value: piece.to_owned(),
                            position: None,
                        }));
                    }
                };
                while index < bytes.len() {
                    if bytes[index] == b'\n' || bytes[index] == b'\r' {
                        push_text(&mut next, &value[start..index]);
                        next.push(Node::Break(mdast::Break { position: None }));
                        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                            index += 1;
                        }
                        start = index + 1;
                    }
                    index += 1;
                }
                push_text(&mut next, &value[start..]);
            }
            other => {
                child = other;
                newline_to_break(&mut child);
                next.push(child);
            }
        }
    }
    *node.children_mut().expect("had children") = next;
}

// ---------------------------------------------------------------------------
// Link destinations
// ---------------------------------------------------------------------------

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_none_or(|&b| b == b'/' || b == b'\\');
    drive || value.starts_with("\\\\")
}

/// `keepWindowsPathDestination`: a Windows path destination keeps its authored text. A
/// destination with `&` keeps the decoded URL, since it may hold character references.
fn keep_windows_path_destination(authored: Option<&str>, decoded: &str) -> String {
    match authored {
        Some(authored) if is_windows_absolute_path(authored) && !authored.contains('&') => {
            authored.to_owned()
        }
        _ => decoded.to_owned(),
    }
}

/// A destination at the start of `rest` (after optional white space): either `<...>` or a bare
/// run that ends at white space or an unmatched `)`.
fn destination_at(rest: &str) -> Option<&str> {
    let rest = rest.trim_start_matches([' ', '\t', '\n', '\r']);
    if let Some(inner) = rest.strip_prefix('<') {
        return inner.split_once('>').map(|(destination, _)| destination);
    }
    let mut depth = 0usize;
    let mut end = 0;
    for (index, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ch if ch.is_whitespace() => break,
            _ => {}
        }
        end = index + ch.len_utf8();
    }
    Some(&rest[..end])
}

/// The destination of a link or image as written between `](` and the closing `)`.
fn authored_destination<'s>(
    source: &'s str,
    position: &Position,
    label: &[Node],
) -> Option<&'s str> {
    let label_end = label
        .last()
        .and_then(Node::position)
        .map_or(position.start.offset + 1, |end| end.end.offset);
    let link_source = source.get(label_end..position.end.offset)?;
    let open = link_source.find("](")? + 2;
    destination_at(&link_source[open..])
}

/// The destination of a definition, as written after its `]:`.
fn authored_definition_destination<'s>(source: &'s str, position: &Position) -> Option<&'s str> {
    let definition_source = source.get(position.start.offset..position.end.offset)?;
    let open = definition_source.find("]:")? + 2;
    destination_at(&definition_source[open..])
}

/// `remarkNormalizeLinksAndTagInlineCode`: Windows drive links become `file:` URLs.
fn normalize_drive_link(url: &str) -> String {
    let bytes = url.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return format!("file:///{}", url.replace('\\', "/"));
    }
    url.to_owned()
}

/// `normalizeUri` (micromark-util-sanitize-uri): percent-encodes what a URL cannot hold, leaving
/// existing escapes alone.
pub(super) fn normalize_uri(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '%'
            && chars
                .get(index + 1)
                .is_some_and(char::is_ascii_alphanumeric)
            && chars
                .get(index + 2)
                .is_some_and(char::is_ascii_alphanumeric)
        {
            out.push('%');
            out.push(chars[index + 1]);
            out.push(chars[index + 2]);
            index += 3;
            continue;
        }
        let keep = ch.is_ascii()
            && matches!(ch,
                '!' | '#' | '$' | '&'..=';' | '=' | '?'..='Z' | '_' | 'a'..='z' | '~');
        if keep {
            out.push(ch);
        } else {
            out.push_str(&encode_uri_component(&ch.to_string()));
        }
        index += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// mdast -> hast
// ---------------------------------------------------------------------------

fn trim_markdown_space_start(value: &str) -> &str {
    value.trim_start_matches([' ', '\t'])
}

/// `trimLines`: removes spaces and tabs around line endings.
fn trim_lines(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut last = 0;
    let mut index = 0;
    let trim_line = |line: &str, start: bool, end: bool| -> String {
        let mut slice = line;
        if start {
            slice = slice.trim_start_matches([' ', '\t']);
        }
        if end {
            slice = slice.trim_end_matches([' ', '\t']);
        }
        slice.to_owned()
    };
    while index < bytes.len() {
        if bytes[index] == b'\n' || bytes[index] == b'\r' {
            let mut ending_end = index + 1;
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                ending_end += 1;
            }
            out.push_str(&trim_line(&value[last..index], last > 0, true));
            out.push_str(&value[index..ending_end]);
            last = ending_end;
            index = ending_end;
            continue;
        }
        index += 1;
    }
    out.push_str(&trim_line(&value[last..], last > 0, false));
    out
}

fn text(value: impl Into<String>) -> Hast {
    Hast::Text(value.into())
}

/// `state.wrap`: line endings between nodes, and around them when `loose`.
fn wrap(nodes: Vec<Hast>, loose: bool) -> Vec<Hast> {
    let mut out = Vec::with_capacity(nodes.len() * 2 + 2);
    if loose {
        out.push(text("\n"));
    }
    let empty = nodes.is_empty();
    for (index, node) in nodes.into_iter().enumerate() {
        if index > 0 {
            out.push(text("\n"));
        }
        out.push(node);
    }
    if loose && !empty {
        out.push(text("\n"));
    }
    out
}

fn identifier_key(identifier: &str) -> String {
    identifier.to_uppercase()
}

struct Definition {
    url: String,
}

pub(super) struct ToHast<'a> {
    source: &'a str,
    templates: &'a [CodexArtifactTemplate],
    definitions: HashMap<String, Definition>,
    footnotes: HashMap<String, &'a [Node]>,
    footnote_order: Vec<String>,
    footnote_counts: HashMap<String, usize>,
}

pub(super) fn to_hast(root: &Node, source: &str, templates: &[CodexArtifactTemplate]) -> Vec<Hast> {
    let mut state = ToHast {
        source,
        templates,
        definitions: HashMap::new(),
        footnotes: HashMap::new(),
        footnote_order: Vec::new(),
        footnote_counts: HashMap::new(),
    };
    state.collect_definitions(root);
    let children = root.children().map(Vec::as_slice).unwrap_or_default();
    let mut result = wrap(state.all(children, false), false);
    if let Some(footer) = state.footer() {
        result.push(text("\n"));
        result.push(footer);
    }
    result
}

impl<'a> ToHast<'a> {
    fn collect_definitions(&mut self, node: &'a Node) {
        match node {
            Node::Definition(definition) => {
                let authored = definition
                    .position
                    .as_ref()
                    .and_then(|position| authored_definition_destination(self.source, position));
                let url =
                    normalize_drive_link(&keep_windows_path_destination(authored, &definition.url));
                self.definitions
                    .entry(identifier_key(&definition.identifier))
                    .or_insert(Definition { url });
            }
            Node::FootnoteDefinition(definition) => {
                self.footnotes
                    .entry(identifier_key(&definition.identifier))
                    .or_insert(definition.children.as_slice());
            }
            _ => {}
        }
        if let Some(children) = node.children() {
            for child in children {
                self.collect_definitions(child);
            }
        }
    }

    /// `state.all`.
    fn all(&mut self, children: &'a [Node], inside_link: bool) -> Vec<Hast> {
        let mut values = Vec::new();
        for (index, child) in children.iter().enumerate() {
            let mut result = self.one(child, inside_link);
            if index > 0 && matches!(children[index - 1], Node::Break(_)) && result.len() == 1 {
                match &mut result[0] {
                    Hast::Text(value) => *value = trim_markdown_space_start(value).to_owned(),
                    Hast::Element(element) => {
                        if let Some(Hast::Text(value)) = element.children.first_mut() {
                            *value = trim_markdown_space_start(value).to_owned();
                        }
                    }
                    Hast::Raw(_) => {}
                }
            }
            values.extend(result);
        }
        values
    }

    fn element(&mut self, tag: &str, children: &'a [Node], inside_link: bool) -> Hast {
        Hast::Element(Element::new(tag, self.all(children, inside_link)))
    }

    fn one(&mut self, node: &'a Node, inside_link: bool) -> Vec<Hast> {
        match node {
            Node::Root(root) => wrap(self.all(&root.children, inside_link), false),
            Node::Paragraph(paragraph) => vec![self.element("p", &paragraph.children, inside_link)],
            Node::Heading(heading) => {
                if let Some(template) = self.template_for(&heading.children) {
                    return vec![template];
                }
                vec![self.element(
                    &format!("h{}", heading.depth),
                    &heading.children,
                    inside_link,
                )]
            }
            Node::ThematicBreak(_) => vec![Hast::Element(Element::new("hr", Vec::new()))],
            Node::Blockquote(quote) => {
                let children = self.all(&quote.children, inside_link);
                vec![Hast::Element(Element::new(
                    "blockquote",
                    wrap(children, true),
                ))]
            }
            Node::Code(code) => {
                let value = if code.value.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", code.value)
                };
                let code_element = Element::new("code", vec![text(value)]);
                vec![Hast::Element(Element::new(
                    "pre",
                    vec![Hast::Element(code_element)],
                ))]
            }
            Node::InlineCode(code) => {
                let value = code.value.replace("\r\n", " ").replace(['\r', '\n'], " ");
                let mut element = Element::new("code", vec![text(value)]);
                if !inside_link {
                    element = element.with_prop("dataInlineCode", "");
                }
                vec![Hast::Element(element)]
            }
            Node::Text(value) => vec![text(trim_lines(&value.value))],
            Node::Emphasis(emphasis) => vec![self.element("em", &emphasis.children, inside_link)],
            Node::Strong(strong) => vec![self.element("strong", &strong.children, inside_link)],
            Node::Delete(delete) => vec![self.element("del", &delete.children, inside_link)],
            Node::Break(_) => vec![Hast::Element(Element::new("br", Vec::new())), text("\n")],
            Node::Html(html) => vec![Hast::Raw(html.value.clone())],
            Node::Link(link) => {
                let authored = link.position.as_ref().and_then(|position| {
                    authored_destination(self.source, position, &link.children)
                });
                let url = normalize_drive_link(&keep_windows_path_destination(authored, &link.url));
                let element = Element::new("a", self.all(&link.children, true))
                    .with_prop("href", normalize_uri(&url));
                vec![Hast::Element(element)]
            }
            Node::LinkReference(reference) => {
                let Some(definition) = self.definitions.get(&identifier_key(&reference.identifier))
                else {
                    return self.revert_link_reference(reference);
                };
                let href = normalize_uri(&definition.url);
                let element =
                    Element::new("a", self.all(&reference.children, true)).with_prop("href", href);
                vec![Hast::Element(element)]
            }
            Node::Image(image) => {
                let authored = image
                    .position
                    .as_ref()
                    .and_then(|position| authored_destination(self.source, position, &[]));
                let url = keep_windows_path_destination(authored, &image.url);
                vec![Hast::Element(
                    Element::new("img", Vec::new()).with_prop("src", normalize_uri(&url)),
                )]
            }
            Node::ImageReference(reference) => {
                let Some(definition) = self.definitions.get(&identifier_key(&reference.identifier))
                else {
                    return vec![text(format!(
                        "![{}{}",
                        reference.alt,
                        reference_suffix(
                            reference.reference_kind,
                            reference.label.as_deref(),
                            &reference.identifier
                        )
                    ))];
                };
                vec![Hast::Element(
                    Element::new("img", Vec::new())
                        .with_prop("src", normalize_uri(&definition.url)),
                )]
            }
            Node::FootnoteReference(reference) => vec![self.footnote_reference(reference)],
            Node::List(list) => self.list(list, inside_link),
            Node::ListItem(item) => self.list_item(item, None, inside_link),
            Node::Table(table) => vec![self.table(table, inside_link)],
            Node::TableRow(row) => vec![self.table_row(row, 1, None, inside_link)],
            Node::TableCell(cell) => vec![self.element("td", &cell.children, inside_link)],
            Node::Definition(_) | Node::FootnoteDefinition(_) => Vec::new(),
            Node::Yaml(_) | Node::Toml(_) => Vec::new(),
            // Constructs this build does not enable (math, MDX) never reach here; the default
            // handler would show their value.
            other => match other {
                Node::Math(math) => vec![text(math.value.clone())],
                _ => Vec::new(),
            },
        }
    }

    /// An artifact template placeholder heading as the card `div`.
    fn template_for(&self, children: &[Node]) -> Option<Hast> {
        let [Node::Text(only)] = children else {
            return None;
        };
        let index = (0..self.templates.len()).find(|index| only.value == template_token(*index))?;
        let template = &self.templates[index];
        let mut element = Element::new("div", Vec::new())
            .with_prop("dataCodexArtifactTemplate", "true")
            .with_prop("dataArtifactKind", template.artifact_kind.clone())
            .with_prop("dataDisplayName", template.display_name.clone())
            .with_prop("dataSkillDirectory", template.skill_directory.clone())
            .with_prop("dataSkillName", template.skill_name.clone());
        if let Some(gallery_kind) = &template.gallery_kind {
            element = element.with_prop("dataGalleryKind", gallery_kind.clone());
        }
        Some(Hast::Element(element))
    }

    /// `revert`: a reference without a definition reads as its source text.
    fn revert_link_reference(&mut self, reference: &'a mdast::LinkReference) -> Vec<Hast> {
        let suffix = reference_suffix(
            reference.reference_kind,
            reference.label.as_deref(),
            &reference.identifier,
        );
        let mut contents = self.all(&reference.children, true);
        match contents.first_mut() {
            Some(Hast::Text(value)) => value.insert(0, '['),
            _ => contents.insert(0, text("[")),
        }
        match contents.last_mut() {
            Some(Hast::Text(value)) => value.push_str(&suffix),
            _ => contents.push(text(suffix)),
        }
        contents
    }

    fn footnote_reference(&mut self, reference: &mdast::FootnoteReference) -> Hast {
        let id = identifier_key(&reference.identifier);
        let safe_id = normalize_uri(&id.to_lowercase());
        let position = self.footnote_order.iter().position(|known| *known == id);
        let seen = self.footnote_counts.get(&id).copied();
        let counter = if seen.is_none() {
            self.footnote_order.push(id.clone());
            self.footnote_order.len()
        } else {
            position.map_or(0, |index| index + 1)
        };
        self.footnote_counts.insert(id, seen.unwrap_or(0) + 1);
        let link = Element::new("a", vec![text(counter.to_string())])
            .with_prop("href", format!("#user-content-fn-{safe_id}"));
        Hast::Element(Element::new("sup", vec![Hast::Element(link)]))
    }

    fn list(&mut self, list: &'a mdast::List, inside_link: bool) -> Vec<Hast> {
        let loose = list.spread || list.children.iter().any(item_loose);
        let items: Vec<Hast> = list
            .children
            .iter()
            .flat_map(|child| match child {
                Node::ListItem(item) => self.list_item(item, Some(loose), inside_link),
                other => self.one(other, inside_link),
            })
            .collect();
        let tag = if list.ordered { "ol" } else { "ul" };
        vec![Hast::Element(Element::new(tag, wrap(items, true)))]
    }

    fn list_item(
        &mut self,
        item: &'a mdast::ListItem,
        list_loose: Option<bool>,
        inside_link: bool,
    ) -> Vec<Hast> {
        let mut results = self.all(&item.children, inside_link);
        let loose = list_loose.unwrap_or(item.spread);
        if let Some(checked) = item.checked {
            let _ = checked;
            let head_is_paragraph = matches!(
                results.first(),
                Some(Hast::Element(element)) if element.tag == "p"
            );
            if !head_is_paragraph {
                results.insert(0, Hast::Element(Element::new("p", Vec::new())));
            }
            if let Some(Hast::Element(paragraph)) = results.first_mut() {
                if !paragraph.children.is_empty() {
                    paragraph.children.insert(0, text(" "));
                }
                paragraph.children.insert(
                    0,
                    Hast::Element(Element::new("input", Vec::new()).with_prop("type", "checkbox")),
                );
            }
        }
        let mut children = Vec::new();
        let last = results.len().saturating_sub(1);
        let tail_is_paragraph = matches!(
            results.last(),
            Some(Hast::Element(element)) if element.tag == "p"
        );
        let empty = results.is_empty();
        for (index, child) in results.into_iter().enumerate() {
            let is_paragraph = matches!(&child, Hast::Element(element) if element.tag == "p");
            if loose || index != 0 || !is_paragraph {
                children.push(text("\n"));
            }
            match child {
                Hast::Element(element) if element.tag == "p" && !loose => {
                    children.extend(element.children);
                }
                other => children.push(other),
            }
            let _ = last;
        }
        if !empty && (loose || !tail_is_paragraph) {
            children.push(text("\n"));
        }
        vec![Hast::Element(Element::new("li", children))]
    }

    fn table(&mut self, table: &'a mdast::Table, inside_link: bool) -> Hast {
        let mut rows: Vec<Hast> = table
            .children
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                Node::TableRow(row) => {
                    Some(self.table_row(row, index, Some(table.align.len()), inside_link))
                }
                _ => None,
            })
            .collect();
        let mut content = Vec::new();
        if !rows.is_empty() {
            let first = rows.remove(0);
            content.push(Hast::Element(Element::new(
                "thead",
                wrap(vec![first], true),
            )));
        }
        if !rows.is_empty() {
            content.push(Hast::Element(Element::new("tbody", wrap(rows, true))));
        }
        Hast::Element(Element::new("table", wrap(content, true)))
    }

    fn table_row(
        &mut self,
        row: &'a mdast::TableRow,
        row_index: usize,
        align_length: Option<usize>,
        inside_link: bool,
    ) -> Hast {
        let tag = if row_index == 0 { "th" } else { "td" };
        let length = align_length.unwrap_or(row.children.len());
        let mut cells = Vec::with_capacity(length);
        for cell_index in 0..length {
            let mut element = Element::new(tag, Vec::new());
            if let Some(Node::TableCell(cell)) = row.children.get(cell_index) {
                element.children = self.all(&cell.children, inside_link);
            }
            cells.push(Hast::Element(element));
        }
        Hast::Element(Element::new("tr", wrap(cells, true)))
    }

    /// `footer`: the `section[data-footnotes]` with each referenced definition and its
    /// back-references.
    fn footer(&mut self) -> Option<Hast> {
        let mut list_items = Vec::new();
        let mut reference_index = 0;
        while reference_index < self.footnote_order.len() {
            let id = self.footnote_order[reference_index].clone();
            reference_index += 1;
            let Some(children) = self.footnotes.get(&id).copied() else {
                continue;
            };
            let mut content = self.all(children, false);
            let safe_id = normalize_uri(&id.to_lowercase());
            let counts = self.footnote_counts.get(&id).copied();
            let mut back_references = Vec::new();
            let mut rereference_index = 0;
            while counts.is_some_and(|counts| {
                rereference_index += 1;
                rereference_index <= counts
            }) {
                if !back_references.is_empty() {
                    back_references.push(text(" "));
                }
                let mut back = vec![text("↩")];
                if rereference_index > 1 {
                    back.push(Hast::Element(Element::new(
                        "sup",
                        vec![text(rereference_index.to_string())],
                    )));
                }
                let suffix = if rereference_index > 1 {
                    format!("-{rereference_index}")
                } else {
                    String::new()
                };
                back_references.push(Hast::Element(
                    Element::new("a", back)
                        .with_prop("href", format!("#user-content-fnref-{safe_id}{suffix}")),
                ));
            }
            match content.last_mut() {
                Some(Hast::Element(tail)) if tail.tag == "p" => {
                    match tail.children.last_mut() {
                        Some(Hast::Text(value)) => value.push(' '),
                        _ => tail.children.push(text(" ")),
                    }
                    tail.children.extend(back_references);
                }
                _ => content.extend(back_references),
            }
            list_items.push(Hast::Element(Element::new("li", wrap(content, true))));
        }
        if list_items.is_empty() {
            return None;
        }
        Some(Hast::Element(
            Element::new(
                "section",
                vec![
                    Hast::Element(Element::new("h2", vec![text("Footnotes")])),
                    text("\n"),
                    Hast::Element(Element::new("ol", wrap(list_items, true))),
                    text("\n"),
                ],
            )
            .with_prop("dataFootnotes", "true"),
        ))
    }
}

fn item_loose(node: &Node) -> bool {
    match node {
        Node::ListItem(item) => item.spread,
        _ => false,
    }
}

/// The text after a reference's `[text]`: `]` for a shortcut, `][]` for a collapsed reference,
/// `][label]` for a full one (`label || identifier`).
fn reference_suffix(kind: ReferenceKind, label: Option<&str>, identifier: &str) -> String {
    match kind {
        ReferenceKind::Shortcut => "]".to_owned(),
        ReferenceKind::Collapsed => "][]".to_owned(),
        ReferenceKind::Full => {
            let label = label
                .filter(|label| !label.is_empty())
                .unwrap_or(identifier);
            format!("][{label}]")
        }
    }
}
