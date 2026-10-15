//! Port of upstream/packages/shared/src/composerContextLegacy.ts
//!
//! Upgrades a message written before inline context references: trailing `<terminal_context>`,
//! `<element_context>` and `<preview_annotation>` blocks, inline or trailing `<review_comment>`
//! blocks, and U+FFFC terminal placeholders. Produces canonical reference links plus records so
//! old messages render and copy through the new path. Event history is never rewritten; this runs
//! in memory on read.
//!
//! Patterns with lookaround or backreferences use `fancy-regex`, which backtracks like JS. Its
//! backtrack-limit errors read as "no match"; upstream has no limit, so a pathological message
//! can keep its legacy text where upstream would upgrade it.

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::composer_context::{
    COMPOSER_CONTEXT_MAX_RECORDS, ComposerContextRecord, ElementContextDetails,
    ElementContextRecord, KnownComposerContextRecord, PreviewAnnotationContextRecord,
    ReviewCommentContextRecord, TerminalContextRecord,
};

use crate::composer_context_references::{
    element_details, format_composer_context_record_reference,
};
use crate::js_string::{JS_DOT, JS_WS, replace_js_whitespace_runs};
use turin_contracts::base_schemas::js_trim_end;

#[derive(Debug, Clone, PartialEq)]
pub struct UpgradedLegacyContext {
    pub text: String,
    pub records: Vec<ComposerContextRecord>,
}

const PLACEHOLDER: char = '\u{FFFC}';
/// Basic Multilingual Plane private-use area, the characters a review token can be built from.
const PRIVATE_USE_COUNT: usize = 0xF8FF - 0xE000 + 1;
fn private_use_index(ch: char) -> Option<usize> {
    let code = ch as u32;
    (0xE000..=0xF8FF)
        .contains(&code)
        .then(|| (code - 0xE000) as usize)
}
/// `Schema.Int` in upstream is `Number.isSafeInteger`: digit runs past this fail the record.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const PREVIEW_FALLBACK_LABEL: &str = "Preview annotation";
const STYLE_HEADING: &str = "Requested visual changes:";
const SCREENSHOT_SENTENCE: &str = "The attached screenshot is the annotated preview crop.";
const PREVIEW_FIELD_PREFIXES: [&str; 5] = [
    "Preview annotation:",
    "Id: ",
    "Page: ",
    "Comment: ",
    "Targets: ",
];

fn trailing_block(tag: &str) -> FancyRegex {
    FancyRegex::new(&format!(
        r"\n*<{tag}>\n((?:(?!\n</{tag}>)[\s\S])*)\n</{tag}>[{JS_WS}]*$"
    ))
    .expect("static regex")
}

static TRAILING_TERMINAL: LazyLock<FancyRegex> =
    LazyLock::new(|| trailing_block("terminal_context"));
static TRAILING_ELEMENT: LazyLock<FancyRegex> = LazyLock::new(|| trailing_block("element_context"));
static TRAILING_PREVIEW: LazyLock<FancyRegex> =
    LazyLock::new(|| trailing_block("preview_annotation"));

/// `<review_comment\b([^>]*)>` or a whole other context block. The `\b` is written as a lookahead
/// because JS word boundaries are ASCII.
static REVIEW_OR_CONTEXT_BLOCK: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(
        r"(?m)<review_comment(?![A-Za-z0-9_])([^>]*)>|(?:^|(?<=[\r\x{2028}\x{2029}]))<(terminal_context|element_context|preview_annotation)>\n[\s\S]*?\n</\2>",
    )
    .expect("static regex")
});

