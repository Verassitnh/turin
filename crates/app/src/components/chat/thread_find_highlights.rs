//! Port of the pure half of upstream/apps/web/src/components/chat/threadFindHighlights.ts:
//! `collectThreadFindRanges` (the text walk and the offset-to-range mapping), and the
//! fold rules that decide whether a match is hidden (`foldsHiding`, `revealFolded`).
//!
//! Upstream walks DOM text nodes and builds `Range`s. Here the caller passes the same
//! tree shape as `FindNode`s, one per outermost `[data-thread-find-text]` scope.
//! Offsets are UTF-16 code units, as in `threadSearch.ts`. The CSS Highlight painter,
//! the MutationObserver and the `beforematch` dispatch are DOM glue; see Deviations.
//!
//! Port of upstream/apps/web/src/components/chat/threadFindHighlights.ts (pure half).

use turin_shared::js_string::utf16_len;
use turin_shared::thread_search::find_thread_search_occurrences;

/// Block-level tags that end a searchable run of text (`threadFindText.ts`
/// `THREAD_FIND_BLOCK_TAGS`). Inline wrappers, including Shiki token spans, do not
/// split a search phrase.
const THREAD_FIND_BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "ul",
];

/// A rendered node as the find walk sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum FindNode {
    /// `ignored` is `[data-thread-find-ignore]` or `[role="toolbar"]`: its text is not
    /// searched and it ends the current run. `svg` is skipped entirely.
    Element {
        tag: String,
        ignored: bool,
        children: Vec<FindNode>,
    },
    Text(String),
}

/// One outermost searchable scope and the timeline row it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct FindScope {
    pub row_id: String,
    pub root: FindNode,
}

/// A text node's position in the walk. `node` numbers text nodes in document order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindTextPoint {
    pub node: usize,
    /// UTF-16 offset within that text node.
    pub offset: usize,
}

/// One occurrence: the same shape as upstream's `ThreadFindRange` (its `Range` becomes
/// start and end points).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadFindRange {
    pub row_id: String,
    pub occurrence: u64,
    pub start: FindTextPoint,
    pub end: FindTextPoint,
}

struct TextNode {
    node: usize,
    start: usize,
    end: usize,
}

/// Accumulates one run of text (between block boundaries) with the text nodes it spans.
#[derive(Default)]
struct Run {
    text: String,
    /// UTF-16 length of `text`, kept so each text node is O(1) to place.
    len: usize,
    nodes: Vec<TextNode>,
}

struct Walker {
    next_node: usize,
    runs: Vec<Run>,
    current: Run,
}

impl Walker {
    fn flush(&mut self) {
        let run = std::mem::take(&mut self.current);
        self.runs.push(run);
    }

    fn visit(&mut self, node: &FindNode, in_pre: bool) {
        match node {
            FindNode::Text(value) => {
                let node = self.next_node;
                self.next_node += 1;
                let start = self.current.len;
                // JS `value.replace(/\r?\n/g, " ")` outside `pre`.
                let appended = if in_pre {
                    value.clone()
                } else {
                    value.replace("\r\n", " ").replace('\n', " ")
                };
                self.current.len += utf16_len(&appended);
                self.current.text.push_str(&appended);
                let end = self.current.len;
                self.current.nodes.push(TextNode { node, start, end });
            }
            FindNode::Element {
                tag,
                ignored,
                children,
            } => {
                if tag == "svg" {
                    return;
                }
                if *ignored {
                    self.flush();
                    return;
                }
                let block = THREAD_FIND_BLOCK_TAGS.contains(&tag.as_str());
                if block {
                    self.flush();
                }
                let child_in_pre = in_pre || tag == "pre";
                for child in children {
                    self.visit(child, child_in_pre);
                }
                if block {
                    self.flush();
                }
            }
        }
    }
}

