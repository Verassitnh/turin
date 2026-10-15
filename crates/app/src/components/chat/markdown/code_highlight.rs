//! Syntax highlighting for chat code blocks.
//!
//! Upstream highlights with Shiki using the `pierre-light` / `pierre-dark`
//! themes (`resolveDiffThemeName`, upstream/apps/web/src/lib/diffRendering.ts)
//! and caches highlighted HTML in an LRU (`MAX_HIGHLIGHT_CACHE_ENTRIES`).
//! Turin parses with tree-sitter (gpui-component's grammars) and maps capture
//! names onto the same theme colors (values from `@pierre/theme`'s
//! `tokenColors` / `semanticTokenColors`). Results are cached by
//! (code hash, language, appearance) so a frame never re-highlights.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::component::Rope;
use gpui_kit::component::highlighter::{LanguageRegistry, SyntaxHighlighter};
use gpui_kit::{HighlightStyle, Hsla, Rgba};

use crate::theme::color::hex;

/// `MAX_HIGHLIGHT_CACHE_ENTRIES`.
const MAX_HIGHLIGHT_CACHE_ENTRIES: usize = 500;

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

#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, Arc<HighlightedCode>>,
    order: VecDeque<CacheKey>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
    static HIGHLIGHTERS: RefCell<HashMap<String, SyntaxHighlighter>> = RefCell::default();
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

/// Highlights `code` for `language`, cached.
pub fn highlight_code(code: &str, language: &str, dark: bool) -> Arc<HighlightedCode> {
    let theme = if dark { &PIERRE_DARK } else { &PIERRE_LIGHT };
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    let key = CacheKey {
        hash: hasher.finish(),
        len: code.len(),
        language: registry_language(language),
        dark,
    };
    if let Some(hit) = CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
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
    let result = Arc::new(HighlightedCode {
        foreground: theme.foreground.into(),
        tokens,
    });
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.entries.len() >= MAX_HIGHLIGHT_CACHE_ENTRIES
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, result.clone());
    });
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
}
