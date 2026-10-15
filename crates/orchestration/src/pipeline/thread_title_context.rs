//! Port of upstream/packages/provider-core/src/server/threadTitleContext.ts
//! and the `assistantCitationsToPlainText` half of
//! upstream/packages/shared/src/assistantCitations.ts it depends on.
//!
//! Lengths are JS `string.length` (UTF-16 units). A cut inside a surrogate
//! pair drops the whole pair, so a result is never longer than JS's.

use regex::Regex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::LazyLock;
use turin_contracts::assistant_citations::{
    ASSISTANT_CITATION_MAX_COMMENT_LENGTH, ASSISTANT_CITATION_MAX_TEXT_LENGTH, AssistantCitation,
};
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::{js_len, js_trim};
use turin_provider_core::js::take_utf16;

use super::attachment_store::attachment_fields;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadTitleRole {
    User,
    Assistant,
    System,
    Reasoning,
}

impl ThreadTitleRole {
    fn prefix(self) -> &'static str {
        match self {
            Self::User => "USER:\n",
            Self::Assistant => "ASSISTANT:\n",
            Self::System => "SYSTEM:\n",
            Self::Reasoning => "REASONING:\n",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThreadTitleMessage<'a> {
    pub role: ThreadTitleRole,
    pub text: &'a str,
    pub attachments: &'a [ChatAttachment],
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTitleContext {
    pub message: String,
    pub attachments: Vec<ChatAttachment>,
}

const MAX_CONTEXT: i64 = 8_000;
const MAX_MESSAGE: i64 = 2_000;
const OMITTED: &str = "[Earlier content truncated]\n\n";
const TRUNCATED: &str = "\n[Content truncated]\n";

fn len(text: &str) -> i64 {
    js_len(text) as i64
}

/// `text.slice(-units)` in UTF-16 units.
fn last_utf16(text: &str, units: usize) -> &str {
    let total = js_len(text);
    let mut skip = total.saturating_sub(units);
    for (byte, c) in text.char_indices() {
        if skip == 0 {
            return &text[byte..];
        }
        skip = skip.saturating_sub(c.len_utf16());
    }
    ""
}

/// Keep the request and its final constraints when a message is too long.
pub fn limit_title_message(text: &str, budget: i64) -> String {
    if len(text) <= budget {
        return text.to_owned();
    }
    let truncated_len = len(TRUNCATED);
    if budget <= truncated_len {
        return String::new();
    }
    let available = budget - truncated_len;
    let head = (available + 1) / 2;
    let tail = available - head;
    let tail_text = if tail > 0 {
        last_utf16(text, tail as usize)
    } else {
        ""
    };
    format!("{}{TRUNCATED}{tail_text}", take_utf16(text, head as usize))
}

struct Section<'a> {
    index: usize,
    message: ThreadTitleMessage<'a>,
    prefix: &'static str,
}

/// Reserve space for user intent before adding assistant findings, in
/// conversation order.
pub fn format_thread_title_context(messages: &[ThreadTitleMessage<'_>]) -> ThreadTitleContext {
    let sections: Vec<Section<'_>> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            // Thinking traces are working notes, not what the thread is about, and they
            // dwarf the answer they precede. Titling on them would be worse and costlier.
            !(matches!(
                message.role,
                ThreadTitleRole::System | ThreadTitleRole::Reasoning
            ) || (js_trim(message.text).is_empty() && message.attachments.is_empty()))
        })
        .map(|(index, message)| Section {
            index,
            message: *message,
            prefix: message.role.prefix(),
        })
        .collect();
    let mut formatted: HashMap<usize, String> = HashMap::new();
    let mut contents_for = |section: &Section<'_>| -> String {
        formatted
            .entry(section.index)
            .or_insert_with(|| {
                let text = assistant_citations_to_plain_text(section.message.text);
                let text = js_trim(&text);
                let names = section
                    .message
                    .attachments
                    .iter()
                    .map(|attachment| attachment_fields(attachment).name)
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(text.to_owned());
                }
                if !names.is_empty() {
                    parts.push(format!("[Attachments: {names}]"));
                }
                parts.join("\n")
            })
            .clone()
    };
    let mut selected: HashMap<usize, String> = HashMap::new();
    let mut remaining = MAX_CONTEXT - len(OMITTED);
    fn add(
        section: &Section<'_>,
        budget: i64,
        selected: &mut HashMap<usize, String>,
        remaining: &mut i64,
        contents: String,
    ) {
        if selected.contains_key(&section.index) {
            return;
        }
        let limit = budget.min(*remaining) - len(section.prefix) - 2;
        if limit <= len(TRUNCATED) {
            return;
        }
        let contents = limit_title_message(&contents, limit);
        if contents.is_empty() {
            return;
        }
        let text = format!("{}{contents}", section.prefix);
        *remaining -= len(&text) + 2;
        selected.insert(section.index, text);
    }

    let first_user = sections
        .iter()
        .find(|section| section.message.role == ThreadTitleRole::User);
    if let Some(first) = first_user {
        let contents = contents_for(first);
        add(first, MAX_MESSAGE, &mut selected, &mut remaining, contents);
    }
    // Up to 6,000 characters go to user messages. Assistant output cannot evict them.
    for section in sections.iter().rev() {
        if section.message.role == ThreadTitleRole::User {
            let contents = contents_for(section);
            let budget = MAX_MESSAGE.min(remaining - 2_000);
            add(section, budget, &mut selected, &mut remaining, contents);
        }
    }
    for section in sections.iter().rev() {
        if section.message.role == ThreadTitleRole::Assistant {
            let contents = contents_for(section);
            add(
                section,
                MAX_MESSAGE,
                &mut selected,
                &mut remaining,
                contents,
            );
        }
    }
    // Use spare space when the conversation has only a few messages.
    for role in [ThreadTitleRole::User, ThreadTitleRole::Assistant] {
        for section in sections.iter().rev() {
            if section.message.role != role {
                continue;
            }
            let Some(previous) = selected.get(&section.index) else {
                continue;
            };
            let previous_len = len(previous);
            let expanded = format!(
                "{}{}",
                section.prefix,
                limit_title_message(
                    &contents_for(section),
                    previous_len + remaining - len(section.prefix),
                )
            );
            remaining -= len(&expanded) - previous_len;
            selected.insert(section.index, expanded);
        }
    }
    let retained: Vec<&Section<'_>> = sections
        .iter()
        .filter(|section| selected.contains_key(&section.index))
        .collect();
    let truncated = retained.iter().any(|section| {
        selected[&section.index] != format!("{}{}", section.prefix, contents_for(section))
    });
    let first_attachment = first_user.and_then(|section| section.message.attachments.first());
    let first_id = first_attachment.map(|attachment| attachment_fields(attachment).id);
    let recent: Vec<&ChatAttachment> = retained
        .iter()
        .flat_map(|section| section.message.attachments.iter())
        .filter(|attachment| Some(attachment_fields(attachment).id) != first_id)
        .collect();
    let keep = if first_attachment.is_some() { 3 } else { 4 };
    let mut attachments: Vec<ChatAttachment> = first_attachment.into_iter().cloned().collect();
    attachments.extend(
        recent[recent.len().saturating_sub(keep)..]
            .iter()
            .map(|attachment| (*attachment).clone()),
    );
    let omitted = if truncated || retained.len() < sections.len() {
        OMITTED
    } else {
        ""
    };
    let joined = retained
        .iter()
        .map(|section| selected[&section.index].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    ThreadTitleContext {
        message: format!("{omitted}{joined}"),
        attachments,
    }
}

