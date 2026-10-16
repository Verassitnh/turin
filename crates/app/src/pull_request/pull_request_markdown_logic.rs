//! Port of upstream/apps/web/src/components/pullRequest/pullRequestMarkdown.logic.ts.
//!
//! [`find_pull_request_autolinks`] is the matcher over plain text.
//! [`remark_pull_request_autolinks`] is `remarkPullRequestAutolinks` itself, over the generic
//! markdown tree of `mdast_find_and_replace` (text nodes only, authored links and code skipped).
//! ChatMarkdown lowers markdown-rs's mdast into a flat `MarkdownDocument` (inline content is
//! text, runs and link targets, not nodes), so [`apply_pull_request_autolinks`] runs the plugin
//! over a document: each run becomes one node of a throwaway tree, the plugin replaces the text
//! nodes, and the result is folded back into runs and links.
use regex::Regex;
use std::sync::LazyLock;
use turin_contracts::js_trim;

use crate::components::chat::markdown::markdown_pipeline::{
    Block, InlineRun, Inlines, LinkAutolink, ListItem, MarkdownDocument, Marks,
};
use crate::components::chat::markdown::mdast_find_and_replace::{
    MarkdownNode, TextMatch, find_and_replace_text,
};
static CANDIDATES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)#[1-9][0-9]*|[0-9a-f]{40}").unwrap());
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestAutolinkKind {
    Reference,
    Commit,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestAutolink {
    pub start: usize,
    pub end: usize,
    pub kind: PullRequestAutolinkKind,
    pub url: String,
    pub label: String,
}
fn word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
/// GitHub-style same-repository candidates with the exact text boundary rules.
pub fn find_pull_request_autolinks(input: &str, repository_url: &str) -> Vec<PullRequestAutolink> {
    let repository_url = repository_url.trim_end_matches('/');
    CANDIDATES
        .find_iter(input)
        .filter_map(|found| {
            let matched = found.as_str();
            let reference = matched.starts_with('#');
            let before = input[..found.start()].chars().next_back();
            let after = input[found.end()..].chars().next();
            if autolink_boundary_rejects(reference, before, after) {
                return None;
            }
            let (kind, url, label) = if reference {
                (
                    PullRequestAutolinkKind::Reference,
                    format!("{repository_url}/issues/{}", &matched[1..]),
                    matched.to_owned(),
                )
            } else {
                (
                    PullRequestAutolinkKind::Commit,
                    format!("{repository_url}/commit/{matched}"),
                    matched[..7].to_owned(),
                )
            };
            Some(PullRequestAutolink {
                start: input[..found.start()].encode_utf16().count(),
                end: input[..found.end()].encode_utf16().count(),
                kind,
                url,
                label,
            })
        })
        .collect()
}

// --- remarkPullRequestAutolinks over the markdown tree ---------------------

const AUTOLINK_IGNORED_TYPES: [&str; 2] = ["link", "linkReference"];

/// `AUTOLINK_WORD_CHARACTER_PATTERN` / `AUTOLINK_COMMIT_PREFIX_PATTERN`: whether the characters
/// around a candidate rule it out. `None` is the start or end of the text node.
fn autolink_boundary_rejects(reference: bool, before: Option<char>, after: Option<char>) -> bool {
    before.is_some_and(|ch| {
        if reference {
            word(ch)
        } else {
            !(is_js_whitespace_char(ch) || "([{".contains(ch))
        }
    }) || after.is_some_and(word)
}

fn is_js_whitespace_char(ch: char) -> bool {
    js_trim(&ch.to_string()).is_empty()
}

/// `remarkPullRequestAutolinks`: GitHub-style same-repository references for pull request prose,
/// never code or authored links. Text nodes are replaced with `link` nodes carrying
/// `data.hProperties.dataPullRequestAutolink` (`reference` or `commit`).
pub fn remark_pull_request_autolinks(tree: &mut MarkdownNode, repository_url: &str) {
    let repository_url = repository_url.trim_end_matches('/');
    find_and_replace_text(
        tree,
        &CANDIDATES,
        true,
        &mut |matched: &str, text_match: &TextMatch<'_>| {
            let reference = matched.starts_with('#');
            let before = text_match.input[..text_match.index].chars().next_back();
            let after = text_match.input[text_match.index + matched.len()..]
                .chars()
                .next();
            if autolink_boundary_rejects(reference, before, after) {
                return None;
            }
            let (url, label, kind) = if reference {
                (
                    format!("{repository_url}/issues/{}", &matched[1..]),
                    matched,
                    "reference",
                )
            } else {
                (
                    format!("{repository_url}/commit/{matched}"),
                    &matched[..7],
                    "commit",
                )
            };
            Some(MarkdownNode {
                url: Some(url),
                data: Some(
                    serde_json::json!({ "hProperties": { "dataPullRequestAutolink": kind } }),
                ),
                ..MarkdownNode::with_children("link", vec![MarkdownNode::text(label)])
            })
        },
        &AUTOLINK_IGNORED_TYPES,
    );
}

