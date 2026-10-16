//! Port of upstream/apps/web/src/composer-code-languages.ts
//!
//! The languages a composer fence can be switched to from its header, and how
//! a choice is written back into the fence's info string. Ids are the names
//! the highlighter and the file icons both know. The list is curated rather
//! than every grammar the highlighter bundles: a picker is for the common
//! case, and any other language can still be typed on the fence line, where
//! it shows as its own entry.

use crate::composer::js_string::is_js_whitespace_char;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeBlockLanguage {
    pub id: &'static str,
    pub label: &'static str,
}

const fn lang(id: &'static str, label: &'static str) -> CodeBlockLanguage {
    CodeBlockLanguage { id, label }
}

pub const CODE_BLOCK_LANGUAGES: &[CodeBlockLanguage] = &[
    lang("", "Plain text"),
    lang("bash", "Bash"),
    lang("c", "C"),
    lang("cpp", "C++"),
    lang("csharp", "C#"),
    lang("css", "CSS"),
    lang("dart", "Dart"),
    lang("diff", "Diff"),
    lang("dockerfile", "Dockerfile"),
    lang("elixir", "Elixir"),
    lang("go", "Go"),
    lang("graphql", "GraphQL"),
    lang("haskell", "Haskell"),
    lang("html", "HTML"),
    lang("java", "Java"),
    lang("javascript", "JavaScript"),
    lang("json", "JSON"),
    lang("jsx", "JSX"),
    lang("kotlin", "Kotlin"),
    lang("lua", "Lua"),
    lang("markdown", "Markdown"),
    lang("php", "PHP"),
    lang("powershell", "PowerShell"),
    lang("python", "Python"),
    lang("r", "R"),
    lang("ruby", "Ruby"),
    lang("rust", "Rust"),
    lang("scala", "Scala"),
    lang("scss", "SCSS"),
    lang("sql", "SQL"),
    lang("svelte", "Svelte"),
    lang("swift", "Swift"),
    lang("toml", "TOML"),
    lang("tsx", "TSX"),
    lang("typescript", "TypeScript"),
    lang("vue", "Vue"),
    lang("xml", "XML"),
    lang("yaml", "YAML"),
    lang("zig", "Zig"),
];

/// Short names people type on the fence line, labelled like the entry they mean.
fn language_alias(language: &str) -> Option<&'static str> {
    Some(match language {
        "cs" => "csharp",
        "c++" => "cpp",
        "js" => "javascript",
        "md" => "markdown",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "sh" | "shell" => "bash",
        "ts" => "typescript",
        "yml" => "yaml",
        _ => return None,
    })
}

fn split_leading_whitespace(info: &str) -> (&str, &str) {
    let end = info
        .char_indices()
        .find(|(_, c)| !is_js_whitespace_char(*c))
        .map_or(info.len(), |(at, _)| at);
    info.split_at(end)
}

/// The language is the info string's first word; the rest belongs to the fence.
pub fn language_of_info_string(info: &str) -> &str {
    let (_, rest) = split_leading_whitespace(info);
    let end = rest
        .char_indices()
        .find(|(_, c)| is_js_whitespace_char(*c))
        .map_or(rest.len(), |(at, _)| at);
    &rest[..end]
}

/// Replaces the language and keeps everything around it: the whitespace the
/// fence had before it and whatever follows it. Plain text clears it, unless
/// something follows: a bare `title=x` would read as the language, so that
/// keeps `text` in front.
pub fn with_info_string_language(info: &str, language: &str) -> String {
    let (leading, after_leading) = split_leading_whitespace(info);
    // The language is the first word, so what follows it is empty or starts
    // with whitespace.
    let rest = &after_leading[language_of_info_string(info).len()..];
    if !language.is_empty() {
        return format!("{leading}{language}{rest}");
    }
    if !turin_contracts::base_schemas::js_trim(rest).is_empty() {
        return format!("{leading}text{rest}");
    }
    String::new()
}

/// The canonical entry a language names, if the picker lists it.
pub fn code_language_entry(language: &str) -> Option<CodeBlockLanguage> {
    let id = if language == "text" {
        ""
    } else {
        language_alias(&language.to_lowercase()).unwrap_or(language)
    };
    let id = id.to_lowercase();
    CODE_BLOCK_LANGUAGES
        .iter()
        .copied()
        .find(|entry| entry.id == id)
}

pub fn code_language_label(language: &str) -> String {
    code_language_entry(language)
        .map_or_else(|| language.to_owned(), |entry| entry.label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_of_info_string_reads_the_first_word() {
        for (info, language) in [
            ("", ""),
            ("ts", "ts"),
            ("  ts  ", "ts"),
            ("js title=example", "js"),
            ("typescript ", "typescript"),
        ] {
            assert_eq!(language_of_info_string(info), language, "{info:?}");
        }
    }

    #[test]
    fn with_info_string_language_keeps_the_rest_of_the_fence() {
        for (info, language, expected) in [
            ("", "ts", "ts"),
            ("js", "python", "python"),
            ("js title=example", "ts", "ts title=example"),
            ("js  {1,3}", "ts", "ts  {1,3}"),
            ("js", "", ""),
            ("js title=example", "", "text title=example"),
            ("  js title=x", "python", "  python title=x"),
            ("  js title=x", "", "  text title=x"),
            (" js", "go", " go"),
            ("  ", "go", "  go"),
        ] {
            assert_eq!(
                with_info_string_language(info, language),
                expected,
                "{info:?} {language:?}"
            );
        }
    }

    #[test]
    fn code_language_label_resolves_aliases() {
        for (language, label) in [
            ("", "Plain text"),
            ("text", "Plain text"),
            ("cpp", "C++"),
            ("ts", "TypeScript"),
            ("arduino", "arduino"),
        ] {
            assert_eq!(code_language_label(language), label);
        }
    }

    #[test]
    fn lists_every_language_once_plain_text_first() {
        let ids: Vec<&str> = CODE_BLOCK_LANGUAGES.iter().map(|l| l.id).collect();
        assert_eq!(ids[0], "");
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