/// Opening fence, or the review closer. `^`/`$` are line anchors.
static REVIEW_BOUNDARY: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?m)(?:^|(?<=[\r\x{2028}\x{2029}]))(`{3,})([^\n]*)$|</review_comment>")
        .expect("static regex")
});

static REVIEW_FENCE: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(&format!(r"(`{{3,}})([^{JS_WS}`]*)[^\n]*\n([\s\S]*?)\n\1"))
        .expect("static regex")
});

static REVIEW_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([a-zA-Z][a-zA-Z0-9_-]*)="([^"]*)""#).expect("static regex"));

static LEGACY_MARKERS: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(&format!(
        "<(?:terminal_context|element_context|preview_annotation|review_comment)(?![A-Za-z0-9_])|{PLACEHOLDER}"
    ))
    .expect("static regex")
});

static TERMINAL_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^({JS_DOT}*?) (?:line ([0-9]+)|lines ([0-9]+)-([0-9]+))$"
    ))
    .expect("static regex")
});

static TERMINAL_LINE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+ \| ?").expect("static regex"));

static ELEMENT_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<([^>]+)>").expect("static regex"));

static ELEMENT_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^([a-z]+): ({JS_DOT}*)$")).expect("static regex"));

static ELEMENT_SECTION_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(html|styles):$").expect("static regex"));

static SOURCE_LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^({JS_DOT}*?)(?::([0-9]+))?(?::([0-9]+))?$")).expect("static regex")
});

static URL_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https?://").expect("static regex"));

static PREVIEW_ELEMENT_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<element_context>\n([\s\S]*?)\n</element_context>").expect("static regex")
});

/// `^</?(?:terminal_context|...)\b` with an ASCII word boundary.
static BLOCK_DELIMITER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^</?(?:terminal_context|element_context|preview_annotation|review_comment)(?-u:\b)",
    )
    .expect("static regex")
});

static PREFIX_WORD_CHAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}\p{M}_@.\-]$").expect("static regex"));

static SUFFIX_WORD_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[\p{L}\p{N}\p{M}_\-]|[.@]+[\p{L}\p{N}\p{M}_\-])").expect("static regex")
});

struct ParsedEntry {
    header: String,
    body: String,
}

/// `^- (.+):$`: the header is non-empty and has no JS line terminator (`.` excludes them).
fn entry_header(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("- ")?.strip_suffix(':')?;
    (!inner.is_empty() && !inner.contains(['\n', '\r', '\u{2028}', '\u{2029}'])).then_some(inner)
}

fn parse_entries(block: &str) -> Option<Vec<ParsedEntry>> {
    let mut entries = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    fn commit(current: &mut Option<(String, Vec<String>)>, entries: &mut Vec<ParsedEntry>) {
        if let Some((header, body_lines)) = current.take() {
            entries.push(ParsedEntry {
                header,
                body: js_trim_end(&body_lines.join("\n")).to_owned(),
            });
        }
    }
    for line in block.split('\n') {
        if let Some(header) = entry_header(line) {
            commit(&mut current, &mut entries);
            current = Some((header.to_owned(), Vec::new()));
            continue;
        }
        if let Some((_, body_lines)) = current.as_mut()
            && let Some(rest) = line.strip_prefix("  ")
        {
            body_lines.push(rest.to_owned());
            continue;
        }
        if !js_trim(line).is_empty() {
            return None;
        }
        if let Some((_, body_lines)) = current.as_mut() {
            body_lines.push(String::new());
        }
    }
    commit(&mut current, &mut entries);
    Some(entries)
}

/// The inline label the old send path wrote for a terminal excerpt: `@terminal-1:509-514`.
fn inline_terminal_label(record: &TerminalContextRecord) -> String {
    let slug =
        replace_js_whitespace_runs(&js_trim(record.terminal_label.as_str()).to_lowercase(), "-");
    let range = if record.line_start == record.line_end {
        record.line_start.to_string()
    } else {
        format!("{}-{}", record.line_start, record.line_end)
    };
    format!("@{slug}:{range}")
}

fn legacy_id(kind: &str, index: usize) -> String {
    format!("legacy_{kind}_{index}")
}

fn parse_safe_int(digits: &str) -> Option<u64> {
    let value: u64 = digits.parse().ok()?;
    (value <= MAX_SAFE_INTEGER).then_some(value)
}

fn terminal_record(entry: &ParsedEntry, index: usize) -> Option<TerminalContextRecord> {
    let header = TERMINAL_HEADER.captures(&entry.header)?;
    let terminal_label = js_trim(header.get(1)?.as_str());
    if terminal_label.is_empty() {
        return None;
    }
    let (start, end) = match (header.get(2), header.get(3), header.get(4)) {
        (Some(line), _, _) => (line.as_str(), line.as_str()),
        (None, Some(start), Some(end)) => (start.as_str(), end.as_str()),
        _ => return None,
    };
    let line_start = parse_safe_int(start)?;
    let line_end = parse_safe_int(end)?;
    let text = entry
        .body
        .split('\n')
        .map(|line| TERMINAL_LINE_PREFIX.replace(line, "").into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let terminal_id = replace_js_whitespace_runs(&terminal_label.to_lowercase(), "-");
    serde_json::from_value(serde_json::json!({
        "version": 1,
        "contextId": legacy_id("terminal", index),
        "kind": "terminal",
        "label": entry.header,
        "terminalId": terminal_id,
        "terminalLabel": terminal_label,
        "lineStart": line_start,
        "lineEnd": line_end,
        "text": text,
    }))
    .ok()
}

/// `parseSource`: `Some(Value::Null)` when the location has no file name, `None` when a line or
/// column number is unsafe (the element record is then invalid as a whole).
fn parse_source(location: &str) -> Option<serde_json::Value> {
    let Some(caps) = SOURCE_LOCATION.captures(location) else {
        return Some(serde_json::Value::Null);
    };
    let file_name = caps.get(1)?.as_str();
    if file_name.is_empty() {
        return Some(serde_json::Value::Null);
    }
    let number = |group: Option<regex::Match<'_>>| -> Option<serde_json::Value> {
        match group {
            None => Some(serde_json::Value::Null),
            Some(digits) => Some(serde_json::json!(parse_safe_int(digits.as_str())?)),
        }
    };
    Some(serde_json::json!({
        "functionName": null,
        "fileName": file_name,
        "lineNumber": number(caps.get(2))?,
        "columnNumber": number(caps.get(3))?,
    }))
}

fn element_record(entry: &ParsedEntry, index: usize) -> Option<ElementContextRecord> {
    let header = ELEMENT_HEADER.captures(&entry.header)?;
    let inner = header.get(1)?.as_str();
    let mut fields: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut sections: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    let mut section: Option<&str> = None;
    for line in entry.body.split('\n') {
        if let Some(name) = section {
            if let Some(rest) = line.strip_prefix("  ") {
                sections.entry(name).or_default().push(rest);
                continue;
            }
            if line.is_empty() {
                sections.entry(name).or_default().push("");
                continue;
            }
        }
        section = None;
        if let Some(field) = ELEMENT_FIELD.captures(line) {
            fields.insert(field.get(1)?.as_str(), field.get(2)?.as_str());
            continue;
        }
        if let Some(start) = ELEMENT_SECTION_START.captures(line) {
            let name = start.get(1)?.as_str();
            section = Some(name);
            sections.insert(name, Vec::new());
        }
    }
    let section_text = |name: &str| {
        js_trim_end(
            &sections
                .get(name)
                .map(|lines| lines.join("\n"))
                .unwrap_or_default(),
        )
        .to_owned()
    };
    let source = match fields.get("source") {
        Some(location) if !location.is_empty() => parse_source(location)?,
        _ => serde_json::Value::Null,
    };
    let component_name = inner
        .bytes()
        .any(|b| b.is_ascii_uppercase())
        .then(|| inner.to_owned());
    serde_json::from_value(serde_json::json!({
        "version": 1,
        "contextId": legacy_id("element", index),
        "kind": "element",
        "label": format!("<{inner}>"),
        "pageUrl": fields.get("url").copied().unwrap_or(""),
        "pageTitle": null,
        "tagName": inner.to_lowercase(),
        "selector": fields.get("selector").copied(),
        "htmlPreview": section_text("html"),
        "componentName": component_name,
        "source": source,
        "styles": section_text("styles"),
    }))
    .ok()
}

/// Whether `line` starts a field or the block after one. A legacy comment runs until one of these.
fn is_preview_field_start(line: &str) -> bool {
    PREVIEW_FIELD_PREFIXES
        .iter()
        .any(|prefix| line.starts_with(prefix))
        || line == STYLE_HEADING
        || BLOCK_DELIMITER.is_match(line)
        || line == SCREENSHOT_SENTENCE
}

fn preview_record(body: &str, index: usize) -> Option<PreviewAnnotationContextRecord> {
    let lines: Vec<&str> = body.split('\n').collect();
    let read = |prefix: &str| -> String {
        let Some(start) = lines.iter().position(|line| line.starts_with(prefix)) else {
            return String::new();
        };
        let mut end = start + 1;
        while end < lines.len() && !is_preview_field_start(lines[end]) {
            end += 1;
        }
        let mut value = vec![&lines[start][prefix.len()..]];
        value.extend_from_slice(&lines[start + 1..end]);
        js_trim(&value.join("\n")).to_owned()
    };

    let mut style_changes: Vec<String> = Vec::new();
    if let Some(heading) = lines.iter().position(|line| *line == STYLE_HEADING) {
        for line in &lines[heading + 1..] {
            let Some(change) = line.strip_prefix("- ") else {
                break;
            };
            style_changes.push(change.to_owned());
        }
    }

    let page = read("Page: ");
    let page_is_url = URL_PREFIX.is_match(&page);
    let mut elements: Vec<ElementContextDetails> = Vec::new();
    for block in PREVIEW_ELEMENT_BLOCK.captures_iter(body) {
        let entries = parse_entries(block.get(1)?.as_str())?;
        for entry in &entries {
            let record = element_record(entry, elements.len() + 1)?;
            elements.push(element_details(&record));
        }
    }

    let page_url = if page_is_url {
        page.clone()
    } else {
        elements
            .first()
            .map(|element| element.page_url.clone())
            .unwrap_or_default()
    };
    let page_title = (!page_is_url && !page.is_empty()).then(|| page.clone());
    let label = if page.is_empty() {
        PREVIEW_FALLBACK_LABEL.to_owned()
    } else {
        page.clone()
    };
    let mut value = serde_json::json!({
        "version": 1,
        "contextId": legacy_id("preview-annotation", index),
        "kind": "preview-annotation",
        "label": label,
        "annotationId": read("Id: "),
        "pageUrl": page_url,
        "pageTitle": page_title,
        "comment": read("Comment: "),
        "targetSummary": read("Targets: "),
        "styleChanges": style_changes,
    });
    if !elements.is_empty() {
        value["elements"] = serde_json::to_value(&elements).ok()?;
    }
    serde_json::from_value(value).ok()
}

fn unescape_attribute(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn is_ascii_digits(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
}

/// The raw text of a review block, cut at its last code fence. Returns `None` when the record
/// would not decode, so the block stays as prose.
fn review_record(attributes: &str, raw_body: &str, index: usize) -> Option<ComposerContextRecord> {
    let mut values: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for caps in REVIEW_ATTRIBUTE.captures_iter(attributes) {
        let name = caps.get(1)?.as_str();
        values.insert(name, unescape_attribute(caps.get(2)?.as_str()));
    }
    let non_empty_trimmed = |name: &str| {
        values
            .get(name)
            .map(|value| js_trim(value))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let file_path = non_empty_trimmed("filePath")?;
    let section_id = non_empty_trimmed("sectionId")?;
    let start_index = values.get("startIndex").map(String::as_str);
    let end_index = values.get("endIndex").map(String::as_str);
    if !is_ascii_digits(start_index) || !is_ascii_digits(end_index) {
        return None;
    }
    let start = parse_safe_int(start_index?)?;
    let end = parse_safe_int(end_index?)?;

    let fences: Vec<_> = REVIEW_FENCE
        .captures_iter(raw_body)
        .filter_map(|caps| caps.ok())
        .collect();
    let fence = fences.last();
    let range_label = non_empty_trimmed("rangeLabel").unwrap_or_else(|| "line".to_owned());
    let base_name = file_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(file_path.as_str());
    let section_title = non_empty_trimmed("sectionTitle").unwrap_or_else(|| "Review".to_owned());
    let text_end = fence
        .and_then(|caps| caps.get(0))
        .map(|whole| whole.start())
        .unwrap_or(raw_body.len());
    let text = js_trim(&raw_body[..text_end]).to_owned();
    let diff = fence
        .and_then(|caps| caps.get(3))
        .map(|m| m.as_str().to_owned())
        .unwrap_or_default();
    let fence_language = fence
        .and_then(|caps| caps.get(2))
        .map(|m| js_trim(m.as_str()))
        .filter(|language| !language.is_empty())
        .unwrap_or("diff")
        .to_owned();

    let record: ReviewCommentContextRecord = serde_json::from_value(serde_json::json!({
        "version": 1,
        "contextId": legacy_id("review-comment", index),
        "kind": "review-comment",
        "label": format!("{base_name} {range_label}"),
        "sectionId": section_id,
        "sectionTitle": section_title,
        "filePath": file_path,
        "startIndex": start.min(end),
        "endIndex": start.max(end),
        "rangeLabel": range_label,
        "text": text,
        "diff": diff,
        "fenceLanguage": fence_language,
    }))
    .ok()?;
    Some(ComposerContextRecord::Known(
        KnownComposerContextRecord::ReviewComment(record),
    ))
}

/// Calls `replace(whole, attributes, body)` for each `<review_comment>` block outside a fence and
/// leaves other context blocks alone. Review source can contain literal closing tags inside its
/// dynamically sized code fence, so the closer must be found by fence length.
fn replace_review_blocks(
    text: &str,
    mut replace: impl FnMut(&str, &str, &str) -> String,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut consumed = 0usize;
    // `openings.lastIndex` in upstream.
    let mut position = 0usize;
    'openings: while let Some(opening) = find_at(&REVIEW_OR_CONTEXT_BLOCK, text, position) {
        let (open_start, open_end) = whole_span(&opening);
        position = open_end;
        if opening.get(2).is_some() {
            continue;
        }
        let body_start = open_end;
        let mut fence_length = 0usize;
        let mut boundary_position = body_start;
        while let Some(boundary) = find_at(&REVIEW_BOUNDARY, text, boundary_position) {
            let (boundary_start, boundary_end) = whole_span(&boundary);
            boundary_position = boundary_end;
            if let Some(fence) = boundary.get(1) {
                let fence_len = fence.as_str().len();
                if fence_length == 0 {
                    fence_length = fence_len;
                } else if fence_len >= fence_length
                    && boundary
                        .get(2)
                        .is_none_or(|info| js_trim(info.as_str()).is_empty())
                {
                    fence_length = 0;
                }
            } else if fence_length == 0 {
                parts.push(text[consumed..open_start].to_owned());
                consumed = boundary_end;
                let attributes = opening.get(1).map(|m| m.as_str()).unwrap_or("");
                parts.push(replace(
                    &text[open_start..consumed],
                    attributes,
                    &text[body_start..boundary_start],
                ));
                position = consumed;
                continue 'openings;
            }
        }
        if consumed < body_start {
            break;
        }
    }
    parts.push(text[consumed..].to_owned());
    parts.concat()
}

fn whole_span(caps: &fancy_regex::Captures<'_>) -> (usize, usize) {
    let whole = caps.get(0).expect("group 0 always participates");
    (whole.start(), whole.end())
}

fn find_at<'t>(
    regex: &FancyRegex,
    text: &'t str,
    position: usize,
) -> Option<fancy_regex::Captures<'t>> {
    regex.captures_from_pos(text, position).ok().flatten()
}

/// Trailing blocks peel off the end. A whole `<terminal_context>` / `<element_context>` /
/// `<preview_annotation>` body is returned as the first capture; the text before it is returned
/// with trailing newlines removed. `None` when nothing matches at the end.
fn strip_trailing(text: &str, pattern: &FancyRegex) -> Option<(String, String)> {
    let caps = find_at(pattern, text, 0)?;
    let whole = caps.get(0)?;
    let before = text[..whole.start()].trim_end_matches('\n').to_owned();
    Some((before, caps.get(1)?.as_str().to_owned()))
}

fn record_of_terminal(record: TerminalContextRecord) -> ComposerContextRecord {
    ComposerContextRecord::Known(KnownComposerContextRecord::Terminal(record))
}

fn record_of_element(record: ElementContextRecord) -> ComposerContextRecord {
    ComposerContextRecord::Known(KnownComposerContextRecord::Element(record))
}

fn record_of_preview(record: PreviewAnnotationContextRecord) -> ComposerContextRecord {
    ComposerContextRecord::Known(KnownComposerContextRecord::PreviewAnnotation(record))
}

pub fn upgrade_legacy_context_message(text: &str) -> UpgradedLegacyContext {
    let unchanged = || UpgradedLegacyContext {
        text: text.to_owned(),
        records: Vec::new(),
    };
    if !LEGACY_MARKERS.is_match(text).unwrap_or(false) {
        return unchanged();
    }

    let mut terminal_entries: Vec<ParsedEntry> = Vec::new();
    let mut element_entries: Vec<ParsedEntry> = Vec::new();
    let mut preview_bodies: Vec<String> = Vec::new();
    let mut reviews: Vec<ComposerContextRecord> = Vec::new();

    // A literal private-use character in the message must never be mistaken for our placeholder, so
    // the token is the first private-use character the text does not contain. One character keeps
    // the patterns a fixed size whatever the message holds. When the text uses every private-use
    // character, the message is left as it is.
    let mut private_use_used = [false; PRIVATE_USE_COUNT];
    for ch in text.chars() {
        if let Some(index) = private_use_index(ch) {
            private_use_used[index] = true;
        }
    }
    let Some(index) = private_use_used.iter().position(|used| !used) else {
        return unchanged();
    };
    let review_token = char::from_u32(0xE000 + index as u32)
        .expect("private-use characters are valid scalar values")
        .to_string();
    let token = regex::escape(&review_token);
    let (Ok(review_index), Ok(trailing_review_tokens)) = (
        Regex::new(&format!("{token}([0-9]+){token}")),
        Regex::new(&format!(r"(?:[{JS_WS}]*{token}[0-9]+{token})+[{JS_WS}]*$")),
    ) else {
        return unchanged();
    };

    // Review blocks become tokens in place first so they neither hide the trailing blocks behind
    // them nor lose their position. Unparseable blocks stay as text.
    let mut rest = replace_review_blocks(text, |whole, attributes, raw_body| {
        match review_record(attributes, raw_body, reviews.len() + 1) {
            Some(record) => {
                reviews.push(record);
                format!("{review_token}{}{review_token}", reviews.len() - 1)
            }
            None => whole.to_owned(),
        }
    });

    // Blocks were appended in send order (terminal, element, preview, review), so they peel off the
    // end in reverse. Each peel exposes the next block as trailing. Reviews peel only when they
    // hide another trailing context block; a review at the end of ordinary prose stays inline.
    let mut trailing_review_indices: Vec<usize> = Vec::new();
    if let Some(tokens) = trailing_review_tokens.find(&rest)
        && !tokens.as_str().is_empty()
    {
        let preceding = rest[..tokens.start()].to_owned();
        let hides_block = [&*TRAILING_PREVIEW, &*TRAILING_ELEMENT, &*TRAILING_TERMINAL]
            .iter()
            .any(|pattern| pattern.is_match(&preceding).unwrap_or(false));
        if hides_block {
            trailing_review_indices = review_index
                .captures_iter(tokens.as_str())
                .filter_map(|caps| caps.get(1)?.as_str().parse().ok())
                .collect();
            rest = preceding;
        }
    }

    loop {
        if let Some((before, body)) = strip_trailing(&rest, &TRAILING_PREVIEW) {
            rest = before;
            preview_bodies.insert(0, body);
            continue;
        }
        if let Some((before, body)) = strip_trailing(&rest, &TRAILING_ELEMENT) {
            let Some(entries) = parse_entries(&body).filter(|entries| !entries.is_empty()) else {
                return unchanged();
            };
            if entries
                .iter()
                .any(|entry| element_record(entry, 1).is_none())
            {
                return unchanged();
            }
            rest = before;
            element_entries.splice(0..0, entries);
            continue;
        }
        if let Some((before, body)) = strip_trailing(&rest, &TRAILING_TERMINAL) {
            let Some(entries) = parse_entries(&body).filter(|entries| !entries.is_empty()) else {
                return unchanged();
            };
            if entries
                .iter()
                .any(|entry| terminal_record(entry, 1).is_none())
            {
                return unchanged();
            }
            rest = before;
            terminal_entries.splice(0..0, entries);
            continue;
        }
        break;
    }

    let terminals: Vec<TerminalContextRecord> = terminal_entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| terminal_record(entry, index + 1))
        .collect();
    let elements: Vec<ElementContextRecord> = element_entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| element_record(entry, index + 1))
        .collect();
    let mut previews: Vec<PreviewAnnotationContextRecord> = Vec::new();
    for (index, preview_body) in preview_bodies.iter().enumerate() {
        match preview_record(preview_body, index + 1) {
            Some(record) => previews.push(record),
            None => return unchanged(),
        }
    }
    let appended_reviews: Vec<ComposerContextRecord> = trailing_review_indices
        .iter()
        .filter_map(|index| reviews.get(*index).cloned())
        .collect();

    let mut body = review_index
        .replace_all(&rest, |caps: &regex::Captures<'_>| {
            caps.get(1)
                .and_then(|index| index.as_str().parse::<usize>().ok())
                .and_then(|index| reviews.get(index))
                .map(format_composer_context_record_reference)
                .unwrap_or_else(|| caps[0].to_owned())
        })
        .into_owned();

    // Placeholders bind to terminal entries in order, like the old materialize step did.
    let mut placeholder_index = 0usize;
    let mut with_placeholders = String::with_capacity(body.len());
    for ch in body.chars() {
        if ch == PLACEHOLDER {
            if let Some(record) = terminals.get(placeholder_index) {
                with_placeholders.push_str(&format_terminal_reference(record));
            }
            placeholder_index += 1;
        } else {
            with_placeholders.push(ch);
        }
    }
    body = with_placeholders;

    // Sent messages carry the materialized `@terminal-1:509-514` label instead of the
    // placeholder; each such label becomes the chip in place.
    let mut placed: Vec<bool> = (0..terminals.len())
        .map(|index| index < placeholder_index)
        .collect();
    for (index, record) in terminals.iter().enumerate() {
        if placed[index] {
            continue;
        }
        let label = inline_terminal_label(record);
        let mut at = body.find(&label);
        while let Some(position) = at {
            let before_ok = !body[..position]
                .chars()
                .next_back()
                .is_some_and(|ch| PREFIX_WORD_CHAR.is_match(ch.encode_utf8(&mut [0; 4])));
            let after = &body[position + label.len()..];
            let after_ok = !SUFFIX_WORD_START.is_match(after);
            if before_ok && after_ok {
                break;
            }
            at = body[position + 1..]
                .find(&label)
                .map(|next| next + position + 1);
        }
        let Some(position) = at else {
            continue;
        };
        body = format!(
            "{}{}{}",
            &body[..position],
            format_terminal_reference(record),
            &body[position + label.len()..],
        );
        placed[index] = true;
    }
    let body = js_trim_end(&body).to_owned();

    let mut appended: Vec<String> = terminals
        .iter()
        .zip(&placed)
        .filter(|(_, placed)| !**placed)
        .map(|(record, _)| format_terminal_reference(record))
        .collect();
    appended.extend(elements.iter().map(|element| {
        format_composer_context_record_reference(&record_of_element(element.clone()))
    }));
    appended.extend(previews.iter().map(|preview| {
        format_composer_context_record_reference(&record_of_preview(preview.clone()))
    }));
    appended.extend(
        appended_reviews
            .iter()
            .map(format_composer_context_record_reference),
    );
    let upgraded_text = if appended.is_empty() {
        body
    } else if !body.is_empty() {
        format!("{body}\n\n{}", appended.join(" "))
    } else {
        appended.join(" ")
    };

    let records: Vec<ComposerContextRecord> = terminals
        .into_iter()
        .map(record_of_terminal)
        .chain(elements.into_iter().map(record_of_element))
        .chain(previews.into_iter().map(record_of_preview))
        .chain(reviews)
        .collect();
    // Conversion is atomic: keep the source if any record would be dropped on the wire.
    if records.len() > COMPOSER_CONTEXT_MAX_RECORDS {
        return unchanged();
    }
    UpgradedLegacyContext {
        text: upgraded_text,
        records,
    }
}

fn format_terminal_reference(record: &TerminalContextRecord) -> String {
    format_composer_context_record_reference(&record_of_terminal(record.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const REVIEW: &str = r#"<review_comment sectionId="s" filePath="f.ts" startIndex="1" endIndex="1">note</review_comment>"#;

    fn upgrade(text: &str) -> UpgradedLegacyContext {
        upgrade_legacy_context_message(text)
    }

    fn values(result: &UpgradedLegacyContext) -> Vec<Value> {
        result
            .records
            .iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .collect()
    }

    fn unchanged(text: &str) -> UpgradedLegacyContext {
        UpgradedLegacyContext {
            text: text.to_owned(),
            records: Vec::new(),
        }
    }

    /// `toMatchObject`: every key in `expected` must equal the record's value.
    fn assert_matches(actual: &Value, expected: &Value) {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => {
                for (key, value) in expected {
                    let found = actual
                        .get(key)
                        .unwrap_or_else(|| panic!("missing key {key}"));
                    assert_matches(found, value);
                }
            }
            (Value::Array(actual), Value::Array(expected)) => {
                assert_eq!(actual.len(), expected.len(), "array length");
                for (a, e) in actual.iter().zip(expected) {
                    assert_matches(a, e);
                }
            }
            _ => assert_eq!(actual, expected),
        }
    }

    #[test]
    fn preserves_review_shaped_payloads_in_each_context_block() {
        let cases = [
            (
                "terminal_context",
                format!("- Build line 1:\n  {REVIEW}"),
                json!({ "kind": "terminal", "text": REVIEW }),
            ),
            (
                "element_context",
                format!("- <div>:\n  html:\n    {REVIEW}"),
                json!({ "kind": "element", "htmlPreview": REVIEW }),
            ),
            (
                "preview_annotation",
                format!("Page: Example\nComment: {REVIEW}"),
                json!({ "kind": "preview-annotation", "comment": REVIEW }),
            ),
            (
                "preview_annotation",
                format!(
                    "Page: Example\n<element_context>\n- <div>:\n  html:\n    {REVIEW}\n</element_context>"
                ),
                json!({ "kind": "preview-annotation", "elements": [{ "htmlPreview": REVIEW }] }),
            ),
        ];
        for (tag, body, payload) in cases {
            for suffix in [String::new(), format!("\n{REVIEW}")] {
                let result = upgrade(&format!(
                    "Before {REVIEW} after\n<{tag}>\n{body}\n</{tag}>{suffix}"
                ));
                assert_matches(&values(&result)[0], &payload);
                let review_count = result
                    .records
                    .iter()
                    .filter(|record| record.kind() == "review-comment")
                    .count();
                assert_eq!(review_count, if suffix.is_empty() { 1 } else { 2 });
                assert!(!result.text.contains('\u{E000}'));
                assert!(result.text.contains(
                    "Before [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1) after"
                ));
            }
        }
    }

    #[test]
    fn keeps_mixed_messages_atomic_for_malformed_blocks() {
        for tag in ["terminal_context", "element_context"] {
            let text = format!("Before {REVIEW} after\n<{tag}>\ninvalid entry\n</{tag}>");
            assert_eq!(upgrade(&text), unchanged(&text));
        }
    }

    #[test]
    fn preserves_malformed_nested_preview_elements() {
        for body in [
            "unparsed content",
            "- Invalid header:\n  html: text",
            "- <div>:\n  html:\n    <div />\nunparsed content",
        ] {
            let text = format!(
                "Before {REVIEW} after\n<preview_annotation>\nPage: Example\n<element_context>\n{body}\n</element_context>\n</preview_annotation>"
            );
            assert_eq!(upgrade(&text), unchanged(&text), "{body}");
        }
    }

    #[test]
    fn allows_an_empty_nested_element_block() {
        let result = upgrade(
            "<preview_annotation>\nPage: Example\n<element_context>\n\n</element_context>\n</preview_annotation>",
        );
        assert_eq!(result.records.len(), 1);
        assert_matches(
            &values(&result)[0],
            &json!({ "kind": "preview-annotation", "pageTitle": "Example" }),
        );
    }

    #[test]
    fn preserves_closing_review_tags_inside_a_fence_of_any_length() {
        for length in [3, 4, 6] {
            let fence = "`".repeat(length);
            let diff = "+ const tag = \"</review_comment>\";\n</review_comment>\n+ final line";
            let block = format!(
                "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"1\">\nComment\n{fence}diff\n{diff}\n{fence}\n</review_comment>"
            );
            let result = upgrade(&format!("Before {block} between {REVIEW} after"));
            assert_eq!(
                result.text,
                "Before [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1) between [f.ts line](t3-context://v1/review-comment/legacy_review-comment_2) after"
            );
            assert_eq!(result.records.len(), 2);
            assert_matches(
                &values(&result)[0],
                &json!({ "text": "Comment", "diff": diff, "fenceLanguage": "diff" }),
            );
        }
    }

    #[test]
    fn does_not_close_a_review_fence_at_a_shorter_backtick_run() {
        let diff = "```\n</review_comment>\n+ remaining source";
        let text = format!(
            "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"1\">\nNote\n````diff\n{diff}\n````\n</review_comment>"
        );
        assert_matches(
            &values(&upgrade(&text))[0],
            &json!({ "text": "Note", "diff": diff }),
        );
    }

    #[test]
    fn preserves_a_review_with_an_unterminated_fence() {
        let text = "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"1\">\nNote\n```diff\n+ source\n</review_comment>";
        assert_eq!(upgrade(text), unchanged(text));
    }

    #[test]
    fn keeps_terminal_source_when_conversion_exceeds_the_wire_contract() {
        let bodies = [
            format!("- {} line 1:\n  output", "x".repeat(256)),
            format!("- Build line 1:\n  {}", "x".repeat(64_001)),
            "- Build lines 9-1:\n  output".to_owned(),
            (0..201)
                .map(|_| "- Build line 1:\n  output")
                .collect::<Vec<_>>()
                .join("\n"),
        ];
        for body in bodies {
            let text = format!("Prompt\n<terminal_context>\n{body}\n</terminal_context>");
            assert_eq!(upgrade(&text), unchanged(&text));
        }
    }

    #[test]
    fn keeps_oversized_element_and_preview_source() {
        for text in [
            format!(
                "<element_context>\n- <div>:\n  html:\n    {}\n</element_context>",
                "x".repeat(8_001)
            ),
            format!(
                "<preview_annotation>\nComment: {}\n</preview_annotation>",
                "x".repeat(8_001)
            ),
        ] {
            assert_eq!(upgrade(&text), unchanged(&text));
        }
    }

    #[test]
    fn does_not_replace_terminal_labels_embedded_in_words() {
        for prompt in [
            "email@build:7",
            "café@build:7",
            "𐐀@build:7",
            "@build:7foo",
            "@build:7st",
            "@build:7é",
            "@build:7.foo",
            "@build:7@mention",
            "@build:7..foo",
            "@build:7.é",
        ] {
            let upgraded = upgrade(&format!(
                "{prompt}\n<terminal_context>\n- Build line 7:\n  output\n</terminal_context>"
            ));
            assert_eq!(
                upgraded.text,
                format!("{prompt}\n\n[Build line 7](t3-context://v1/terminal/legacy_terminal_1)")
            );
        }
    }

    #[test]
    fn upgrades_terminal_labels_before_punctuation() {
        for suffix in [
            ".",
            ",",
            ":",
            ";",
            "!",
            "?",
            ")",
            "@",
            ". Next sentence",
            "@ next",
            ".)",
        ] {
            let result = upgrade(&format!(
                "See @build:7{suffix}\n<terminal_context>\n- Build line 7:\n  output\n</terminal_context>"
            ));
            assert_eq!(
                result.text,
                format!("See [Build line 7](t3-context://v1/terminal/legacy_terminal_1){suffix}")
            );
            assert_eq!(result.records.len(), 1);
        }
    }

    #[test]
    fn preserves_malformed_trailing_blocks_as_text() {
        for tag in ["terminal_context", "element_context"] {
            for body in ["partial output", "- Unrecognized header:\n  partial output"] {
                let text = format!("message\n<{tag}>\n{body}\n</{tag}>");
                assert_eq!(upgrade(&text), unchanged(&text));
            }
        }
    }

    #[test]
    fn preserves_partially_parsed_blocks() {
        for tag in ["terminal_context", "element_context"] {
            let entry = if tag == "terminal_context" {
                "- Build line 7:\n  output"
            } else {
                "- <button>:\n  html:\n    <button>Pay</button>"
            };
            for body in [
                format!("unexpected prefix\n{entry}"),
                format!("{entry}\nunexpected suffix"),
                format!("{entry}\nunexpected middle\n{entry}"),
                format!("  orphaned body\n{entry}"),
            ] {
                let text = format!("Prompt\n<{tag}>\n{body}\n</{tag}>");
                assert_eq!(upgrade(&text), unchanged(&text));
            }
        }
    }

    #[test]
    fn preserves_oversized_legacy_reviews_instead_of_creating_dangling_references() {
        for body in [
            "x".repeat(16_001),
            format!("note\n```diff\n{}\n```", "+x".repeat(16_001)),
        ] {
            let text = format!(
                "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"1\">{body}</review_comment>"
            );
            assert_eq!(upgrade(&text), unchanged(&text));
        }
    }

    #[test]
    fn preserves_literal_review_tokens_even_beside_real_review_blocks() {
        let upgraded = upgrade(&format!(
            "Literal \u{E000}0\u{E000} before {REVIEW} after \u{E000}7\u{E000}"
        ));
        assert_eq!(
            upgraded.text,
            "Literal \u{E000}0\u{E000} before [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1) after \u{E000}7\u{E000}"
        );
        assert_eq!(upgraded.records.len(), 1);
        let unmatched =
            "Literal \u{E000}0\u{E000} <review_comment>invalid</review_comment> \u{E000}99\u{E000}";
        assert_eq!(upgrade(unmatched), unchanged(unmatched));
    }

    #[test]
    fn matches_complete_terminal_line_ranges_rather_than_numeric_prefixes() {
        let upgraded = upgrade(
            &[
                "Compare @terminal:10 and @terminal:1-2 with @terminal:1",
                "<terminal_context>",
                "- Terminal line 1:",
                "  1 | one",
                "- Terminal line 10:",
                "  10 | ten",
                "- Terminal lines 1-2:",
                "  1 | one",
                "  2 | two",
                "</terminal_context>",
            ]
            .join("\n"),
        );
        assert_eq!(
            upgraded.text,
            "Compare [Terminal line 10](t3-context://v1/terminal/legacy_terminal_2) and [Terminal lines 1-2](t3-context://v1/terminal/legacy_terminal_3) with [Terminal line 1](t3-context://v1/terminal/legacy_terminal_1)"
        );
    }

    #[test]
    fn keeps_url_valued_preview_pages_as_locations() {
        let upgraded = upgrade(
            "<preview_annotation>\nPage: https://example.com/checkout\n</preview_annotation>",
        );
        assert_matches(
            &values(&upgraded)[0],
            &json!({ "pageUrl": "https://example.com/checkout", "pageTitle": null }),
        );
    }

    #[test]
    fn passes_plain_text_through_untouched() {
        assert_eq!(upgrade("hello **world**"), unchanged("hello **world**"));
    }

    #[test]
    fn preserves_markdown_hard_breaks_while_removing_legacy_context() {
        let text = [
            "Line with hard break  ",
            "next",
            "",
            "<terminal_context>",
            "- Build line 7:",
            "  7 | done",
            "</terminal_context>",
        ]
        .join("\n");
        assert!(upgrade(&text).text.contains("Line with hard break  \nnext"));
    }

    #[test]
    fn binds_placeholders_to_trailing_terminal_entries_in_order() {
        let text = [
            "Look at \u{FFFC} and \u{FFFC} please",
            "",
            "<terminal_context>",
            "- Terminal 1 lines 509-510:",
            "  509 | error: boom",
            "  510 |   at main.ts:1",
            "",
            "- Build line 7:",
            "  7 | done",
            "</terminal_context>",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            "Look at [Terminal 1 lines 509-510](t3-context://v1/terminal/legacy_terminal_1) and [Build line 7](t3-context://v1/terminal/legacy_terminal_2) please"
        );
        assert_eq!(
            values(&upgraded),
            vec![
                json!({
                    "version": 1,
                    "contextId": "legacy_terminal_1",
                    "kind": "terminal",
                    "label": "Terminal 1 lines 509-510",
                    "terminalId": "terminal-1",
                    "terminalLabel": "Terminal 1",
                    "lineStart": 509,
                    "lineEnd": 510,
                    "text": "error: boom\n  at main.ts:1",
                }),
                json!({
                    "version": 1,
                    "contextId": "legacy_terminal_2",
                    "kind": "terminal",
                    "label": "Build line 7",
                    "terminalId": "build",
                    "terminalLabel": "Build",
                    "lineStart": 7,
                    "lineEnd": 7,
                    "text": "done",
                }),
            ]
        );
    }

    #[test]
    fn appends_references_for_terminal_entries_without_placeholders_and_drops_extra_placeholders() {
        let text = "\u{FFFC} and \u{FFFC}\n\n<terminal_context>\n- T line 1:\n  1 | x\n</terminal_context>";
        assert_eq!(
            upgrade(text).text,
            "[T line 1](t3-context://v1/terminal/legacy_terminal_1) and"
        );
        let no_placeholder = "hi\n\n<terminal_context>\n- T line 1:\n  1 | x\n</terminal_context>";
        assert_eq!(
            upgrade(no_placeholder).text,
            "hi\n\n[T line 1](t3-context://v1/terminal/legacy_terminal_1)"
        );
    }

    #[test]
    fn binds_materialized_inline_terminal_labels_from_sent_messages_in_place() {
        let text = [
            "Look at @terminal-1:509-510 and again @build:7 please",
            "",
            "<terminal_context>",
            "- Terminal 1 lines 509-510:",
            "  509 | error: boom",
            "  510 |   at main.ts:1",
            "",
            "- Build line 7:",
            "  7 | done",
            "</terminal_context>",
        ]
        .join("\n");
        assert_eq!(
            upgrade(&text).text,
            "Look at [Terminal 1 lines 509-510](t3-context://v1/terminal/legacy_terminal_1) and again [Build line 7](t3-context://v1/terminal/legacy_terminal_2) please"
        );
    }

    #[test]
    fn upgrades_a_trailing_element_block_into_element_records() {
        let text = [
            "fix this",
            "",
            "<element_context>",
            "- <Button> (Button.tsx:12):",
            "  url: http://localhost:3000/checkout",
            "  selector: #pay",
            "  source: src/Button.tsx:12:3",
            "  html:",
            "    <button id=\"pay\">Pay</button>",
            "  styles:",
            "    color: red;",
            "</element_context>",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            "fix this\n\n[<Button>](t3-context://v1/element/legacy_element_1)"
        );
        assert_eq!(
            values(&upgraded),
            vec![json!({
                "version": 1,
                "contextId": "legacy_element_1",
                "kind": "element",
                "label": "<Button>",
                "pageUrl": "http://localhost:3000/checkout",
                "pageTitle": null,
                "tagName": "button",
                "selector": "#pay",
                "htmlPreview": "<button id=\"pay\">Pay</button>",
                "componentName": "Button",
                "source": { "functionName": null, "fileName": "src/Button.tsx", "lineNumber": 12, "columnNumber": 3 },
                "styles": "color: red;",
            })]
        );
    }

    #[test]
    fn keeps_every_line_of_a_multiline_legacy_annotation_comment() {
        // Older clients wrote the comment verbatim, newlines and all.
        let text = [
            "<preview_annotation>",
            "Preview annotation:",
            "Id: ann_2",
            "Page: Checkout",
            "Comment: First instruction",
            "Second instruction",
            "Targets: 1 selected element.",
            "</preview_annotation>",
        ]
        .join("\n");
        assert_matches(
            &values(&upgrade(&text))[0],
            &json!({ "kind": "preview-annotation", "comment": "First instruction\nSecond instruction" }),
        );
    }

    #[test]
    fn keeps_a_blank_line_inside_a_legacy_annotation_comment() {
        let text = [
            "<preview_annotation>",
            "Preview annotation:",
            "Comment: First paragraph",
            "",
            "Second paragraph",
            "Targets: 1 selected element.",
            "</preview_annotation>",
        ]
        .join("\n");
        assert_matches(
            &values(&upgrade(&text))[0],
            &json!({ "comment": "First paragraph\n\nSecond paragraph" }),
        );
    }

    #[test]
    fn keeps_markup_inside_a_legacy_annotation_comment() {
        let text = [
            "<preview_annotation>",
            "Preview annotation:",
            "Comment: Change this",
            "<div>keep me</div>",
            "and this too",
            "</preview_annotation>",
        ]
        .join("\n");
        assert_matches(
            &values(&upgrade(&text))[0],
            &json!({ "comment": "Change this\n<div>keep me</div>\nand this too" }),
        );
    }

    #[test]
    fn upgrades_trailing_preview_annotation_blocks() {
        let text = [
            "bigger",
            "",
            "<preview_annotation>",
            "Preview annotation:",
            "Id: ann_1",
            "Page: Checkout",
            "Comment: Make it bigger",
            "Targets: 1 selected element.",
            "Requested visual changes:",
            "- font-size: 12px → 20px",
            "The attached screenshot is the annotated preview crop.",
            "<element_context>",
            "- <button>:",
            "  url: http://localhost:3000/",
            "  selector: #pay",
            "  source: src/Pay.tsx:12:3",
            "  html:",
            "    <button>Pay</button>",
            "</element_context>",
            "</preview_annotation>",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            "bigger\n\n[Checkout](t3-context://v1/preview-annotation/legacy_preview-annotation_1)"
        );
        assert_eq!(
            values(&upgraded),
            vec![json!({
                "version": 1,
                "contextId": "legacy_preview-annotation_1",
                "kind": "preview-annotation",
                "label": "Checkout",
                "annotationId": "ann_1",
                "pageUrl": "http://localhost:3000/",
                "pageTitle": "Checkout",
                "comment": "Make it bigger",
                "targetSummary": "1 selected element.",
                "styleChanges": ["font-size: 12px → 20px"],
                "elements": [{
                    "pageUrl": "http://localhost:3000/",
                    "pageTitle": null,
                    "tagName": "button",
                    "selector": "#pay",
                    "htmlPreview": "<button>Pay</button>",
                    "componentName": null,
                    "source": { "functionName": null, "fileName": "src/Pay.tsx", "lineNumber": 12, "columnNumber": 3 },
                    "styles": "",
                }],
            })]
        );
    }

    #[test]
    fn keeps_a_literal_preview_opening_tag_inside_its_annotation_comment() {
        let upgraded = upgrade(
            &[
                "Explain this",
                "<preview_annotation>",
                "Id: annotation-1",
                "Page: Checkout",
                "Comment: Render <preview_annotation>",
                "Targets: 1 selected element.",
                "</preview_annotation>",
            ]
            .join("\n"),
        );
        assert_eq!(
            upgraded.text,
            "Explain this\n\n[Checkout](t3-context://v1/preview-annotation/legacy_preview-annotation_1)"
        );
        assert_eq!(upgraded.records.len(), 1);
        assert_matches(
            &values(&upgraded)[0],
            &json!({
                "annotationId": "annotation-1",
                "comment": "Render <preview_annotation>",
                "targetSummary": "1 selected element.",
            }),
        );
    }

    #[test]
    fn keeps_adjacent_preview_annotations_separate_and_in_their_original_order() {
        let upgraded = upgrade(
            &[
                "Compare",
                "<preview_annotation>",
                "Id: annotation-1",
                "Page: First",
                "Comment: First comment",
                "</preview_annotation>",
                "<preview_annotation>",
                "Id: annotation-2",
                "Page: Second",
                "Comment: Second comment",
                "</preview_annotation>",
            ]
            .join("\n"),
        );
        assert_eq!(
            upgraded.text,
            "Compare\n\n[First](t3-context://v1/preview-annotation/legacy_preview-annotation_1) [Second](t3-context://v1/preview-annotation/legacy_preview-annotation_2)"
        );
        let values = values(&upgraded);
        assert_matches(
            &values[0],
            &json!({ "annotationId": "annotation-1", "comment": "First comment" }),
        );
        assert_matches(
            &values[1],
            &json!({ "annotationId": "annotation-2", "comment": "Second comment" }),
        );
    }

    #[test]
    fn replaces_inline_review_comments_in_place_and_neutralizes_forged_closers() {
        let text = [
            "Before",
            "<review_comment sectionId=\"diff-1\" sectionTitle=\"Changes\" filePath=\"a/b.ts\" startIndex=\"4\" endIndex=\"6\" rangeLabel=\"L4-L6\">",
            "Why? &lt;/review_comment&gt; still text",
            "```diff",
            "+ const x = 1;",
            "```",
            "</review_comment>",
            "After",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            "Before\n[b.ts L4-L6](t3-context://v1/review-comment/legacy_review-comment_1)\nAfter"
        );
        assert_eq!(
            values(&upgraded),
            vec![json!({
                "version": 1,
                "contextId": "legacy_review-comment_1",
                "kind": "review-comment",
                "label": "b.ts L4-L6",
                "sectionId": "diff-1",
                "sectionTitle": "Changes",
                "filePath": "a/b.ts",
                "startIndex": 4,
                "endIndex": 6,
                "rangeLabel": "L4-L6",
                "text": "Why? &lt;/review_comment&gt; still text",
                "diff": "+ const x = 1;",
                "fenceLanguage": "diff",
            })]
        );
    }

    #[test]
    fn appends_trailing_review_blocks_after_the_other_trailing_blocks() {
        let text = [
            "prose",
            "",
            "<terminal_context>",
            "- T line 1:",
            "  1 | a",
            "</terminal_context>",
            "",
            "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"2\">",
            "note",
            "```diff",
            "+x",
            "```",
            "</review_comment>",
            "",
            "<review_comment sectionId=\"s\" filePath=\"g.ts\" startIndex=\"3\" endIndex=\"3\">",
            "note 2",
            "```diff",
            "+y",
            "```",
            "</review_comment>",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            "prose\n\n[T line 1](t3-context://v1/terminal/legacy_terminal_1) [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1) [g.ts line](t3-context://v1/review-comment/legacy_review-comment_2)"
        );
        let ids: Vec<&str> = upgraded
            .records
            .iter()
            .map(|record| record.context_id().as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "legacy_terminal_1",
                "legacy_review-comment_1",
                "legacy_review-comment_2"
            ]
        );
    }

    #[test]
    fn keeps_adjacent_trailing_reviews_in_place_including_their_original_spacing() {
        let upgraded = upgrade(&format!("Compare {REVIEW}  {REVIEW}"));
        assert_eq!(
            upgraded.text,
            "Compare [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1)  [f.ts line](t3-context://v1/review-comment/legacy_review-comment_2)"
        );
    }

    #[test]
    fn keeps_a_trailing_review_beside_a_terminal_chip_whose_payload_follows_it() {
        let upgraded = upgrade(&format!(
            "@build:7 {REVIEW}\n\n<terminal_context>\n- Build line 7:\n  output\n</terminal_context>"
        ));
        assert_eq!(
            upgraded.text,
            "[Build line 7](t3-context://v1/terminal/legacy_terminal_1) [f.ts line](t3-context://v1/review-comment/legacy_review-comment_1)"
        );
    }

    #[test]
    fn handles_the_combined_legacy_send_order_terminal_element_preview_review() {
        let text = [
            "See \u{FFFC}",
            "<review_comment sectionId=\"s\" filePath=\"f.ts\" startIndex=\"1\" endIndex=\"1\">",
            "note",
            "```diff",
            "+x",
            "```",
            "</review_comment>",
            "",
            "<terminal_context>",
            "- T line 1:",
            "  1 | a",
            "</terminal_context>",
            "",
            "<element_context>",
            "- <div>:",
            "  url: http://x/",
            "</element_context>",
            "",
            "<preview_annotation>",
            "Preview annotation:",
            "Id: p1",
            "Page: P",
            "</preview_annotation>",
        ]
        .join("\n");
        let upgraded = upgrade(&text);
        assert_eq!(
            upgraded.text,
            [
                "See [T line 1](t3-context://v1/terminal/legacy_terminal_1)",
                "[f.ts line](t3-context://v1/review-comment/legacy_review-comment_1)",
                "",
                "[<div>](t3-context://v1/element/legacy_element_1) [P](t3-context://v1/preview-annotation/legacy_preview-annotation_1)",
            ]
            .join("\n")
        );
        let ids: Vec<&str> = upgraded
            .records
            .iter()
            .map(|record| record.context_id().as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "legacy_terminal_1",
                "legacy_element_1",
                "legacy_preview-annotation_1",
                "legacy_review-comment_1",
            ]
        );
    }
}

