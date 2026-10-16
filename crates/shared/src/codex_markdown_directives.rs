//! Port of upstream/packages/shared/src/codexMarkdownDirectives.ts
//!
//! `renderCodexDirectivesForCopy` and `renderCodexInlineDirectivesAsMarkdown`, plus the directive
//! matches thread find (`threadFindText.ts`) substitutes before parsing. The remark plugin
//! (`remarkCodexDirectives`) and `splitCodexArtifactTemplateMarkdown` have no counterpart: the
//! renderers work from the source ranges found here.
//!
//! Moved here from `turin-app` (`components/chat/timeline`) so the thread find extractor in this
//! crate shares one directive scanner with the copy adapter. The citation and template
//! resolution now come from [`crate::codex_file_citations`] and
//! [`crate::codex_artifact_templates`] instead of private copies.
//!
//! Upstream parses the whole message with remark (`remark-parse` plus a
//! micromark directive extension restricted to Codex's three directive
//! names), rewrites the directive nodes, and then splices replacements into
//! the source at the nodes' offsets. There is no micromark here, so
//! [`collect_directive_matches`] finds the same nodes with a small scanner
//! that implements only the Markdown that decides whether a directive is a
//! directive:
//!
//! - fenced code blocks (no directives inside), ATX headings, blank lines and
//!   list items as block boundaries, with `>` and list markers stripped
//!   before looking for fences and `::artifact-template` leaf directives;
//! - inline backslash escapes, code spans, autolinks and raw HTML tags (no
//!   directives inside), and links/images, because a citation inside link
//!   text stays literal and image alt text has no directive nodes at all;
//! - the directive grammar itself: `:name[label]{attributes}` text
//!   directives (not after an unescaped `:`) and `::name[label]{attributes}`
//!   leaf directives alone on their line (which, like upstream, interrupt a
//!   paragraph).
//!
//! Known gaps against CommonMark: indented code blocks and HTML blocks are
//! not recognised (their text is scanned like a paragraph), container
//! nesting is approximated by stripping prefixes, attribute values decode
//! only numeric and the five XML named character references, and a
//! follow-up's label text is its raw source rather than its rendered text.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::{is_js_whitespace, js_trim};

use crate::codex_artifact_templates::{
    CodexArtifactTemplate, CodexArtifactTemplateAttributes,
    codex_artifact_template_presentation_label, resolve_codex_artifact_template,
};
use crate::codex_file_citations::{
    CodexFileCitationAttributes, codex_file_citation_markdown, resolve_codex_file_citation_link,
};

const CODEX_FILE_CITATION_NAME: &str = "codex-file-citation";
const CODEX_FOLLOW_UP_NAME: &str = "codex-followup";
const CODEX_ARTIFACT_TEMPLATE_NAME: &str = "artifact-template";

/// Directive attributes as mdast-util-directive produces them: a bare name
/// has the value `""`, a repeated name keeps the last value.
type Attributes = HashMap<String, String>;

// ---------------------------------------------------------------------------
// Directive grammar (micromark-extension-directive, restricted like upstream's
// `restrictedDirective` gates)
// ---------------------------------------------------------------------------

/// `directiveNameEnds`: the name must be followed by the end, whitespace or
/// punctuation other than `-`/`_`. Non-ASCII punctuation is not recognised
/// (no Unicode category tables); that cannot change the output, because a
/// directive only renders when its name is followed by `[` or `{`.
fn directive_name_ends(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(c) => c.is_whitespace() || (c.is_ascii_punctuation() && c != '-' && c != '_'),
    }
}

struct ParsedDirective {
    end: usize,
    /// Byte range of the label's content, inside the brackets.
    label: Option<(usize, usize)>,
    attributes: Option<Attributes>,
    /// The attributes in `Object.entries` order (first occurrence of each name), for
    /// [`restored_leaf_directive_text`].
    attribute_list: Vec<(String, String)>,
}

fn is_space_or_tab(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn is_line_ending(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// `[label]` starting at `start`; returns the index after `]`.
fn parse_label(bytes: &[u8], start: usize, limit: usize, allow_eol: bool) -> Option<usize> {
    let mut balance = 0_usize;
    let mut i = start + 1;
    while i < limit {
        match bytes[i] {
            b'\\' if matches!(bytes.get(i + 1), Some(b'[' | b']')) && i + 1 < limit => i += 1,
            b'[' => balance += 1,
            b']' if balance == 0 => return Some(i + 1),
            b']' => balance -= 1,
            b if is_line_ending(b) && !allow_eol => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

static CHARACTER_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)&(?:#([0-9]{1,7})|#x([0-9a-f]{1,6})|([0-9a-z]{1,31}));").expect("static regex")
});

/// Attribute values decode character references (not backslash escapes).
fn decode_character_references(value: &str) -> String {
    CHARACTER_REFERENCE
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let numeric = captures
                .get(1)
                .and_then(|decimal| decimal.as_str().parse::<u32>().ok())
                .or_else(|| {
                    captures
                        .get(2)
                        .and_then(|hex| u32::from_str_radix(hex.as_str(), 16).ok())
                });
            if let Some(code) = numeric {
                return match char::from_u32(code) {
                    Some(c) if code != 0 => c.to_string(),
                    _ => '\u{fffd}'.to_string(),
                };
            }
            let named = captures.get(3).map_or("", |name| name.as_str());
            match named {
                "amp" => "&".to_string(),
                "lt" => "<".to_string(),
                "gt" => ">".to_string(),
                "quot" => "\"".to_string(),
                "apos" => "'".to_string(),
                _ => captures
                    .get(0)
                    .map_or_else(String::new, |all| all.as_str().to_string()),
            }
        })
        .into_owned()
}

/// Characters that end an unquoted value or a `#id` / `.class` shortcut.
fn ends_unquoted(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'=' | b'>' | b'`' | b'}')
}

