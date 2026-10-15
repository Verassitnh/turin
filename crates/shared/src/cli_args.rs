//! Port of upstream/packages/shared/src/cliArgs.ts

use std::collections::HashMap;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedCliArgs {
    /// `null` values are boolean flags (present without a value).
    pub flags: HashMap<String, Option<String>>,
    pub positionals: Vec<String>,
}

pub struct ParseCliArgsOptions<'a> {
    pub boolean_flags: Option<&'a [String]>,
}

fn is_js_whitespace(unit: u16) -> bool {
    let ch = match char::from_u32(unit as u32) {
        Some(ch) if (unit as u32) < 0x1_0000 => ch, // BMP unit
        _ => return false, // lone surrogate is not whitespace
    };
    matches!(
        ch,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Tokenizes a shell-style argument string (quote- and escape-aware).
///
/// Single quotes preserve everything; double quotes honor `\\` escapes for
/// `"`, `\\`, `$`, and `` ` ``; an unquoted backslash escapes whitespace only;
/// other backslashes stay literal.
pub fn tokenize_cli_args(args: Option<&str>) -> Vec<String> {
    let input = args.map(turin_contracts::base_schemas::js_trim);
    let Some(input) = input.filter(|input| !input.is_empty()) else {
        return Vec::new();
    };

    let units: Vec<u16> = input.encode_utf16().collect();
    let unit_at = |index: usize| -> Option<u16> { units.get(index).copied() };

    let mut tokens: Vec<String> = Vec::new();
    let mut current: Vec<u16> = Vec::new();
    let mut quote: Option<u16> = None;
    let mut quoted = false;

    let mut index = 0usize;
    while index < units.len() {
        let unit = units[index];

        if let Some(open_quote) = quote {
            if unit == open_quote {
                quote = None;
                quoted = true;
            } else if unit == b'\\' as u16 && open_quote == b'"' as u16 {
                let next = unit_at(index + 1);
                match next {
                    Some(next) if matches!(next as u8, b'"' | b'\\' | b'$' | b'`') => {
                        current.push(next);
                        index += 1;
                    }
                    _ => current.push(unit),
                }
            } else {
                current.push(unit);
            }
            index += 1;
            continue;
        }

        if unit == b'\'' as u16 || unit == b'"' as u16 {
            quote = Some(unit);
            quoted = true;
        } else if is_js_whitespace(unit) {
            if !current.is_empty() || quoted {
                tokens.push(String::from_utf16_lossy(&current));
                current.clear();
                quoted = false;
            }
        } else if unit == b'\\' as u16 {
            let next = unit_at(index + 1);
            match next {
                Some(next) if is_js_whitespace(next) => {
                    current.push(next);
                    index += 1;
                }
                _ => current.push(unit),
            }
        } else {
            current.push(unit);
        }
        index += 1;
    }

    if !current.is_empty() || quoted {
        tokens.push(String::from_utf16_lossy(&current));
    }
    tokens
}

/// Parse CLI-style arguments into flags and positionals.
///
/// Accepts a string (quote-aware tokenized) or a pre-split argv array.
/// Supports `--key value`, `--key=value`, and `--flag` (boolean) syntax.
pub fn parse_cli_args(
    args: CliArgsSource<'_>,
    options: Option<&ParseCliArgsOptions<'_>>,
) -> ParsedCliArgs {
    let tokens: Vec<String> = match args {
        CliArgsSource::String(args) => tokenize_cli_args(Some(args)),
        CliArgsSource::Argv(argv) => argv.to_vec(),
    };
    let boolean_set = options.and_then(|options| options.boolean_flags.as_ref());

    let mut flags: HashMap<String, Option<String>> = HashMap::new();
    let mut positionals: Vec<String> = Vec::new();

    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];

        if let Some(rest) = token.strip_prefix("--") {
            if rest.is_empty() {
                index += 1;
                continue;
            }

            // Handle --key=value syntax
            if let Some(eq_index) = rest.find('=') {
                flags.insert(
                    rest[..eq_index].to_owned(),
                    Some(rest[eq_index + 1..].to_owned()),
                );
                index += 1;
                continue;
            }

            // Known boolean flag — never consumes next token
            if boolean_set.is_some_and(|set| set.iter().any(|flag| flag == rest)) {
                flags.insert(rest.to_owned(), None);
                index += 1;
                continue;
            }

            // Handle --key value or --flag (boolean)
            let next = tokens.get(index + 1);
            match next {
                Some(next) if !next.starts_with("--") => {
                    flags.insert(rest.to_owned(), Some(next.clone()));
                    index += 2;
                }
                _ => {
                    flags.insert(rest.to_owned(), None);
                    index += 1;
                }
            }
        } else {
            positionals.push(token.clone());
            index += 1;
        }
    }

    ParsedCliArgs { flags, positionals }
}

