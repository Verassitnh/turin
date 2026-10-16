//! Port of upstream/apps/web/src/components/chat/HighlightedTokens.tsx
//! and the text-identity half of HighlightedCodeLines.tsx.
//!
//! `with_embedded_scripts` recolors a span with another grammar. The unbash
//! parser that finds those spans (`embeddedScripts.ts`) belongs to U3c, so
//! these tests pass the spans in. `starts` and `ends` are indexed by UTF-16
//! code units of the decoded script, matching JavaScript `token.content.length`.
//! The stored values are byte ranges into the outer source.

#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxToken {
    pub content: String,
    pub offset: usize,
    pub color: Option<String>,
    pub font_style: u8,
}

/// One embedded script. `starts[i]` / `ends[i]` map UTF-16 code unit `i` of
/// the decoded script onto a half-open byte range of the outer source.
#[derive(Clone, Debug)]
pub struct EmbeddedScript {
    pub text: String,
    pub language: String,
    pub starts: Vec<usize>,
    pub ends: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenStyle {
    pub color: Option<String>,
    pub italic: bool,
    pub bold: bool,
    pub underline: bool,
}

/// `syntaxTokenStyle`. Bit 1 italic, bit 2 bold, bit 4 underline.
pub fn syntax_token_style(token: &SyntaxToken) -> TokenStyle {
    TokenStyle {
        color: token.color.clone(),
        italic: token.font_style & 1 != 0,
        bold: token.font_style & 2 != 0,
        underline: token.font_style & 4 != 0,
    }
}

pub struct KeyedLine<T> {
    pub key: String,
    pub tokens: Vec<T>,
    pub ending: String,
}

/// `keyedLines`.
pub fn keyed_lines<T: Clone>(
    code: &str,
    lines: &[Vec<T>],
    content: impl Fn(&T) -> &str,
) -> Vec<KeyedLine<T>> {
    let endings = line_endings(code);
    let mut line_start = 0usize;
    lines
        .iter()
        .enumerate()
        .map(|(index, tokens)| {
            let text: String = tokens.iter().map(&content).collect();
            let key = format!("{line_start}:{text}");
            let ending = endings.get(index).cloned().unwrap_or_default();
            line_start += text.len() + ending.len();
            KeyedLine {
                key,
                tokens: tokens.clone(),
                ending,
            }
        })
        .collect()
}

fn line_endings(code: &str) -> Vec<String> {
    let mut endings = Vec::new();
    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            endings.push("\r\n".into());
            index += 2;
        } else if bytes[index] == b'\n' {
            endings.push("\n".into());
            index += 1;
        } else {
            index += 1;
        }
    }
    endings
}

#[derive(Clone, Debug, PartialEq)]
pub struct WordPiece {
    pub key: String,
    pub text: String,
    pub token: SyntaxToken,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub key: String,
    pub pieces: Vec<WordPiece>,
}

/// `wordsOf`. Whitespace runs stay strings, matching `split(/(\s+)/)`.
pub fn words_of(tokens: &[SyntaxToken]) -> Vec<WordPart> {
    let mut parts = Vec::new();
    let mut word: Option<Word> = None;
    for token in tokens {
        let mut offset = token.offset;
        for text in split_keeping_whitespace(&token.content) {
            if text.is_empty() {
                continue;
            }
            if text.as_bytes().first().is_some_and(|byte| is_ws(*byte)) {
                if let Some(finished) = word.take() {
                    parts.push(WordPart::Word(finished));
                }
                parts.push(WordPart::Space(text.to_owned()));
            } else {
                let word = word.get_or_insert_with(|| Word {
                    key: offset.to_string(),
                    pieces: Vec::new(),
                });
                word.pieces.push(WordPiece {
                    key: offset.to_string(),
                    text: text.to_owned(),
                    token: token.clone(),
                });
            }
            offset += text.len();
        }
    }
    if let Some(finished) = word {
        parts.push(WordPart::Word(finished));
    }
    parts
}

#[derive(Clone, Debug, PartialEq)]
pub enum WordPart {
    Space(String),
    Word(Word),
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

fn split_keeping_whitespace(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let whitespace = is_ws(bytes[index]);
        let start = index;
        index += 1;
        while index < bytes.len() && is_ws(bytes[index]) == whitespace {
            index += 1;
        }
        parts.push(&text[start..index]);
    }
    parts
}