/// `{attributes}` starting at `start`; returns the index after `}`.
fn parse_attributes(
    source: &str,
    start: usize,
    limit: usize,
    allow_eol: bool,
) -> Option<(usize, Vec<(String, String)>)> {
    let bytes = source.as_bytes();
    // mdast-util-directive's `directiveAttributes`: one entry per id, class, name and value.
    let mut list: Vec<(String, String)> = Vec::new();
    let mut i = start + 1;
    let skip_whitespace = |mut i: usize| -> Option<usize> {
        while i < limit && (is_space_or_tab(bytes[i]) || is_line_ending(bytes[i])) {
            if is_line_ending(bytes[i]) && !allow_eol {
                return None;
            }
            i += 1;
        }
        Some(i)
    };
    // After a value or bare name: whitespace, `}`, or (after a shortcut) another shortcut.
    let separated = |i: usize, shortcut: bool| {
        i < limit
            && (bytes[i].is_ascii_whitespace()
                || bytes[i] == b'}'
                || (shortcut && matches!(bytes[i], b'#' | b'.')))
    };
    loop {
        i = skip_whitespace(i)?;
        if i >= limit {
            return None;
        }
        match bytes[i] {
            b'}' => break,
            marker @ (b'#' | b'.') => {
                let value_start = i + 1;
                let mut j = value_start;
                while j < limit && !ends_unquoted(bytes[j]) && !matches!(bytes[j], b'#' | b'.') {
                    j += 1;
                }
                if j == value_start || !separated(j, true) {
                    return None;
                }
                let value = &source[value_start..j];
                let key = if marker == b'#' { "id" } else { "class" };
                list.push((key.to_string(), decode_character_references(value)));
                i = j;
            }
            b if b.is_ascii_alphabetic() || b == b'_' || b == b':' => {
                let name_start = i;
                let mut j = i + 1;
                while j < limit
                    && (bytes[j].is_ascii_alphanumeric()
                        || matches!(bytes[j], b'-' | b'.' | b':' | b'_'))
                {
                    j += 1;
                }
                let name = source[name_start..j].to_string();
                let after_name = skip_whitespace(j)?;
                if after_name < limit && bytes[after_name] == b'=' {
                    let value_start = skip_whitespace(after_name + 1)?;
                    if value_start >= limit {
                        return None;
                    }
                    let (value, next) = match bytes[value_start] {
                        quote @ (b'"' | b'\'') => {
                            let mut k = value_start + 1;
                            while k < limit && bytes[k] != quote {
                                if is_line_ending(bytes[k]) && !allow_eol {
                                    return None;
                                }
                                k += 1;
                            }
                            if k >= limit {
                                return None;
                            }
                            (&source[value_start + 1..k], k + 1)
                        }
                        _ => {
                            let mut k = value_start;
                            while k < limit && !ends_unquoted(bytes[k]) {
                                k += 1;
                            }
                            if k == value_start {
                                return None;
                            }
                            (&source[value_start..k], k)
                        }
                    };
                    if !separated(next, false) {
                        return None;
                    }
                    list.push((name, decode_character_references(value)));
                    i = next;
                } else {
                    if !separated(j, false) {
                        return None;
                    }
                    list.push((name, String::new()));
                    i = j;
                }
            }
            _ => return None,
        }
    }
    // `exitAttributes`: a repeated name keeps the last value in its first position, and a
    // non-empty `class` accumulates.
    let mut cleaned: Vec<(String, String)> = Vec::new();
    for (name, value) in list {
        match cleaned.iter_mut().find(|(known, _)| *known == name) {
            Some((_, existing)) if name == "class" && !existing.is_empty() => {
                existing.push(' ');
                existing.push_str(&value);
            }
            Some((_, existing)) => *existing = value,
            None => cleaned.push((name, value)),
        }
    }
    Some((i + 1, cleaned))
}

