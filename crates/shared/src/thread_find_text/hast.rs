//! The slice of hast that thread find reads, plus the three rehype steps it applies to assistant
//! Markdown: `rehypePreserveBareAnchorPlaceholders`, `rehype-raw` and `rehype-sanitize`
//! (upstream/packages/shared/src/markdownPipeline.ts `CHAT_MARKDOWN_REHYPE_PLUGINS`).
//!
//! `rehype-raw` feeds the whole tree to parse5, so a raw `<details>` in one node can wrap the
//! Markdown paragraphs that follow it until a later raw `</details>`. The same result comes from
//! serializing the tree to HTML and parsing it as a fragment in a `<template>` context, which is
//! what parse5's fragment parser does for `rehype-raw`; html5ever is that parser here.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{
    Attribute, ExpandedName, ParseOpts, QualName, local_name, namespace_url, ns, parse_fragment,
};
use regex::Regex;

/// A hast node. `Raw` is an unparsed HTML string (`allowDangerousHtml`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hast {
    Text(String),
    Raw(String),
    Element(Element),
}

/// A hast element. Only the properties thread find reads are kept, under hast's camelCase names
/// (`href`, `src`, `dataInlineCode` and the `dataCodex...` artifact template properties).
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Element {
    pub tag: String,
    pub props: BTreeMap<String, String>,
    pub children: Vec<Hast>,
}

impl Element {
    pub(crate) fn new(tag: &str, children: Vec<Hast>) -> Self {
        Self {
            tag: tag.to_owned(),
            props: BTreeMap::new(),
            children,
        }
    }

    pub(crate) fn with_prop(mut self, name: &str, value: impl Into<String>) -> Self {
        self.props.insert(name.to_owned(), value.into());
        self
    }
}

// ---------------------------------------------------------------------------
// rehypePreserveBareAnchorPlaceholders
// ---------------------------------------------------------------------------

static RAW_TAGS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<!--[\s\S]*?(?:-->|$)|</?[A-Za-z](?:[^"'<>]|"[^"]*"|'[^']*')*>"#)
        .expect("valid regex")
});
static CLOSING_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^</([a-z]+)\s*>$").expect("valid regex"));
static OPENING_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<([a-z]+)(?:\s|/?>)").expect("valid regex"));
static RAW_TEXT_ELEMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:script|style|textarea|title|xmp|iframe|noembed|noframes|plaintext)$")
        .expect("valid regex")
});
static BARE_ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<a\s*/?>$").expect("valid regex"));

#[derive(Default)]
struct AnchorScan {
    /// One entry per `<a>` still open: the raw node's index when it is a bare placeholder.
    anchors: Vec<Option<usize>>,
    raw_text_tag: Option<String>,
    raw_index: usize,
}

fn scan_raw_node(value: &str, scan: &mut AnchorScan) {
    let index = scan.raw_index;
    scan.raw_index += 1;
    let mut offset = 0;
    while scan.raw_text_tag.as_deref() != Some("plaintext") {
        // Raw text ends at its closing tag even inside comment-looking text.
        let found = match &scan.raw_text_tag {
            Some(tag) => {
                let closer =
                    Regex::new(&format!(r"(?i)</{}\s*>", regex::escape(tag))).expect("valid regex");
                closer.find_at(value, offset).map(|m| (m.start(), m.end()))
            }
            None => RAW_TAGS
                .find_at(value, offset)
                .map(|m| (m.start(), m.end())),
        };
        let Some((start, end)) = found else { break };
        let tag = &value[start..end];
        offset = end;
        if scan.raw_text_tag.take().is_some() {
            continue;
        }
        if tag.starts_with("<!--") {
            continue;
        }
        let closing = CLOSING_TAG
            .captures(tag)
            .map(|captures| captures[1].to_ascii_lowercase());
        let opening = OPENING_TAG
            .captures(tag)
            .map(|captures| captures[1].to_ascii_lowercase());
        if let Some(opening) = opening
            .as_deref()
            .filter(|name| RAW_TEXT_ELEMENT.is_match(name))
        {
            scan.raw_text_tag = Some(opening.to_owned());
        } else if opening.as_deref() == Some("a") {
            scan.anchors
                .push((value == tag && BARE_ANCHOR.is_match(tag)).then_some(index));
        } else if closing.as_deref() == Some("a") {
            scan.anchors.pop();
        }
        if offset >= value.len() {
            break;
        }
    }
}

fn scan_raw_nodes(nodes: &[Hast], scan: &mut AnchorScan) {
    for node in nodes {
        match node {
            Hast::Raw(value) => scan_raw_node(value, scan),
            Hast::Element(element) => scan_raw_nodes(&element.children, scan),
            Hast::Text(_) => {}
        }
    }
}