/// UTF-16 length of `text[..byte_offset]`. `None` when the offset splits a character.
fn utf16_units(text: &str, byte_offset: usize) -> Option<usize> {
    Some(text.get(..byte_offset)?.encode_utf16().count())
}

/// `withEmbeddedScripts`. A grammar that throws keeps the colors around it.
pub fn with_embedded_scripts(
    code: &str,
    lines: &[Vec<SyntaxToken>],
    embedded: &[EmbeddedScript],
    mut tokenize: impl FnMut(&str, &str) -> Result<Vec<Vec<SyntaxToken>>, ()>,
) -> Vec<Vec<SyntaxToken>> {
    if embedded.is_empty() {
        return lines.to_vec();
    }
    let mut styles = vec![None; code.len()];
    for token in lines.iter().flatten() {
        let end = token.offset + token.content.len();
        for slot in styles.iter_mut().take(end).skip(token.offset) {
            *slot = Some(token.clone());
        }
    }
    for script in embedded {
        let Ok(script_lines) = tokenize(&script.text, &script.language) else {
            continue;
        };
        for token in script_lines.into_iter().flatten() {
            if token.content.is_empty() {
                continue;
            }
            let Some(start_at) = utf16_units(&script.text, token.offset) else {
                continue;
            };
            let Some(end_at) = utf16_units(&script.text, token.offset + token.content.len()) else {
                continue;
            };
            if end_at == 0 {
                continue;
            }
            let Some(&start) = script.starts.get(start_at) else {
                continue;
            };
            let Some(&end) = script.ends.get(end_at - 1) else {
                continue;
            };
            if start < end && end <= styles.len() {
                for slot in &mut styles[start..end] {
                    *slot = Some(token.clone());
                }
            }
        }
    }
    restyled_lines(code, &styles)
}

fn restyled_lines(code: &str, styles: &[Option<SyntaxToken>]) -> Vec<Vec<SyntaxToken>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let breaks = line_break_spans(code);
    for span in breaks {
        let end = span.map(|(index, _)| index).unwrap_or(code.len());
        let mut tokens = Vec::new();
        let mut index = start;
        while index < end {
            let style = styles.get(index).and_then(|token| token.as_ref());
            let mut next = index + 1;
            while next < end
                && styles
                    .get(next)
                    .and_then(|token| token.as_ref())
                    .map(|token| (&token.color, token.font_style))
                    == style.map(|token| (&token.color, token.font_style))
            {
                next += 1;
            }
            tokens.push(SyntaxToken {
                content: code[index..next].to_owned(),
                offset: index,
                color: style.and_then(|token| token.color.clone()),
                font_style: style.map(|token| token.font_style).unwrap_or(0),
            });
            index = next;
        }
        lines.push(tokens);
        if let Some((index, ending)) = span {
            start = index + ending.len();
        }
    }
    lines
}

fn line_break_spans(code: &str) -> Vec<Option<(usize, &str)>> {
    let mut spans = Vec::new();
    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            spans.push(Some((index, "\r\n")));
            index += 2;
        } else if bytes[index] == b'\n' {
            spans.push(Some((index, "\n")));
            index += 1;
        } else {
            index += 1;
        }
    }
    spans.push(None);
    spans
}

