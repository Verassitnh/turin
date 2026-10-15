//! Port of upstream/packages/shared/src/githubMarkdownLinks.ts
//!
//! Upstream matches references with a regex that uses lookbehind and
//! lookahead, which the `regex` crate does not support. The matcher below
//! reproduces the regex's alternation order and backtracking by hand, so a
//! rejected greedy match falls back to the same shorter match JS would try.

use markdown::mdast::Node;
use markdown::{ParseOptions, to_mdast};

use turin_contracts::base_schemas::is_js_whitespace;

/// Link GitHub references in prose without rewriting code, URLs, HTML, or existing links.
pub fn github_markdown_links(markdown: &str, pull_request_url: Option<&str>) -> String {
    let Some(pull_request_url) = pull_request_url.filter(|value| !value.is_empty()) else {
        return markdown.to_owned();
    };
    if !has_reference_candidate(markdown) {
        return markdown.to_owned();
    }
    let Ok(url) = url::Url::parse(pull_request_url) else {
        return markdown.to_owned();
    };
    let mut segments = url.path().split('/').filter(|segment| !segment.is_empty());
    let (Some(owner), Some(repo)) = (segments.next(), segments.next()) else {
        return markdown.to_owned();
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return markdown.to_owned();
    }
    let origin = url.origin().ascii_serialization();

    let Ok(root) = to_mdast(markdown, &ParseOptions::gfm()) else {
        return markdown.to_owned();
    };

    let mut edits: Vec<Edit> = Vec::new();
    let mut context = Context {
        markdown,
        owner,
        repo,
        origin: &origin,
        edits: &mut edits,
    };
    context.visit(&root);

    let mut result = markdown.to_owned();
    for edit in edits.iter().rev() {
        result = format!(
            "{}{}{}",
            &result[..edit.start],
            edit.value,
            &result[edit.end..]
        );
    }
    result
}

struct Edit {
    start: usize,
    end: usize,
    value: String,
}

struct Context<'a> {
    markdown: &'a str,
    owner: &'a str,
    repo: &'a str,
    origin: &'a str,
    edits: &'a mut Vec<Edit>,
}

impl Context<'_> {
    fn visit(&mut self, node: &Node) {
        match node {
            Node::Link(_)
            | Node::LinkReference(_)
            | Node::Image(_)
            | Node::ImageReference(_)
            | Node::Code(_)
            | Node::InlineCode(_)
            | Node::Html(_)
            | Node::Definition(_) => return,
            Node::Text(_) => {
                self.visit_text(node);
                return;
            }
            _ => {}
        }
        let Some(children) = node.children() else {
            return;
        };
        // An HTML anchor or code element already wraps its content, so
        // references inside it stay as they are.
        let mut protected_html_depth: i64 = 0;
        for child in children {
            if let Node::Html(_) = child {
                let Some(position) = child.position() else {
                    continue;
                };
                let tag = &self.markdown[position.start.offset..position.end.offset];
                protected_html_depth = html_protected_depth(tag, protected_html_depth);
            } else if protected_html_depth == 0 {
                self.visit(child);
            }
        }
    }

    fn visit_text(&mut self, node: &Node) {
        let Some(position) = node.position() else {
            return;
        };
        let start = position.start.offset;
        let end = position.end.offset;
        // Work on source slices so escaped references and Markdown offsets stay intact.
        let source = &self.markdown[start..end];
        for reference in find_references(source) {
            if reference.sha.is_some_and(|sha| !has_hex_letter(sha)) {
                continue;
            }
            let path = if let Some(number) = reference.number {
                format!(
                    "{}/issues/{}",
                    self.repository(reference.issue_repo),
                    number
                )
            } else if let Some(sha) = reference.sha {
                format!("{}/commit/{}", self.repository(reference.commit_repo), sha)
            } else if let Some(mention) = reference.mention {
                match mention.split_once('/') {
                    Some((team_org, team)) => format!("orgs/{team_org}/teams/{team}"),
                    None => mention.to_owned(),
                }
            } else {
                continue;
            };
            self.edits.push(Edit {
                start: start + reference.start,
                end: start + reference.start + reference.label.len(),
                value: format!("[{}]({}/{})", reference.label, self.origin, path),
            });
        }
    }

    fn repository(&self, qualifier: Option<&str>) -> String {
        match qualifier {
            Some(qualifier) if qualifier.contains('/') => qualifier.to_owned(),
            Some(qualifier) => format!("{qualifier}/{}", self.repo),
            None => format!("{}/{}", self.owner, self.repo),
        }
    }
}

