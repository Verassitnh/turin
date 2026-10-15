//! Port of upstream/apps/server/src/orchestration-v2/assistantStreaming.ts

use serde_json::Value;
use std::collections::HashMap;
use turin_contracts::settings::ResponseStreamingMode;

fn list_item_start(line: &str) -> bool {
    let line = line.trim_start_matches([' ', '\t']);
    let mut chars = line.chars();
    if matches!(chars.next(), Some('-' | '*' | '+')) {
        return matches!(chars.next(), Some(' ' | '\t'));
    }
    let digits = line
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (1..=9).contains(&digits)
        && matches!(line.as_bytes().get(digits), Some(b'.' | b')'))
        && matches!(line.as_bytes().get(digits + 1), Some(b' ' | b'\t'))
}

fn heading(line: &str) -> bool {
    let line = line
        .strip_prefix("   ")
        .or_else(|| line.strip_prefix("  "))
        .or_else(|| line.strip_prefix(' '))
        .unwrap_or(line);
    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&hashes) && matches!(line.as_bytes().get(hashes), None | Some(b' ' | b'\t'))
}

fn section_title(line: &str) -> bool {
    if heading(line) {
        return true;
    }
    let line = line.trim_start_matches(' ');
    if !line.starts_with("**") {
        return false;
    }
    let body = line.strip_suffix(':').unwrap_or(line);
    let Some(body) = body.strip_suffix("**") else {
        return false;
    };
    !body.is_empty() && !body.contains("**")
}

fn fence(line: &str) -> Option<(&str, usize)> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    let marker = line.get(indent..)?;
    let symbol = marker.as_bytes().first()?;
    if *symbol != b'`' && *symbol != b'~' {
        return None;
    }
    let count = marker.bytes().take_while(|byte| *byte == *symbol).count();
    (count >= 3).then_some((&line[indent..indent + count], indent))
}

/// Returns a UTF-8 safe byte split at the last stable markdown boundary.
pub fn split_buffered_assistant_text(text: &str) -> (&str, &str) {
    let mut open: Option<(u8, usize, usize)> = None;
    let mut boundary = None;
    let mut start = 0;
    let mut title_awaiting_content = false;
    loop {
        let newline = text[start..].find('\n').map(|offset| start + offset);
        let line = text[start..newline.unwrap_or(text.len())].trim_end_matches([' ', '\t', '\r']);
        if open.is_none() && start > 0 && !title_awaiting_content && list_item_start(line) {
            boundary = Some(start);
        }
        let Some(newline) = newline else {
            break;
        };
        if let Some((marker, indent)) = fence(line) {
            if let Some((open_marker, open_length, open_indent)) = open {
                if marker.as_bytes()[0] == open_marker
                    && marker.len() >= open_length
                    && indent <= open_indent + 3
                    && line.len() == indent + marker.len()
                {
                    open = None;
                    boundary = Some(newline + 1);
                }
            } else {
                open = Some((marker.as_bytes()[0], marker.len(), indent));
                title_awaiting_content = false;
            }
        } else if open.is_none()
            && line.bytes().all(|byte| byte == b' ' || byte == b'\t')
            && start > 0
        {
            if !title_awaiting_content {
                boundary = Some(newline + 1);
            }
        } else if open.is_none() {
            if start > 0 && !title_awaiting_content && line.starts_with('#') && heading(line) {
                boundary = Some(start);
            }
            title_awaiting_content = section_title(line);
        }
        start = newline + 1;
    }
    if let Some(boundary) = boundary {
        text.split_at(boundary)
    } else {
        ("", text)
    }
}

/// Adapter events are kept as JSON here because provider-core's adapter event
/// type is still being finished. The filter preserves every untouched field.
pub struct AssistantStreamingFilter {
    mode: ResponseStreamingMode,
    delivered: HashMap<String, (String, i64)>,
}

impl AssistantStreamingFilter {
    pub fn new(mode: ResponseStreamingMode) -> Self {
        Self {
            mode,
            delivered: HashMap::new(),
        }
    }

    pub fn filter(&mut self, mut event: Value, now_ms: i64) -> Option<Value> {
        let kind = event["type"].as_str()?;
        if kind == "node.updated"
            && event["node"]["kind"] == "assistant_message"
            && event["node"]["status"] == "running"
        {
            return None;
        }
        let field = match kind {
            "message.updated" if event["message"]["role"] == "assistant" => "message",
            "turn_item.updated"
                if event["turnItem"]["type"] == "assistant_message"
                    || event["turnItem"]["type"] == "reasoning" =>
            {
                "turnItem"
            }
            _ => return Some(event),
        };
        let key = format!("{}:{}", kind, event[field]["id"].as_str()?);
        if event[field]["streaming"] != true {
            self.delivered.remove(&key);
            return Some(event);
        }
        if self.mode == ResponseStreamingMode::Turn {
            return None;
        }
        let previous = self.delivered.get(&key);
        if previous.is_some_and(|(_, at)| now_ms - *at < 400) {
            return None;
        }
        let (ready, _) = split_buffered_assistant_text(event[field]["text"].as_str()?);
        if ready.is_empty() || previous.is_some_and(|(text, _)| text == ready) {
            return None;
        }
        self.delivered.insert(key, (ready.to_owned(), now_ms));
        event[field]["text"] = Value::String(ready.to_owned());
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn paragraph_and_fence_boundaries() {
        assert_eq!(
            split_buffered_assistant_text("Intro\n\n```ts\nx()\n\n"),
            ("Intro\n\n", "```ts\nx()\n\n")
        );
        assert_eq!(
            split_buffered_assistant_text("```ts\nx()\n```\nrest"),
            ("```ts\nx()\n```\n", "rest")
        );
        assert_eq!(
            split_buffered_assistant_text("Intro\n\n## Results\n\n"),
            ("Intro\n\n", "## Results\n\n")
        );
        assert_eq!(
            split_buffered_assistant_text("**Results**\n\nBody\n\nNext"),
            ("**Results**\n\nBody\n\n", "Next")
        );
    }
    #[test]
    fn coalesces_for_400_ms_and_flushes_final() {
        let mut filter = AssistantStreamingFilter::new(ResponseStreamingMode::Paragraph);
        let event = |text: &str, streaming| json!({"type":"message.updated","message":{"id":"m","role":"assistant","text":text,"streaming":streaming}});
        assert!(filter.filter(event("First", true), 0).is_none());
        assert_eq!(
            filter.filter(event("First\n\nSec", true), 10).unwrap()["message"]["text"],
            "First\n\n"
        );
        assert!(
            filter
                .filter(event("First\n\nSecond\n\nThi", true), 100)
                .is_none()
        );
        assert_eq!(
            filter
                .filter(event("First\n\nSecond\n\nThird", true), 410)
                .unwrap()["message"]["text"],
            "First\n\nSecond\n\n"
        );
        assert_eq!(
            filter
                .filter(event("First\n\nSecond\n\nThird", false), 420)
                .unwrap()["message"]["text"],
            "First\n\nSecond\n\nThird"
        );
    }
}