/// The characters a highlighted document paints, including endings Shiki drops.
pub fn rendered_code_text(code: &str, lines: &[Vec<SyntaxToken>]) -> String {
    keyed_lines(code, lines, |token| token.content.as_str())
        .into_iter()
        .map(|line| {
            let mut text: String = line
                .tokens
                .iter()
                .map(|token| token.content.as_str())
                .collect();
            text.push_str(&line.ending);
            text
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens_of(contents: &[&str]) -> Vec<SyntaxToken> {
        let mut offset = 0;
        contents
            .iter()
            .map(|content| {
                let token = SyntaxToken {
                    content: (*content).to_owned(),
                    offset,
                    color: None,
                    font_style: 0,
                };
                offset += content.len();
                token
            })
            .collect()
    }

    fn shape(parts: &[WordPart]) -> Vec<String> {
        parts
            .iter()
            .map(|part| match part {
                WordPart::Space(text) => text.clone(),
                WordPart::Word(word) => word
                    .pieces
                    .iter()
                    .map(|piece| piece.text.as_str())
                    .collect(),
            })
            .collect()
    }

    fn plain_split(line: &str) -> Vec<String> {
        split_keeping_whitespace(line)
            .into_iter()
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn wraps_the_same_words_as_the_plain_text_fallback() {
        let cases = [
            (
                vec!["echo", " ", "\"", "$HOME", "/${", "PATH", "%%:*", "}\""],
                "echo \"$HOME/${PATH%%:*}\"",
            ),
            (
                vec!["rsync", " ", "-a", " ", "--exclude", " ", "build"],
                "rsync -a --exclude build",
            ),
            (
                vec!["git", " && ", "echo", " ", "\"two  words\"", "; "],
                "git && echo \"two  words\"; ",
            ),
            (vec!["  ", "indented"], "  indented"),
            (vec![], ""),
        ];
        for (contents, line) in cases {
            assert_eq!(shape(&words_of(&tokens_of(&contents))), plain_split(line));
        }
    }

    #[test]
    fn keeps_each_pieces_own_token_so_colors_survive_regrouping() {
        let parts = words_of(&tokens_of(&["\"", "$HOME", "\""]));
        let WordPart::Word(word) = &parts[0] else {
            panic!("expected a word");
        };
        let contents: Vec<_> = word
            .pieces
            .iter()
            .map(|piece| piece.token.content.as_str())
            .collect();
        assert_eq!(contents, ["\"", "$HOME", "\""]);
    }

    #[test]
    fn keyed_lines_render_the_same_text_as_the_source() {
        for code in [
            "git status\ngit diff",
            "Get-ChildItem\r\nWrite-Output done\r\n",
            "mixed\r\nendings\n\nlone \r stays",
        ] {
            let lines: Vec<Vec<SyntaxToken>> = split_lines_dropping_endings(code)
                .iter()
                .map(|line| tokens_of(&[line.as_str()]))
                .collect();
            assert_eq!(rendered_code_text(code, &lines), code);
        }
    }

    fn split_lines_dropping_endings(code: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        let bytes = code.as_bytes();
        let mut index = 0;
        if code.is_empty() {
            return vec![String::new()];
        }
        while index < bytes.len() {
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                lines.push(std::mem::take(&mut current));
                index += 2;
            } else if bytes[index] == b'\n' {
                lines.push(std::mem::take(&mut current));
                index += 1;
            } else {
                current.push(bytes[index] as char);
                index += 1;
            }
        }
        lines.push(current);
        lines
    }

    #[test]
    fn keys_empty_lines_apart() {
        let code = "a\r\n\r\n\r\nb";
        let lines: Vec<Vec<SyntaxToken>> = split_lines_dropping_endings(code)
            .iter()
            .map(|line| tokens_of(&[line.as_str()]))
            .collect();
        let keys: Vec<_> = keyed_lines(code, &lines, |token| token.content.as_str())
            .into_iter()
            .map(|line| line.key)
            .collect();
        let unique = keys.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique, keys.len());
    }

    fn token(content: &str, offset: usize, color: &str) -> SyntaxToken {
        SyntaxToken {
            content: content.into(),
            offset,
            color: Some(color.into()),
            font_style: 0,
        }
    }

    /// Stand-in grammar: each word is one token colored with the language name.
    fn tokenize(text: &str, language: &str) -> Result<Vec<Vec<SyntaxToken>>, ()> {
        if language == "unknown" {
            return Err(());
        }
        let mut line_start = 0;
        let mut lines = Vec::new();
        for line in split_lines_dropping_endings(text) {
            let mut tokens = Vec::new();
            for part in split_keeping_whitespace(&line) {
                if part.is_empty() {
                    continue;
                }
                let offset = line_start + (part.as_ptr() as usize - line.as_ptr() as usize);
                tokens.push(token(part, offset, language));
            }
            let ending = text[line_start + line.len()..]
                .strip_prefix("\r\n")
                .map(|_| 2)
                .or_else(|| {
                    text[line_start + line.len()..]
                        .strip_prefix('\n')
                        .map(|_| 1)
                })
                .unwrap_or(0);
            line_start += line.len() + ending;
            lines.push(tokens);
        }
        Ok(lines)
    }

    fn colors_of(code: &str, scripts: &[EmbeddedScript]) -> Vec<(String, Option<String>)> {
        let lines = with_embedded_scripts(
            code,
            &tokenize(code, "shellscript").unwrap(),
            scripts,
            tokenize,
        );
        let painted = lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|token| token.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(painted, code.replace("\r\n", "\n"));
        lines
            .into_iter()
            .flatten()
            .map(|token| (token.content, token.color))
            .collect()
    }

    fn identity(text: &str, start: usize) -> EmbeddedScript {
        EmbeddedScript {
            text: text.to_owned(),
            language: "python".into(),
            starts: (start..start + text.len()).collect(),
            ends: (start + 1..start + text.len() + 1).collect(),
        }
    }

    #[test]
    fn colors_each_nested_script_with_its_own_grammar() {
        let code = "bash -lc \"python3 -c 'print(1)'\"";
        let print_at = code.find("print(1)").unwrap();
        let colors = colors_of(code, &[identity("print(1)", print_at)]);
        assert_eq!(
            colors,
            vec![
                ("bash -lc \"python3 -c '".into(), Some("shellscript".into())),
                ("print(1)".into(), Some("python".into())),
                ("'\"".into(), Some("shellscript".into())),
            ]
        );
    }

    #[test]
    fn indexes_a_non_ascii_script_in_utf16_units() {
        let code = "éé";
        assert_eq!(code.len(), 4);
        assert_eq!(code.encode_utf16().count(), 2);
        let outer = vec![vec![token(code, 0, "shellscript")]];
        let script = EmbeddedScript {
            text: code.into(),
            language: "python".into(),
            starts: vec![0, 2],
            ends: vec![2, 4],
        };
        let lines = with_embedded_scripts(code, &outer, &[script], |text, language| {
            Ok(vec![vec![token(text, 0, language)]])
        });
        let painted: String = lines
            .iter()
            .flatten()
            .map(|token| token.content.as_str())
            .collect();
        assert_eq!(painted, code);
        assert_eq!(lines[0][0].color.as_deref(), Some("python"));
    }

    #[test]
    fn colors_an_escape_sequence_like_the_character_it_spells() {
        let code = "bash -c \"python3 -c 'print(\\\"x\\\")'\"";
        let source_body = "print(\\\"x\\\")";
        let start = code.find(source_body).unwrap();
        // Decoded `print("x")` maps onto the source, backslashes included.
        let decoded = "print(\"x\")";
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        let mut source_at = start;
        for character in decoded.chars() {
            // An escape's backslash belongs to the character it spells, so the
            // painted source token is `\"` rather than a bare quote.
            let escaped = code.as_bytes().get(source_at) == Some(&b'\\');
            starts.push(source_at);
            source_at += character.len_utf8() + usize::from(escaped);
            ends.push(source_at);
        }
        let script = EmbeddedScript {
            text: decoded.into(),
            language: "python".into(),
            starts,
            ends,
        };
        assert!(colors_of(code, &[script]).contains(&(source_body.into(), Some("python".into()))));
    }

    #[test]
    fn keeps_the_surrounding_colors_where_a_grammar_is_missing() {
        let code = "cat > x.ts <<'EOF'\r\nbody\r\nEOF";
        let body_at = code.find("body").unwrap();
        let mut missing = identity("body", body_at);
        missing.language = "unknown".into();
        let without = colors_of(code, &[]);
        let with_missing = colors_of(code, &[missing]);
        let chars = |colors: Vec<(String, Option<String>)>| {
            colors
                .into_iter()
                .flat_map(|(content, color)| {
                    let painted = color;
                    content.chars().map(|_| painted.clone()).collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(chars(with_missing), chars(without));
    }

    #[test]
    fn font_style_bits_match_the_css_mapping() {
        let mut token = token("x", 0, "#fff");
        token.font_style = 1 | 2 | 4;
        let style = syntax_token_style(&token);
        assert!(style.italic && style.bold && style.underline);
        assert_eq!(style.color.as_deref(), Some("#fff"));
    }

    #[test]
    fn highlighted_lines_keep_source_text_including_markup_and_blank_lines() {
        let code =
            "const html = \"<img src=x onerror=alert(1)>\";\n\n/* multi\nline */\n\tconst x = 1;\n";
        for end in 0..=code.len() {
            let prefix = &code[..end];
            let lines: Vec<Vec<SyntaxToken>> = split_lines_dropping_endings(prefix)
                .iter()
                .map(|line| tokens_of(&[line.as_str()]))
                .collect();
            assert_eq!(rendered_code_text(prefix, &lines), prefix);
            assert!(!rendered_code_text(prefix, &lines).contains("&lt;"));
        }
    }
}