fn autolink_kind_of(node: &MarkdownNode) -> Option<LinkAutolink> {
    match node
        .data
        .as_ref()?
        .pointer("/hProperties/dataPullRequestAutolink")?
        .as_str()?
    {
        "reference" => Some(LinkAutolink::PullRequestReference),
        "commit" => Some(LinkAutolink::PullRequestCommit),
        _ => None,
    }
}

/// The length of the source text an autolink replaced: a reference keeps its text, a commit is
/// shown as seven characters of a forty character hash.
fn autolink_source_len(kind: LinkAutolink, label: &str) -> usize {
    match kind {
        LinkAutolink::PullRequestReference => label.len(),
        LinkAutolink::PullRequestCommit => 40,
    }
}

/// One paragraph of inline content through the plugin. Each run is one mdast node, as the tree
/// remark builds: plain runs are `text`, a run under a link is a `link`, code is `inlineCode`.
fn autolink_inlines(inlines: &Inlines, repository_url: &str) -> Inlines {
    if !CANDIDATES.is_match(&inlines.text) {
        return inlines.clone();
    }
    let runs: Vec<(std::ops::Range<usize>, Marks)> = inlines.run_ranges().collect();
    let is_plain = |marks: &Marks| marks.link.is_none() && !marks.code && !marks.footnote_ref;
    let mut root = MarkdownNode::with_children(
        "root",
        runs.iter()
            .map(|(range, marks)| {
                let text = &inlines.text[range.clone()];
                let child = if let Some(link) = marks.link {
                    MarkdownNode {
                        url: inlines.links.get(link).cloned(),
                        ..MarkdownNode::with_children("link", vec![MarkdownNode::text(text)])
                    }
                } else if marks.code {
                    MarkdownNode {
                        value: Some(text.to_owned()),
                        ..MarkdownNode::new("inlineCode")
                    }
                } else if marks.footnote_ref {
                    MarkdownNode {
                        value: Some(text.to_owned()),
                        ..MarkdownNode::new("footnoteReference")
                    }
                } else {
                    MarkdownNode::text(text)
                };
                MarkdownNode::with_children("paragraph", vec![child])
            })
            .collect(),
    );
    remark_pull_request_autolinks(&mut root, repository_url);

    let mut out = Inlines {
        links: inlines.links.clone(),
        autolinks: inlines.autolinks.clone(),
        ..Default::default()
    };
    // (old start, old end, new start): where each stretch of source text landed, to move images.
    let mut segments: Vec<(usize, usize, usize)> = Vec::new();
    let paragraphs = root.children.unwrap_or_default();
    for ((range, marks), paragraph) in runs.iter().zip(paragraphs) {
        let children = paragraph.children.unwrap_or_default();
        if !is_plain(marks)
            || children.len() <= 1 && children.first().is_none_or(|c| c.node_type == "text")
        {
            // Unchanged: copy the run as it was.
            segments.push((range.start, range.end, out.text.len()));
            out.text.push_str(&inlines.text[range.clone()]);
            out.runs.push(InlineRun {
                len: range.len(),
                marks: *marks,
            });
            continue;
        }
        let mut old_cursor = range.start;
        for child in children {
            let (new_text, old_len, run_marks) = match autolink_kind_of(&child) {
                Some(kind) => {
                    let label = child
                        .children
                        .as_ref()
                        .and_then(|children| children.first())
                        .and_then(|node| node.value.clone())
                        .unwrap_or_default();
                    let link = out.links.len();
                    out.links.push(child.url.clone().unwrap_or_default());
                    out.autolinks.push((link, kind));
                    let old_len = autolink_source_len(kind, &label);
                    (
                        label,
                        old_len,
                        Marks {
                            link: Some(link),
                            ..*marks
                        },
                    )
                }
                None => {
                    let value = child.value.clone().unwrap_or_default();
                    let len = value.len();
                    (value, len, *marks)
                }
            };
            segments.push((old_cursor, old_cursor + old_len, out.text.len()));
            out.text.push_str(&new_text);
            out.runs.push(InlineRun {
                len: new_text.len(),
                marks: run_marks,
            });
            old_cursor += old_len;
        }
    }
    out.images = inlines
        .images
        .iter()
        .map(|(offset, alt, url)| {
            let moved = segments
                .iter()
                .find(|(start, end, _)| start <= offset && offset <= end)
                .map_or(*offset, |(start, end, new_start)| {
                    new_start + (offset - start).min(end - start)
                });
            (moved, alt.clone(), url.clone())
        })
        .collect();
    out
}

