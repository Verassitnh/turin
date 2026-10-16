//! Syntax highlighting for chat code blocks.
//!
//! Upstream highlights with Shiki using the `pierre-light` / `pierre-dark`
//! themes (`resolveDiffThemeName`, upstream/apps/web/src/lib/diffRendering.ts)
//! and caches highlighted HTML in an LRU bounded by entries and bytes
//! (`MAX_HIGHLIGHT_CACHE_ENTRIES`, `MAX_HIGHLIGHT_CACHE_MEMORY_BYTES`;
//! `crate::lru_cache`).
//! Turin parses with tree-sitter (gpui-component's grammars) and maps capture
//! names onto the same theme colors (values from `@pierre/theme`'s
//! `tokenColors` / `semanticTokenColors`). Results are cached by
//! (code hash, language, appearance) so a frame never re-highlights.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::component::Rope;
use gpui_kit::component::highlighter::{LanguageRegistry, SyntaxHighlighter};
use gpui_kit::{HighlightStyle, Hsla, Rgba};

use crate::lru_cache::LruCache;
use crate::theme::color::hex;

/// `MAX_HIGHLIGHT_CACHE_ENTRIES`.
const MAX_HIGHLIGHT_CACHE_ENTRIES: usize = 500;
/// `MAX_HIGHLIGHT_CACHE_MEMORY_BYTES`.
const MAX_HIGHLIGHT_CACHE_MEMORY_BYTES: usize = 50 * 1024 * 1024;

/// Token colors of one pierre theme.
struct PierreTheme {
    foreground: Rgba,
    comment: Rgba,
    string: Rgba,
    number: Rgba,
    constant: Rgba,
    keyword: Rgba,
    storage_type: Rgba,
    function: Rgba,
    type_: Rgba,
    variable: Rgba,
    parameter: Rgba,
    punctuation: Rgba,
    tag: Rgba,
    attribute: Rgba,
    regexp: Rgba,
    escape: Rgba,
    namespace: Rgba,
    heading: Rgba,
}

const PIERRE_LIGHT: PierreTheme = PierreTheme {
    foreground: hex(0x070707),
    comment: hex(0x84848a),
    string: hex(0x199f43),
    number: hex(0x1ca1c7),
    constant: hex(0xd5a910),
    keyword: hex(0xfc2b73),
    storage_type: hex(0xc635e4),
    function: hex(0x7b43f8),
    type_: hex(0xc635e4),
    variable: hex(0xd47628),
    parameter: hex(0x79797f),
    punctuation: hex(0x79797f),
    tag: hex(0xd52c36),
    attribute: hex(0x16a994),
    regexp: hex(0x17a5af),
    escape: hex(0x1ca1c7),
    namespace: hex(0xd5a910),
    heading: hex(0xd52c36),
};

const PIERRE_DARK: PierreTheme = PierreTheme {
    foreground: hex(0xfbfbfb),
    comment: hex(0x84848a),
    string: hex(0x5ecc71),
    number: hex(0x68cdf2),
    constant: hex(0xffd452),
    keyword: hex(0xff678d),
    storage_type: hex(0xd568ea),
    function: hex(0x9d6afb),
    type_: hex(0xd568ea),
    variable: hex(0xffa359),
    parameter: hex(0xadadb1),
    punctuation: hex(0x79797f),
    tag: hex(0xff6762),
    attribute: hex(0x61d5c0),
    regexp: hex(0x64d1db),
    escape: hex(0x68cdf2),
    namespace: hex(0xffca00),
    heading: hex(0xff6762),
};

