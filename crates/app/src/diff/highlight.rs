//! Syntax highlighting of diff lines.
//!
//! `@pierre/diffs` tokenizes the lines it has per side with Shiki
//! (`pierre-light` / `pierre-dark`); Turin reuses the chat code highlighter
//! (tree-sitter mapped onto the same token colors). Each side's lines are
//! joined and highlighted once; [`SideHighlights::line`] slices the tokens of
//! one line back out.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::Hsla;

use super::pierre::types::FileDiffMetadata;
use crate::components::chat::markdown::code_highlight::{HighlightedCode, highlight_code};

/// Files past this many bytes per side skip tokenization
/// (`DEFAULT_TOKENIZE_MAX_LENGTH` is 1e5 characters).
const TOKENIZE_MAX_LENGTH: usize = 100_000;
const MAX_CACHE_ENTRIES: usize = 64;

/// Highlighting of one side (additions or deletions) of a file.
pub struct SideHighlights {
    code: Arc<HighlightedCode>,
    /// Byte offset of each line in the joined text.
    starts: Vec<usize>,
}

/// One line's colored runs.
pub struct LineTokens {
    pub foreground: Hsla,
    /// Byte ranges relative to the line (without its newline).
    pub tokens: Vec<(Range<usize>, Hsla)>,
}

impl SideHighlights {
    fn new(lines: &[String], language: &str, dark: bool) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut joined = String::new();
        for line in lines {
            starts.push(joined.len());
            joined.push_str(line);
        }
        let code = if joined.len() > TOKENIZE_MAX_LENGTH {
            Arc::new(HighlightedCode {
                foreground: highlight_code("", language, dark).foreground,
                tokens: Vec::new(),
            })
        } else {
            highlight_code(&joined, language, dark)
        };
        Self { code, starts }
    }

    pub fn foreground(&self) -> Hsla {
        self.code.foreground
    }

    /// The tokens of `lines[index]`, whose text (newline stripped) is `len`
    /// bytes long.
    pub fn line(&self, index: usize, len: usize) -> LineTokens {
        let mut tokens = Vec::new();
        if let Some(&start) = self.starts.get(index) {
            let end = start + len;
            let first = self
                .code
                .tokens
                .partition_point(|(range, _)| range.end <= start);
            for (range, color) in &self.code.tokens[first..] {
                if range.start >= end {
                    break;
                }
                let from = range.start.max(start) - start;
                let to = range.end.min(end) - start;
                if from < to {
                    tokens.push((from..to, *color));
                }
            }
        }
        LineTokens {
            foreground: self.code.foreground,
            tokens,
        }
    }
}

/// Both sides of one file.
pub struct FileHighlights {
    pub additions: SideHighlights,
    pub deletions: SideHighlights,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    file: String,
    version: u32,
    dark: bool,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, Arc<FileHighlights>>,
    order: VecDeque<CacheKey>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// `getFiletypeFromFileName`, for the grammars the app embeds.
pub fn language_for_path(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "rs" => "rust",
        "py" => "python",
        "json" | "jsonc" | "json5" => "json",
        "toml" => "toml",
        "md" | "mdx" | "markdown" => "markdown",
        "yml" | "yaml" => "yaml",
        "css" => "css",
        "html" | "htm" => "html",
        "sh" | "bash" | "zsh" => "bash",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "lua" => "lua",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "proto" => "proto",
        "diff" | "patch" => "diff",
        _ => match name.as_str() {
            "dockerfile" | "makefile" => "make",
            _ => "text",
        },
    }
}

/// Plain foreground only (used while async highlighting is in flight).
pub fn plain_file_highlights(file: &FileDiffMetadata, dark: bool) -> Arc<FileHighlights> {
    let language = file
        .lang
        .clone()
        .unwrap_or_else(|| language_for_path(&file.name).to_string());
    let foreground = highlight_code("", &language, dark).foreground;
    Arc::new(FileHighlights {
        additions: SideHighlights::plain(&file.addition_lines, foreground),
        deletions: SideHighlights::plain(&file.deletion_lines, foreground),
    })
}

impl SideHighlights {
    fn plain(lines: &[String], foreground: Hsla) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut joined = String::new();
        for line in lines {
            starts.push(joined.len());
            joined.push_str(line);
        }
        Self {
            code: Arc::new(HighlightedCode {
                foreground,
                tokens: Vec::new(),
            }),
            starts,
        }
    }
}

/// Highlights `file`, cached by identity, content version and appearance.
pub fn file_highlights(
    file: &FileDiffMetadata,
    identity: &str,
    version: u32,
    dark: bool,
) -> Arc<FileHighlights> {
    let key = CacheKey {
        file: identity.to_string(),
        version,
        dark,
    };
    if let Some(hit) = CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
        return hit;
    }
    let language = file
        .lang
        .clone()
        .unwrap_or_else(|| language_for_path(&file.name).to_string());
    let highlights = Arc::new(FileHighlights {
        additions: SideHighlights::new(&file.addition_lines, &language, dark),
        deletions: SideHighlights::new(&file.deletion_lines, &language, dark),
    });
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.entries.len() >= MAX_CACHE_ENTRIES
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, highlights.clone());
    });
    highlights
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_extensions_to_grammars() {
        assert_eq!(language_for_path("apps/web/src/DiffPanel.tsx"), "tsx");
        assert_eq!(language_for_path("crates/app/Cargo.toml"), "toml");
        assert_eq!(language_for_path("README.md"), "markdown");
        assert_eq!(language_for_path("LICENSE"), "text");
    }

    #[test]
    fn slices_line_tokens_out_of_the_joined_text() {
        let lines = vec!["const a = 1;\n".to_string(), "let b = 2;\n".to_string()];
        let side = SideHighlights::new(&lines, "typescript", false);
        let second = side.line(1, "let b = 2;".len());
        assert!(second.tokens.iter().all(|(range, _)| range.end <= 10));
        assert!(!second.tokens.is_empty());
        assert!(second.tokens.windows(2).all(|w| w[0].0.end <= w[1].0.start));
        assert!(side.line(5, 3).tokens.is_empty());
    }
}
