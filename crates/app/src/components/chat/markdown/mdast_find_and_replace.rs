//! Port of upstream/apps/web/src/vendor/mdast-find-and-replace.ts.
//!
//! Adapted upstream from mdast-util-find-and-replace
//! (<https://github.com/syntax-tree/mdast-util-find-and-replace>, MIT,
//! Copyright (c) Titus Wormer): the dependency-free subset the pull request
//! autolinks use. It walks a markdown tree, finds a pattern in every text
//! node outside the ignored node types, and splits the text around each
//! replacement the callback accepts.
//!
//! Match offsets are byte offsets into the text node (upstream's are UTF-16
//! code units); [`TextMatch::input`] is the whole text node, so a caller
//! reads the characters around a match with `input[..index]` and
//! `input[index + matched.len()..]`. A rejected match resumes the search one
//! character later (upstream: `lastIndex = position + 1`, one code unit).
// Its only caller, the pull request autolinks, is wired through the document
// bridge; the generic walker stays complete.
#![allow(dead_code)]

use regex::Regex;

/// `MarkdownNode`: the slice of an mdast node the walker reads and builds.
/// `data` is mdast's open `data` bag (`hProperties` and friends).
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownNode {
    pub node_type: String,
    pub value: Option<String>,
    pub children: Option<Vec<MarkdownNode>>,
    pub url: Option<String>,
    pub data: Option<serde_json::Value>,
}

impl MarkdownNode {
    pub fn new(node_type: &str) -> Self {
        Self {
            node_type: node_type.to_owned(),
            value: None,
            children: None,
            url: None,
            data: None,
        }
    }

    pub fn text(value: &str) -> Self {
        Self {
            value: Some(value.to_owned()),
            ..Self::new("text")
        }
    }

    pub fn with_children(node_type: &str, children: Vec<MarkdownNode>) -> Self {
        Self {
            children: Some(children),
            ..Self::new(node_type)
        }
    }
}

/// `TextMatch`.
#[derive(Debug, Clone, Copy)]
pub struct TextMatch<'a> {
    pub index: usize,
    pub input: &'a str,
}