fn convert_raw_nodes(nodes: &mut [Hast], to_text: &[usize], counter: &mut usize) {
    for node in nodes {
        match node {
            Hast::Raw(value) => {
                if to_text.contains(counter) {
                    *node = Hast::Text(std::mem::take(value));
                }
                *counter += 1;
            }
            Hast::Element(element) => convert_raw_nodes(&mut element.children, to_text, counter),
            Hast::Text(_) => {}
        }
    }
}

/// Keeps unmatched inline `<A>` placeholders from opening an HTML link over later blocks: a raw
/// node that is only `<a>` and is never closed becomes text.
pub(crate) fn preserve_bare_anchor_placeholders(nodes: &mut [Hast]) {
    let mut scan = AnchorScan::default();
    scan_raw_nodes(nodes, &mut scan);
    let to_text: Vec<usize> = scan.anchors.into_iter().flatten().collect();
    if !to_text.is_empty() {
        convert_raw_nodes(nodes, &to_text, &mut 0);
    }
}

// ---------------------------------------------------------------------------
// rehype-raw
// ---------------------------------------------------------------------------

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "image", "img",
    "input", "keygen", "link", "meta", "param", "source", "track", "wbr",
];

fn attribute_name(property: &str) -> String {
    // hast `dataFooBar` is the attribute `data-foo-bar`.
    if let Some(rest) = property
        .strip_prefix("data")
        .filter(|rest| rest.chars().next().is_some_and(char::is_uppercase))
    {
        let mut name = String::from("data");
        for ch in rest.chars() {
            if ch.is_uppercase() {
                name.push('-');
                name.extend(ch.to_lowercase());
            } else {
                name.push(ch);
            }
        }
        return name;
    }
    property.to_owned()
}

fn property_name(attribute: &str) -> String {
    // `data-foo-bar` is the hast property `dataFooBar`.
    if let Some(rest) = attribute.strip_prefix("data-") {
        let mut name = String::from("data");
        let mut upper = true;
        for ch in rest.chars() {
            if ch == '-' {
                upper = true;
            } else if upper {
                name.extend(ch.to_uppercase());
                upper = false;
            } else {
                name.push(ch);
            }
        }
        return name;
    }
    attribute.to_owned()
}

fn push_escaped(out: &mut String, text: &str, attribute: bool) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' if !attribute => out.push_str("&lt;"),
            '>' if !attribute => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
}

fn serialize(nodes: &[Hast], out: &mut String) {
    for node in nodes {
        match node {
            Hast::Text(text) => push_escaped(out, text, false),
            Hast::Raw(raw) => out.push_str(raw),
            Hast::Element(element) => {
                out.push('<');
                out.push_str(&element.tag);
                for (name, value) in &element.props {
                    out.push(' ');
                    out.push_str(&attribute_name(name));
                    out.push_str("=\"");
                    push_escaped(out, value, true);
                    out.push('"');
                }
                out.push('>');
                if !VOID_ELEMENTS.contains(&element.tag.as_str()) {
                    serialize(&element.children, out);
                    out.push_str("</");
                    out.push_str(&element.tag);
                    out.push('>');
                }
            }
        }
    }
}

enum DomKind {
    Document,
    Element {
        name: QualName,
        attrs: Vec<(String, String)>,
        template_contents: Option<usize>,
    },
    Text(String),
    Other,
}

struct DomNode {
    kind: DomKind,
    parent: Option<usize>,
    children: Vec<usize>,
}

struct Dom {
    nodes: Vec<DomNode>,
}

impl Dom {
    fn new() -> Self {
        Self {
            nodes: vec![DomNode {
                kind: DomKind::Document,
                parent: None,
                children: Vec::new(),
            }],
        }
    }

