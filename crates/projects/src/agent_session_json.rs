//! Port of upstream/apps/server/src/project/AgentSessionJson.ts
//!
//! Streaming projection of one JSON record. Upstream feeds a stream-json
//! tokenizer and assembles only the schema-selected paths. This port runs the
//! same selection on a byte-level pushdown reader: keys always decode (they
//! decide selection), selected values are decoded, unselected values are
//! scanned and validated but never materialized. Duplicate keys keep the last
//! value, as JSON.parse does.

use serde_json::{Map, Value};
use thiserror::Error;

/// Upstream `TranscriptJsonLimitError`: allocation budget or nesting depth was
/// exceeded. Unlike malformed input (which yields `None`), this rejects the
/// whole transcript.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct TranscriptJsonLimitError(pub String);

/// Upstream `JsonPath` element: a property name or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonPathSegment {
    Key(String),
    Index(usize),
}

/// Encoded shape of the record schema, enough to decide which paths a
/// projection keeps. Replaces upstream's `SchemaAST` walk: Rust schemas are not
/// introspectable, so the caller describes the same structure by hand.
#[derive(Debug, Clone)]
pub enum JsonSelectSchema {
    /// String, number, boolean or literal: selected only as a whole value.
    Scalar,
    /// `Unknown`, `Any`, or a record with dynamic keys: every descendant kept.
    Any,
    Object(Vec<(&'static str, JsonSelectSchema)>),
    /// Array whose elements all share one schema (upstream `rest`).
    Array(Box<JsonSelectSchema>),
    Union(Vec<JsonSelectSchema>),
}

/// Upstream `createTranscriptJsonSelector`: whether a path is part of the schema.
pub fn create_transcript_json_selector(
    schema: JsonSelectSchema,
) -> impl Fn(&[JsonPathSegment]) -> bool {
    move |path| includes(&schema, path, 0)
}

fn includes(node: &JsonSelectSchema, path: &[JsonPathSegment], index: usize) -> bool {
    if index == path.len() {
        return true;
    }
    match node {
        JsonSelectSchema::Object(properties) => properties.iter().any(|(name, child)| {
            matches!(&path[index], JsonPathSegment::Key(key) if key == name)
                && includes(child, path, index + 1)
        }),
        JsonSelectSchema::Array(element) => match &path[index] {
            JsonPathSegment::Index(_) => includes(element, path, index + 1),
            JsonPathSegment::Key(_) => true,
        },
        JsonSelectSchema::Union(types) => types.iter().any(|ty| includes(ty, path, index)),
        JsonSelectSchema::Any => true,
        JsonSelectSchema::Scalar => false,
    }
}

/// Upstream MAX depth default for `createTranscriptJsonReader`.
pub const TRANSCRIPT_JSON_MAX_DEPTH: usize = 128;

/// Bytes charged per materialized value, on top of its text (upstream: per token).
const VALUE_CHARGE_BYTES: usize = 64;

type Reserve<'a> = Box<dyn FnMut(usize) -> Result<(), TranscriptJsonLimitError> + 'a>;
type Select<'a> = Box<dyn Fn(&[JsonPathSegment]) -> bool + 'a>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A value (after `:`, after `,` in an array, or at the root).
    Value,
    /// Right after `[`: a value or `]`.
    ArrayFirst,
    /// Right after `{`: a key or `}`.
    ObjectFirst,
    /// After `,` in an object: a key only.
    ObjectKey,
    Colon,
    /// After a complete value: `,` or the enclosing closer.
    AfterValue,
    /// The root container closed; only whitespace may follow.
    Done,
}

struct Frame {
    is_object: bool,
    /// Path of this container. Empty when the container is not selected.
    path: Vec<JsonPathSegment>,
    selected: bool,
    /// Object: key of the value being read.
    key: Option<String>,
    /// Array: index of the next element, counting unselected elements too.
    index: usize,
    object: Option<Map<String, Value>>,
    array: Option<Vec<Value>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    Normal,
    Backslash,
    /// Remaining hex digits of a `\u` escape.
    Unicode(u8),
}

struct StringToken {
    is_key: bool,
    capture: bool,
    raw: Vec<u8>,
    escape: Escape,
}

struct NumberToken {
    buf: Vec<u8>,
    selected: bool,
}

struct LiteralToken {
    word: &'static [u8],
    pos: usize,
    value: Option<Value>,
}

#[derive(Default)]
enum Lex {
    #[default]
    None,
    Str(StringToken),
    Num(NumberToken),
    Lit(LiteralToken),
}

/// Upstream `createTranscriptJsonReader`. Feed chunks with [`write`], then call
/// [`finish`]. Input is `&str`, so chunk boundaries never split a UTF-8 sequence.
///
/// [`write`]: TranscriptJsonReader::write
/// [`finish`]: TranscriptJsonReader::finish
pub struct TranscriptJsonReader<'a> {
    reserve: Reserve<'a>,
    select: Select<'a>,
    max_depth: usize,
    frames: Vec<Frame>,
    expect: Expect,
    lex: Lex,
    root: Option<Value>,
    malformed: bool,
}