/// After the name: an optional label, then optional attributes. A label or
/// attribute block that fails to parse just ends the directive early, as
/// micromark's `effects.attempt(..., after, after)` does.
fn parse_directive_tail(
    source: &str,
    mut j: usize,
    limit: usize,
    allow_eol: bool,
) -> ParsedDirective {
    let bytes = source.as_bytes();
    let mut label = None;
    if j < limit
        && bytes[j] == b'['
        && let Some(end) = parse_label(bytes, j, limit, allow_eol)
    {
        label = Some((j + 1, end - 1));
        j = end;
    }
    let mut attributes = None;
    let mut attribute_list = Vec::new();
    if j < limit
        && bytes[j] == b'{'
        && let Some((end, parsed)) = parse_attributes(source, j, limit, allow_eol)
    {
        attributes = Some(parsed.iter().cloned().collect::<Attributes>());
        attribute_list = parsed;
        j = end;
    }
    ParsedDirective {
        end: j,
        label,
        attributes,
        attribute_list,
    }
}

/// `:codex-file-citation…` or `:codex-followup…` at `start`.
fn parse_text_directive(
    source: &str,
    start: usize,
    limit: usize,
) -> Option<(&'static str, ParsedDirective)> {
    let rest = &source[start + 1..limit];
    let name = [CODEX_FILE_CITATION_NAME, CODEX_FOLLOW_UP_NAME]
        .into_iter()
        .find(|name| {
            rest.strip_prefix(name)
                .is_some_and(|after| directive_name_ends(after.chars().next()))
        })?;
    let after_name = start + 1 + name.len();
    Some((name, parse_directive_tail(source, after_name, limit, true)))
}

/// `::artifact-template…` at `start`, which must run to `line_end` (trailing
/// spaces and tabs are part of the directive).
fn parse_leaf_directive(source: &str, start: usize, line_end: usize) -> Option<ParsedDirective> {
    let rest = source[start..line_end].strip_prefix("::")?;
    let after = rest.strip_prefix(CODEX_ARTIFACT_TEMPLATE_NAME)?;
    if !directive_name_ends(after.chars().next()) {
        return None;
    }
    let after_name = start + 2 + CODEX_ARTIFACT_TEMPLATE_NAME.len();
    let mut directive = parse_directive_tail(source, after_name, line_end, false);
    let bytes = source.as_bytes();
    let mut end = directive.end;
    while end < line_end && is_space_or_tab(bytes[end]) {
        end += 1;
    }
    if end != line_end {
        return None;
    }
    directive.end = end;
    Some(directive)
}

// ---------------------------------------------------------------------------
// Matching (upstream: parseCodexMarkdown + transformCodexDirectives +
// collectDirectiveMatches)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectiveReplacement {
    /// A rendered file citation (`data.codexFileCitationMarkdown`).
    Markdown(String),
    /// A rendered artifact template card (`data.codexArtifactTemplate`).
    Template(CodexArtifactTemplate),
    /// A follow-up suggestion: keep its label source (`labelRange`).
    LabelRange(usize, usize),
    /// A leaf directive that did not resolve (`restoreLeafDirective`): its own paragraph holding
    /// this rebuilt source text. Copy and inline Markdown leave the source as written.
    Literal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveMatch {
    /// Byte range of the directive in the source.
    pub start: usize,
    pub end: usize,
    pub replacement: DirectiveReplacement,
}

struct LinkOpener {
    /// Index of the `[`.
    position: usize,
    is_image: bool,
    /// `matches.len()` when the opener was seen.
    matches_len: usize,
    /// Links cannot contain links: forming one deactivates earlier `[`s.
    active: bool,
}

static AUTOLINK_OR_HTML: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:",
        // URI autolink
        r"<[A-Za-z][A-Za-z0-9+.\-]{1,31}:[^\x00-\x20<>]*>",
        // email autolink
        r"|<[A-Za-z0-9.!#$%&'*+/=?^_`{|}~\-]+@[A-Za-z0-9](?:[A-Za-z0-9\-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9\-]{0,61}[A-Za-z0-9])?)*>",
        // open tag
        r#"|<[A-Za-z][A-Za-z0-9\-]*(?:[ \t\r\n]+[A-Za-z_:][A-Za-z0-9_.:\-]*(?:[ \t\r\n]*=[ \t\r\n]*(?:[^ \t\r\n"'=<>`]+|'[^']*'|"[^"]*"))?)*[ \t\r\n]*/?>"#,
        // closing tag
        r"|</[A-Za-z][A-Za-z0-9\-]*[ \t\r\n]*>",
        // comment
        r"|<!--(?s:.*?)-->",
        r")",
    ))
    .expect("static regex")
});

static LINK_REFERENCE_DEFINITION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[((?:[^\[\]\\]|\\.){1,999})\]:").expect("static regex"));