    fn add(&mut self, kind: DomKind) -> usize {
        self.nodes.push(DomNode {
            kind,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    fn detach(&mut self, node: usize) {
        if let Some(parent) = self.nodes[node].parent.take() {
            self.nodes[parent].children.retain(|child| *child != node);
        }
    }

    fn last_text_child(&self, parent: usize) -> Option<usize> {
        let last = *self.nodes[parent].children.last()?;
        matches!(self.nodes[last].kind, DomKind::Text(_)).then_some(last)
    }

    fn append_child(&mut self, parent: usize, child: NodeOrText<usize>) {
        match child {
            NodeOrText::AppendText(text) => {
                if let Some(last) = self.last_text_child(parent) {
                    if let DomKind::Text(existing) = &mut self.nodes[last].kind {
                        existing.push_str(&text);
                    }
                } else {
                    let node = self.add(DomKind::Text(text.to_string()));
                    self.nodes[node].parent = Some(parent);
                    self.nodes[parent].children.push(node);
                }
            }
            NodeOrText::AppendNode(node) => {
                self.detach(node);
                self.nodes[node].parent = Some(parent);
                self.nodes[parent].children.push(node);
            }
        }
    }
}

impl TreeSink for Dom {
    type Handle = usize;
    type Output = Dom;

    fn finish(self) -> Dom {
        self
    }

    fn parse_error(&mut self, _msg: Cow<'static, str>) {}

    fn get_document(&mut self) -> usize {
        0
    }

    fn elem_name<'a>(&'a self, target: &'a usize) -> ExpandedName<'a> {
        match &self.nodes[*target].kind {
            DomKind::Element { name, .. } => name.expanded(),
            _ => panic!("not an element"),
        }
    }

    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> usize {
        let template_contents = flags.template.then(|| self.add(DomKind::Document));
        let attrs = attrs
            .into_iter()
            .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
            .collect();
        self.add(DomKind::Element {
            name,
            attrs,
            template_contents,
        })
    }

    fn create_comment(&mut self, _text: StrTendril) -> usize {
        self.add(DomKind::Other)
    }

    fn create_pi(&mut self, _target: StrTendril, _data: StrTendril) -> usize {
        self.add(DomKind::Other)
    }

    fn append(&mut self, parent: &usize, child: NodeOrText<usize>) {
        self.append_child(*parent, child);
    }

    fn append_based_on_parent_node(
        &mut self,
        element: &usize,
        prev_element: &usize,
        child: NodeOrText<usize>,
    ) {
        if self.nodes[*element].parent.is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append_child(*prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &mut self,
        _name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
    }

    fn get_template_contents(&mut self, target: &usize) -> usize {
        match &self.nodes[*target].kind {
            DomKind::Element {
                template_contents: Some(contents),
                ..
            } => *contents,
            _ => panic!("not a template element"),
        }
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn set_quirks_mode(&mut self, _mode: QuirksMode) {}

    fn append_before_sibling(&mut self, sibling: &usize, new_node: NodeOrText<usize>) {
        let Some(parent) = self.nodes[*sibling].parent else {
            return;
        };
        let new_id = match new_node {
            NodeOrText::AppendNode(node) => {
                self.detach(node);
                node
            }
            NodeOrText::AppendText(text) => {
                let position = self.nodes[parent]
                    .children
                    .iter()
                    .position(|child| child == sibling)
                    .unwrap_or(0);
                if position > 0 {
                    let previous = self.nodes[parent].children[position - 1];
                    if let DomKind::Text(existing) = &mut self.nodes[previous].kind {
                        existing.push_str(&text);
                        return;
                    }
                }
                self.add(DomKind::Text(text.to_string()))
            }
        };
        let position = self.nodes[parent]
            .children
            .iter()
            .position(|child| child == sibling)
            .unwrap_or(0);
        self.nodes[new_id].parent = Some(parent);
        self.nodes[parent].children.insert(position, new_id);
    }

    fn add_attrs_if_missing(&mut self, target: &usize, attrs: Vec<Attribute>) {
        if let DomKind::Element {
            attrs: existing, ..
        } = &mut self.nodes[*target].kind
        {
            for attr in attrs {
                let name = attr.name.local.to_string();
                if !existing.iter().any(|(known, _)| *known == name) {
                    existing.push((name, attr.value.to_string()));
                }
            }
        }
    }

    fn remove_from_parent(&mut self, target: &usize) {
        self.detach(*target);
    }

    fn reparent_children(&mut self, node: &usize, new_parent: &usize) {
        let children = std::mem::take(&mut self.nodes[*node].children);
        for child in children {
            self.nodes[child].parent = Some(*new_parent);
            self.nodes[*new_parent].children.push(child);
        }
    }
}

fn dom_to_hast(dom: &Dom, node: usize, out: &mut Vec<Hast>) {
    match &dom.nodes[node].kind {
        DomKind::Text(text) => out.push(Hast::Text(text.clone())),
        DomKind::Element { name, attrs, .. } => {
            let mut element = Element::new(&name.local, Vec::new());
            for (attribute, value) in attrs {
                let property = property_name(attribute);
                if matches!(property.as_str(), "href" | "src" | "type")
                    || property.starts_with("data")
                {
                    element.props.insert(property, value.clone());
                }
            }
            // A `<template>`'s contents live outside its children, and sanitizing drops it.
            if &*name.local != "template" {
                for child in &dom.nodes[node].children {
                    dom_to_hast(dom, *child, &mut element.children);
                }
            }
            out.push(Hast::Element(element));
        }
        DomKind::Document | DomKind::Other => {}
    }
}

/// `rehype-raw`: parses every `Raw` node, in the context of the elements around it.
pub(crate) fn parse_raw(nodes: &[Hast]) -> Vec<Hast> {
    if !contains_raw(nodes) {
        return nodes.to_vec();
    }
    let mut html = String::new();
    serialize(nodes, &mut html);
    let context = QualName::new(None, ns!(html), local_name!("template"));
    // hast-util-raw parses with `scriptingEnabled: false`, so `<noscript>` content is markup.
    let options = ParseOpts {
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };
    let dom = parse_fragment(Dom::new(), options, context, Vec::new()).one(html);
    // The fragment algorithm roots the result under a synthetic `html` element.
    let mut out = Vec::new();
    if let Some(root) = dom.nodes[0].children.first() {
        for child in &dom.nodes[*root].children {
            dom_to_hast(&dom, *child, &mut out);
        }
    }
    out
}

fn contains_raw(nodes: &[Hast]) -> bool {
    nodes.iter().any(|node| match node {
        Hast::Raw(_) => true,
        Hast::Element(element) => contains_raw(&element.children),
        Hast::Text(_) => false,
    })
}

// ---------------------------------------------------------------------------
// rehype-sanitize (CHAT_MARKDOWN_SANITIZE_SCHEMA)
// ---------------------------------------------------------------------------

const TAG_NAMES: &[&str] = &[
    "a",
    "b",
    "blockquote",
    "br",
    "code",
    "dd",
    "del",
    "details",
    "div",
    "dl",
    "dt",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "img",
    "input",
    "ins",
    "kbd",
    "li",
    "ol",
    "p",
    "picture",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "section",
    "source",
    "span",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "tt",
    "ul",
    "var",
];

/// `schema.ancestors`: these only survive inside a `table`.
const TABLE_ONLY: &[&str] = &["tbody", "td", "th", "thead", "tfoot", "tr"];

const HREF_PROTOCOLS: &[&str] = &[
    "http",
    "https",
    "irc",
    "ircs",
    "mailto",
    "xmpp",
    "file",
    "t3-citation",
    "t3-context",
    crate::thread_links::THREAD_LINK_PROTOCOL,
];
const SRC_PROTOCOLS: &[&str] = &["http", "https", "file", "t3-context"];

const ARTIFACT_TEMPLATE_PROPERTIES: &[&str] = &[
    "dataCodexArtifactTemplate",
    "dataArtifactKind",
    "dataDisplayName",
    "dataGalleryKind",
    "dataSkillDirectory",
    "dataSkillName",
];

fn safe_protocol(value: &str, protocols: &[&str]) -> bool {
    let colon = value.find(':');
    let Some(colon) = colon else { return true };
    let before = |marker: char| value.find(marker).is_some_and(|index| colon > index);
    if before('/') || before('?') || before('#') {
        return true;
    }
    protocols.contains(&&value[..colon])
}

fn property_allowed(tag: &str, name: &str, value: &str) -> bool {
    match (tag, name) {
        ("a", "href") => safe_protocol(value, HREF_PROTOCOLS),
        ("img", "src") => safe_protocol(value, SRC_PROTOCOLS),
        ("code", "dataInlineCode") => true,
        ("div", name) => ARTIFACT_TEMPLATE_PROPERTIES.contains(&name),
        _ => false,
    }
}

fn sanitize_children(children: Vec<Hast>, stack: &mut Vec<String>) -> Vec<Hast> {
    let mut out = Vec::with_capacity(children.len());
    for child in children {
        match child {
            Hast::Text(text) => out.push(Hast::Text(text)),
            // `transform` has no case for `raw`, so anything unparsed is dropped.
            Hast::Raw(_) => {}
            Hast::Element(element) => {
                out.extend(sanitize_element(element, stack));
            }
        }
    }
    out
}

fn sanitize_element(element: Element, stack: &mut Vec<String>) -> Vec<Hast> {
    let Element {
        tag,
        props,
        children,
    } = element;
    stack.push(tag.clone());
    let content = sanitize_children(children, stack);
    stack.pop();
    let mut safe = TAG_NAMES.contains(&tag.as_str());
    if safe && TABLE_ONLY.contains(&tag.as_str()) {
        safe = stack.iter().any(|ancestor| ancestor == "table");
    }
    if !safe {
        // `strip: ["script"]` removes the element with its content; others keep their children.
        return if tag == "script" { Vec::new() } else { content };
    }
    let props = props
        .into_iter()
        .filter(|(name, value)| property_allowed(&tag, name, value))
        .collect();
    vec![Hast::Element(Element {
        tag,
        props,
        children: content,
    })]
}

/// `rehypeSanitize(CHAT_MARKDOWN_SANITIZE_SCHEMA)`, reduced to what thread find reads.
pub(crate) fn sanitize(nodes: Vec<Hast>) -> Vec<Hast> {
    sanitize_children(nodes, &mut Vec::new())
}
