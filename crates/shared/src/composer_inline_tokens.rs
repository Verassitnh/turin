//! Port of upstream/packages/shared/src/composerInlineTokens.ts

use crate::js_string::{JS_WS, decode_uri_component, slice_utf16, utf16_len};
use turin_contracts::base_schemas::is_js_whitespace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerInlineTokenKind {
    Mention,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerInlineToken {
    pub kind: ComposerInlineTokenKind,
    pub value: String,
    pub source: String,
    /// UTF-16 offsets, like upstream.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CollectComposerInlineTokensOptions {
    pub preserve_trailing_from: Vec<ComposerInlineToken>,
}

/// JS `.` (no `s` flag) does not match line terminators.
fn is_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `text` indexed by char with its UTF-16 length; keeps JS unit arithmetic
/// exact (astral chars are two units).
struct CharUnits {
    chars: Vec<char>,
    /// UTF-16 offset of `chars[index]`.
    offsets: Vec<usize>,
}

impl CharUnits {
    fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut offsets = Vec::with_capacity(text.len() + 1);
        let mut units = 0usize;
        for ch in text.chars() {
            offsets.push(units);
            units += ch.len_utf16();
            chars.push(ch);
        }
        offsets.push(units);
        Self { chars, offsets }
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn unit_at(&self, index: usize) -> usize {
        self.offsets[index]
    }

    fn slice_text<'a>(&self, text: &'a str, from: usize, to: usize) -> &'a str {
        slice_utf16(text, from, to)
    }

    fn starts_token_here(&self, index: usize) -> bool {
        // Upstream prefix `(^|\s)`: start of text or a JS whitespace char.
        index == 0 || is_js_whitespace(self.chars[index - 1])
    }

    /// `(?=\s)`: an actual JS whitespace char must follow. End of text does
    /// not satisfy it, which is what keeps a half-typed token plain.
    fn followed_by_whitespace(&self, index: usize) -> bool {
        index < self.len() && is_js_whitespace(self.chars[index])
    }

    /// `(?=\s|$)`: whitespace or end of text, for sent prompts.
    fn followed_by_whitespace_or_end(&self, index: usize) -> bool {
        index == self.len() || self.followed_by_whitespace(index)
    }
}

fn is_currency_symbol(ch: char) -> bool {
    static SC: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    SC.get_or_init(|| regex::Regex::new(r"\p{Sc}").expect("valid regex"))
        .is_match(&ch.to_string())
}

fn is_skill_name_char(ch: char) -> bool {
    // [a-zA-Z0-9:_-]
    ch.is_ascii_alphanumeric() || ch == ':' || ch == '_' || ch == '-'
}