/// Link label matching: case-folded, whitespace collapsed, trimmed.
fn normalize_link_label(label: &str) -> String {
    label
        .split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Finds the closing backtick run of exactly `length` backticks.
fn find_code_span_close(bytes: &[u8], from: usize, limit: usize, length: usize) -> Option<usize> {
    let mut i = from;
    while i < limit {
        if bytes[i] == b'`' {
            let run_start = i;
            while i < limit && bytes[i] == b'`' {
                i += 1;
            }
            if i - run_start == length {
                return Some(run_start);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn skip_link_whitespace(bytes: &[u8], mut i: usize, limit: usize) -> usize {
    while i < limit && (is_space_or_tab(bytes[i]) || is_line_ending(bytes[i])) {
        i += 1;
    }
    i
}

/// `(destination "title")` starting at `start`; returns the index after `)`.
fn parse_inline_link_tail(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let mut j = skip_link_whitespace(bytes, start + 1, limit);
    if j < limit && bytes[j] == b')' {
        return Some(j + 1);
    }
    if j < limit && bytes[j] == b'<' {
        j += 1;
        while j < limit && bytes[j] != b'>' {
            if is_line_ending(bytes[j]) || bytes[j] == b'<' {
                return None;
            }
            if bytes[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j >= limit {
            return None;
        }
        j += 1;
    } else {
        let mut depth = 0_usize;
        while j < limit {
            let b = bytes[j];
            if b == b'\\' && j + 1 < limit && bytes[j + 1].is_ascii_punctuation() {
                j += 2;
                continue;
            }
            if b <= 0x20 || b == 0x7f {
                break;
            }
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            j += 1;
        }
        if depth != 0 {
            return None;
        }
    }
    let before_title = j;
    j = skip_link_whitespace(bytes, j, limit);
    if j < limit && j > before_title && matches!(bytes[j], b'"' | b'\'' | b'(') {
        let close = if bytes[j] == b'(' { b')' } else { bytes[j] };
        j += 1;
        while j < limit && bytes[j] != close {
            if bytes[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j >= limit {
            return None;
        }
        j = skip_link_whitespace(bytes, j + 1, limit);
    }
    (j < limit && bytes[j] == b')').then_some(j + 1)
}

/// Whether `]` at `close - 1` closes a link (inline, full, collapsed or
/// shortcut reference); returns the index after the link.
fn link_end(
    source: &str,
    text: &str,
    after_close: usize,
    limit: usize,
    definitions: &HashSet<String>,
) -> Option<usize> {
    let bytes = source.as_bytes();
    if after_close < limit
        && bytes[after_close] == b'('
        && let Some(end) = parse_inline_link_tail(bytes, after_close, limit)
    {
        return Some(end);
    }
    if after_close < limit && bytes[after_close] == b'[' {
        let mut j = after_close + 1;
        while j < limit && bytes[j] != b']' {
            if bytes[j] == b'[' {
                return None;
            }
            if bytes[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j < limit {
            let reference = &source[after_close + 1..j];
            let label = if js_trim(reference).is_empty() {
                text
            } else {
                reference
            };
            // A full reference to an undefined label is not a link, and
            // also blocks the shortcut reading.
            return definitions
                .contains(&normalize_link_label(label))
                .then_some(j + 1);
        }
    }
    definitions
        .contains(&normalize_link_label(text))
        .then_some(after_close)
}

/// Scans one paragraph or heading's inline content.
fn scan_inline(
    source: &str,
    start: usize,
    end: usize,
    definitions: &HashSet<String>,
    matches: &mut Vec<DirectiveMatch>,
) {
    let bytes = source.as_bytes();
    let mut openers: Vec<LinkOpener> = Vec::new();
    let mut previous_colon = false;
    let mut i = start;
    while i < end {
        let b = bytes[i];
        let mut literal_colon = false;
        match b {
            b'\\' if i + 1 < end && bytes[i + 1].is_ascii_punctuation() => {
                // An escaped `:` does not block a following directive.
                i += 2;
            }
            b'`' => {
                let mut run_end = i;
                while run_end < end && bytes[run_end] == b'`' {
                    run_end += 1;
                }
                i = match find_code_span_close(bytes, run_end, end, run_end - i) {
                    Some(close) => close + (run_end - i),
                    None => run_end,
                };
            }
            b'<' => {
                i += AUTOLINK_OR_HTML
                    .find(&source[i..end])
                    .map_or(1, |found| found.end());
            }
            b':' if !previous_colon => match parse_text_directive(source, i, end) {
                Some((name, directive)) => {
                    record_text_directive(source, i, name, &directive, matches);
                    i = directive.end;
                }
                None => {
                    literal_colon = true;
                    i += 1;
                }
            },
            b':' => {
                literal_colon = true;
                i += 1;
            }
            b'!' if i + 1 < end && bytes[i + 1] == b'[' => {
                openers.push(LinkOpener {
                    position: i + 1,
                    is_image: true,
                    matches_len: matches.len(),
                    active: true,
                });
                i += 2;
            }
            b'[' => {
                openers.push(LinkOpener {
                    position: i,
                    is_image: false,
                    matches_len: matches.len(),
                    active: true,
                });
                i += 1;
            }
            b']' => {
                let link = openers
                    .pop()
                    .filter(|opener| opener.active)
                    .and_then(|opener| {
                        let text = &source[opener.position + 1..i];
                        link_end(source, text, i + 1, end, definitions).map(|after| (opener, after))
                    });
                match link {
                    Some((opener, after)) => {
                        if opener.is_image {
                            // Image alt text is a plain string in mdast.
                            matches.truncate(opener.matches_len);
                        } else {
                            // `renderFileCitation(..., insideLink)` keeps these literal.
                            let mut index = opener.matches_len;
                            while index < matches.len() {
                                if matches!(
                                    matches[index].replacement,
                                    DirectiveReplacement::Markdown(_)
                                ) {
                                    matches.remove(index);
                                } else {
                                    index += 1;
                                }
                            }
                            for earlier in openers.iter_mut().filter(|opener| !opener.is_image) {
                                earlier.active = false;
                            }
                        }
                        i = after;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
        previous_colon = literal_colon;
    }
}

/// `renderFileCitation` / `renderFollowUp` for a parsed text directive.
fn record_text_directive(
    source: &str,
    start: usize,
    name: &str,
    directive: &ParsedDirective,
    matches: &mut Vec<DirectiveMatch>,
) {
    let empty = Attributes::new();
    let attributes = directive.attributes.as_ref().unwrap_or(&empty);
    if name == CODEX_FILE_CITATION_NAME {
        let citation_attributes = CodexFileCitationAttributes {
            path: attributes.get("path").map(String::as_str),
            line_range_start: attributes.get("line_range_start").map(String::as_str),
        };
        if let Some(citation) = resolve_codex_file_citation_link(Some(&citation_attributes)) {
            matches.push(DirectiveMatch {
                start,
                end: directive.end,
                replacement: DirectiveReplacement::Markdown(codex_file_citation_markdown(
                    &citation,
                )),
            });
        }
        return;
    }
    // Shows only the label. T3 has no suggestion UI, so the hidden prompt is not shown.
    let has_prompt = attributes
        .get("prompt")
        .is_some_and(|prompt| !js_trim(prompt).is_empty());
    let label = directive
        .label
        .filter(|(label_start, label_end)| !js_trim(&source[*label_start..*label_end]).is_empty());
    if let (true, Some((label_start, label_end))) = (has_prompt, label) {
        matches.push(DirectiveMatch {
            start,
            end: directive.end,
            replacement: DirectiveReplacement::LabelRange(label_start, label_end),
        });
    }
}

/// Strips leading indentation, `>` markers and list markers. Returns the
/// prefix length and whether a list marker (a new list item) was stripped.
fn strip_container_prefix(line: &str) -> (usize, bool) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut list_marker = false;
    loop {
        while i < bytes.len() && is_space_or_tab(bytes[i]) {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'>' {
            i += 1;
            continue;
        }
        let followed_by_space = |j: usize| j >= bytes.len() || is_space_or_tab(bytes[j]);
        if i < bytes.len() && matches!(bytes[i], b'-' | b'*' | b'+') && followed_by_space(i + 1) {
            i += 1;
            list_marker = true;
            continue;
        }
        let mut digits_end = i;
        while digits_end < bytes.len() && digits_end - i < 9 && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        if digits_end > i
            && digits_end < bytes.len()
            && matches!(bytes[digits_end], b'.' | b')')
            && followed_by_space(digits_end + 1)
        {
            i = digits_end + 1;
            list_marker = true;
            continue;
        }
        return (i, list_marker);
    }
}

/// A fence opener: three or more backticks (with no backtick in the info
/// string) or tildes.
fn fence_open(content: &str) -> Option<(u8, usize)> {
    let fence_char = *content.as_bytes().first()?;
    if fence_char != b'`' && fence_char != b'~' {
        return None;
    }
    let length = content.bytes().take_while(|b| *b == fence_char).count();
    if length < 3 || (fence_char == b'`' && content[length..].contains('`')) {
        return None;
    }
    Some((fence_char, length))
}

fn fence_closes(content: &str, fence_char: u8, length: usize) -> bool {
    let run = content.bytes().take_while(|b| *b == fence_char).count();
    run >= length && content[run..].bytes().all(is_space_or_tab)
}

fn is_atx_heading(content: &str) -> bool {
    let hashes = content.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes)
        && content
            .as_bytes()
            .get(hashes)
            .is_none_or(|b| is_space_or_tab(*b))
}

/// `(line start, content end)` for every line; the end excludes `\r?\n`.
fn line_ranges(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' || bytes[i] == b'\r' {
            ranges.push((start, i));
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    ranges.push((start, bytes.len()));
    ranges
}

/// `textContent` of the label's Markdown: text, code and raw HTML values, concatenated.
fn markdown_text_content(node: &markdown::mdast::Node) -> String {
    use markdown::mdast::Node;
    match node {
        Node::Text(text) => text.value.clone(),
        Node::InlineCode(code) => code.value.clone(),
        Node::Code(code) => code.value.clone(),
        Node::Html(html) => html.value.clone(),
        other => other
            .children()
            .map(|children| children.iter().map(markdown_text_content).collect())
            .unwrap_or_default(),
    }
}

/// `sourceForDirective` for a leaf directive. Upstream runs the directive plugin without a source
/// file, so the text is always rebuilt from the node: `::name`, `[label text]` when there is a
/// label, and `{name="value" ...}` (JSON-quoted values, bare names as `name=""`) when there are
/// attributes.
fn restored_leaf_directive_text(source: &str, directive: &ParsedDirective) -> String {
    let mut text = format!("::{CODEX_ARTIFACT_TEMPLATE_NAME}");
    if let Some((start, end)) = directive.label.filter(|(start, end)| start < end) {
        let label = &source[start..end];
        let content = markdown::to_mdast(label, &markdown::ParseOptions::gfm())
            .map(|root| markdown_text_content(&root))
            .unwrap_or_else(|_| label.to_string());
        text.push('[');
        text.push_str(&content);
        text.push(']');
    }
    if !directive.attribute_list.is_empty() {
        let attributes: Vec<String> = directive
            .attribute_list
            .iter()
            .map(|(name, value)| {
                format!(
                    "{name}={}",
                    serde_json::to_string(value).unwrap_or_default()
                )
            })
            .collect();
        text.push('{');
        text.push_str(&attributes.join(" "));
        text.push('}');
    }
    text
}

/// The directives in `markdown` that render as something else, in source order within each
/// paragraph. Ranges are byte offsets.
pub fn collect_directive_matches(markdown: &str) -> Vec<DirectiveMatch> {
    let lines = line_ranges(markdown);
    let definitions: HashSet<String> = lines
        .iter()
        .filter_map(|(start, end)| {
            let line = &markdown[*start..*end];
            let (prefix, _) = strip_container_prefix(line);
            LINK_REFERENCE_DEFINITION
                .captures(&line[prefix..])
                .and_then(|captures| captures.get(1))
                .map(|label| normalize_link_label(label.as_str()))
        })
        .collect();

    let mut matches = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut paragraph: Option<(usize, usize)> = None;
    let flush = |paragraph: &mut Option<(usize, usize)>, matches: &mut Vec<DirectiveMatch>| {
        if let Some((start, end)) = paragraph.take() {
            scan_inline(markdown, start, end, &definitions, matches);
        }
    };

    for (line_start, line_end) in lines {
        let line = &markdown[line_start..line_end];
        let (prefix, list_marker) = strip_container_prefix(line);
        let content = &line[prefix..];
        if let Some((fence_char, length)) = fence {
            if fence_closes(content, fence_char, length) {
                fence = None;
            }
            continue;
        }
        if content.bytes().all(is_space_or_tab) {
            flush(&mut paragraph, &mut matches);
            continue;
        }
        if let Some(opened) = fence_open(content) {
            flush(&mut paragraph, &mut matches);
            fence = Some(opened);
            continue;
        }
        if list_marker {
            flush(&mut paragraph, &mut matches);
        }
        let content_start = line_start + prefix;
        if let Some(directive) = parse_leaf_directive(markdown, content_start, line_end) {
            // A leaf directive interrupts a paragraph.
            flush(&mut paragraph, &mut matches);
            let empty = Attributes::new();
            let attributes = directive.attributes.as_ref().unwrap_or(&empty);
            let template_attributes = CodexArtifactTemplateAttributes {
                artifact_kind: attributes.get("artifact_kind").map(String::as_str),
                display_name: attributes.get("display_name").map(String::as_str),
                gallery_kind: attributes
                    .get("gallery_kind")
                    .map(|kind| Some(kind.as_str())),
                skill_directory: attributes.get("skill_directory").map(String::as_str),
                skill_name: attributes.get("skill_name").map(String::as_str),
            };
            matches.push(DirectiveMatch {
                start: content_start,
                end: directive.end,
                replacement: match resolve_codex_artifact_template(Some(&template_attributes)) {
                    Some(template) => DirectiveReplacement::Template(template),
                    None => DirectiveReplacement::Literal(restored_leaf_directive_text(
                        markdown, &directive,
                    )),
                },
            });
            continue;
        }
        if is_atx_heading(content) {
            flush(&mut paragraph, &mut matches);
            scan_inline(markdown, line_start, line_end, &definitions, &mut matches);
            continue;
        }
        paragraph = Some((paragraph.map_or(line_start, |(start, _)| start), line_end));
    }
    flush(&mut paragraph, &mut matches);
    matches
}

fn render_directive_matches(
    markdown: &str,
    replacement_for: impl Fn(&DirectiveMatch) -> Option<String>,
) -> String {
    let mut matches = collect_directive_matches(markdown);
    matches.sort_by_key(|directive_match| std::cmp::Reverse(directive_match.start));
    let mut rendered = markdown.to_string();
    for directive_match in &matches {
        let replacement = match directive_match.replacement {
            DirectiveReplacement::LabelRange(start, end) => Some(markdown[start..end].to_string()),
            _ => replacement_for(directive_match),
        };
        if let Some(replacement) = replacement {
            rendered.replace_range(directive_match.start..directive_match.end, &replacement);
        }
    }
    rendered
}

/// Native Markdown renderers use this adapter because they cannot consume a Remark tree.
pub fn render_codex_inline_directives_as_markdown(markdown: &str) -> String {
    if !markdown.contains(&format!(":{CODEX_FILE_CITATION_NAME}"))
        && !markdown.contains(&format!(":{CODEX_FOLLOW_UP_NAME}"))
    {
        return markdown.to_string();
    }

    render_directive_matches(markdown, |directive_match| {
        match &directive_match.replacement {
            DirectiveReplacement::Markdown(markdown) => Some(markdown.clone()),
            _ => None,
        }
    })
}

/// Matches the Markdown emitted when users copy rendered Codex directive UI.
pub fn render_codex_directives_for_copy(markdown: &str) -> String {
    if !markdown.contains(&format!(":{CODEX_FILE_CITATION_NAME}"))
        && !markdown.contains(&format!(":{CODEX_FOLLOW_UP_NAME}"))
        && !markdown.contains(&format!("::{CODEX_ARTIFACT_TEMPLATE_NAME}"))
    {
        return markdown.to_string();
    }

    render_directive_matches(markdown, |directive_match| {
        match &directive_match.replacement {
            DirectiveReplacement::Markdown(markdown) => Some(markdown.clone()),
            DirectiveReplacement::Template(template) => Some(format!(
                "{} ({})",
                template.display_name,
                codex_artifact_template_presentation_label(&template.artifact_kind)
                    .unwrap_or_default()
            )),
            DirectiveReplacement::LabelRange(..) | DirectiveReplacement::Literal(_) => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const FILE_CITATION: &str =
        r#":codex-file-citation{path="outputs/report.xlsx" purpose="output"}"#;
    const FOLLOW_UP: &str =
        r#":codex-followup[Prepare print version]{prompt="Prepare the document for printing."}"#;
    const ARTIFACT_TEMPLATE: &str = r#"::artifact-template{skill_name="artifact-template-hello-world" skill_directory="/Users/test/.codex/skills/artifact-template-hello-world" display_name="Hello World" artifact_kind="document"}"#;

    // "directive copy adapter"

    #[test]
    fn copies_the_markdown_representations_shown_by_citation_chips_and_template_cards() {
        assert_eq!(
            render_codex_directives_for_copy(&format!(
                "Created {FILE_CITATION}.\n\n{ARTIFACT_TEMPLATE}\n\n- {FOLLOW_UP}"
            )),
            "Created [report.xlsx](<outputs/report.xlsx>).\n\nHello World (Document template)\n\n- Prepare print version"
        );
    }

    #[test]
    fn leaves_excluded_and_malformed_directive_source_unchanged() {
        let markdown = [
            format!("`{FILE_CITATION}`"),
            r#"::artifact-template{display_name="Hello World"}"#.to_string(),
        ]
        .join("\n\n");
        assert_eq!(render_codex_directives_for_copy(&markdown), markdown);
    }

    // "$name file citation round trips". Upstream re-parses the output with
    // remark and `parseMarkdownFileLink`; here the produced link is asserted
    // directly.
    #[test]
    fn preserves_the_literal_path_and_line_in_file_citations() {
        for (path, expected) in [
            (
                r"C:\Users\test\[draft]\report.md",
                r"[report.md](<C:%5CUsers%5Ctest%5C[draft]%5Creport.md#L7>)",
            ),
            (
                r"\\server\share\report.md",
                r"[report.md](<%5C%5Cserver%5Cshare%5Creport.md#L7>)",
            ),
            ("outputs/report.md", "[report.md](<outputs/report.md#L7>)"),
            (
                "/tmp/report%5C.md",
                "[report%5C.md](</tmp/report%255C.md#L7>)",
            ),
        ] {
            assert_eq!(
                render_codex_directives_for_copy(&format!(
                    r#":codex-file-citation{{path="{path}" line_range_start="7"}}"#
                )),
                expected,
                "{path}"
            );
        }
    }

    // "native Markdown adapters": these exercise
    // `renderCodexInlineDirectivesAsMarkdown`, which shares the matcher and
    // the citation/follow-up replacements with the copy adapter.

    #[test]
    fn renders_file_citations_as_portable_links() {
        assert_eq!(
            render_codex_directives_for_copy(&format!("Created {FILE_CITATION}.")),
            "Created [report.xlsx](<outputs/report.xlsx>)."
        );
    }

    #[test]
    fn renders_follow_up_suggestions_as_their_label_markdown() {
        assert_eq!(
            render_codex_directives_for_copy(&format!(
                "- {FOLLOW_UP}\n- :codex-followup[Review **document**]{{prompt=\"Review it.\"}}"
            )),
            "- Prepare print version\n- Review **document**"
        );
    }

    #[test]
    fn does_not_render_excluded_follow_up_syntax() {
        for markdown in [format!("\\{FOLLOW_UP}"), format!("`{FOLLOW_UP}`")] {
            assert_eq!(render_codex_directives_for_copy(&markdown), markdown);
        }
    }

    #[test]
    fn does_not_render_excluded_citation_syntax() {
        for markdown in [
            format!("\\{FILE_CITATION}"),
            format!("`{FILE_CITATION}`"),
            format!("```text\n{FILE_CITATION}\n```"),
            format!("[See {FILE_CITATION}](https://example.com)"),
        ] {
            assert_eq!(render_codex_directives_for_copy(&markdown), markdown);
        }
    }

    #[test]
    fn leaves_malformed_and_code_artifact_template_examples_in_markdown() {
        for markdown in [
            r#"::artifact-template{display_name="Hello World"}"#.to_string(),
            format!("`{ARTIFACT_TEMPLATE}`"),
        ] {
            assert_eq!(render_codex_directives_for_copy(&markdown), markdown);
        }
    }

    // "remarkCodexDirectives": upstream compares the directive tree with
    // plain remark's; for the copy adapter that means the source comes back
    // unchanged.

    #[test]
    fn does_not_change_unrelated_colon_syntax() {
        for markdown in [
            "Meeting at 10:30",
            "Open src/main.ts:42",
            "Use :hover and :tada:",
            "::note",
            ":::note\ncontent\n:::",
            r#":codex-file-citation-extra{path="outputs/report.xlsx"}"#,
            "::artifact-template-extra",
            r#":codex-followup-extra[Label]{prompt="Prompt"}"#,
        ] {
            assert_eq!(render_codex_directives_for_copy(markdown), markdown);
        }
    }

    #[test]
    fn keeps_malformed_supported_directives_literal() {
        for markdown in [
            r#":codex-file-citation{purpose="output"}"#,
            r#"::artifact-template{skill_name="artifact-template-hello-world"}"#,
            ":codex-followup[Prepare print version]",
            r#":codex-followup[ ]{prompt="Prepare the document for printing."}"#,
            r#":codex-followup[Prepare print version]{prompt=" "}"#,
        ] {
            assert_eq!(render_codex_directives_for_copy(markdown), markdown);
        }
    }

    #[test]
    fn renders_a_follow_up_as_its_label_and_an_artifact_template_card() {
        assert_eq!(
            render_codex_directives_for_copy(&format!("Next: {FOLLOW_UP}")),
            "Next: Prepare print version"
        );
        assert_eq!(
            render_codex_directives_for_copy(ARTIFACT_TEMPLATE),
            "Hello World (Document template)"
        );
    }

    // Not upstream tests: pin the scanner's Markdown handling.

    #[test]
    fn keeps_citations_in_image_alt_text_and_reference_links_literal() {
        for markdown in [
            format!("![{FILE_CITATION}](image.png)"),
            format!("[See {FILE_CITATION}][docs]\n\n[docs]: https://example.com"),
            format!("[See {FILE_CITATION}]\n\n[see {FILE_CITATION}]: https://example.com"),
            format!("<a title=\"{}\">", FILE_CITATION.replace('"', "'")),
        ] {
            assert_eq!(render_codex_directives_for_copy(&markdown), markdown);
        }
    }

    #[test]
    fn renders_citations_next_to_brackets_that_are_not_links() {
        assert_eq!(
            render_codex_directives_for_copy(&format!("[note] {FILE_CITATION} [x]")),
            "[note] [report.xlsx](<outputs/report.xlsx>) [x]"
        );
        assert_eq!(
            render_codex_directives_for_copy(&format!("[See {FILE_CITATION}][undefined]")),
            "[See [report.xlsx](<outputs/report.xlsx>)][undefined]"
        );
    }

    #[test]
    fn leaf_directives_interrupt_paragraphs_and_need_their_own_line() {
        assert_eq!(
            render_codex_directives_for_copy(&format!("Before\n{ARTIFACT_TEMPLATE}  \nAfter")),
            "Before\nHello World (Document template)\nAfter"
        );
        let trailing = format!("{ARTIFACT_TEMPLATE} trailing");
        assert_eq!(render_codex_directives_for_copy(&trailing), trailing);
        let doubled = format!(":{FILE_CITATION}");
        assert_eq!(render_codex_directives_for_copy(&doubled), doubled);
        assert_eq!(
            render_codex_directives_for_copy(&format!("\\:{FILE_CITATION}")),
            "\\:[report.xlsx](<outputs/report.xlsx>)"
        );
    }

    #[test]
    fn escapes_citation_labels_and_decodes_attribute_references() {
        assert_eq!(
            render_codex_directives_for_copy(
                r#":codex-file-citation{path="a/my_[file]&amp;.md" line_range_start="0x10"}"#
            ),
            r"[my\_\[file\]\&.md](<a/my_[file]&.md#L16>)"
        );
    }
}
