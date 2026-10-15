//! Port of upstream/packages/shared/src/threadLinks.ts

use crate::js_string::{decode_uri_component, is_js_whitespace_char};
use turin_contracts::base_schemas::{ThreadId, js_trim};

/// Agents mention another thread in Markdown as `[title](t3-thread://v1/<threadId>)`. The id is
/// the reference and resolves in the environment of the message that holds it. Titles change, so
/// clients show the thread's current title; the label only stands in for a thread they cannot see.
pub const THREAD_LINK_PROTOCOL: &str = "t3-thread";
const THREAD_LINK_HREF_PREFIX: &str = "t3-thread://v1/";

/// The id as written. Thread ids can hold percent escapes of their own, so it is not decoded.
pub fn parse_thread_link_href(href: &str) -> Option<ThreadId> {
    let id = href.strip_prefix(THREAD_LINK_HREF_PREFIX)?;
    ThreadId::new(id).ok()
}

/// Agents often percent-encode the id anyway. When the id as written names no thread, clients try
/// this decoded form. None when decoding changes nothing or fails.
pub fn percent_decoded_thread_link_id(thread_id: &ThreadId) -> Option<ThreadId> {
    let decoded = decode_uri_component(thread_id.as_str()).ok()?;
    if decoded == thread_id.as_str() {
        return None;
    }
    ThreadId::new(decoded).ok()
}

/// A thread link whose label survives Markdown: no brackets, backslashes, or line breaks.
pub fn format_thread_link(thread_id: &str, label: &str) -> String {
    let spaced: String = label
        .chars()
        .map(|ch| match ch {
            '[' | ']' | '\\' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect();
    // JS `replace(/\s+/g, " ")`
    let mut collapsed = String::with_capacity(spaced.len());
    let mut in_whitespace = false;
    for ch in spaced.chars() {
        if is_js_whitespace_char(ch) {
            if !in_whitespace {
                collapsed.push(' ');
            }
            in_whitespace = true;
        } else {
            collapsed.push(ch);
            in_whitespace = false;
        }
    }
    let cleaned = js_trim(&collapsed);
    let text = if cleaned.is_empty() {
        thread_id
    } else {
        cleaned
    };
    format!("[{text}]({THREAD_LINK_HREF_PREFIX}{thread_id})")
}

pub fn has_thread_links(markdown: &str) -> bool {
    markdown.contains(&format!("]({THREAD_LINK_HREF_PREFIX}"))
}

/// Relabels each thread link with `title(threadId)`, pointing it at the thread that title came
/// from. A link it returns nothing for keeps its label.
pub fn relabel_thread_links(markdown: &str, title: impl Fn(&ThreadId) -> Option<String>) -> String {
    if !has_thread_links(markdown) {
        return markdown.to_owned();
    }
    let chars: Vec<char> = markdown.chars().collect();
    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0;
    for hit in scan_outside_code(&chars) {
        let Some(href) = hit.href else {
            continue;
        };
        out.extend(&chars[cursor..hit.start]);
        let source: String = chars[hit.start..hit.end].iter().collect();
        out.push_str(&relabel_one(&href, &source, &title));
        cursor = hit.end;
    }
    out.extend(&chars[cursor..]);
    out
}

fn relabel_one(href: &str, source: &str, title: &impl Fn(&ThreadId) -> Option<String>) -> String {
    let Some(written) = parse_thread_link_href(href) else {
        return source.to_owned();
    };
    // The decoded id only stands in when the id as written names no thread.
    let decoded = percent_decoded_thread_link_id(&written);
    let thread_id = match decoded {
        Some(decoded) if title(&written).is_none() && title(&decoded).is_some() => decoded,
        _ => written,
    };
    match title(&thread_id).map(|label| js_trim(&label).to_owned()) {
        Some(label) if !label.is_empty() => format_thread_link(thread_id.as_str(), &label),
        _ => source.to_owned(),
    }
}

struct Hit {
    start: usize,
    end: usize,
    /// Only thread-link matches carry an href; code spans and fences are skipped over untouched.
    href: Option<String>,
}

/// Port of the upstream alternation
/// `(fence)|(span)|\[[^\]\n]*\]\((t3-thread://v1/[^\s)]+)\)`, scanned left to right like
/// `String.prototype.replace` with `/g`. Code comes first so a link written inside a code span or
/// fence is skipped. The regex engine has no backreferences, so the fence and span closers are
/// matched by hand.
fn scan_outside_code(chars: &[char]) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let matched = fence_end(chars, index)
            .map(|end| (end, None))
            .or_else(|| span_end(chars, index).map(|end| (end, None)))
            .or_else(|| href_link(chars, index).map(|(end, href)| (end, Some(href))));
        match matched {
            Some((end, href)) => {
                hits.push(Hit {
                    start: index,
                    end,
                    href,
                });
                index = end;
            }
            None => index += 1,
        }
    }
    hits
}

fn run_length(chars: &[char], start: usize, ch: char) -> usize {
    chars[start..].iter().take_while(|&&c| c == ch).count()
}

/// `(`{3,}|~{3,})[\s\S]*?(?:\2|$)`: the closer repeats the opening run, or the input ends.
fn fence_end(chars: &[char], start: usize) -> Option<usize> {
    let ch = chars[start];
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = run_length(chars, start, ch);
    if run < 3 {
        return None;
    }
    let body = start + run;
    let delimiter = &chars[start..body];
    (body..chars.len())
        .find(|&at| chars[at..].starts_with(delimiter))
        .map(|at| at + run)
        .or(Some(chars.len()))
}