fn autolink_blocks(blocks: &[Block], repository_url: &str) -> Vec<Block> {
    blocks
        .iter()
        .map(|block| autolink_block(block, repository_url))
        .collect()
}

fn autolink_block(block: &Block, repository_url: &str) -> Block {
    match block {
        Block::Paragraph(inlines) => Block::Paragraph(autolink_inlines(inlines, repository_url)),
        Block::Heading { level, content } => Block::Heading {
            level: *level,
            content: autolink_inlines(content, repository_url),
        },
        Block::Blockquote(children) => Block::Blockquote(autolink_blocks(children, repository_url)),
        Block::Alert { kind, children } => Block::Alert {
            kind: *kind,
            children: autolink_blocks(children, repository_url),
        },
        Block::List {
            ordered,
            start,
            spread,
            items,
        } => Block::List {
            ordered: *ordered,
            start: *start,
            spread: *spread,
            items: items
                .iter()
                .map(|item| ListItem {
                    checked: item.checked,
                    spread: item.spread,
                    children: autolink_blocks(&item.children, repository_url),
                })
                .collect(),
        },
        Block::Table {
            align,
            header,
            rows,
        } => Block::Table {
            align: align.clone(),
            header: header
                .iter()
                .map(|cell| autolink_inlines(cell, repository_url))
                .collect(),
            rows: rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| autolink_inlines(cell, repository_url))
                        .collect()
                })
                .collect(),
        },
        Block::Footnotes(notes) => Block::Footnotes(
            notes
                .iter()
                .map(|(label, children)| (label.clone(), autolink_blocks(children, repository_url)))
                .collect(),
        ),
        Block::ThematicBreak | Block::Code { .. } | Block::Html(_) => block.clone(),
    }
}

