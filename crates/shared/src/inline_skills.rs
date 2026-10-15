//! Port of upstream/packages/shared/src/inlineSkills.ts
//!
//! Upstream finds `$skill` tokens with a lookahead regex. The `regex` crate has no lookaround, so
//! [`match_inline_skills`] walks the text by hand and applies the same three checks in the same
//! order: a currency symbol after whitespace or at the start, a negative lookahead for amounts
//! such as `$12k` or `$1e3`, and a name that is one token of `[a-zA-Z0-9:_-]` containing a letter.

use std::sync::LazyLock;

use regex::Regex;

use turin_contracts::base_schemas::js_trim;

use turin_contracts::base_schemas::is_js_whitespace;

/// `Pick<ServerProviderSkill, "name" | "displayName">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSkill {
    pub name: String,
    pub display_name: Option<String>,
}

/// `js_trim`-style title case: words split on whitespace, `:`, `_` and `-`.
fn title_case_words(value: &str) -> String {
    value
        .split(|ch: char| is_js_whitespace(ch) || matches!(ch, ':' | '_' | '-'))
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn format_provider_skill_display_name(skill: &InlineSkill) -> String {
    if let Some(display_name) = skill
        .display_name
        .as_deref()
        .map(|name| js_trim(name).to_owned())
        .filter(|name| !name.is_empty())
    {
        return display_name;
    }
    title_case_words(&skill.name)
}

/// One matched `$skill` token. `start..end` is the byte range of the whole token, including the
/// whitespace before the `$`, so callers slice the text with it the way upstream does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSkillMatch<'a> {
    pub start: usize,
    pub end: usize,
    pub skill: &'a InlineSkill,
    pub raw_text: String,
}

static CURRENCY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\p{Sc}\z").expect("static regex"));

fn is_currency(ch: char) -> bool {
    let mut buffer = [0; 4];
    CURRENCY.is_match(ch.encode_utf8(&mut buffer))
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-')
}

fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..).and_then(|rest| rest.chars().next())
}

/// `\s|$`, the lookahead that closes a token or an amount.
fn ends_word(text: &str, at: usize) -> bool {
    at == text.len() || char_at(text, at).is_some_and(is_js_whitespace)
}

/// `[0-9][0-9_]*(?:[kKmMbBtT]|[eE][0-9]+)?(?:\s|$)` starting at `at`: an amount such as `12`,
/// `1_000k` or `2e10`, which a `$` in front of it does not name a skill.
fn is_amount_at(text: &str, at: usize) -> bool {
    if !char_at(text, at).is_some_and(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let bytes = text.as_bytes();
    let run_start = at + 1;
    let mut run_end = run_start;
    while run_end < bytes.len() && (bytes[run_end].is_ascii_digit() || bytes[run_end] == b'_') {
        run_end += 1;
    }
    // Every length the greedy `[0-9_]*` can backtrack to.
    (run_start..=run_end).any(|end| {
        if ends_word(text, end) {
            return true;
        }
        match char_at(text, end) {
            Some(ch) if "kKmMbBtT".contains(ch) => ends_word(text, end + 1),
            Some(ch) if ch == 'e' || ch == 'E' => {
                let digits_start = end + 1;
                let digits_end = (digits_start..bytes.len())
                    .take_while(|&index| bytes[index].is_ascii_digit())
                    .last()
                    .map_or(digits_start, |last| last + 1);
                digits_end > digits_start && ends_word(text, digits_end)
            }
            _ => false,
        }
    })
}

/// The token a `$` at `start` introduces, if the upstream pattern matches there. Returns the
/// match end and the name.
fn token_at(text: &str, start: usize) -> Option<(usize, &str)> {
    // `^` (start of input) or a whitespace character, which the match then includes.
    let mut candidates = Vec::with_capacity(2);
    if start == 0 {
        candidates.push(0);
    }
    if let Some(ch) = char_at(text, start).filter(|ch| is_js_whitespace(*ch)) {
        candidates.push(start + ch.len_utf8());
    }
    candidates.into_iter().find_map(|currency_at| {
        let currency = char_at(text, currency_at).filter(|ch| is_currency(*ch))?;
        let name_at = currency_at + currency.len_utf8();
        if is_amount_at(text, name_at) {
            return None;
        }
        let run_end = text[name_at..]
            .char_indices()
            .find(|(_, ch)| !is_token_char(*ch))
            .map_or(text.len(), |(offset, _)| name_at + offset);
        let run = &text[name_at..run_end];
        // `(?=[a-zA-Z0-9:_-]*[a-zA-Z])`: the run has a letter. `[a-zA-Z0-9]` then leads it.
        if !run.chars().any(|ch| ch.is_ascii_alphabetic()) {
            return None;
        }
        if !char_at(text, name_at).is_some_and(|ch| ch.is_ascii_alphanumeric()) {
            return None;
        }
        // `(?=\s|$)` after the greedy run: a shorter run would be followed by a token character.
        if !ends_word(text, run_end) {
            return None;
        }
        Some((run_end, run))
    })
}

pub fn match_inline_skills<'a>(text: &str, skills: &'a [InlineSkill]) -> Vec<InlineSkillMatch<'a>> {
    let mut matches = Vec::new();
    let mut position = 0;
    while position <= text.len() {
        let Some((end, name)) = token_at(text, position) else {
            match char_at(text, position) {
                Some(ch) => position += ch.len_utf8(),
                None => break,
            }
            continue;
        };
        if let Some(skill) = skills.iter().find(|candidate| candidate.name == name) {
            matches.push(InlineSkillMatch {
                start: position,
                end,
                skill,
                raw_text: format!("${name}"),
            });
        }
        position = end;
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, display_name: Option<&str>) -> InlineSkill {
        InlineSkill {
            name: name.to_owned(),
            display_name: display_name.map(str::to_owned),
        }
    }

    #[test]
    fn formats_a_display_name_or_title_cases_the_skill_name() {
        assert_eq!(
            format_provider_skill_display_name(&skill("test-t3-app", Some("  T3 App Testing  "))),
            "T3 App Testing"
        );
        assert_eq!(
            format_provider_skill_display_name(&skill("test-t3_app:now", Some("   "))),
            "Test T3 App Now"
        );
        assert_eq!(
            format_provider_skill_display_name(&skill("review", None)),
            "Review"
        );
    }

    #[test]
    fn matches_a_dollar_token_after_whitespace_or_at_the_start() {
        let skills = [skill("test-t3-app", Some("T3 App Testing"))];
        let matches = match_inline_skills("Use $test-t3-app now", &skills);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].raw_text, "$test-t3-app");
        assert_eq!(
            &"Use $test-t3-app now"[matches[0].start..matches[0].end],
            " $test-t3-app"
        );
        assert_eq!(
            match_inline_skills("$test-t3-app", &skills)
                .first()
                .map(|found| (found.start, found.end)),
            Some((0, 12))
        );
    }

    #[test]
    fn ignores_unknown_names_amounts_and_tokens_without_a_letter() {
        let skills = [skill("review", None), skill("12k", None), skill("x1", None)];
        assert!(match_inline_skills("Use $unknown here", &skills).is_empty());
        assert!(match_inline_skills("costs $12k total", &skills).is_empty());
        assert!(match_inline_skills("rate $1e3 now", &skills).is_empty());
        assert!(match_inline_skills("pre$review", &skills).is_empty());
        assert!(match_inline_skills("ends with $review.", &skills).is_empty());
        assert_eq!(match_inline_skills("ok $review", &skills).len(), 1);
    }
}