/// `parseCliArgs` accepts `string | readonly string[]`.
pub enum CliArgsSource<'a> {
    String(&'a str),
    Argv(&'a [String]),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag_map(pairs: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn preserves_quoted_values_and_escaped_spaces() {
        assert_eq!(
            tokenize_cli_args(Some(
                r#"--config model="gpt 5" --enable foo\ bar --config=profile='work profile'"#
            )),
            vec![
                "--config",
                "model=gpt 5",
                "--enable",
                "foo bar",
                "--config=profile=work profile"
            ]
        );
    }

    #[test]
    fn preserves_literal_backslashes_in_path_values() {
        assert_eq!(
            tokenize_cli_args(Some(
                r#"--config cacheDir=C:\Users\me --config "quoted=C:\Users\me""#
            )),
            vec![
                "--config",
                r"cacheDir=C:\Users\me",
                "--config",
                r"quoted=C:\Users\me"
            ]
        );
    }

    #[test]
    fn returns_empty_result_for_empty_string() {
        assert_eq!(parse_cli_args(CliArgsSource::String(""), None), ParsedCliArgs::default());
    }

    #[test]
    fn returns_empty_result_for_whitespace_only_string() {
        assert_eq!(parse_cli_args(CliArgsSource::String("   "), None), ParsedCliArgs::default());
    }

    #[test]
    fn returns_empty_result_for_empty_array() {
        assert_eq!(parse_cli_args(CliArgsSource::Argv(&[]), None), ParsedCliArgs::default());
    }

    #[test]
    fn parses_chrome_boolean_flag() {
        let parsed = parse_cli_args(CliArgsSource::String("--chrome"), None);
        assert_eq!(parsed.flags, flag_map(&[("chrome", None)]));
        assert_eq!(parsed.positionals, Vec::<String>::new());
    }

    #[test]
    fn parses_chrome_with_verbose() {
        let parsed = parse_cli_args(CliArgsSource::String("--chrome --verbose"), None);
        assert_eq!(parsed.flags, flag_map(&[("chrome", None), ("verbose", None)]));
    }

    #[test]
    fn parses_effort_with_a_value() {
        let parsed = parse_cli_args(CliArgsSource::String("--effort high"), None);
        assert_eq!(parsed.flags, flag_map(&[("effort", Some("high"))]));
    }

    #[test]
    fn parses_chrome_effort_high_debug() {
        let parsed = parse_cli_args(CliArgsSource::String("--chrome --effort high --debug"), None);
        assert_eq!(
            parsed.flags,
            flag_map(&[("chrome", None), ("effort", Some("high")), ("debug", None)])
        );
    }

    #[test]
    fn parses_model_with_full_model_name() {
        let parsed = parse_cli_args(CliArgsSource::String("--model claude-sonnet-4-6"), None);
        assert_eq!(parsed.flags, flag_map(&[("model", Some("claude-sonnet-4-6"))]));
    }

    #[test]
    fn parses_append_system_prompt_with_value_and_chrome() {
        let parsed = parse_cli_args(
            CliArgsSource::String("--append-system-prompt always-think-step-by-step --chrome"),
            None,
        );
        assert_eq!(
            parsed.flags,
            flag_map(&[("append-system-prompt", Some("always-think-step-by-step")), ("chrome", None)])
        );
    }

    #[test]
    fn parses_quoted_append_system_prompt_with_value_and_chrome() {
        let parsed = parse_cli_args(
            CliArgsSource::String(r#"--append-system-prompt "always think step by step" --chrome"#),
            None,
        );
        assert_eq!(
            parsed.flags,
            flag_map(&[("append-system-prompt", Some("always think step by step")), ("chrome", None)])
        );
    }

    #[test]
    fn parses_max_budget_usd_with_numeric_value() {
        let parsed = parse_cli_args(CliArgsSource::String("--chrome --max-budget-usd 5.00"), None);
        assert_eq!(
            parsed.flags,
            flag_map(&[("chrome", None), ("max-budget-usd", Some("5.00"))])
        );
    }

    #[test]
    fn parses_effort_equals_high_syntax() {
        let parsed = parse_cli_args(CliArgsSource::String("--effort=high"), None);
        assert_eq!(parsed.flags, flag_map(&[("effort", Some("high"))]));
    }

    #[test]
    fn parses_key_value_mixed_with_boolean_flags() {
        let parsed =
            parse_cli_args(CliArgsSource::String("--chrome --model=claude-sonnet-4-6 --debug"), None);
        assert_eq!(
            parsed.flags,
            flag_map(&[("chrome", None), ("model", Some("claude-sonnet-4-6")), ("debug", None)])
        );
    }

    #[test]
    fn collects_positional_arguments() {
        let parsed = parse_cli_args(CliArgsSource::String("1.2.3"), None);
        assert_eq!(parsed.flags, HashMap::new());
        assert_eq!(parsed.positionals, vec!["1.2.3"]);
    }

    #[test]
    fn collects_positionals_mixed_with_flags_argv_array() {
        let argv = ["1.2.3", "--root", "/path", "--github-output"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        let parsed = parse_cli_args(CliArgsSource::Argv(&argv), None);
        assert_eq!(parsed.flags, flag_map(&[("root", Some("/path")), ("github-output", None)]));
        assert_eq!(parsed.positionals, vec!["1.2.3"]);
    }

    #[test]
    fn handles_extra_whitespace_between_tokens() {
        let parsed = parse_cli_args(CliArgsSource::String("  --chrome   --verbose  "), None);
        assert_eq!(parsed.flags, flag_map(&[("chrome", None), ("verbose", None)]));
    }

    #[test]
    fn ignores_bare_double_dash_with_no_flag_name() {
        assert_eq!(parse_cli_args(CliArgsSource::String("--"), None), ParsedCliArgs::default());
    }

    #[test]
    fn boolean_flag_does_not_consume_next_token_as_value() {
        let boolean_flags = vec!["github-output".to_owned()];
        let argv = ["--github-output", "1.2.3"].iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let parsed = parse_cli_args(
            CliArgsSource::Argv(&argv),
            Some(&ParseCliArgsOptions { boolean_flags: Some(&boolean_flags) }),
        );
        assert_eq!(parsed.flags, flag_map(&[("github-output", None)]));
        assert_eq!(parsed.positionals, vec!["1.2.3"]);
    }

    #[test]
    fn non_boolean_flag_still_consumes_next_token() {
        let boolean_flags = vec!["github-output".to_owned()];
        let argv = ["--root", "/path", "1.2.3"].iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let parsed = parse_cli_args(
            CliArgsSource::Argv(&argv),
            Some(&ParseCliArgsOptions { boolean_flags: Some(&boolean_flags) }),
        );
        assert_eq!(parsed.flags, flag_map(&[("root", Some("/path"))]));
        assert_eq!(parsed.positionals, vec!["1.2.3"]);
    }

    #[test]
    fn mixes_boolean_and_value_flags_with_positionals() {
        let boolean_flags = vec!["github-output".to_owned()];
        let argv = ["--github-output", "--root", "/path", "1.2.3"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        let parsed = parse_cli_args(
            CliArgsSource::Argv(&argv),
            Some(&ParseCliArgsOptions { boolean_flags: Some(&boolean_flags) }),
        );
        assert_eq!(
            parsed.flags,
            flag_map(&[("github-output", None), ("root", Some("/path"))])
        );
        assert_eq!(parsed.positionals, vec!["1.2.3"]);
    }
}