/// `remarkPullRequestAutolinks` over a parsed document: what ChatMarkdown's `extraRemarkPlugins`
/// does to the tree before it renders.
pub fn apply_pull_request_autolinks(
    document: &MarkdownDocument,
    repository_url: &str,
) -> MarkdownDocument {
    MarkdownDocument {
        blocks: autolink_blocks(&document.blocks, repository_url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat::markdown::markdown_pipeline::parse_chat_markdown;
    use pretty_assertions::assert_eq;
    #[test]
    fn references_need_word_boundaries() {
        let links = find_pull_request_autolinks("see #42, x#43 and #1z", "https://github.com/a/b/");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "#42");
        assert_eq!(links[0].url, "https://github.com/a/b/issues/42");
    }
    #[test]
    fn commits_need_whitespace_or_open_delimiter() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let links = find_pull_request_autolinks(
            &format!("{sha} ({sha}) x{sha} {sha}x"),
            "https://github.com/a/b",
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label, "0123456");
        assert_eq!(links[0].url, format!("https://github.com/a/b/commit/{sha}"));
    }
    #[test]
    fn zero_reference_is_not_candidate() {
        assert!(find_pull_request_autolinks("#0", "https://github.com/a/b").is_empty());
    }
    #[test]
    fn repository_url_loses_every_trailing_slash() {
        let links = find_pull_request_autolinks("#7", "https://github.com/a/b///");
        assert_eq!(links[0].url, "https://github.com/a/b/issues/7");
    }
    #[test]
    fn offsets_are_utf16_code_units() {
        // An astral character is two UTF-16 units, so `#7` starts at 2 and ends at 4.
        let links = find_pull_request_autolinks("\u{1F600} #7", "https://github.com/a/b");
        assert_eq!((links[0].start, links[0].end), (3, 5));
    }
    #[test]
    fn commit_prefix_accepts_any_js_whitespace() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let links = find_pull_request_autolinks(&format!("\u{a0}{sha}"), "https://github.com/a/b");
        assert_eq!(links.len(), 1);
    }

    fn text_tree(children: Vec<MarkdownNode>) -> MarkdownNode {
        MarkdownNode::with_children(
            "root",
            vec![MarkdownNode::with_children("paragraph", children)],
        )
    }

    fn link_urls(tree: &MarkdownNode) -> Vec<(String, String, String)> {
        let mut found = Vec::new();
        fn walk(node: &MarkdownNode, found: &mut Vec<(String, String, String)>) {
            if node.node_type == "link" {
                let label = node
                    .children
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|n| n.value.clone())
                    .unwrap_or_default();
                let kind = node
                    .data
                    .as_ref()
                    .and_then(|d| d.pointer("/hProperties/dataPullRequestAutolink"))
                    .and_then(|k| k.as_str())
                    .unwrap_or("authored")
                    .to_owned();
                found.push((node.url.clone().unwrap_or_default(), label, kind));
            }
            for child in node.children.iter().flatten() {
                walk(child, found);
            }
        }
        walk(tree, &mut found);
        found
    }

    #[test]
    fn the_tree_plugin_links_references_and_commits_but_not_authored_links_or_code() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let mut tree = text_tree(vec![
            MarkdownNode::text(&format!("see #42 and {sha}, x#43")),
            MarkdownNode::with_children("link", vec![MarkdownNode::text("#44")]),
            MarkdownNode {
                value: Some("#45".to_owned()),
                ..MarkdownNode::new("inlineCode")
            },
        ]);
        remark_pull_request_autolinks(&mut tree, "https://github.com/a/b/");
        assert_eq!(
            link_urls(&tree),
            [
                (
                    "https://github.com/a/b/issues/42".to_owned(),
                    "#42".to_owned(),
                    "reference".to_owned()
                ),
                (
                    format!("https://github.com/a/b/commit/{sha}"),
                    "0123456".to_owned(),
                    "commit".to_owned()
                ),
                // The authored link keeps its own children untouched.
                ("".to_owned(), "#44".to_owned(), "authored".to_owned()),
            ]
        );
    }

    #[test]
    fn the_tree_plugin_agrees_with_the_text_matcher() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        for input in [
            "see #42, x#43 and #1z".to_owned(),
            format!("{sha} ({sha}) x{sha} {sha}x"),
            "#0 #7".to_owned(),
            "\u{a0}#9".to_owned(),
        ] {
            let mut tree = text_tree(vec![MarkdownNode::text(&input)]);
            remark_pull_request_autolinks(&mut tree, "https://github.com/a/b");
            let from_tree: Vec<String> = link_urls(&tree)
                .into_iter()
                .map(|(url, _, _)| url)
                .collect();
            let from_matcher: Vec<String> =
                find_pull_request_autolinks(&input, "https://github.com/a/b")
                    .into_iter()
                    .map(|link| link.url)
                    .collect();
            assert_eq!(from_tree, from_matcher, "{input:?}");
        }
    }

    #[test]
    fn a_rejected_candidate_does_not_hide_the_one_inside_it() {
        // `#12a` is rejected for the word character after it, then the search resumes at `12a`.
        let mut tree = text_tree(vec![MarkdownNode::text("#12a #7")]);
        remark_pull_request_autolinks(&mut tree, "https://github.com/a/b");
        let urls: Vec<String> = link_urls(&tree)
            .into_iter()
            .map(|(url, _, _)| url)
            .collect();
        assert_eq!(urls, ["https://github.com/a/b/issues/7"]);
    }

    fn document_text(document: &MarkdownDocument) -> &Inlines {
        match &document.blocks[0] {
            Block::Paragraph(inlines) => inlines,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    #[test]
    fn the_document_pass_turns_references_into_links_and_shortens_commits() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let parsed = parse_chat_markdown(
            &format!("Fixes #12, see **#13**, `#14`, [#15](https://example.com) and {sha}."),
            false,
        );
        let linked = apply_pull_request_autolinks(&parsed, "https://github.com/a/b");
        let inlines = document_text(&linked);

        assert_eq!(inlines.text, "Fixes #12, see #13, #14, #15 and 0123456.");
        let covered: usize = inlines.runs.iter().map(|run| run.len).sum();
        assert_eq!(covered, inlines.text.len());

        let linked_labels: Vec<(String, Option<LinkAutolink>)> = inlines
            .run_ranges()
            .filter_map(|(range, marks)| {
                let link = marks.link?;
                let kind = inlines
                    .autolinks
                    .iter()
                    .find(|(index, _)| *index == link)
                    .map(|(_, kind)| *kind);
                Some((inlines.text[range].to_owned(), kind))
            })
            .collect();
        assert_eq!(
            linked_labels,
            [
                ("#12".to_owned(), Some(LinkAutolink::PullRequestReference)),
                ("#13".to_owned(), Some(LinkAutolink::PullRequestReference)),
                // The authored link is not an autolink.
                ("#15".to_owned(), None),
                ("0123456".to_owned(), Some(LinkAutolink::PullRequestCommit)),
            ]
        );
        assert!(
            inlines
                .links
                .contains(&"https://github.com/a/b/issues/12".to_owned())
        );
        assert!(
            inlines
                .links
                .contains(&format!("https://github.com/a/b/commit/{sha}"))
        );
        assert!(
            inlines.links.contains(&"https://example.com/".to_owned())
                || inlines.links.contains(&"https://example.com".to_owned())
        );
        // The strong run keeps its mark around the autolink.
        let strong_link = inlines
            .run_ranges()
            .find(|(range, _)| &inlines.text[range.clone()] == "#13")
            .map(|(_, marks)| marks)
            .expect("the #13 run");
        assert!(strong_link.strong && strong_link.link.is_some());
        // Inline code stays code.
        let code = inlines
            .run_ranges()
            .find(|(range, _)| &inlines.text[range.clone()] == "#14")
            .map(|(_, marks)| marks)
            .expect("the #14 run");
        assert!(code.code && code.link.is_none());
    }

    #[test]
    fn the_document_pass_leaves_text_without_candidates_alone_and_reaches_nested_blocks() {
        let plain = parse_chat_markdown("nothing to see", false);
        assert_eq!(
            apply_pull_request_autolinks(&plain, "https://github.com/a/b"),
            plain
        );

        let nested = parse_chat_markdown("> - item #5\n\n| a |\n|---|\n| #6 |", false);
        let linked = apply_pull_request_autolinks(&nested, "https://github.com/a/b");
        let Block::Blockquote(children) = &linked.blocks[0] else {
            panic!("expected a blockquote");
        };
        let Block::List { items, .. } = &children[0] else {
            panic!("expected a list");
        };
        let Block::Paragraph(item) = &items[0].children[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(item.autolinks.len(), 1);
        let Block::Table { rows, .. } = &linked.blocks[1] else {
            panic!("expected a table");
        };
        assert_eq!(rows[0][0].autolinks.len(), 1);
    }

    #[test]
    fn an_image_after_an_autolink_keeps_its_place_in_the_text() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let parsed = parse_chat_markdown(
            &format!("{sha} then ![alt](https://example.com/a.png)"),
            false,
        );
        let before = document_text(&parsed).images.clone();
        let linked = apply_pull_request_autolinks(&parsed, "https://github.com/a/b");
        let inlines = document_text(&linked);
        assert_eq!(inlines.images.len(), before.len());
        // The hash shrank from 40 to 7 characters, so the image moved left by 33 bytes.
        assert_eq!(inlines.images[0].0, before[0].0 - 33);
        assert!(inlines.text.starts_with("0123456 then"));
    }
}
