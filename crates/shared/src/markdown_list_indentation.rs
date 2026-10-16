//! Port of upstream/packages/shared/src/markdownListIndentation.ts
//!
//! CommonMark treats four or more spaces after a list marker as an indented code block. In chat
//! output, excessive spacing is commonly accidental alignment such as `-       text`, which
//! otherwise produces a full code card for every bullet. Only normalize blocks that retain excess
//! indentation and start on the marker's own line; explicit fences and conventional indented
//! blocks remain code.
//!
//! Upstream re-parses with the processor's own parser; here the caller passes the parse function
//! it built the tree with.

use markdown::mdast::{self, Node};
use markdown::unist::Position;

use turin_contracts::base_schemas::js_trim;

/// Parses Markdown with the options the tree being normalized was parsed with.
pub type MarkdownParser<'a> = &'a dyn Fn(&str) -> Option<Node>;

const INLINE_PARSE_PREFIX: &str = "t3-markdown-inline-prefix:";

fn is_js_space_or_tab(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

fn is_same_line_over_indented_code(
    node: &Node,
    parent_position: Option<&Position>,
    source: &str,
) -> bool {
    let Node::Code(code) = node else { return false };
    if !code.value.starts_with(is_js_space_or_tab) {
        return false;
    }
    let (Some(node_position), Some(parent_position)) = (&code.position, parent_position) else {
        return false;
    };
    if node_position.start.line != parent_position.start.line {
        return false;
    }
    !matches!(
        source.as_bytes().get(node_position.start.offset),
        Some(b'`' | b'~')
    )
}

/// `parseRecoveredMarkdown`: the text is parsed behind a prefix, which forces block-looking input
/// into a paragraph while GFM inline syntax still parses. Later root children stay blocks.
fn parse_recovered_markdown(value: &str, parser: MarkdownParser<'_>) -> (Vec<Node>, String) {
    let source = format!("{INLINE_PARSE_PREFIX}{value}");
    let fallback = |source: String| {
        (
            vec![Node::Text(mdast::Text {
                value: value.to_owned(),
                position: None,
            })],
            source,
        )
    };
    let Some(Node::Root(root)) = parser(&source) else {
        return fallback(source);
    };
    let mut blocks = root.children;
    let Some(Node::Paragraph(paragraph)) = blocks.first_mut() else {
        return fallback(source);
    };
    let Some(Node::Text(first)) = paragraph.children.first_mut() else {
        return fallback(source);
    };
    let Some(first_value) = first.value.strip_prefix(INLINE_PARSE_PREFIX) else {
        return fallback(source);
    };
    let first_value = first_value.to_owned();
    if first_value.is_empty() {
        paragraph.children.remove(0);
    } else {
        first.value = first_value;
    }
    (blocks, source)
}

/// `remarkNormalizeListItemIndentation`: a code block that is only excess spacing after a list
/// marker (`-       text`) is prose, not a code card.
pub fn remark_normalize_list_item_indentation(
    node: &mut Node,
    source: &str,
    parser: MarkdownParser<'_>,
) {
    let parent_is_list_item = matches!(node, Node::ListItem(_));
    let parent_position = node.position().cloned();
    let Some(children) = node.children_mut() else {
        return;
    };
    let old = std::mem::take(children);
    let mut next = Vec::with_capacity(old.len());
    for mut child in old {
        if parent_is_list_item
            && is_same_line_over_indented_code(&child, parent_position.as_ref(), source)
        {
            let Node::Code(code) = &child else {
                unreachable!("checked above")
            };
            let code_position = code.position.clone();
            let (mut blocks, recovered_source) =
                parse_recovered_markdown(js_trim(&code.value), parser);
            if let (Some(first), Some(position)) = (blocks.first_mut(), code_position) {
                first.position_set(Some(position));
            }
            for mut block in blocks {
                remark_normalize_list_item_indentation(&mut block, &recovered_source, parser);
                next.push(block);
            }
        } else {
            remark_normalize_list_item_indentation(&mut child, source, parser);
            next.push(child);
        }
    }
    *node.children_mut().expect("had children") = next;
}
