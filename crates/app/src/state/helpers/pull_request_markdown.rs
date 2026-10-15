//! Port of upstream/packages/client-runtime/src/pullRequestMarkdown.ts

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::js_trim;
use url::Url;

use super::shared_support::{JS_NOT_LINE_TERMINATOR_CLASS, JS_WS_CLASS, is_js_whitespace};

/// What the reader can be told the upload is. The uuid says nothing on its own: the type
/// only appears in the signed redirect GitHub answers with. But the shape does: GitHub
/// writes a dropped image into the body as an `<img>` tag, so a bare attachment link on its
/// own line is what it does with a video. Anything else keeps the neutral name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestAttachmentMedia {
    Video,
    Image,
    Unknown,
}

/// `id` is positional on purpose: the same attachment can be embedded twice in one body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestBodySegment {
    Markdown {
        id: String,
        text: String,
    },
    Attachment {
        id: String,
        url: String,
        media: PullRequestAttachmentMedia,
    },
}

/// Compiles a pattern where `{WS}` stands for the members of JS `\s` and `{NLT}` for the inside
/// of a class matching any non-line-terminator character (JS `.`). Rust's `\s` and `.` differ.
fn js_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{WS}", JS_WS_CLASS)
            .replace("{NLT}", JS_NOT_LINE_TERMINATOR_CLASS),
    )
    .expect("valid regex")
}