impl PierreTheme {
    /// tree-sitter capture name → TextMate scope color.
    fn color(&self, capture: &str) -> Option<Rgba> {
        let mut name = capture;
        loop {
            let color = match name {
                "comment" | "comment.doc" => Some(self.comment),
                "string" | "string.special" | "string.special.symbol" | "text.literal" => {
                    Some(self.string)
                }
                "string.escape" => Some(self.escape),
                "string.regex" | "string.regexp" => Some(self.regexp),
                "number" | "boolean" | "constant.builtin" | "constant.numeric" => Some(self.number),
                "constant" | "enum" | "variant" => Some(self.constant),
                "keyword" | "keyword.control" | "preproc" => Some(self.keyword),
                "keyword.type" | "keyword.storage" => Some(self.storage_type),
                "function" | "function.method" | "function.call" | "function.builtin"
                | "function.macro" | "constructor" => Some(self.function),
                "type" | "type.builtin" | "type.interface" | "class" => Some(self.type_),
                "variable" | "property" | "variable.member" | "field" => Some(self.variable),
                "variable.parameter" | "parameter" => Some(self.parameter),
                "variable.special" | "variable.builtin" | "module" | "namespace" => {
                    Some(self.namespace)
                }
                "punctuation"
                | "punctuation.bracket"
                | "punctuation.delimiter"
                | "punctuation.special"
                | "operator"
                | "punctuation.list_marker" => Some(self.punctuation),
                "tag" | "tag.doctype" => Some(self.tag),
                "attribute" => Some(self.attribute),
                "title" | "text.title" => Some(self.heading),
                "emphasis.strong" => Some(self.constant),
                "emphasis" => Some(self.keyword),
                "link_uri" | "link_text" | "text.uri" => Some(self.string),
                "label" => Some(self.constant),
                "embedded" => None,
                _ => None,
            };
            if color.is_some() {
                return color;
            }
            // `function.method.call` → `function.method` → `function`.
            name = &name[..name.rfind('.')?];
        }
    }
}

struct Resolver(&'static PierreTheme);

impl gpui_kit::base::input::HighlightStyleResolver for Resolver {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        self.0.color(name).map(|color| HighlightStyle {
            color: Some(color.into()),
            ..Default::default()
        })
    }
}

/// Highlighted token colors for one code block, as byte ranges into the code.
#[derive(Debug, Default)]
pub struct HighlightedCode {
    pub foreground: Hsla,
    pub tokens: Vec<(Range<usize>, Hsla)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    hash: u64,
    len: usize,
    language: String,
    dark: bool,
}

thread_local! {
    static HIGHLIGHTERS: RefCell<HashMap<String, SyntaxHighlighter>> = RefCell::default();
}

/// The highlight LRU (`crate::lru_cache`), bounded by entries and bytes like
/// upstream. Shared across threads so a background highlight can fill it.
type HighlightCache = LruCache<CacheKey, Arc<HighlightedCode>>;

fn shared_cache() -> &'static Mutex<HighlightCache> {
    static CACHE: LazyLock<Mutex<HighlightCache>> = LazyLock::new(|| {
        Mutex::new(LruCache::new(
            MAX_HIGHLIGHT_CACHE_ENTRIES,
            MAX_HIGHLIGHT_CACHE_MEMORY_BYTES,
        ))
    });
    &CACHE
}

/// Uncolored gaps take the theme foreground, so a painted block never skips a byte.
fn fill_highlight_gaps(
    code_len: usize,
    foreground: Hsla,
    tokens: Vec<(Range<usize>, Hsla)>,
) -> Vec<(Range<usize>, Hsla)> {
    let mut filled = Vec::new();
    let mut cursor = 0usize;
    for (range, color) in tokens {
        if range.start < cursor || range.end > code_len || range.start >= range.end {
            continue;
        }
        if range.start > cursor {
            filled.push((cursor..range.start, foreground));
        }
        cursor = range.end;
        filled.push((range, color));
    }
    if cursor < code_len {
        filled.push((cursor..code_len, foreground));
    }
    filled
}

/// Fence languages Shiki accepts that the grammar registry spells differently.
fn registry_language(language: &str) -> String {
    let lower = language.to_ascii_lowercase();
    match lower.as_str() {
        "ts" | "mts" | "cts" => "typescript".into(),
        "js" | "mjs" | "cjs" | "jsx" => "javascript".into(),
        "sh" | "shell" | "zsh" | "shellscript" | "console" => "bash".into(),
        "py" => "python".into(),
        "rs" => "rust".into(),
        "rb" => "ruby".into(),
        "yml" => "yaml".into(),
        "md" => "markdown".into(),
        "c++" | "cc" | "hpp" => "cpp".into(),
        "cs" => "csharp".into(),
        "kt" => "kotlin".into(),
        "ex" | "exs" => "elixir".into(),
        "proto" | "protobuf" => "proto".into(),
        "jsonc" | "json5" => "json".into(),
        _ => lower,
    }
}