#[cfg(test)]
mod qc_regressions {
    use super::*;

    #[test]
    fn a_long_private_use_run_does_not_panic_the_legacy_upgrade() {
        // The review token used to double until it outgrew the regex size limit, which a run of
        // about 32k private-use characters reached. The placeholder is now one character.
        let message = |run: usize| {
            format!(
                "{}<element_context>\nhtml: <div>\n</element_context>",
                "\u{E000}".repeat(run)
            )
        };
        let short = upgrade_legacy_context_message(&message(3));
        let long = upgrade_legacy_context_message(&message(40_000));
        assert_eq!(long.records.len(), short.records.len());
        assert_eq!(
            long.text.chars().filter(|ch| *ch == '\u{E000}').count(),
            40_000
        );
    }

    #[test]
    fn a_message_using_every_private_use_character_is_left_unchanged() {
        let every: String = (0xE000_u32..=0xF8FF).filter_map(char::from_u32).collect();
        let text = format!("{every}<element_context>\nhtml: <div>\n</element_context>");
        let upgraded = upgrade_legacy_context_message(&text);
        assert_eq!(upgraded.text, text);
        assert!(upgraded.records.is_empty());
    }

    #[test]
    fn element_fields_follow_js_dot_across_line_terminators() {
        // Node 22: /^([a-z]+): (.*)$/.exec("html: <div>\r") is null, and so is one with U+2028.
        assert!(ELEMENT_FIELD.captures("html: <div>\r").is_none());
        assert!(ELEMENT_FIELD.captures("html: a\u{2028}b").is_none());
        let field = ELEMENT_FIELD.captures("html: <div>").unwrap();
        assert_eq!(field.get(2).unwrap().as_str(), "<div>");
    }

    #[test]
    fn source_location_rejects_a_trailing_carriage_return_like_upstream() {
        // Node 22: /^(.*?)(?::([0-9]+))?(?::([0-9]+))?$/.exec("src/a.ts:3\r") is null.
        assert!(SOURCE_LOCATION.captures("src/a.ts:3\r").is_none());
        assert!(SOURCE_LOCATION.captures("src/a.ts:3").is_some());
    }

    #[test]
    fn review_fence_is_found_after_a_lone_carriage_return() {
        // Node 22: "text\r```js\rcode".search(/^(`{3,})([^\n]*)$/m) is 5.
        let found = REVIEW_BOUNDARY.find("text\r```js\rcode").unwrap().unwrap();
        assert_eq!(found.start(), 5);
    }
}
