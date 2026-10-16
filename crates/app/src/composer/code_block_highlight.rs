//! Port of upstream/apps/web/src/components/composerCodeBlockHighlight.ts
//!
//! The ProseMirror plugin is the editor view. This module is the offset
//! flattener, the size cap, and the per-block cache. Upstream tokenizes on
//! the JS thread after Shiki resolves. Turin runs that tokenize on a worker
//! thread so a long fence cannot stall the UI thread. One worker handles a
//! refresh batch; a highlighter that fails to load leaves its block plain
//! and the rest of the batch still paints.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread::{self, JoinHandle, ThreadId};

use turin_contracts::base_schemas::js_trim;

use crate::composer::code_languages::language_of_info_string;
use crate::composer::js_string::{slice_utf16, utf16_len};
use crate::diff::diff_rendering::DiffThemeName;

pub const MAX_HIGHLIGHTED_CODE_BLOCK_LENGTH: usize = 20_000;

pub fn should_highlight_code_block(code: &str) -> bool {
    utf16_len(code) <= MAX_HIGHLIGHTED_CODE_BLOCK_LENGTH
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightToken {
    pub content: String,
    pub color: Option<String>,
}

pub trait BlockHighlighter: Send {
    fn code_to_tokens(
        &self,
        code: &str,
        language: &str,
        theme: DiffThemeName,
    ) -> Result<Vec<Vec<HighlightToken>>, ()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDecoration {
    pub from: usize,
    pub to: usize,
    pub color: String,
}

/// Flattens a highlighter's line/token structure into UTF-16 offsets within
/// the block text. A throwing grammar comes back as plain text.
pub fn tokenize_block(
    highlighter: &dyn BlockHighlighter,
    code: &str,
    language: &str,
    theme: DiffThemeName,
) -> Vec<TokenDecoration> {
    let tokens = match highlighter.code_to_tokens(code, language, theme) {
        Ok(tokens) => tokens,
        Err(()) => return Vec::new(),
    };
    let mut decorations = Vec::new();
    let mut offset = 0usize;
    for (line_index, line) in tokens.iter().enumerate() {
        if line_index > 0 {
            offset += if starts_with_crlf(code, offset) { 2 } else { 1 };
        }
        for token in line {
            let length = utf16_len(&token.content);
            if let Some(color) = token.color.as_deref().filter(|color| !color.is_empty())
                && !js_trim(&token.content).is_empty()
            {
                decorations.push(TokenDecoration {
                    from: offset,
                    to: offset + length,
                    color: color.to_owned(),
                });
            }
            offset += length;
        }
    }
    decorations
}

fn starts_with_crlf(code: &str, offset: usize) -> bool {
    slice_utf16(code, offset, offset + 2) == "\r\n"
}

pub fn block_signature(theme: DiffThemeName, language: &str, text: &str) -> String {
    format!("{}\u{0000}{language}\u{0000}{text}", theme_key(theme))
}

fn theme_key(theme: DiffThemeName) -> &'static str {
    match theme {
        DiffThemeName::PierreLight => "pierre-light",
        DiffThemeName::PierreDark => "pierre-dark",
    }
}

/// A top-level composer code block. `content_size` is the editor's content
/// length; decorations are skipped when it disagrees with the text, matching
/// the unmarked `text*` assumption upstream documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerCodeBlock {
    pub language: String,
    pub text: String,
    pub pos: usize,
    pub content_size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedBlock {
    pub decorations: Vec<TokenDecoration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineDecoration {
    pub from: usize,
    pub to: usize,
    pub color: String,
}

pub fn build_decorations(
    blocks: &[ComposerCodeBlock],
    cache: &HashMap<String, HighlightedBlock>,
    theme: DiffThemeName,
) -> Vec<InlineDecoration> {
    let mut decorations = Vec::new();
    for block in blocks {
        if !should_highlight_code_block(&block.text) {
            continue;
        }
        if block.content_size != utf16_len(&block.text) {
            continue;
        }
        let Some(highlighted) = cache.get(&block_signature(theme, &block.language, &block.text))
        else {
            continue;
        };
        let start = block.pos + 1;
        for decoration in &highlighted.decorations {
            decorations.push(InlineDecoration {
                from: start + decoration.from,
                to: start + decoration.to,
                color: decoration.color.clone(),
            });
        }
    }
    decorations
}

pub trait HighlighterSource: Send + Sync {
    fn load(&self, language: &str) -> Result<Box<dyn BlockHighlighter>, ()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedBlock {
    pub signature: String,
    pub decorations: Vec<TokenDecoration>,
}

pub struct HighlightOutcome {
    pub worker: ThreadId,
    pub painted: Vec<PaintedBlock>,
}

pub enum HighlightRefresh {
    Idle,
    Repaint,
    Working(JoinHandle<HighlightOutcome>),
}

#[derive(Default)]
pub struct HighlightSession {
    cache: HashMap<String, HighlightedBlock>,
    live: HashSet<String>,
    theme: Option<DiffThemeName>,
}

impl HighlightSession {
    pub fn cache(&self) -> &HashMap<String, HighlightedBlock> {
        &self.cache
    }

    /// Plans a refresh. Pending blocks are tokenized on a new thread.
    pub fn refresh<S>(
        &mut self,
        blocks: &[ComposerCodeBlock],
        theme: DiffThemeName,
        source: Arc<S>,
    ) -> HighlightRefresh
    where
        S: HighlighterSource + 'static,
    {
        let theme_changed = self.theme.is_some_and(|painted| painted != theme);
        self.theme = Some(theme);
        self.live = blocks
            .iter()
            .map(|block| block_signature(theme, &block.language, &block.text))
            .collect();
        self.cache
            .retain(|signature, _| self.live.contains(signature));

        let pending: Vec<ComposerCodeBlock> = blocks
            .iter()
            .filter(|block| {
                should_highlight_code_block(&block.text)
                    && !self.cache.contains_key(&block_signature(
                        theme,
                        &block.language,
                        &block.text,
                    ))
            })
            .cloned()
            .collect();
        if pending.is_empty() {
            return if theme_changed {
                HighlightRefresh::Repaint
            } else {
                HighlightRefresh::Idle
            };
        }
        let live = self.live.clone();
        HighlightRefresh::Working(thread::spawn(move || {
            tokenize_pending(pending, theme, live, source)
        }))
    }

    pub fn apply(&mut self, outcome: HighlightOutcome) {
        for painted in outcome.painted {
            if !self.live.contains(&painted.signature) {
                continue;
            }
            self.cache.insert(
                painted.signature,
                HighlightedBlock {
                    decorations: painted.decorations,
                },
            );
        }
    }
}

fn tokenize_pending<S: HighlighterSource>(
    pending: Vec<ComposerCodeBlock>,
    theme: DiffThemeName,
    live: HashSet<String>,
    source: Arc<S>,
) -> HighlightOutcome {
    let worker = thread::current().id();
    let mut painted = Vec::new();
    for block in pending {
        let signature = block_signature(theme, &block.language, &block.text);
        if !live.contains(&signature) {
            continue;
        }
        let language = language_of_info_string(&block.language);
        let language = if language.is_empty() {
            "text"
        } else {
            language
        };
        let Ok(highlighter) = source.load(language) else {
            continue;
        };
        painted.push(PaintedBlock {
            signature,
            decorations: tokenize_block(highlighter.as_ref(), &block.text, language, theme),
        });
    }
    HighlightOutcome { worker, painted }
}

#[cfg(test)]
#[path = "code_block_highlight_tests.rs"]
mod tests;
