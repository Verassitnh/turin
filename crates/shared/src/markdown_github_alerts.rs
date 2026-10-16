//! Port of upstream/packages/shared/src/markdownGithubAlerts.ts
//!
//! GitHub's blockquote alerts: a quote whose first line is `[!NOTE]` (or TIP, IMPORTANT,
//! WARNING, CAUTION) renders as a titled callout. They are GitHub's own extension rather than GFM,
//! so the parser leaves the marker as literal text in the quote. Upstream lifts the marker off the
//! mdast into a `data-alert` attribute for the blockquote renderer to style, and removes the
//! marker line itself; this port keeps the second half (the app's `Block::Alert` carries the
//! kind, see `markdown_pipeline.rs`).
//!
//! Only a marker with nothing after it on its own line counts, which is GitHub's rule:
//! `> [!NOTE] aside` is an ordinary quote.

use markdown::mdast::Node;

/// `GITHUB_ALERT_MARKER`: `[!NOTE]` and friends on a line of their own. Returns the marker's
/// length (with its line ending) and whether it ended its line.
fn github_alert_marker(text: &str) -> Option<(usize, bool)> {
    let rest = text.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let name = &rest[..close];
    if !["note", "tip", "important", "warning", "caution"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
    {
        return None;
    }
    let consumed = 2 + close + 1;
    let after = &text[consumed..];
    if after.is_empty() {
        Some((consumed, false))
    } else if after.starts_with("\r\n") {
        Some((consumed + 2, true))
    } else if after.starts_with('\n') {
        Some((consumed + 1, true))
    } else {
        None
    }
}

/// `remarkGithubAlerts`: drops the marker line from a quote's first paragraph (post-order, so
/// nested quotes are handled first).
pub fn remark_github_alerts(node: &mut Node) {
    if let Some(children) = node.children_mut() {
        for child in children.iter_mut() {
            remark_github_alerts(child);
        }
    }
    let Node::Blockquote(quote) = node else {
        return;
    };
    let Some(Node::Paragraph(paragraph)) = quote.children.first_mut() else {
        return;
    };
    let Some(Node::Text(text)) = paragraph.children.first_mut() else {
        return;
    };
    let Some((consumed, ends_line)) = github_alert_marker(&text.value) else {
        return;
    };
    let remainder = text.value[consumed..].to_owned();
    if !remainder.is_empty() {
        text.value = remainder;
    } else if ends_line || paragraph.children.len() == 1 {
        paragraph.children.remove(0);
        if paragraph.children.is_empty() {
            quote.children.remove(0);
        }
    }
}