/// `/[#@]|\b[a-f\d]{7,40}\b/i`: a `#` or `@`, or a whole word of 7 to 40 hex digits.
fn has_reference_candidate(markdown: &str) -> bool {
    if markdown.contains(['#', '@']) {
        return true;
    }
    markdown
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|word| {
            (7..=40).contains(&word.len()) && word.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn has_hex_letter(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| matches!(byte.to_ascii_lowercase(), b'a'..=b'f'))
}

/// `\w` without the `u` flag: ASCII letters, digits, and underscore only.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// `[\w.-]`
fn is_repo_byte(byte: u8) -> bool {
    is_word_byte(byte) || byte == b'.' || byte == b'-'
}

/// `[\w-]`
fn is_owner_byte(byte: u8) -> bool {
    is_word_byte(byte) || byte == b'-'
}

/// `[a-z\d-]`, case-insensitive.
fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

/// Index just past the maximal run of `class` bytes starting at `from`.
fn run_end(source: &[u8], from: usize, class: fn(u8) -> bool) -> usize {
    let mut end = from;
    while end < source.len() && class(source[end]) {
        end += 1;
    }
    end
}

/// `(?![\w])` at `index`.
fn not_followed_by_word(source: &[u8], index: usize) -> bool {
    source.get(index).is_none_or(|byte| !is_word_byte(*byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reference<'a> {
    start: usize,
    end: usize,
    label: &'a str,
    issue_repo: Option<&'a str>,
    number: Option<&'a str>,
    commit_repo: Option<&'a str>,
    sha: Option<&'a str>,
    mention: Option<&'a str>,
}

/// `matchAll` over the source slice with the upstream reference regex.
fn find_references(source: &str) -> Vec<Reference<'_>> {
    let bytes = source.as_bytes();
    let mut references = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        if let Some(found) = match_reference_at(source, position) {
            position = found.end;
            references.push(found);
        } else {
            position += source[position..].chars().next().map_or(1, char::len_utf8);
        }
    }
    references
}

/// The regex at one index: lookbehind, then the issue, commit, and mention
/// alternatives in that order, each with its trailing `(?![\w])`.
fn match_reference_at(source: &str, start: usize) -> Option<Reference<'_>> {
    let bytes = source.as_bytes();
    // The lookbehind sees only the slice, so a slice start always passes.
    if start > 0 {
        let before = bytes[start - 1];
        if is_word_byte(before) || before == b'/' || before == b'\\' {
            return None;
        }
    }
    let (end, fields) = match_issue(bytes, start)
        .or_else(|| match_commit(bytes, start))
        .or_else(|| match_mention(bytes, start))?;
    Some(Reference {
        start,
        end,
        label: &source[start..end],
        issue_repo: fields.issue_repo,
        number: fields.number,
        commit_repo: fields.commit_repo,
        sha: fields.sha,
        mention: fields.mention,
    })
}

#[derive(Debug, Default, Clone, Copy)]
struct Fields<'a> {
    issue_repo: Option<&'a str>,
    number: Option<&'a str>,
    commit_repo: Option<&'a str>,
    sha: Option<&'a str>,
    mention: Option<&'a str>,
}

/// Index of each separator the optional repository prefix can end at, in regex
/// order: `owner/repo` (dotted) first, then a bare owner name. The prefix spans
/// `start..separator`.
fn repo_prefix_separators(bytes: &[u8], start: usize, separator: u8) -> Vec<usize> {
    let mut separators = Vec::new();
    // `[\w.-]+\/[\w.-]+`
    let first_end = run_end(bytes, start, is_repo_byte);
    if first_end > start && bytes.get(first_end) == Some(&b'/') {
        let second_end = run_end(bytes, first_end + 1, is_repo_byte);
        if second_end > first_end + 1 && bytes.get(second_end) == Some(&separator) {
            separators.push(second_end);
        }
    }
    // `[\w-]+`
    let owner_end = run_end(bytes, start, is_owner_byte);
    if owner_end > start && bytes.get(owner_end) == Some(&separator) {
        separators.push(owner_end);
    }
    separators
}

/// `(?:(repo)?#([1-9]\d*))` plus the trailing lookahead.
fn match_issue(bytes: &[u8], start: usize) -> Option<(usize, Fields<'_>)> {
    let mut candidates: Vec<(usize, Option<usize>)> = repo_prefix_separators(bytes, start, b'#')
        .into_iter()
        .map(|hash| (hash, Some(hash)))
        .collect();
    if bytes.get(start) == Some(&b'#') {
        candidates.push((start, None));
    }
    for (hash, repo_end) in candidates {
        let number_start = hash + 1;
        if !bytes
            .get(number_start)
            .is_some_and(|byte| (b'1'..=b'9').contains(byte))
        {
            continue;
        }
        let number_end = run_end(bytes, number_start, |byte| byte.is_ascii_digit());
        if !not_followed_by_word(bytes, number_end) {
            continue;
        }
        return Some((
            number_end,
            Fields {
                issue_repo: repo_end.map(|end| slice(bytes, start, end)),
                number: Some(slice(bytes, number_start, number_end)),
                ..Fields::default()
            },
        ));
    }
    None
}

