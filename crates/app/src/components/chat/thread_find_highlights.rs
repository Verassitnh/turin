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
