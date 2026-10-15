//! Ports of the slice of upstream/packages/shared/src/model.ts the view-model
//! layer reads (model labels and catalog resolution).

use std::sync::LazyLock;

use regex::Regex;

use turin_contracts::model::model_slug_aliases_by_provider;

/// `SelectableModelOption`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectableModelOption {
    pub slug: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// `formatCodexModelName`.
pub fn format_codex_model_name(name: &str) -> String {
    static GPT_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^gpt").unwrap());
    static DASH_LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-([a-z])").unwrap());
    let renamed = GPT_PREFIX.replace(name, "GPT");
    DASH_LETTER
        .replace_all(&renamed, |captures: &regex::Captures<'_>| {
            format!("-{}", captures[1].to_uppercase())
        })
        .into_owned()
}

/// `formatModelSlugName`.
pub fn format_model_slug_name(slug: &str) -> String {
    static GPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^gpt-[0-9]").unwrap());
    static KNOWN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(claude-(opus|sonnet|haiku|fable)|gemini|grok|composer)-[0-9]").unwrap()
    });
    static MINOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(claude-[a-z]+-[0-9]+)-([0-9]{1,2})").unwrap());
    let separator = slug.rfind('/').map_or(0, |index| index + 1);
    let (prefix, name) = slug.split_at(separator);
    if GPT.is_match(name) {
        return format!("{prefix}{}", format_codex_model_name(name));
    }
    if !KNOWN.is_match(name) {
        return slug.to_owned();
    }
    // `.replace(/^(claude-[a-z]+-\d+)-(\d{1,2})(?=-|\[|$)/i, "$1.$2")`: Rust's regex has no
    // lookahead, so check the delimiter after the match by hand.
    let name = match MINOR.captures(name) {
        Some(captures) => {
            let whole = captures.get(0).expect("group 0");
            let next = name[whole.end()..].chars().next();
            if matches!(next, None | Some('-') | Some('[')) {
                format!("{}.{}{}", &captures[1], &captures[2], &name[whole.end()..])
            } else {
                name.to_owned()
            }
        }
        None => name.to_owned(),
    };
    let titled = name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{prefix}{titled}")
}

/// `normalizeCustomModelSlug`: custom identifiers are provider-owned, so only trim.
pub fn normalize_custom_model_slug(model: Option<&str>) -> Option<String> {
    let trimmed = turin_contracts::js_trim(model?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `normalizeModelSlug`.
pub fn normalize_model_slug(model: Option<&str>, driver: &str) -> Option<String> {
    let trimmed = normalize_custom_model_slug(model)?;
    let aliased = model_slug_aliases_by_provider(driver)
        .and_then(|aliases| aliases.iter().find(|(alias, _)| *alias == trimmed));
    Some(aliased.map_or(trimmed, |(_, canonical)| (*canonical).to_owned()))
}

/// `resolveSelectableModel`.
pub fn resolve_selectable_model(
    driver: &str,
    value: Option<&str>,
    options: &[SelectableModelOption],
) -> Option<String> {
    let trimmed = turin_contracts::js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    if let Some(direct) = options.iter().find(|option| option.slug == trimmed) {
        return Some(direct.slug.clone());
    }
    let lower = trimmed.to_lowercase();
    if let Some(by_name) = options
        .iter()
        .find(|option| option.name.to_lowercase() == lower)
    {
        return Some(by_name.slug.clone());
    }
    if let Some(by_alias) = options.iter().find(|option| {
        option
            .aliases
            .iter()
            .any(|alias| alias.to_lowercase() == lower)
    }) {
        return Some(by_alias.slug.clone());
    }
    let normalized = normalize_model_slug(Some(trimmed), driver)?;
    options
        .iter()
        .find(|option| option.slug == normalized)
        .map(|option| option.slug.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_model_slugs() {
        assert_eq!(format_model_slug_name("gpt-5.4"), "GPT-5.4");
        assert_eq!(format_model_slug_name("claude-opus-4-6"), "Claude Opus 4.6");
        assert_eq!(
            format_model_slug_name("claude-opus-5[1m]"),
            "Claude Opus 5[1m]"
        );
        assert_eq!(
            format_model_slug_name("claude-sonnet-4-6-thinking"),
            "Claude Sonnet 4.6 Thinking"
        );
        assert_eq!(format_model_slug_name("custom/model"), "custom/model");
        assert_eq!(format_model_slug_name("openai/gpt-5"), "openai/GPT-5");
    }

    #[test]
    fn resolves_selectable_models() {
        let options = vec![SelectableModelOption {
            slug: "claude-haiku-4-5".into(),
            name: "Claude Haiku 4.5".into(),
            aliases: vec!["claude-haiku-4-5-20251001".into()],
        }];
        assert_eq!(
            resolve_selectable_model("claudeAgent", Some("claude-haiku-4-5-20251001"), &options)
                .as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            resolve_selectable_model("claudeAgent", Some("CLAUDE HAIKU 4.5"), &options).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(resolve_selectable_model("codex", Some(" "), &options), None);
        assert_eq!(
            resolve_selectable_model("codex", Some("nope"), &options),
            None
        );
    }
}