/// A skill name may start with a digit, but compact monetary amounts and
/// numeric expressions like "$20", "$20k", "$100M", and "$1e6" must stay prose:
/// the composer chips any matched `$name` token, known or not. Tokens beginning
/// with digits must not match numbers with currency/exponent suffixes, and must
/// contain at least one letter. Any currency symbol is accepted as the sigil.
///
/// Upstream expresses this as one regex with lookarounds
/// `(^|\s)\p{Sc}(?![0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$))(?=[a-zA-Z0-9:_-]*[a-zA-Z])([a-zA-Z0-9][a-zA-Z0-9:_-]*)`
/// plus a `(?=\s)` lookahead while typing; the Rust `regex` crate has no
/// lookarounds, so the same acceptance rules are hand-rolled here.
///
/// `sent_prompt` swaps the lookahead for upstream's `SKILL_MENTION_PATTERN`
/// `(?=\s|$)`, so a name that ends the text also counts.
fn scan_skill_tokens(
    text: &str,
    units: &CharUnits,
    sent_prompt: bool,
    tokens: &mut Vec<ComposerInlineToken>,
) {
    let mut index = 0usize;
    while index < units.len() {
        let ch = units.chars[index];
        if units.starts_token_here(index)
            && is_currency_symbol(ch)
            && index + 1 < units.len()
            && units.chars[index + 1].is_ascii_alphanumeric()
        {
            let name_start = index + 1;
            let mut cursor = name_start + 1;
            let mut has_letter = units.chars[name_start].is_ascii_alphabetic();
            while cursor < units.len() && is_skill_name_char(units.chars[cursor]) {
                has_letter = has_letter || units.chars[cursor].is_ascii_alphabetic();
                cursor += 1;
            }
            // (?=\s): while typing, a token only becomes a chip once a
            // delimiter follows it. Sent prompts also accept end of text.
            // (?!...): reject `[0-9][0-9_]*(suffix)?(\s|$)` amounts.
            let delimited = if sent_prompt {
                units.followed_by_whitespace_or_end(cursor)
            } else {
                units.followed_by_whitespace(cursor)
            };
            let looks_like_amount = looks_like_compact_amount(units, index + 1);
            if has_letter && delimited && !looks_like_amount {
                let start = units.unit_at(index);
                let end = units.unit_at(cursor);
                tokens.push(ComposerInlineToken {
                    kind: ComposerInlineTokenKind::Skill,
                    value: units.chars[name_start..cursor].iter().collect(),
                    source: units.slice_text(text, start, end).to_owned(),
                    start,
                    end,
                });
                // Global matching resumes after this token.
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
}

/// `^[0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$)` tested at `index`.
fn looks_like_compact_amount(units: &CharUnits, index: usize) -> bool {
    if index >= units.len() || !units.chars[index].is_ascii_digit() {
        return false;
    }
    let mut probe = index + 1;
    while probe < units.len() && (units.chars[probe].is_ascii_digit() || units.chars[probe] == '_')
    {
        probe += 1;
    }
    let mut after_suffix = probe;
    if probe < units.len() && "kKmMbBtT".contains(units.chars[probe]) {
        after_suffix = probe + 1;
    } else if probe < units.len() && (units.chars[probe] == 'e' || units.chars[probe] == 'E') {
        let mut exponent = probe + 1;
        let mut digits = 0usize;
        while exponent < units.len() && units.chars[exponent].is_ascii_digit() {
            exponent += 1;
            digits += 1;
        }
        if digits > 0 {
            after_suffix = exponent;
        }
    }
    after_suffix == units.len() || is_js_whitespace(units.chars[after_suffix])
}

/// `text.replace(/\\(.)/g, "$1")`: every backslash-escaped char loses its backslash.
fn unescape_backslashes(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.clone().next() {
                Some(escaped) if !is_line_terminator(escaped) => {
                    chars.next();
                    result.push(escaped);
                }
                _ => result.push(ch),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

// /^[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*(?:\/[^\s@"]+)*$/
fn is_scoped_package_reference(value: &str) -> bool {
    static SCOPED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    SCOPED
        .get_or_init(|| {
            regex::Regex::new(&format!(
                r#"^[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*(?:/[^{JS_WS}@"]+)*$"#
            ))
            .expect("valid regex")
        })
        .is_match(value)
}

/// `@token` mentions: `(^|\s)@(?:"((?:\\.|[^"\\])*)"|([^\s@"]+))(?=\s)`.
fn scan_mention_tokens(text: &str, units: &CharUnits, tokens: &mut Vec<ComposerInlineToken>) {
    let mut index = 0usize;
    while index < units.len() {
        let ch = units.chars[index];
        if units.starts_token_here(index) && ch == '@' {
            let mut cursor = index + 1;
            let value: Option<(String, bool)>; // (path, quoted)
            if cursor < units.len() && units.chars[cursor] == '"' {
                // Quoted: `"((?:\\.|[^"\\])*)"`
                cursor += 1;
                let body_start = cursor;
                let mut closed = false;
                while cursor < units.len() {
                    let c = units.chars[cursor];
                    if c == '\\' {
                        // `\\.`: the escaped char must exist and not be a line terminator.
                        if cursor + 1 < units.len() && !is_line_terminator(units.chars[cursor + 1])
                        {
                            cursor += 2;
                            continue;
                        }
                        break;
                    }
                    if c == '"' {
                        cursor += 1;
                        closed = true;
                        break;
                    }
                    cursor += 1;
                }
                if closed && units.followed_by_whitespace(cursor) {
                    let body: String = units.chars[body_start..cursor - 1].iter().collect();
                    value = Some((unescape_backslashes(&body), true));
                } else {
                    value = None;
                }
            } else {
                // Unquoted: `[^\s@"]+`
                let body_start = cursor;
                while cursor < units.len()
                    && !is_js_whitespace(units.chars[cursor])
                    && units.chars[cursor] != '@'
                    && units.chars[cursor] != '"'
                {
                    cursor += 1;
                }
                value = (units.followed_by_whitespace(cursor) && cursor > body_start).then(|| {
                    (
                        units.chars[body_start..cursor].iter().collect::<String>(),
                        false,
                    )
                });
            }
            if let Some((value, quoted)) = value {
                // Autocomplete emits canonical file links, so ambiguous bare
                // @scope/package text stays a package.
                if !value.is_empty() && (quoted || !is_scoped_package_reference(&value)) {
                    let start = units.unit_at(index);
                    let end = units.unit_at(cursor);
                    tokens.push(ComposerInlineToken {
                        kind: ComposerInlineTokenKind::Mention,
                        value,
                        source: units.slice_text(text, start, end).to_owned(),
                        start,
                        end,
                    });
                }
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
}

// /^[A-Za-z][A-Za-z0-9+.-]*:/
fn has_uri_scheme(path: &str) -> bool {
    static SCHEME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    SCHEME
        .get_or_init(|| regex::Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("valid regex"))
        .is_match(path)
}

// /^[A-Za-z]:[\\/]/
fn is_windows_drive_path(path: &str) -> bool {
    static DRIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    DRIVE
        .get_or_init(|| regex::Regex::new(r"^[A-Za-z]:[\\/]").expect("valid regex"))
        .is_match(path)
}

/// While typing, a token only becomes a chip once a delimiter follows it, so a
/// half-typed name at the end of the text stays plain.
///
/// The label body is bounded rather than `*` (upstream: 512) so unterminated
/// bracket runs stay linear. Only a basename ever survives the
/// `label !== basename` check below, so the cap cannot reject a link a user
/// could meaningfully write; the longest filename any common filesystem allows
/// is 255.
const MAX_FILE_LINK_LABEL_LENGTH: usize = 512;

/// Canonical file links: `(^|\s)\[((?:\\.|[^\]\\\\]){0,512})\]\(([^)\s]+)\)(?=\s)`.
fn scan_file_link_tokens(text: &str, units: &CharUnits, tokens: &mut Vec<ComposerInlineToken>) {
    // A link needs a `]` somewhere after its `[`; past the last one no
    // attempt can succeed, which keeps unterminated bracket runs linear.
    let Some(last_close) = units.chars.iter().rposition(|ch| *ch == ']') else {
        return;
    };
    let mut index = 0usize;
    while index < last_close {
        let ch = units.chars[index];
        if units.starts_token_here(index)
            && ch == '['
            && let Some(FileLinkMatch { end, token }) = scan_file_link_at(text, units, index)
        {
            // Upstream `matchAll` consumes a candidate even when it is then
            // rejected, so a rejected link's inner brackets never form tokens.
            index = token_end_index(units, end);
            if let Some(token) = token {
                tokens.push(token);
            }
            continue;
        }
        index += 1;
    }
}

/// A file-link regex match at one `[`. `token` is `None` when the match
/// consumed its span but failed the path/label checks.
struct FileLinkMatch {
    /// UTF-16 offset just past the closing `)`.
    end: usize,
    token: Option<ComposerInlineToken>,
}

fn token_end_index(units: &CharUnits, end_units: usize) -> usize {
    // First char index whose offset is >= end_units.
    units.offsets.partition_point(|offset| *offset < end_units)
}

fn scan_file_link_at(text: &str, units: &CharUnits, index: usize) -> Option<FileLinkMatch> {
    // Label body: `(?:\\.|[^\]\\]){0,512}`.
    let mut cursor = index + 1;
    let mut body_units = 0usize;
    while cursor < units.len() {
        let c = units.chars[cursor];
        if c == '\\' && cursor + 1 < units.len() && !is_line_terminator(units.chars[cursor + 1]) {
            let pair_units = c.len_utf16() + units.chars[cursor + 1].len_utf16();
            if body_units + pair_units > MAX_FILE_LINK_LABEL_LENGTH {
                return None;
            }
            body_units += pair_units;
            cursor += 2;
            continue;
        }
        if c == ']' {
            break;
        }
        if c == '\\' {
            return None; // trailing lone backslash cannot be part of the body
        }
        let char_units = c.len_utf16();
        if body_units + char_units > MAX_FILE_LINK_LABEL_LENGTH {
            return None;
        }
        body_units += char_units;
        cursor += 1;
    }
    if cursor >= units.len() || units.chars[cursor] != ']' {
        return None;
    }
    let label_end = cursor;
    cursor += 1; // ']'
    if cursor >= units.len() || units.chars[cursor] != '(' {
        return None;
    }
    cursor += 1; // '('
    let destination_start = cursor;
    while cursor < units.len()
        && units.chars[cursor] != ')'
        && !is_js_whitespace(units.chars[cursor])
    {
        cursor += 1;
    }
    if cursor == destination_start
        || cursor >= units.len()
        || units.chars[cursor] != ')'
        || !units.followed_by_whitespace(cursor + 1)
    {
        return None;
    }
    let destination_end = cursor;
    cursor += 1; // ')'

    let label: String =
        unescape_backslashes(&units.chars[index + 1..label_end].iter().collect::<String>());
    let encoded_path: String = units.chars[destination_start..destination_end]
        .iter()
        .collect();
    // Preserve malformed source rather than dropping a user-authored token.
    let path = decode_uri_component(&encoded_path).unwrap_or_else(|_| encoded_path.clone());
    let separator_index = path.rfind('/').max(path.rfind('\\'));
    let basename = match separator_index {
        Some(sep) => &path[sep + 1..],
        None => path.as_str(),
    };
    let has_external_scheme = has_uri_scheme(&path) && !is_windows_drive_path(&path);
    let end = units.unit_at(cursor);
    if path.is_empty() || has_external_scheme || label != basename {
        return Some(FileLinkMatch { end, token: None });
    }
    let start = units.unit_at(index);
    Some(FileLinkMatch {
        end,
        token: Some(ComposerInlineToken {
            kind: ComposerInlineTokenKind::Mention,
            value: path,
            source: units.slice_text(text, start, end).to_owned(),
            start,
            end,
        }),
    })
}

/// Upstream `SKILL_MENTION_PATTERN` over a sent prompt: `$name` skill mentions,
/// where the name may end the text. Provider adapters use this in place of
/// their own copies. `value` is the name without its sigil, as in upstream's
/// group 2.
pub fn collect_sent_prompt_skill_tokens(text: &str) -> Vec<ComposerInlineToken> {
    let units = CharUnits::new(text);
    let mut matches: Vec<ComposerInlineToken> = Vec::new();
    scan_skill_tokens(text, &units, true, &mut matches);
    matches
}

pub fn collect_composer_inline_tokens(
    text: &str,
    options: CollectComposerInlineTokensOptions,
) -> Vec<ComposerInlineToken> {
    let units = CharUnits::new(text);
    let mut matches: Vec<ComposerInlineToken> = Vec::new();

    scan_file_link_tokens(text, &units, &mut matches);
    scan_mention_tokens(text, &units, &mut matches);
    scan_skill_tokens(text, &units, false, &mut matches);

    for token in options.preserve_trailing_from {
        if token.end == utf16_len(text)
            && slice_utf16(text, token.start, token.end) == token.source
            && !matches.iter().any(|match_| {
                match_.kind == token.kind && match_.start == token.start && match_.end == token.end
            })
        {
            matches.push(token);
        }
    }

    matches.sort_by_key(|token| token.start);
    matches
}

#[cfg(test)]
mod tests;
