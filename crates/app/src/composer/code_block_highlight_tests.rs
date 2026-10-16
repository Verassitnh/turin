//! Port of upstream/apps/web/src/components/composerCodeBlockHighlight.test.ts

use std::sync::Arc;
use std::thread;

use super::*;

const THEME: DiffThemeName = DiffThemeName::PierreDark;

struct FakeHighlighter {
    color_for: fn(&str) -> Option<String>,
    fail: bool,
}

impl BlockHighlighter for FakeHighlighter {
    fn code_to_tokens(
        &self,
        code: &str,
        _language: &str,
        _theme: DiffThemeName,
    ) -> Result<Vec<Vec<HighlightToken>>, ()> {
        if self.fail {
            return Err(());
        }
        Ok(split_lines(code)
            .into_iter()
            .map(|line| {
                split_keeping_whitespace(&line)
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .map(|content| HighlightToken {
                        color: (self.color_for)(&content),
                        content,
                    })
                    .collect()
            })
            .collect())
    }
}

fn split_lines(code: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = code;
    while let Some(index) = rest.find('\n') {
        let (line, after) = rest.split_at(index);
        let line = line.strip_suffix('\r').unwrap_or(line);
        lines.push(line.to_owned());
        rest = &after[1..];
    }
    lines.push(rest.to_owned());
    lines
}

fn split_keeping_whitespace(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_space = None;
    for character in line.chars() {
        let space = character.is_ascii_whitespace();
        if in_space.is_some_and(|was_space| was_space != space) {
            parts.push(std::mem::take(&mut current));
        }
        in_space = Some(space);
        current.push(character);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn fake(color_for: fn(&str) -> Option<String>) -> FakeHighlighter {
    FakeHighlighter {
        color_for,
        fail: false,
    }
}

struct ScriptedSource {
    fail_languages: Vec<String>,
}

impl HighlighterSource for ScriptedSource {
    fn load(&self, language: &str) -> Result<Box<dyn BlockHighlighter>, ()> {
        if self.fail_languages.iter().any(|failed| failed == language) {
            return Err(());
        }
        Ok(Box::new(fake(|_| Some("#ff0000".into()))))
    }
}

fn block(language: &str, text: &str, pos: usize) -> ComposerCodeBlock {
    ComposerCodeBlock {
        language: language.into(),
        text: text.into(),
        pos,
        content_size: utf16_len(text),
    }
}

#[test]
fn maps_token_offsets_to_positions_within_the_block_text() {
    let highlighter = fake(|content| (content == "const").then(|| "#ff0000".into()));
    let decorations = tokenize_block(&highlighter, "const answer = 42", "ts", THEME);
    assert_eq!(
        decorations,
        [TokenDecoration {
            from: 0,
            to: 5,
            color: "#ff0000".into(),
        }]
    );
}

#[test]
fn accounts_for_the_newline_between_lines() {
    let highlighter = fake(|content| (content == "two").then(|| "#00ff00".into()));
    let decorations = tokenize_block(&highlighter, "one\ntwo", "ts", THEME);
    assert_eq!(
        decorations,
        [TokenDecoration {
            from: 4,
            to: 7,
            color: "#00ff00".into(),
        }]
    );
}

#[test]
fn accounts_for_both_characters_of_a_crlf_line_break() {
    let highlighter = fake(|content| (content == "two").then(|| "#00ff00".into()));
    let decorations = tokenize_block(&highlighter, "one\r\ntwo", "ts", THEME);
    assert_eq!(
        decorations,
        [TokenDecoration {
            from: 5,
            to: 8,
            color: "#00ff00".into(),
        }]
    );
}

#[test]
fn skips_whitespace_only_tokens_so_indentation_is_not_decorated() {
    let highlighter = fake(|_| Some("#0000ff".into()));
    let decorations = tokenize_block(&highlighter, "  indented", "ts", THEME);
    assert_eq!(
        decorations,
        [TokenDecoration {
            from: 2,
            to: 10,
            color: "#0000ff".into(),
        }]
    );
}

#[test]
fn falls_back_to_plain_text_when_the_grammar_throws() {
    let throwing = FakeHighlighter {
        color_for: |_| None,
        fail: true,
    };
    assert!(tokenize_block(&throwing, "const answer = 42", "nope", THEME).is_empty());
}

#[test]
fn highlights_a_fence_up_to_the_cap() {
    let code = "x".repeat(MAX_HIGHLIGHTED_CODE_BLOCK_LENGTH);
    assert!(should_highlight_code_block(&code));
}

#[test]
fn leaves_a_fence_past_the_cap_plain() {
    let code = "x".repeat(MAX_HIGHLIGHTED_CODE_BLOCK_LENGTH + 1);
    assert!(!should_highlight_code_block(&code));
}

#[test]
fn still_paints_blocks_whose_highlighter_loaded_off_the_ui_thread() {
    let caller = thread::current().id();
    let mut session = HighlightSession::default();
    let blocks = vec![block("ts", "a", 0), block("py", "b", 4)];
    let refresh = session.refresh(
        &blocks,
        THEME,
        Arc::new(ScriptedSource {
            fail_languages: vec!["py".into()],
        }),
    );
    let HighlightRefresh::Working(handle) = refresh else {
        panic!("pending blocks should tokenize on a worker");
    };
    let outcome = handle.join().expect("worker");
    assert_ne!(outcome.worker, caller);
    session.apply(outcome);
    let painted = build_decorations(&blocks, session.cache(), THEME);
    assert_eq!(painted.len(), 1);
    assert_eq!(painted[0].color, "#ff0000");
    assert_eq!(painted[0].from, 1);
    assert!(
        !session
            .cache()
            .contains_key(&block_signature(THEME, "py", "b"))
    );
}