/// `(`+)[^\n]*?\4`: the closer is the same backtick run, on the same line. Like the regex, a
/// shorter opening run is retried when the full run has no closer.
fn span_end(chars: &[char], start: usize) -> Option<usize> {
    if chars[start] != '`' {
        return None;
    }
    let run = run_length(chars, start, '`');
    (1..=run).rev().find_map(|width| {
        let mut at = start + width;
        while at + width <= chars.len() {
            if chars[at..at + width].iter().all(|&c| c == '`') {
                return Some(at + width);
            }
            if chars[at] == '\n' {
                break;
            }
            at += 1;
        }
        None
    })
}

/// `\[[^\]\n]*\]\((t3-thread://v1/[^\s)]+)\)`
fn href_link(chars: &[char], start: usize) -> Option<(usize, String)> {
    if chars[start] != '[' {
        return None;
    }
    let close = (start + 1..chars.len()).find(|&at| chars[at] == ']' || chars[at] == '\n')?;
    if chars[close] != ']' || chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let href_start = close + 2;
    let prefix: Vec<char> = THREAD_LINK_HREF_PREFIX.chars().collect();
    if !chars[href_start..].starts_with(&prefix) {
        return None;
    }
    let id_start = href_start + prefix.len();
    let id_end = (id_start..chars.len())
        .find(|&at| is_js_whitespace_char(chars[at]) || chars[at] == ')')
        .unwrap_or(chars.len());
    if id_end == id_start || chars.get(id_end) != Some(&')') {
        return None;
    }
    let href: String = chars[href_start..id_end].iter().collect();
    Some((id_end + 1, href))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ThreadId {
        ThreadId::new(value).unwrap()
    }

    #[test]
    fn takes_the_thread_id_verbatim_percent_escapes_included() {
        assert_eq!(
            parse_thread_link_href("t3-thread://v1/mcp:1234").map(|id| id.as_str().to_owned()),
            Some("mcp:1234".to_owned())
        );
        assert_eq!(
            parse_thread_link_href("t3-thread://v1/thread:delegated-task:mcp%3A1")
                .map(|id| id.as_str().to_owned()),
            Some("thread:delegated-task:mcp%3A1".to_owned())
        );
    }

    #[test]
    fn rejects_other_links_and_an_empty_id() {
        assert_eq!(parse_thread_link_href("https://t3.codes"), None);
        assert_eq!(parse_thread_link_href("t3-thread://v1/"), None);
        assert_eq!(parse_thread_link_href("t3-thread://v1/ "), None);
    }

    #[test]
    fn resolves_a_percent_encoded_id_when_the_id_as_written_names_no_thread() {
        let titles = [
            ("thread:project:1", "Decoded"),
            ("provider%3A1", "Literal escape"),
        ];
        let lookup = |thread_id: &ThreadId| {
            titles
                .iter()
                .find(|(key, _)| *key == thread_id.as_str())
                .map(|(_, title)| (*title).to_owned())
        };
        assert_eq!(
            relabel_thread_links(
                "[a](t3-thread://v1/thread%3Aproject%3A1) [b](t3-thread://v1/provider%3A1)",
                lookup,
            ),
            "[Decoded](t3-thread://v1/thread:project:1) [Literal escape](t3-thread://v1/provider%3A1)"
        );
        // A thread with an empty title still exists, so its link is not redirected.
        let untitled = [("a%3A1", ""), ("a:1", "Other")];
        let lookup = |thread_id: &ThreadId| {
            untitled
                .iter()
                .find(|(key, _)| *key == thread_id.as_str())
                .map(|(_, title)| (*title).to_owned())
        };
        assert_eq!(
            relabel_thread_links("[Kept](t3-thread://v1/a%3A1)", lookup),
            "[Kept](t3-thread://v1/a%3A1)"
        );
    }

    #[test]
    fn leaves_links_inside_code_spans_and_fences_as_written() {
        let markdown = [
            "Live [old](t3-thread://v1/t1), literal `[old](t3-thread://v1/t1)`.",
            "```md",
            "[old](t3-thread://v1/t1)",
            "```",
            "After [old](t3-thread://v1/t1)",
        ]
        .join("\n");
        assert_eq!(
            relabel_thread_links(&markdown, |_| Some("New".to_owned())),
            [
                "Live [New](t3-thread://v1/t1), literal `[old](t3-thread://v1/t1)`.",
                "```md",
                "[old](t3-thread://v1/t1)",
                "```",
                "After [New](t3-thread://v1/t1)",
            ]
            .join("\n")
        );
    }

    #[test]
    fn formats_a_label_that_would_otherwise_break_the_markdown_link() {
        assert_eq!(
            format_thread_link("t1", "Fix [ci] \\ build"),
            "[Fix ci build](t3-thread://v1/t1)"
        );
        assert_eq!(format_thread_link("t1", " ] "), "[t1](t3-thread://v1/t1)");
    }

    #[test]
    fn relabels_links_with_the_current_title_and_leaves_unknown_threads_alone() {
        assert_eq!(
            relabel_thread_links(
                "See [Old name](t3-thread://v1/renamed) and [Gone](t3-thread://v1/deleted).",
                |thread_id| (thread_id == &id("renamed"))
                    .then(|| "Fix [the] build\nnow".to_owned()),
            ),
            "See [Fix the build now](t3-thread://v1/renamed) and [Gone](t3-thread://v1/deleted)."
        );
    }
}