/// `findAndReplaceText`. `global` stands for the regex's `g` flag: without it
/// only the first match of each text node is considered. `replace` returns
/// `None` for upstream's `false` (leave this match as text).
pub fn find_and_replace_text(
    tree: &mut MarkdownNode,
    find: &Regex,
    global: bool,
    replace: &mut dyn FnMut(&str, &TextMatch<'_>) -> Option<MarkdownNode>,
    ignored_types: &[&str],
) {
    visit(tree, find, global, replace, ignored_types);
}

fn visit(
    node: &mut MarkdownNode,
    find: &Regex,
    global: bool,
    replace: &mut dyn FnMut(&str, &TextMatch<'_>) -> Option<MarkdownNode>,
    ignored_types: &[&str],
) {
    let Some(children) = node.children.as_mut() else {
        return;
    };
    let mut child_index = 0;
    while child_index < children.len() {
        let child = &children[child_index];
        if ignored_types.contains(&child.node_type.as_str()) {
            child_index += 1;
            continue;
        }
        let Some(value) = child
            .value
            .as_deref()
            .filter(|_| child.node_type == "text")
            .map(str::to_owned)
        else {
            visit(
                &mut children[child_index],
                find,
                global,
                replace,
                ignored_types,
            );
            child_index += 1;
            continue;
        };

        let mut replacements: Vec<MarkdownNode> = Vec::new();
        let mut start = 0;
        let mut changed = false;
        let mut search_from = 0;
        while search_from <= value.len() {
            let Some(found) = find.find_at(&value, search_from) else {
                break;
            };
            let position = found.start();
            let matched = found.as_str();
            let replacement = replace(
                matched,
                &TextMatch {
                    index: position,
                    input: &value,
                },
            );
            match replacement {
                None => {
                    search_from = position + next_char_len(&value, position);
                }
                Some(replacement) => {
                    if start < position {
                        replacements.push(MarkdownNode::text(&value[start..position]));
                    }
                    replacements.push(replacement);
                    start = position + matched.len();
                    changed = true;
                    // A global JS regex continues from the end of the match;
                    // an empty match would loop forever there, so step on.
                    search_from = if matched.is_empty() {
                        start + next_char_len(&value, start)
                    } else {
                        start
                    };
                }
            }
            if !global {
                break;
            }
        }

        if !changed {
            child_index += 1;
            continue;
        }
        if start < value.len() {
            replacements.push(MarkdownNode::text(&value[start..]));
        }
        let inserted = replacements.len();
        children.splice(child_index..child_index + 1, replacements);
        child_index += inserted;
    }
}

/// `lastIndex = position + 1` advances one code unit; here one character
/// (one past the end when `position` is at the end, which stops the search).
fn next_char_len(value: &str, position: usize) -> usize {
    value[position..].chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(children: Vec<MarkdownNode>) -> MarkdownNode {
        MarkdownNode::with_children(
            "root",
            vec![MarkdownNode::with_children("paragraph", children)],
        )
    }

    fn marker(label: &str) -> MarkdownNode {
        MarkdownNode {
            url: Some(format!("https://example.com/{label}")),
            ..MarkdownNode::with_children("link", vec![MarkdownNode::text(label)])
        }
    }

    fn summarize(node: &MarkdownNode) -> String {
        match (&node.value, &node.children) {
            (Some(value), _) => format!("{}({value:?})", node.node_type),
            (None, Some(children)) => format!(
                "{}[{}]",
                node.node_type,
                children
                    .iter()
                    .map(summarize)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            (None, None) => node.node_type.clone(),
        }
    }

    fn digits() -> Regex {
        Regex::new(r"#[0-9]+").unwrap()
    }

    #[test]
    fn splits_a_text_node_around_each_replacement() {
        let mut tree = paragraph(vec![MarkdownNode::text("see #1 and #22 now")]);
        find_and_replace_text(
            &mut tree,
            &digits(),
            true,
            &mut |matched, _| Some(marker(matched)),
            &[],
        );
        assert_eq!(
            summarize(&tree),
            r##"root[paragraph[text("see "), link[text("#1")], text(" and "), link[text("#22")], text(" now")]]"##
        );
    }

    #[test]
    fn leaves_a_text_node_alone_when_nothing_matches_or_every_match_is_rejected() {
        let mut tree = paragraph(vec![
            MarkdownNode::text("no references"),
            MarkdownNode::text("#7"),
        ]);
        find_and_replace_text(&mut tree, &digits(), true, &mut |_, _| None, &[]);
        assert_eq!(
            summarize(&tree),
            r##"root[paragraph[text("no references"), text("#7")]]"##
        );
    }

    #[test]
    fn a_rejected_match_advances_the_search_one_character() {
        // `#12` is rejected; the search resumes one character later, inside it.
        let mut tree = paragraph(vec![MarkdownNode::text("#12")]);
        let mut seen = Vec::new();
        find_and_replace_text(
            &mut tree,
            &Regex::new(r"#?[0-9]+").unwrap(),
            true,
            &mut |matched, _| {
                seen.push(matched.to_owned());
                None
            },
            &[],
        );
        assert_eq!(seen, ["#12", "12", "2"]);
    }

    #[test]
    fn hands_the_callback_the_match_offset_and_the_whole_text() {
        let mut tree = paragraph(vec![MarkdownNode::text("ab #3 cd")]);
        let mut calls = Vec::new();
        find_and_replace_text(
            &mut tree,
            &digits(),
            true,
            &mut |matched, text_match| {
                calls.push((
                    matched.to_owned(),
                    text_match.index,
                    text_match.input.to_owned(),
                ));
                None
            },
            &[],
        );
        assert_eq!(calls, [("#3".to_owned(), 3, "ab #3 cd".to_owned())]);
    }

    #[test]
    fn without_the_global_flag_only_the_first_match_of_a_node_is_considered() {
        let mut tree = paragraph(vec![MarkdownNode::text("#1 #2")]);
        find_and_replace_text(
            &mut tree,
            &digits(),
            false,
            &mut |matched, _| Some(marker(matched)),
            &[],
        );
        assert_eq!(
            summarize(&tree),
            r##"root[paragraph[link[text("#1")], text(" #2")]]"##
        );
    }

    #[test]
    fn skips_ignored_node_types_and_nodes_that_are_not_text() {
        let mut tree = paragraph(vec![
            MarkdownNode::with_children("link", vec![MarkdownNode::text("#1")]),
            MarkdownNode {
                value: Some("#2".to_owned()),
                ..MarkdownNode::new("inlineCode")
            },
            MarkdownNode::with_children("emphasis", vec![MarkdownNode::text("#3")]),
        ]);
        find_and_replace_text(
            &mut tree,
            &digits(),
            true,
            &mut |matched, _| Some(marker(matched)),
            &["link", "linkReference"],
        );
        assert_eq!(
            summarize(&tree),
            r##"root[paragraph[link[text("#1")], inlineCode("#2"), emphasis[link[text("#3")]]]]"##
        );
    }

    #[test]
    fn handles_matches_at_both_ends_and_non_ascii_text() {
        let mut tree = paragraph(vec![MarkdownNode::text("#4é\u{1F600}#5")]);
        find_and_replace_text(
            &mut tree,
            &digits(),
            true,
            &mut |matched, _| Some(marker(matched)),
            &[],
        );
        assert_eq!(
            summarize(&tree),
            "root[paragraph[link[text(\"#4\")], text(\"é\u{1F600}\"), link[text(\"#5\")]]]"
        );
    }

    #[test]
    fn a_rejected_match_before_an_accepted_one_keeps_the_text_between() {
        let mut tree = paragraph(vec![MarkdownNode::text("x#1 #2")]);
        find_and_replace_text(
            &mut tree,
            &digits(),
            true,
            &mut |matched, text_match| (text_match.index != 1).then(|| marker(matched)),
            &[],
        );
        assert_eq!(
            summarize(&tree),
            r##"root[paragraph[text("x#1 "), link[text("#2")]]]"##
        );
    }
}
