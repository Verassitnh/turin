//! Port of upstream/packages/shared/src/githubMarkdownLinks.ts
//!
//! Upstream parses with `unified().use(remarkParse).use(remarkGfm)`; the `markdown` crate
//! (markdown-rs, the same micromark lineage) with GFM constructs gives an equivalent mdast with
//! byte offsets. The reference pattern needs lookaround, which is why this uses `fancy-regex`
//! (JS `\w` is ASCII-only, so it is spelled out as `[A-Za-z0-9_]`).

use std::sync::LazyLock;

use fancy_regex::Regex;
use markdown::mdast::Node;

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?<![A-Za-z0-9_/\\])(?:([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+|[A-Za-z0-9_-]+)?#([1-9][0-9]*)|(?:([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+|[A-Za-z0-9_-]+)@)?([a-f0-9]{7,40})(?![A-Za-z0-9_])|@([a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:/[a-z0-9][a-z0-9-]*)?))(?![A-Za-z0-9_])",
    )
    .expect("valid reference regex")
});
static QUICK_CHECK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[#@]|\b[a-f0-9]{7,40}\b").expect("valid regex"));
static PROTECTED_HTML_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(/)?(?:a|code|pre|script|style)(?=[\s>])[^>]*>").expect("valid regex")
});
static HAS_LOWER_HEX_LETTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[a-f]").expect("valid regex"));

struct Edit {
    start: usize,
    end: usize,
    value: String,
}