static FENCE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^[{WS}]{0,3}((?:`{3,})|(?:~{3,}))([{NLT}]*)$"));
/// How far a `<video>` tag may reach for its closing tag. Real embeds are one to three lines,
/// and the bound keeps an unclosed tag from making each one rescan the rest of the body.
const VIDEO_TAG_MAX_LINES: usize = 8;
/// Four spaces open an indented code block, so its contents stay verbatim markdown.
static INDENTED_CODE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?: {4}|\t)").expect("valid regex"));
static BARE_URL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^<?(https?://[^{WS}]+?)>?$"));
static IMAGE_EXTENSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)\.(?:png|jpe?g|gif|webp|avif|heic|heif|svg)(?:$|[?#])"));
static MARKDOWN_LINK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^\[[^\]]*\]\((https?://[^){WS}]+)\)$"));
static VIDEO_EXTENSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)\.(?:mp4|webm|mov|m4v|ogv)(?:$|[?#])"));
/// A dropped video becomes a bare asset link; a dropped image becomes an `<img>` tag.
static GITHUB_ASSET_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)^https://github\.com/user-attachments/assets/[A-Za-z0-9_-]+$"));
static VIDEO_TAG_SRC_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(r#"(?i)<(?:video|source)(?-u:\b)[^>]*(?-u:\b)src[{WS}]*=[{WS}]*["']([^"']+)["']"#)
});
/// Only a tag that owns its line is an embed; inline, it is prose the renderer should keep.
static STANDALONE_VIDEO_TAG_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)^[{WS}]*<video(?-u:\b)"));
static VIDEO_TAG_END_PATTERN: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)</video>[{WS}]*$"));

/// Anything else (`javascript:`, `data:`, a relative path) is not an upload to link out to.
fn is_web_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|parsed| parsed.scheme() == "https" || parsed.scheme() == "http")
}

fn attachment_from_line(line: &str) -> Option<(String, PullRequestAttachmentMedia)> {
    let trimmed = js_trim(line);
    let url = BARE_URL_PATTERN
        .captures(trimmed)
        .or_else(|| MARKDOWN_LINK_PATTERN.captures(trimmed))
        .and_then(|captures| captures.get(1))?
        .as_str();
    if !is_web_url(url) {
        return None;
    }
    if VIDEO_EXTENSION_PATTERN.is_match(url) || GITHUB_ASSET_PATTERN.is_match(url) {
        return Some((url.to_owned(), PullRequestAttachmentMedia::Video));
    }
    if IMAGE_EXTENSION_PATTERN.is_match(url) {
        return Some((url.to_owned(), PullRequestAttachmentMedia::Image));
    }
    None
}

// Blank lines around a run are dropped, but never leading spaces: four of them open an
// indented code block, so trimming a run would silently turn code into prose.
fn flush_markdown(markdown: &mut Vec<&str>, segments: &mut Vec<PullRequestBodySegment>) {
    let joined = markdown.join("\n");
    markdown.clear();
    let text = joined
        .trim_start_matches('\n')
        .trim_end_matches(is_js_whitespace);
    if !js_trim(text).is_empty() {
        segments.push(PullRequestBodySegment::Markdown {
            id: format!("markdown:{}", segments.len()),
            text: text.to_owned(),
        });
    }
}

/// Lifts standalone media links and video tags out of prose for both clients.
/// Ordinary links and code examples remain markdown. Images already embedded
/// with markdown or HTML keep their original renderer.
pub fn split_pull_request_body(body: &str) -> Vec<PullRequestBodySegment> {
    let mut segments: Vec<PullRequestBodySegment> = Vec::new();
    let mut markdown: Vec<&str> = Vec::new();
    let mut open_fence: Option<String> = None;

    let lines: Vec<&str> = body.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if let Some(fence_match) = FENCE_PATTERN.captures(line) {
            let fence = fence_match.get(1).map_or("", |m| m.as_str());
            let rest = fence_match.get(2).map_or("", |m| m.as_str());
            // A fence closes only on the same marker, at least as long as the one that opened it,
            // and with nothing after it: `~~~` cannot end a ``` block, and ```` ```ts ```` is an
            // info string opening a nested fence, not a close.
            let closes = open_fence.as_deref().is_some_and(|open| {
                fence.chars().next() == open.chars().next()
                    && fence.len() >= open.len()
                    && js_trim(rest).is_empty()
            });
            if open_fence.is_none() {
                open_fence = Some(fence.to_owned());
            } else if closes {
                open_fence = None;
            }
            markdown.push(line);
            continue;
        }
        if open_fence.is_some() || INDENTED_CODE_PATTERN.is_match(line) {
            markdown.push(line);
            continue;
        }

        if let Some((url, media)) = attachment_from_line(line) {
            flush_markdown(&mut markdown, &mut segments);
            segments.push(PullRequestBodySegment::Attachment {
                id: format!("attachment:{}", segments.len()),
                url,
                media,
            });
            continue;
        }

        if !STANDALONE_VIDEO_TAG_PATTERN.is_match(line) {
            markdown.push(line);
            continue;
        }
        // A tag can span lines, so look ahead for its close: bounded, and without building the
        // joined text until one is actually found.
        let start = index - 1;
        let last_candidate = (start + VIDEO_TAG_MAX_LINES).min(lines.len()) - 1;
        let mut cursor = start;
        while cursor < last_candidate && !VIDEO_TAG_END_PATTERN.is_match(lines[cursor]) {
            cursor += 1;
        }
        let source = if VIDEO_TAG_END_PATTERN.is_match(lines[cursor]) {
            VIDEO_TAG_SRC_PATTERN
                .captures(&lines[start..=cursor].join("\n"))
                .and_then(|captures| captures.get(1).map(|m| m.as_str().to_owned()))
        } else {
            None
        };
        match source {
            Some(source) if is_web_url(&source) => {
                flush_markdown(&mut markdown, &mut segments);
                segments.push(PullRequestBodySegment::Attachment {
                    id: format!("attachment:{}", segments.len()),
                    url: source,
                    // The author wrote the tag, so this one is a video whatever the URL looks like.
                    media: PullRequestAttachmentMedia::Video,
                });
                index = cursor + 1;
            }
            _ => {
                // Unclosed, or nothing linkable in it: prose. Only this line is consumed, so the
                // lines that were looked at are still parsed on their own terms.
                markdown.push(line);
            }
        }
    }

    flush_markdown(&mut markdown, &mut segments);
    segments
}

#[cfg(test)]
mod tests {
    use super::PullRequestAttachmentMedia::{Image, Video};
    use super::*;

    fn markdown(id: usize, text: &str) -> PullRequestBodySegment {
        PullRequestBodySegment::Markdown {
            id: format!("markdown:{id}"),
            text: text.to_owned(),
        }
    }

    fn attachment(
        id: usize,
        url: &str,
        media: PullRequestAttachmentMedia,
    ) -> PullRequestBodySegment {
        PullRequestBodySegment::Attachment {
            id: format!("attachment:{id}"),
            url: url.to_owned(),
            media,
        }
    }

    fn assert_single_markdown(body: &str) {
        assert_eq!(
            split_pull_request_body(body),
            vec![markdown(0, body)],
            "{body}"
        );
    }

    #[test]
    fn keeps_a_plain_body_as_a_single_markdown_run() {
        assert_eq!(
            split_pull_request_body("## What changed\n\nSome prose."),
            vec![markdown(0, "## What changed\n\nSome prose.")]
        );
    }

    // GitHub writes a dropped image into the body as an `<img>` tag, so a bare attachment link on
    // its own line is the shape it uses for a video: every one sampled in the wild was one.
    #[test]
    fn lifts_a_dropped_video_out_and_keeps_the_prose_around_it() {
        let url = "https://github.com/user-attachments/assets/2f8c1a90-1b2c-4d5e-8f90-abcdef123456";
        assert_eq!(
            split_pull_request_body(&format!("Before\n\n{url}\n\nAfter")),
            vec![
                markdown(0, "Before"),
                attachment(1, url, Video),
                markdown(2, "After")
            ]
        );
    }

    #[test]
    fn names_a_bare_link_to_a_video_file_a_video() {
        assert_eq!(
            split_pull_request_body("https://example.com/demo.mp4?raw=1"),
            vec![attachment(0, "https://example.com/demo.mp4?raw=1", Video)]
        );
    }

    #[test]
    fn reads_the_source_out_of_a_video_tag_including_a_multi_line_one() {
        assert_eq!(
            split_pull_request_body(
                "<video controls>\n  <source src=\"https://example.com/a.webm\">\n</video>"
            ),
            vec![attachment(0, "https://example.com/a.webm", Video)]
        );
    }

    #[test]
    fn leaves_a_dropped_image_as_markdown_so_it_still_renders_as_an_image() {
        assert_single_markdown(
            "![shot](https://github.com/user-attachments/assets/2f8c1a90-1b2c-4d5e-8f90-ab)",
        );
    }

    #[test]
    fn leaves_the_img_tag_github_writes_for_a_dropped_image_as_markdown() {
        assert_single_markdown(
            "<img width=\"1414\" alt=\"image\" src=\"https://github.com/user-attachments/assets/7195f963-51a9-4331-be74-3e06be760422\" />",
        );
    }

    #[test]
    fn leaves_an_ordinary_link_alone_whether_it_is_bare_or_written_as_markdown() {
        assert_single_markdown("https://example.com/page\n\n[the docs](https://example.com/docs)");
    }

    #[test]
    fn never_lifts_a_link_inside_fenced_code_out_of_it() {
        assert_single_markdown("```\nhttps://example.com/demo.mp4\n```");
    }

    #[test]
    fn does_not_let_a_different_fence_marker_close_an_open_fence() {
        assert_single_markdown("```\n~~~\nhttps://example.com/demo.mp4\n```");
    }

    #[test]
    fn keeps_a_video_tag_that_shares_its_line_with_prose_as_markdown() {
        assert_single_markdown("Before <video src=\"https://example.com/a.mp4\"></video> after");
    }

    #[test]
    fn keeps_the_indentation_that_opens_a_code_block() {
        assert_single_markdown("    const answer = 42;");
    }

    #[test]
    fn treats_an_info_string_fence_as_a_nested_opener_not_a_close() {
        assert_single_markdown("```\n```javascript\nhttps://example.com/demo.mp4\n```");
    }

    #[test]
    fn leaves_an_indented_code_line_alone_even_when_it_is_only_a_video_link() {
        assert_single_markdown("    https://example.com/demo.mp4");
    }

    #[test]
    fn leaves_an_unclosed_video_tag_as_prose_without_eating_the_rest_of_the_body() {
        assert_single_markdown(
            "<video src=\"https://example.com/a.mp4\">\nstill prose\nmore prose",
        );
    }

    #[test]
    fn stays_linear_when_the_body_is_full_of_unclosed_video_tags() {
        let body = vec!["<video src=\"https://example.com/a.mp4\">"; 4_000].join("\n");
        let started_at = std::time::Instant::now();
        assert_eq!(split_pull_request_body(&body).len(), 1);
        assert!(started_at.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn refuses_a_non_http_source_rather_than_making_a_link_out_of_it() {
        assert_single_markdown("<video src=\"javascript:alert(1)\"></video>");
    }

    #[test]
    fn recognizes_media_on_custom_hosts_without_a_request() {
        for (body, media) in [
            (
                "https://drop.example.com/capture.PNG?token=abc#preview",
                Image,
            ),
            ("[Recording](https://drop.example.com/recording.mov)", Video),
            ("<https://drop.example.com/shot.webp>", Image),
        ] {
            let segments = split_pull_request_body(body);
            assert_eq!(segments.len(), 1, "{body}");
            match &segments[0] {
                PullRequestBodySegment::Attachment { media: found, .. } => {
                    assert_eq!(*found, media, "{body}")
                }
                other => panic!("expected an attachment for {body}, got {other:?}"),
            }
        }
    }

    #[test]
    fn keeps_inline_links_and_media_examples_in_code_as_prose() {
        assert_single_markdown(
            "See [recording](https://drop.example.com/a.mp4) for context.\n\n```md\nhttps://drop.example.com/a.png\n```",
        );
    }
}