/// Collects visible occurrences without modifying rendered markdown. `scopes` are the
/// outermost searchable scopes in document order; a nested scope belongs to its
/// outer one and is not passed separately.
pub fn collect_thread_find_ranges(scopes: &[FindScope], query: &str) -> Vec<ThreadFindRange> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_len = utf16_len(query);
    let mut ranges = Vec::new();
    let mut occurrence_by_row_id = std::collections::HashMap::<&str, u64>::new();

    for scope in scopes {
        let mut walker = Walker {
            next_node: 0,
            runs: Vec::new(),
            current: Run::default(),
        };
        walker.visit(&scope.root, false);
        walker.flush();
        for run in walker.runs {
            push_run_ranges(
                &run,
                query,
                query_len,
                &scope.row_id,
                &mut occurrence_by_row_id,
                &mut ranges,
            );
        }
    }
    ranges
}

fn push_run_ranges<'a>(
    run: &Run,
    query: &str,
    query_len: usize,
    row_id: &'a str,
    occurrence_by_row_id: &mut std::collections::HashMap<&'a str, u64>,
    ranges: &mut Vec<ThreadFindRange>,
) {
    // Offsets ascend, so both node cursors only move forward: linear in text nodes.
    let mut start_index = 0;
    let mut end_index = 0;
    for offset in find_thread_search_occurrences(&run.text, query) {
        while start_index < run.nodes.len() && run.nodes[start_index].end <= offset {
            start_index += 1;
        }
        if end_index < start_index {
            end_index = start_index;
        }
        while end_index < run.nodes.len() && run.nodes[end_index].end < offset + query_len {
            end_index += 1;
        }
        let (Some(start), Some(end)) = (run.nodes.get(start_index), run.nodes.get(end_index))
        else {
            continue;
        };
        let occurrence = occurrence_by_row_id.entry(row_id).or_insert(0);
        ranges.push(ThreadFindRange {
            row_id: row_id.to_owned(),
            occurrence: *occurrence,
            start: FindTextPoint {
                node: start.node,
                offset: offset - start.start,
            },
            end: FindTextPoint {
                node: end.node,
                offset: offset + query_len - end.start,
            },
        });
        *occurrence += 1;
    }
}

/// One occurrence of the query inside one rendered text flow, as byte offsets into that
/// flow's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowFindRange {
    pub range: std::ops::Range<usize>,
    /// Zero-based occurrence within the message (`ThreadFindRange::occurrence`).
    pub occurrence: u64,
}

/// The occurrences of `query` in one text flow (a paragraph, a heading, a table cell, a
/// code block), numbered from `first_occurrence`. The DOM painter walks text nodes; a
/// native flow is already one run of rendered text, so the same
/// `find_thread_search_occurrences` scan gives the ranges, and the caller carries the
/// running count across the message's flows in document order. `query` is the trimmed
/// query upstream searches with.
pub fn plan_flow_find_ranges(text: &str, query: &str, first_occurrence: u64) -> Vec<FlowFindRange> {
    if text.is_empty() || query.is_empty() {
        return Vec::new();
    }
    let query_len = utf16_len(query);
    let offsets = find_thread_search_occurrences(text, query);
    let mut ranges = Vec::with_capacity(offsets.len());
    // Offsets ascend, so one forward walk maps every UTF-16 offset to its byte offset.
    let mut chars = text.char_indices().peekable();
    let (mut byte, mut utf16) = (0usize, 0usize);
    let mut to_byte = |target: usize| -> Option<usize> {
        while utf16 < target {
            let (index, ch) = chars.next()?;
            byte = index + ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        (utf16 == target).then_some(byte)
    };
    for (position, offset) in offsets.into_iter().enumerate() {
        let (Some(start), Some(end)) = (to_byte(offset), to_byte(offset + query_len)) else {
            continue;
        };
        ranges.push(FlowFindRange {
            range: start..end,
            occurrence: first_occurrence + position as u64,
        });
    }
    ranges
}

/// One ancestor that folds text: `hidden` (the `hidden` attribute) or
/// `[data-thread-find-fold]` (clipped body). Innermost first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FindFoldAncestor {
    pub hidden: bool,
    pub bottom: f64,
}