/// Link GitHub references in prose without rewriting code, URLs, HTML, or existing links.
pub fn github_markdown_links(markdown_source: &str, pull_request_url: Option<&str>) -> String {
    let Some(pull_request_url) = pull_request_url.filter(|u| !u.is_empty()) else {
        return markdown_source.to_owned();
    };
    if !QUICK_CHECK.is_match(markdown_source).unwrap_or(false) {
        return markdown_source.to_owned();
    }
    let Ok(url) = reqwest::Url::parse(pull_request_url) else {
        return markdown_source.to_owned();
    };
    let mut segments = url.path().split('/').filter(|s| !s.is_empty());
    let (Some(owner), Some(repo)) = (segments.next(), segments.next()) else {
        return markdown_source.to_owned();
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return markdown_source.to_owned();
    }
    let origin = url.origin().ascii_serialization();
    let Ok(tree) = markdown::to_mdast(markdown_source, &markdown::ParseOptions::gfm()) else {
        return markdown_source.to_owned();
    };

    let repository = |qualifier: Option<&str>| match qualifier {
        Some(q) if q.contains('/') => q.to_owned(),
        Some(q) => format!("{q}/{repo}"),
        None => format!("{owner}/{repo}"),
    };
    let mut edits: Vec<Edit> = Vec::new();

    fn offsets(node: &Node) -> Option<(usize, usize)> {
        let position = node.position()?;
        Some((position.start.offset, position.end.offset))
    }

    fn visit(
        node: &Node,
        source: &str,
        origin: &str,
        repository: &dyn Fn(Option<&str>) -> String,
        edits: &mut Vec<Edit>,
    ) {
        if matches!(
            node,
            Node::Link(_)
                | Node::LinkReference(_)
                | Node::Image(_)
                | Node::ImageReference(_)
                | Node::Code(_)
                | Node::InlineCode(_)
                | Node::Html(_)
                | Node::Definition(_)
        ) {
            return;
        }
        if let Node::Text(_) = node {
            let Some((start, end)) = offsets(node) else {
                return;
            };
            // Work on source slices so escaped references and Markdown offsets stay intact.
            let Some(slice) = source.get(start..end) else {
                return;
            };
            for captures in REFERENCE.captures_iter(slice).flatten() {
                let whole = captures.get(0).expect("group 0");
                let label = whole.as_str();
                let issue_repo = captures.get(1).map(|m| m.as_str());
                let number = captures.get(2).map(|m| m.as_str());
                let commit_repo = captures.get(3).map(|m| m.as_str());
                let sha = captures.get(4).map(|m| m.as_str());
                let mention = captures.get(5).map(|m| m.as_str());
                if let Some(sha) = sha
                    && !HAS_LOWER_HEX_LETTER.is_match(sha).unwrap_or(false)
                {
                    continue;
                }
                let path = if let Some(number) = number {
                    format!("{}/issues/{number}", repository(issue_repo))
                } else if let Some(sha) = sha {
                    format!("{}/commit/{sha}", repository(commit_repo))
                } else if let Some(mention) = mention {
                    match mention.split_once('/') {
                        Some((org, team)) => format!("orgs/{org}/teams/{team}"),
                        None => mention.to_owned(),
                    }
                } else {
                    continue;
                };
                edits.push(Edit {
                    start: start + whole.start(),
                    end: start + whole.end(),
                    value: format!("[{label}]({origin}/{path})"),
                });
            }
        } else if let Some(children) = node.children() {
            let mut protected_html_depth: i64 = 0;
            for child in children {
                if let Node::Html(_) = child {
                    let tag = offsets(child)
                        .and_then(|(start, end)| source.get(start..end))
                        .unwrap_or("");
                    for captured in PROTECTED_HTML_TAG.captures_iter(tag).flatten() {
                        let delta = if captured.get(1).is_some() { -1 } else { 1 };
                        protected_html_depth = (protected_html_depth + delta).max(0);
                    }
                } else if protected_html_depth == 0 {
                    visit(child, source, origin, repository, edits);
                }
            }
        }
    }

    visit(&tree, markdown_source, &origin, &repository, &mut edits);
    let mut result = markdown_source.to_owned();
    for edit in edits.iter().rev() {
        result.replace_range(edit.start..edit.end, &edit.value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://github.com/team/repo/pull/8";

    #[test]
    fn links_closing_references_cross_repository_references_commits_and_mentions() {
        assert_eq!(
            github_markdown_links(
                "Closes #123, fixes other/project#9; other#4 @julius @team/mobile abc1234 other/project@abcdef1",
                Some(URL),
            ),
            "Closes [#123](https://github.com/team/repo/issues/123), fixes [other/project#9](https://github.com/other/project/issues/9); [other#4](https://github.com/other/repo/issues/4) [@julius](https://github.com/julius) [@team/mobile](https://github.com/orgs/team/teams/mobile) [abc1234](https://github.com/team/repo/commit/abc1234) [other/project@abcdef1](https://github.com/other/project/commit/abcdef1)"
        );
    }

    #[test]
    fn leaves_code_links_images_urls_escapes_emails_and_plain_numbers_alone() {
        let text = "`#123 @person abc1234`\n\n```\n#123\n```\n\n[#123](https://example.com) ![#123](image.png) https://example.com/#123 user@example.com \\#123 1234567\n\n[ref]: https://example.com/#123";
        assert_eq!(github_markdown_links(text, Some(URL)), text);
    }

    #[test]
    fn keeps_formatting_and_links_references_in_lists_and_quotes() {
        assert_eq!(
            github_markdown_links("> **Fixes #42**\n\n- See #9", Some(URL)),
            "> **Fixes [#42](https://github.com/team/repo/issues/42)**\n\n- See [#9](https://github.com/team/repo/issues/9)"
        );
    }

    #[test]
    fn does_nothing_without_repository_context() {
        assert_eq!(github_markdown_links("#123", None), "#123");
    }

    #[test]
    fn does_not_nest_links_inside_html_anchors_or_code() {
        let text = "See <a href=\"https://example.com\">#12</a> and <code>#13</code>, then #14.";
        assert_eq!(
            github_markdown_links(text, Some(URL)),
            "See <a href=\"https://example.com\">#12</a> and <code>#13</code>, then [#14](https://github.com/team/repo/issues/14)."
        );
    }
}