impl<'a> TranscriptJsonReader<'a> {
    pub fn new(
        reserve: impl FnMut(usize) -> Result<(), TranscriptJsonLimitError> + 'a,
        select: impl Fn(&[JsonPathSegment]) -> bool + 'a,
    ) -> Self {
        Self::with_max_depth(reserve, select, TRANSCRIPT_JSON_MAX_DEPTH)
    }

    pub fn with_max_depth(
        reserve: impl FnMut(usize) -> Result<(), TranscriptJsonLimitError> + 'a,
        select: impl Fn(&[JsonPathSegment]) -> bool + 'a,
        max_depth: usize,
    ) -> Self {
        Self {
            reserve: Box::new(reserve),
            select: Box::new(select),
            max_depth,
            frames: Vec::new(),
            expect: Expect::Value,
            lex: Lex::None,
            root: None,
            malformed: false,
        }
    }

    /// Consume one chunk. Malformed input stops consumption silently; budget
    /// and depth exhaustion return an error.
    pub fn write(&mut self, chunk: &str) -> Result<(), TranscriptJsonLimitError> {
        for &byte in chunk.as_bytes() {
            if self.malformed {
                return Ok(());
            }
            self.consume_byte(byte)?;
        }
        Ok(())
    }

    /// Upstream `finish`: the projected root, or `None` for malformed, truncated,
    /// scalar, or unselected input.
    pub fn finish(self) -> Result<Option<Value>, TranscriptJsonLimitError> {
        if self.malformed || self.expect != Expect::Done {
            return Ok(None);
        }
        Ok(self.root)
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), TranscriptJsonLimitError> {
        (self.reserve)(bytes)
    }

    fn fail(&mut self) -> Result<(), TranscriptJsonLimitError> {
        self.malformed = true;
        Ok(())
    }