/// `(?:(repo)@)?([a-f\d]{7,40})(?![\w])`
fn match_commit(bytes: &[u8], start: usize) -> Option<(usize, Fields<'_>)> {
    let mut candidates: Vec<(usize, Option<usize>)> = repo_prefix_separators(bytes, start, b'@')
        .into_iter()
        .map(|at| (at + 1, Some(at)))
        .collect();
    candidates.push((start, None));
    for (sha_start, repo_end) in candidates {
        // `[a-f\d]{7,40}` must take the whole hex run: any shorter match is
        // followed by another hex digit, which the lookahead rejects.
        let sha_end = run_end(bytes, sha_start, |byte| byte.is_ascii_hexdigit());
        let length = sha_end - sha_start;
        if !(7..=40).contains(&length) || !not_followed_by_word(bytes, sha_end) {
            continue;
        }
        return Some((
            sha_end,
            Fields {
                commit_repo: repo_end.map(|end| slice(bytes, start, end)),
                sha: Some(slice(bytes, sha_start, sha_end)),
                ..Fields::default()
            },
        ));
    }
    None
}

/// `@([a-z\d](?:[a-z\d-]*[a-z\d])?(?:\/[a-z\d][a-z\d-]*)?)(?![\w])`
fn match_mention(bytes: &[u8], start: usize) -> Option<(usize, Fields<'_>)> {
    if bytes.get(start) != Some(&b'@') {
        return None;
    }
    let name_start = start + 1;
    if !bytes.get(name_start).is_some_and(u8::is_ascii_alphanumeric) {
        return None;
    }
    let slug_end = run_end(bytes, name_start, is_slug_byte);
    // Longer names first; the bare first character is the last resort.
    let mut name_ends: Vec<usize> = (name_start + 2..=slug_end)
        .rev()
        .filter(|&end| bytes[end - 1].is_ascii_alphanumeric())
        .collect();
    name_ends.push(name_start + 1);
    for name_end in name_ends {
        // The optional `/segment` is greedy, so each of its lengths is tried
        // before the segment is skipped.
        if bytes.get(name_end) == Some(&b'/')
            && bytes
                .get(name_end + 1)
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            let segment_run_end = run_end(bytes, name_end + 2, is_slug_byte);
            for segment_end in (name_end + 2..=segment_run_end).rev() {
                if not_followed_by_word(bytes, segment_end) {
                    return Some((
                        segment_end,
                        Fields {
                            mention: Some(slice(bytes, name_start, segment_end)),
                            ..Fields::default()
                        },
                    ));
                }
            }
        }
        if not_followed_by_word(bytes, name_end) {
            return Some((
                name_end,
                Fields {
                    mention: Some(slice(bytes, name_start, name_end)),
                    ..Fields::default()
                },
            ));
        }
    }
    None
}

/// A slice between two ASCII-delimited positions of `bytes`.
fn slice(bytes: &[u8], start: usize, end: usize) -> &str {
    // Every boundary here sits next to an ASCII byte, so it is a char boundary.
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

/// Tracks `<a>`, `<code>`, `<pre>`, `<script>`, and `<style>` tags inside one
/// HTML node, so references inside them are left alone.
fn html_protected_depth(tag_source: &str, depth: i64) -> i64 {
    let bytes = tag_source.as_bytes();
    let mut depth = depth;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let mut name_start = index + 1;
        let closing = bytes.get(name_start) == Some(&b'/');
        if closing {
            name_start += 1;
        }
        let matched_name = ["a", "code", "pre", "script", "style"]
            .into_iter()
            .find(|name| {
                let end = name_start + name.len();
                end <= bytes.len()
                    && bytes[name_start..end].eq_ignore_ascii_case(name.as_bytes())
                    && tag_name_ends(tag_source, end)
            });
        let Some(name) = matched_name else {
            index += 1;
            continue;
        };
        let Some(close_offset) = tag_source[name_start + name.len()..].find('>') else {
            index += 1;
            continue;
        };
        depth = (depth + if closing { -1 } else { 1 }).max(0);
        index = name_start + name.len() + close_offset + 1;
    }
    depth
}

/// `(?=[\s>])` at `end`.
fn tag_name_ends(source: &str, end: usize) -> bool {
    source[end..]
        .chars()
        .next()
        .is_some_and(|ch| ch == '>' || is_js_whitespace(ch))
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

    #[test]
    fn backtracks_a_mention_to_the_last_boundary_before_a_word_character() {
        // `@foo-bar_` cannot end before `_`, so the name shrinks to `foo`, which
        // is followed by a hyphen (not a word character).
        assert_eq!(
            find_references("@foo-bar_")
                .into_iter()
                .map(|reference| reference.label)
                .collect::<Vec<_>>(),
            vec!["@foo"]
        );
    }
}