/// `estimateHighlightedSize`: upstream sizes the HTML (`html.length * 2`) or the
/// source (`code.length * 3`), whichever is larger; here the token list stands
/// in for the HTML.
fn estimate_highlighted_size(highlighted: &HighlightedCode, code: &str) -> usize {
    let tokens_bytes = highlighted.tokens.len() * std::mem::size_of::<(Range<usize>, Hsla)>();
    tokens_bytes.max(code.len() * 3)
}

fn cache_key(code: &str, language: &str, dark: bool) -> CacheKey {
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    CacheKey {
        hash: hasher.finish(),
        len: code.len(),
        language: registry_language(language),
        dark,
    }
}

/// A cached highlight, if this code was already colored. Safe to call on the UI thread.
pub fn cached_highlight(code: &str, language: &str, dark: bool) -> Option<Arc<HighlightedCode>> {
    let key = cache_key(code, language, dark);
    shared_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(&key).cloned())
}

/// One foreground run, used until a background highlight finishes.
pub fn plain_highlight(code: &str, dark: bool) -> Arc<HighlightedCode> {
    let theme = if dark { &PIERRE_DARK } else { &PIERRE_LIGHT };
    let foreground: Hsla = theme.foreground.into();
    Arc::new(HighlightedCode {
        foreground,
        tokens: if code.is_empty() {
            Vec::new()
        } else {
            vec![(0..code.len(), foreground)]
        },
    })
}

/// Highlights `code` for `language`, cached. Highlighters stay thread-local;
/// the result cache is shared so a background thread can fill it.
pub fn highlight_code(code: &str, language: &str, dark: bool) -> Arc<HighlightedCode> {
    let theme = if dark { &PIERRE_DARK } else { &PIERRE_LIGHT };
    let key = cache_key(code, language, dark);
    if let Some(hit) = shared_cache()
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(&key).cloned())
    {
        return hit;
    }
    let tokens = if LanguageRegistry::singleton()
        .language(&key.language)
        .is_some()
    {
        HIGHLIGHTERS.with(|highlighters| {
            let mut highlighters = highlighters.borrow_mut();
            let highlighter = highlighters
                .entry(key.language.clone())
                .or_insert_with(|| SyntaxHighlighter::new(&key.language));
            let rope = Rope::from(code);
            highlighter.update(None, &rope, None);
            highlighter
                .styles(&(0..code.len()), &Resolver(theme))
                .into_iter()
                .filter_map(|(range, style)| style.color.map(|color| (range, color)))
                .filter(|(range, _)| range.start < range.end)
                .collect()
        })
    } else {
        Vec::new()
    };
    let foreground: Hsla = theme.foreground.into();
    let result = Arc::new(HighlightedCode {
        foreground,
        tokens: fill_highlight_gaps(code.len(), foreground, tokens),
    });
    if let Ok(mut cache) = shared_cache().lock() {
        cache.set(
            key,
            result.clone(),
            estimate_highlighted_size(&result, code),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_names_fall_back_to_their_prefix() {
        assert_eq!(
            PIERRE_LIGHT.color("function.method.call"),
            Some(PIERRE_LIGHT.function)
        );
        assert_eq!(
            PIERRE_DARK.color("punctuation.bracket"),
            Some(PIERRE_DARK.punctuation)
        );
        assert_eq!(PIERRE_LIGHT.color("nonsense"), None);
    }

    #[test]
    fn fence_aliases_map_to_grammars() {
        assert_eq!(registry_language("TS"), "typescript");
        assert_eq!(registry_language("sh"), "bash");
    }

    #[test]
    fn uncolored_gaps_take_the_foreground() {
        let foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0,
            a: 1.0,
        };
        let accent = Hsla {
            h: 0.2,
            ..foreground
        };
        let filled = fill_highlight_gaps(10, foreground, vec![(2..4, accent), (7..9, accent)]);
        let covered: usize = filled
            .iter()
            .map(|(range, _)| range.end - range.start)
            .sum();
        assert_eq!(covered, 10);
        assert_eq!(filled[0], (0..2, foreground));
        assert_eq!(filled[1], (2..4, accent));
        assert_eq!(filled[2], (4..7, foreground));
        assert_eq!(filled[4], (9..10, foreground));
    }
}