    fn consume_byte(&mut self, byte: u8) -> Result<(), TranscriptJsonLimitError> {
        match std::mem::take(&mut self.lex) {
            Lex::None => self.token(byte),
            Lex::Str(mut string) => {
                match string.escape {
                    Escape::Backslash => {
                        let next = match byte {
                            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                                Escape::Normal
                            }
                            b'u' => Escape::Unicode(4),
                            _ => return self.fail(),
                        };
                        string.escape = next;
                        if string.capture {
                            string.raw.push(byte);
                        }
                        self.lex = Lex::Str(string);
                    }
                    Escape::Unicode(remaining) => {
                        if !byte.is_ascii_hexdigit() {
                            return self.fail();
                        }
                        string.escape = if remaining == 1 {
                            Escape::Normal
                        } else {
                            Escape::Unicode(remaining - 1)
                        };
                        if string.capture {
                            string.raw.push(byte);
                        }
                        self.lex = Lex::Str(string);
                    }
                    Escape::Normal => match byte {
                        b'"' => return self.end_string(string),
                        b'\\' => {
                            string.escape = Escape::Backslash;
                            if string.capture {
                                string.raw.push(byte);
                            }
                            self.lex = Lex::Str(string);
                        }
                        0x00..=0x1f => return self.fail(),
                        _ => {
                            if string.capture {
                                string.raw.push(byte);
                            }
                            self.lex = Lex::Str(string);
                        }
                    },
                }
                Ok(())
            }
            Lex::Num(mut number) => {
                if matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    number.buf.push(byte);
                    self.lex = Lex::Num(number);
                    return Ok(());
                }
                // The number ends before this byte, which is then read as a token.
                match serde_json::from_slice::<Value>(&number.buf) {
                    Ok(value @ Value::Number(_)) => {
                        self.finish_value(number.selected.then_some(value))?;
                    }
                    _ => return self.fail(),
                }
                self.token(byte)
            }
            Lex::Lit(mut literal) => {
                if byte != literal.word[literal.pos] {
                    return self.fail();
                }
                literal.pos += 1;
                if literal.pos == literal.word.len() {
                    self.finish_value(literal.value)?;
                } else {
                    self.lex = Lex::Lit(literal);
                }
                Ok(())
            }
        }
    }

    fn token(&mut self, byte: u8) -> Result<(), TranscriptJsonLimitError> {
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            return Ok(());
        }
        match self.expect {
            Expect::Done => self.fail(),
            Expect::Colon => {
                if byte != b':' {
                    return self.fail();
                }
                self.expect = Expect::Value;
                Ok(())
            }
            Expect::AfterValue => {
                let Some(is_object) = self.frames.last().map(|frame| frame.is_object) else {
                    return self.fail();
                };
                match (is_object, byte) {
                    (true, b',') => self.expect = Expect::ObjectKey,
                    (true, b'}') => return self.close(true),
                    (false, b',') => self.expect = Expect::Value,
                    (false, b']') => return self.close(false),
                    _ => return self.fail(),
                }
                Ok(())
            }
            Expect::ObjectFirst => match byte {
                b'}' => self.close(true),
                b'"' => self.start_key(),
                _ => self.fail(),
            },
            Expect::ObjectKey => match byte {
                b'"' => self.start_key(),
                _ => self.fail(),
            },
            Expect::ArrayFirst if byte == b']' => self.close(false),
            Expect::ArrayFirst | Expect::Value => self.start_value(byte),
        }
    }

    fn start_key(&mut self) -> Result<(), TranscriptJsonLimitError> {
        self.lex = Lex::Str(StringToken {
            is_key: true,
            capture: true,
            raw: Vec::new(),
            escape: Escape::Normal,
        });
        Ok(())
    }

    fn start_value(&mut self, byte: u8) -> Result<(), TranscriptJsonLimitError> {
        match byte {
            b'{' => self.open(true),
            b'[' => self.open(false),
            b'"' => {
                let (_, selected) = self.enter_value()?;
                self.lex = Lex::Str(StringToken {
                    is_key: false,
                    capture: selected,
                    raw: Vec::new(),
                    escape: Escape::Normal,
                });
                Ok(())
            }
            b'-' | b'0'..=b'9' => {
                let (_, selected) = self.enter_value()?;
                self.lex = Lex::Num(NumberToken {
                    buf: vec![byte],
                    selected,
                });
                Ok(())
            }
            b't' | b'f' | b'n' => {
                let (word, value): (&'static [u8], Value) = match byte {
                    b't' => (b"true", Value::Bool(true)),
                    b'f' => (b"false", Value::Bool(false)),
                    _ => (b"null", Value::Null),
                };
                let (_, selected) = self.enter_value()?;
                self.lex = Lex::Lit(LiteralToken {
                    word,
                    pos: 1,
                    value: selected.then_some(value),
                });
                Ok(())
            }
            _ => self.fail(),
        }
    }

    /// Path and selection of the value about to start. Selection needs the
    /// parent to be selected, mirroring upstream `startValue`.
    fn enter_value(&mut self) -> Result<(Vec<JsonPathSegment>, bool), TranscriptJsonLimitError> {
        let (path, selected) = match self.frames.last() {
            None => (Vec::new(), (self.select)(&[])),
            Some(parent) if parent.selected => {
                let segment = if parent.is_object {
                    JsonPathSegment::Key(parent.key.clone().unwrap_or_default())
                } else {
                    JsonPathSegment::Index(parent.index)
                };
                let mut path = parent.path.clone();
                path.push(segment);
                let selected = (self.select)(&path);
                (path, selected)
            }
            Some(_) => (Vec::new(), false),
        };
        if selected {
            self.reserve(VALUE_CHARGE_BYTES)?;
        }
        Ok((path, selected))
    }

    fn open(&mut self, is_object: bool) -> Result<(), TranscriptJsonLimitError> {
        let (path, selected) = self.enter_value()?;
        if self.frames.len() + 1 > self.max_depth {
            return Err(TranscriptJsonLimitError(
                "Transcript JSON nesting exceeds the depth limit".to_owned(),
            ));
        }
        self.frames.push(Frame {
            is_object,
            path,
            selected,
            key: None,
            index: 0,
            object: (selected && is_object).then(Map::new),
            array: (selected && !is_object).then(Vec::new),
        });
        self.expect = if is_object {
            Expect::ObjectFirst
        } else {
            Expect::ArrayFirst
        };
        Ok(())
    }

    fn close(&mut self, is_object: bool) -> Result<(), TranscriptJsonLimitError> {
        let Some(frame) = self.frames.pop() else {
            return self.fail();
        };
        let value = frame.selected.then(|| {
            if is_object {
                Value::Object(frame.object.unwrap_or_default())
            } else {
                Value::Array(frame.array.unwrap_or_default())
            }
        });
        self.finish_value(value)
    }

    fn end_string(&mut self, string: StringToken) -> Result<(), TranscriptJsonLimitError> {
        if string.capture {
            self.reserve(string.raw.len() * 2)?;
        }
        let text = decode_json_string(&string.raw);
        if string.is_key {
            // Keys are always captured: they decide which children are selected.
            if let Some(frame) = self.frames.last_mut() {
                frame.key = Some(text);
            }
            self.expect = Expect::Colon;
            return Ok(());
        }
        self.finish_value(string.capture.then(|| Value::String(text)))
    }

    /// Attach a completed value to its parent. `value` is `Some` only when the
    /// value is selected.
    fn finish_value(&mut self, value: Option<Value>) -> Result<(), TranscriptJsonLimitError> {
        let Some(parent) = self.frames.last_mut() else {
            // Upstream only completes on a container; a root scalar leaves the
            // result undefined.
            self.root = value.filter(|value| value.is_object() || value.is_array());
            self.expect = Expect::Done;
            return Ok(());
        };
        if let Some(value) = value {
            if parent.is_object {
                if let (Some(key), Some(object)) = (parent.key.take(), parent.object.as_mut()) {
                    object.insert(key, value);
                }
            } else if let Some(array) = parent.array.as_mut() {
                array.push(value);
            }
        }
        if !parent.is_object {
            parent.index += 1;
        }
        self.expect = Expect::AfterValue;
        Ok(())
    }
}