/// Indexes into `ancestors` of the folds that keep a range out of sight.
///
/// A `hidden` fold always hides. A clipped body still shows its first lines, so it
/// hides only when the range's bottom edge is below the fold's bottom edge.
pub fn folds_hiding(range_bottom: f64, ancestors: &[FindFoldAncestor]) -> Vec<usize> {
    ancestors
        .iter()
        .enumerate()
        .filter(|(_, fold)| fold.hidden || range_bottom > fold.bottom)
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> FindNode {
        FindNode::Text(value.to_owned())
    }

    fn el(tag: &str, children: Vec<FindNode>) -> FindNode {
        FindNode::Element {
            tag: tag.to_owned(),
            ignored: false,
            children,
        }
    }

    fn row(root: FindNode) -> Vec<FindScope> {
        vec![FindScope {
            row_id: "row".to_owned(),
            root,
        }]
    }

    /// The matched text of a range, rebuilt from the walked text nodes.
    fn matched(nodes: &[&str], range: &ThreadFindRange) -> String {
        let mut out = String::new();
        let (first, last) = (range.start.node, range.end.node);
        for (node, text) in nodes.iter().enumerate().take(last + 1).skip(first) {
            let value: Vec<u16> = text.encode_utf16().collect();
            let from = if node == first { range.start.offset } else { 0 };
            let to = if node == last {
                range.end.offset
            } else {
                value.len()
            };
            out.push_str(&String::from_utf16_lossy(&value[from..to]));
        }
        out
    }

    #[test]
    fn finds_matches_split_across_inline_token_spans_in_order() {
        // Shiki-style tokens split one identifier over several text nodes.
        let root = el(
            "pre",
            vec![el(
                "code",
                vec![
                    el("span", vec![text("con")]),
                    el("span", vec![text("st")]),
                    text(" a = "),
                    el("span", vec![text("c")]),
                    text("onst; const"),
                ],
            )],
        );
        let ranges = collect_thread_find_ranges(&row(root), "const");
        let nodes = ["con", "st", " a = ", "c", "onst; const"];
        assert_eq!(
            ranges
                .iter()
                .map(|range| range.occurrence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(ranges.iter().all(|range| range.row_id == "row"));
        assert!(ranges.iter().all(|range| matched(&nodes, range) == "const"));
    }

    #[test]
    fn finds_every_match_across_thousands_of_token_nodes() {
        let tokens: Vec<FindNode> = (0..4000)
            .flat_map(|i| {
                [
                    el("span", vec![text("id")]),
                    el("span", vec![text(&format!("{i} "))]),
                ]
            })
            .collect();
        let root = el("pre", vec![el("code", tokens)]);
        let ranges = collect_thread_find_ranges(&row(root), "id");
        assert_eq!(ranges.len(), 4000);
        assert!(
            ranges
                .iter()
                .enumerate()
                .all(|(index, range)| range.occurrence == index as u64)
        );
    }

    #[test]
    fn block_tags_end_a_run_so_phrases_do_not_span_paragraphs() {
        let root = el(
            "div",
            vec![el("p", vec![text("Deploy")]), el("p", vec![text("ment")])],
        );
        assert!(collect_thread_find_ranges(&row(root), "deployment").is_empty());
    }

    #[test]
    fn newlines_in_prose_read_as_spaces_but_not_in_pre() {
        let prose = el("p", vec![text("a\nb")]);
        assert_eq!(collect_thread_find_ranges(&row(prose), "a b").len(), 1);
        let code = el("pre", vec![text("a\nb")]);
        assert!(collect_thread_find_ranges(&row(code), "a b").is_empty());
    }

    #[test]
    fn ignored_and_svg_content_is_not_searched() {
        let root = el(
            "div",
            vec![
                FindNode::Element {
                    tag: "span".to_owned(),
                    ignored: true,
                    children: vec![text("COD4")],
                },
                el("svg", vec![text("COD4")]),
                text("COD4"),
            ],
        );
        let ranges = collect_thread_find_ranges(&row(root), "COD4");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, FindTextPoint { node: 0, offset: 0 });
    }

    #[test]
    fn occurrence_numbers_continue_across_scopes_of_one_row() {
        let scopes = vec![
            FindScope {
                row_id: "a".to_owned(),
                root: text("COD4"),
            },
            FindScope {
                row_id: "b".to_owned(),
                root: text("COD4 COD4"),
            },
            FindScope {
                row_id: "a".to_owned(),
                root: text("COD4"),
            },
        ];
        let got: Vec<(String, u64)> = collect_thread_find_ranges(&scopes, "COD4")
            .into_iter()
            .map(|range| (range.row_id, range.occurrence))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a".to_owned(), 0),
                ("b".to_owned(), 0),
                ("b".to_owned(), 1),
                ("a".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(collect_thread_find_ranges(&row(text("COD4")), "").is_empty());
    }

    #[test]
    fn folds_hide_hidden_bodies_and_text_below_a_clip() {
        let ancestors = [
            FindFoldAncestor {
                hidden: false,
                bottom: 100.0,
            },
            FindFoldAncestor {
                hidden: true,
                bottom: 0.0,
            },
        ];
        // Above the clip bottom: only the hidden fold hides it.
        assert_eq!(folds_hiding(50.0, &ancestors), vec![1]);
        // Past the clip bottom: both hide it, innermost first.
        assert_eq!(folds_hiding(150.0, &ancestors), vec![0, 1]);
        assert!(folds_hiding(50.0, &ancestors[..1]).is_empty());
    }
}

#[cfg(test)]
mod flow_range_tests {
    use super::*;

    fn ranges(text: &str, query: &str, first: u64) -> Vec<(std::ops::Range<usize>, u64)> {
        plan_flow_find_ranges(text, query, first)
            .into_iter()
            .map(|found| (found.range, found.occurrence))
            .collect()
    }

    #[test]
    fn numbers_each_case_insensitive_occurrence_from_the_running_count() {
        assert_eq!(
            ranges("Foo foo FOO", "foo", 0),
            vec![(0..3, 0), (4..7, 1), (8..11, 2)]
        );
        // The second flow of a message continues the first one's count.
        assert_eq!(ranges("a foo", "foo", 3), vec![(2..5, 3)]);
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(ranges("aaaa", "aa", 0), vec![(0..2, 0), (2..4, 1)]);
    }

    #[test]
    fn maps_utf16_offsets_to_bytes_across_multibyte_text() {
        // "é" is 2 bytes and 1 UTF-16 unit; "😀" is 4 bytes and 2 units.
        let text = "é😀 needle é needle";
        let found = ranges(text, "needle", 0);
        assert_eq!(found.len(), 2);
        for (range, _) in &found {
            assert_eq!(&text[range.clone()], "needle");
        }
        assert_eq!(found[0].0, 7..13);
    }

    #[test]
    fn an_empty_query_or_text_finds_nothing() {
        assert!(plan_flow_find_ranges("text", "", 0).is_empty());
        assert!(plan_flow_find_ranges("", "text", 0).is_empty());
        assert!(plan_flow_find_ranges("text", "absent", 0).is_empty());
    }

    #[test]
    fn matches_the_dom_painters_occurrence_numbering_for_one_run() {
        // `collect_thread_find_ranges` over one text node and the flow plan agree on count.
        let root = FindNode::Element {
            tag: "p".to_owned(),
            ignored: false,
            children: vec![FindNode::Text("one two one".to_owned())],
        };
        let dom = collect_thread_find_ranges(
            &[FindScope {
                row_id: "row".to_owned(),
                root,
            }],
            "one",
        );
        let flow = plan_flow_find_ranges("one two one", "one", 0);
        assert_eq!(dom.len(), flow.len());
        assert_eq!(
            dom.iter().map(|range| range.occurrence).collect::<Vec<_>>(),
            flow.iter()
                .map(|range| range.occurrence)
                .collect::<Vec<_>>()
        );
    }
}