const CITATION_PROTOCOL: &str = "t3-citation:";
const CITATION_HREF_PREFIX: &str = "t3-citation://v1/";
// Percent encoding needs up to nine characters per UTF-16 code unit; 16k covers selectors.
const MAX_CITATION_HREF_LENGTH: usize =
    9 * (ASSISTANT_CITATION_MAX_TEXT_LENGTH + ASSISTANT_CITATION_MAX_COMMENT_LENGTH) + 16_000;

/// Upstream bounds the href run with `{1,N}`; an over-long run then fails to
/// match at all, which the length check in `parse_assistant_citation_href`
/// reproduces (the source is kept either way).
static CITATION_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[Assistant quote\]\((t3-citation://v1/[^\s)]+)\)").expect("citation pattern")
});

/// `decodeURIComponent`; `None` where JS would throw.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn is_digits_1_to_16(value: &str) -> bool {
    (1..=16).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_assistant_citation_href(href: &str) -> Option<AssistantCitation> {
    if !href.starts_with(CITATION_HREF_PREFIX) || js_len(href) > MAX_CITATION_HREF_LENGTH {
        return None;
    }
    let url = url::Url::parse(href).ok()?;
    let path = url.path().strip_prefix('/').unwrap_or(url.path());
    let parts: Vec<&str> = path.split('/').collect();
    if format!("{}:", url.scheme()) != CITATION_PROTOCOL
        || url.host_str() != Some("v1")
        || parts.len() != 3
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.port().is_some()
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return None;
    }
    let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |key: &str| {
        params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    let count = |key: &str| params.iter().filter(|(name, _)| name == key).count();
    let required = ["text", "start", "end", "prefix", "suffix"];
    let comment = get("comment");
    if params.len() != required.len() + usize::from(comment.is_some())
        || required.iter().any(|key| count(key) != 1)
    {
        return None;
    }
    let start = get("start").unwrap_or_default();
    let end = get("end").unwrap_or_default();
    if !is_digits_1_to_16(start) || !is_digits_1_to_16(end) {
        return None;
    }
    let mut value = json!({
        "version": 1,
        "environmentId": decode_uri_component(parts[0])?,
        "threadId": decode_uri_component(parts[1])?,
        "messageId": decode_uri_component(parts[2])?,
        "text": get("text"),
        "start": start.parse::<u64>().ok()?,
        "end": end.parse::<u64>().ok()?,
        "prefix": get("prefix"),
        "suffix": get("suffix"),
    });
    if let Some(comment) = comment {
        value["comment"] = json!(comment);
    }
    serde_json::from_value(value).ok()
}

/// Titles and previews include the selected text and user comment without
/// Markdown escaping.
pub fn assistant_citations_to_plain_text(prompt: &str) -> String {
    CITATION_LINK
        .replace_all(prompt, |captures: &regex::Captures<'_>| {
            let source = &captures[0];
            match parse_assistant_citation_href(&captures[1]) {
                None => source.to_owned(),
                Some(citation) => match citation.comment {
                    None => citation.text,
                    Some(comment) => format!("{}\nComment: {comment}", citation.text),
                },
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests;