/// Decode the body of a JSON string (between the quotes). Escapes were validated
/// by the reader, so decoding is total. Lone surrogates become U+FFFD: Rust
/// strings cannot hold them.
fn decode_json_string(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' {
            let start = i;
            while i < raw.len() && raw[i] != b'\\' {
                i += 1;
            }
            out.push_str(&String::from_utf8_lossy(&raw[start..i]));
            continue;
        }
        let escape = raw.get(i + 1).copied().unwrap_or(b'\\');
        i += 2;
        let simple = match escape {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            _ => {
                let unit = hex4(raw.get(i..i + 4).unwrap_or(&[]));
                i += 4;
                if (0xD800..0xDC00).contains(&unit) {
                    let low = if raw.get(i) == Some(&b'\\') && raw.get(i + 1) == Some(&b'u') {
                        hex4(raw.get(i + 2..i + 6).unwrap_or(&[]))
                    } else {
                        0
                    };
                    if (0xDC00..0xE000).contains(&low) {
                        i += 6;
                        let code = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    } else {
                        out.push('\u{FFFD}');
                    }
                    continue;
                }
                char::from_u32(unit).unwrap_or('\u{FFFD}')
            }
        };
        out.push(simple);
    }
    out
}

fn hex4(digits: &[u8]) -> u32 {
    std::str::from_utf8(digits)
        .ok()
        .and_then(|text| u32::from_str_radix(text, 16).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::{JsonPathSegment, TranscriptJsonLimitError, TranscriptJsonReader};

    fn key(name: &str) -> JsonPathSegment {
        JsonPathSegment::Key(name.to_owned())
    }

    fn read_with(
        json: &str,
        size: usize,
        select: impl Fn(&[JsonPathSegment]) -> bool,
    ) -> Option<Value> {
        let mut reader = TranscriptJsonReader::new(|_| Ok(()), select);
        let chars: Vec<char> = json.chars().collect();
        for chunk in chars.chunks(size) {
            reader.write(&chunk.iter().collect::<String>()).unwrap();
        }
        reader.finish().unwrap()
    }

    fn read(json: &str, size: usize) -> Option<Value> {
        read_with(json, size, |_| true)
    }

    #[test]
    fn matches_json_parse_across_chunk_boundaries() {
        for size in [1, 2, 7, 64, 1024] {
            for text in [
                r#"{"a":1,"a":2,"b":"before","b":"after"}"#,
                r#"{"a":{"x":1},"a":{"y":2},"b":[],"c":{}}"#,
                r#"{"a":[null,true,false,1,-2.3e4,"😀\u0061\\\"",{},[],[1,2]]}"#,
                r#"{"a":"s","a":null,"b":null,"b":"s","c":0,"c":false}"#,
                r#"{"__proto__":{"polluted":true},"constructor":1,"__proto__":2}"#,
            ] {
                let expected: Value = serde_json::from_str(text).unwrap();
                assert_eq!(read(text, size), Some(expected), "size {size}: {text}");
            }
        }
    }

    #[test]
    fn projects_siblings_and_array_elements_without_merging_repeated_parent_objects() {
        let text = r#"{"message":{"usage":{"input":100},"content":"large"},"message":{"usage":{"output":5},"content":[1,2]},"rows":[{"keep":1,"drop":2},{"keep":3}],"drop":{"keep":4}}"#;
        let projected = read_with(text, 1, |path| {
            if path.first() == Some(&key("drop")) {
                return false;
            }
            !path.contains(&key("content")) && !path.contains(&key("drop"))
        });
        assert_eq!(
            projected,
            Some(serde_json::json!({
                "message": { "usage": { "output": 5 } },
                "rows": [{ "keep": 1 }, { "keep": 3 }],
            }))
        );
    }

    #[test]
    fn rejects_malformed_input() {
        for text in [
            r#"{"a":"#,
            r#"{"a":1} trailing"#,
            r#"{"a":1}{"a":2}"#,
            r#"{"a":"bad\x"}"#,
            r#"{"a":[1,]}"#,
        ] {
            assert_eq!(read(text, 1), None, "{text}");
        }
    }

    #[test]
    fn retains_the_allocation_and_depth_limits() {
        let mut limited = TranscriptJsonReader::new(
            |_| Err(TranscriptJsonLimitError("budget".to_owned())),
            |_| true,
        );
        assert_eq!(
            limited.write(r#"{"a":1}"#),
            Err(TranscriptJsonLimitError("budget".to_owned()))
        );

        let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        let mut reader = TranscriptJsonReader::new(|_| Ok(()), |_| true);
        assert!(reader.write(&deep).is_err());
    }

    #[test]
    fn root_scalars_and_unselected_roots_are_undefined() {
        assert_eq!(read("5", 1), None);
        assert_eq!(
            read_with(r#"{"a":1}"#, 1, |path| path.is_empty()),
            Some(serde_json::json!({}))
        );
        assert_eq!(read_with(r#"{"a":1}"#, 1, |_| false), None);
    }

    #[test]
    fn decodes_surrogate_pairs_and_replaces_lone_surrogates() {
        assert_eq!(
            read(r#"{"a":"\ud83d\ude00"}"#, 3),
            Some(serde_json::json!({"a": "😀"}))
        );
        assert_eq!(
            read(r#"{"a":"\ud83d"}"#, 3),
            Some(serde_json::json!({"a": "\u{FFFD}"}))
        );
    }
}
