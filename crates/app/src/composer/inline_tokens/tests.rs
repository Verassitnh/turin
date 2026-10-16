//! Port of upstream/packages/shared/src/composerInlineTokens.test.ts

use super::*;

fn skill(value: &str, source: &str, start: usize, end: usize) -> ComposerInlineToken {
    ComposerInlineToken {
        kind: ComposerInlineTokenKind::Skill,
        value: value.to_owned(),
        source: source.to_owned(),
        start,
        end,
    }
}

fn mention(value: &str, source: &str, start: usize, end: usize) -> ComposerInlineToken {
    ComposerInlineToken {
        kind: ComposerInlineTokenKind::Mention,
        value: value.to_owned(),
        source: source.to_owned(),
        start,
        end,
    }
}

#[test]
fn collects_file_links_mentions_and_skills_with_source_ranges() {
    let text = "Use $ui and inspect [Chat.tsx](src/Chat.tsx) with @AGENTS.md please";

    assert_eq!(
        collect_composer_inline_tokens(text, Default::default()),
        vec![
            skill("ui", "$ui", 4, 7),
            mention("src/Chat.tsx", "[Chat.tsx](src/Chat.tsx)", 20, 44),
            mention("AGENTS.md", "@AGENTS.md", 50, 60),
        ]
    );
}

#[test]
fn collects_currency_prefixed_skill_names_that_begin_with_a_digit() {
    for prefix in ["$", "€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
        let text = format!("Use {prefix}2spec next");
        let end = 9 + prefix.chars().map(|c| c.len_utf16()).sum::<usize>();
        assert_eq!(
            collect_composer_inline_tokens(&text, Default::default()),
            vec![skill("2spec", &format!("{prefix}2spec"), 4, end)],
            "{prefix}"
        );
    }
}

#[test]
fn leaves_currency_amounts_as_text() {
    for prefix in ["€", "£", "¥", "₹", "₩", "₿", "𑿝"] {
        let text = format!("{prefix}20 {prefix}1_000 {prefix}20k {prefix}100M {prefix}1e6 ");
        assert_eq!(
            collect_composer_inline_tokens(&text, Default::default()),
            vec![],
            "{prefix}"
        );
    }
}

#[test]
fn leaves_digits_only_dollar_amounts_and_compact_monetary_expressions_as_text() {
    assert_eq!(
        collect_composer_inline_tokens("I'll pay $20 tomorrow", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("Budget is $1_000 total", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("Budget is $20k tomorrow", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("Cost is $100M total", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("Limit is $1e6 here", Default::default()),
        vec![]
    );
}

#[test]
fn does_not_convert_incomplete_trailing_tokens() {
    assert_eq!(
        collect_composer_inline_tokens("Use $ui", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("Inspect @AGENTS.md", Default::default()),
        vec![]
    );
}

#[test]
fn keeps_the_delimiter_after_a_token_outside_its_source_range() {
    let text = "Inspect [package.json](package.json) next";

    assert_eq!(
        collect_composer_inline_tokens(text, Default::default()),
        vec![mention(
            "package.json",
            "[package.json](package.json)",
            8,
            36
        )]
    );
    assert_eq!(
        crate::composer::js_string::slice_utf16(text, 36, usize::MAX),
        " next"
    );
}

#[test]
fn preserves_a_confirmed_pill_when_only_its_trailing_delimiter_is_removed() {
    let with_delimiter = "[package.json](package.json) ";
    let confirmed = collect_composer_inline_tokens(with_delimiter, Default::default());

    assert_eq!(
        collect_composer_inline_tokens(
            with_delimiter.trim_end(),
            CollectComposerInlineTokensOptions {
                preserve_trailing_from: confirmed
            }
        ),
        vec![mention(
            "package.json",
            "[package.json](package.json)",
            0,
            28
        )]
    );
}

#[test]
fn does_not_preserve_a_pill_after_its_source_is_edited() {
    let confirmed =
        collect_composer_inline_tokens("[package.json](package.json) ", Default::default());

    assert_eq!(
        collect_composer_inline_tokens(
            "[package.json](package-json)",
            CollectComposerInlineTokensOptions {
                preserve_trailing_from: confirmed
            }
        ),
        vec![]
    );
}

#[test]
fn ignores_normal_web_links() {
    assert_eq!(
        collect_composer_inline_tokens(
            "Read [docs](https://example.com) first",
            Default::default()
        ),
        vec![]
    );
}

#[test]
fn keeps_scoped_package_references_as_plain_text() {
    for reference in ["@expo/ui", "@jane/foo.js", "@scope/pkg/sub/path"] {
        assert_eq!(
            collect_composer_inline_tokens(
                &format!("Install {reference} next"),
                Default::default()
            ),
            vec![],
            "{reference}"
        );
    }
}

#[test]
fn keeps_scoped_package_references_plain_across_incomplete_input_and_ime_whitespace() {
    assert_eq!(
        collect_composer_inline_tokens("Install @expo/ui", Default::default()),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens("入力 @expo/ui　を追加", Default::default()),
        vec![]
    );
}

#[test]
fn keeps_bare_non_scoped_file_paths_as_mentions() {
    assert_eq!(
        collect_composer_inline_tokens("Inspect @README.md next", Default::default()),
        vec![mention("README.md", "@README.md", 8, 18)]
    );
}

#[test]
fn keeps_canonical_file_links_for_scoped_paths_as_mentions() {
    assert_eq!(
        collect_composer_inline_tokens("Inspect [sub](@scope/pkg/sub) next", Default::default()),
        vec![mention("@scope/pkg/sub", "[sub](@scope/pkg/sub)", 8, 29)]
    );
}

#[test]
fn allows_ambiguous_scoped_paths_through_explicit_quoted_mentions() {
    assert_eq!(
        collect_composer_inline_tokens("Inspect @\"expo/ui\" next", Default::default()),
        vec![mention("expo/ui", "@\"expo/ui\"", 8, 18)]
    );
}

#[test]
fn still_collects_a_file_link_whose_label_is_at_the_length_cap() {
    let label = format!("{}.tsx", "a".repeat(508));
    let text = format!("see [{label}](src/{label}) ok");

    let tokens = collect_composer_inline_tokens(&text, Default::default());
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, format!("src/{label}"));
}

#[test]
fn leaves_a_file_link_past_the_label_cap_as_plain_text() {
    let label = format!("{}.tsx", "a".repeat(509));
    assert_eq!(
        collect_composer_inline_tokens(
            &format!("see [{label}](src/{label}) ok"),
            Default::default()
        ),
        vec![]
    );
}

#[test]
fn leaves_a_context_reference_link_alone() {
    assert_eq!(
        collect_composer_inline_tokens(
            "see [checkout.png](t3-context://v1/image/ctx_abc) ok",
            Default::default()
        ),
        vec![]
    );
    assert_eq!(
        collect_composer_inline_tokens(
            "see ![ctx_abc](t3-context://v1/image/ctx_abc) ok",
            Default::default()
        ),
        vec![]
    );
}

#[test]
fn stays_fast_on_unterminated_bracket_runs() {
    // Unbounded, the label body rescanned the rest of the text from every
    // whitespace: this input took seconds.
    let started = std::time::Instant::now();
    let text: String = " [".repeat(40_000);
    assert_eq!(
        collect_composer_inline_tokens(&text, Default::default()),
        vec![]
    );
    assert!(started.elapsed().as_millis() < 1_000);
}
