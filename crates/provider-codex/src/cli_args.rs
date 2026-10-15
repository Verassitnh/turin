//! Port of `tokenizeCliArgs` in upstream/packages/shared/src/cliArgs.ts.
//!
//! Shell-style tokenizer: quotes group, `\` escapes whitespace, and inside
//! double quotes `\` escapes only `"`, `\`, `$`, and `` ` ``. Every other
//! backslash stays literal, so Windows-style paths survive.

/// Splits `args` into argv tokens. Empty or whitespace-only input gives no tokens.
pub fn tokenize_cli_args(args: &str) -> Vec<String> {
    let input = args.trim();
    if input.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut quoted = false;
    let mut index = 0;

    while index < chars.len() {
        let char = chars[index];

        if let Some(open) = quote {
            if char == open {
                quote = None;
                quoted = true;
            } else if char == '\\' && open == '"' {
                match chars.get(index + 1) {
                    Some(next) if matches!(next, '"' | '\\' | '$' | '`') => {
                        current.push(*next);
                        index += 1;
                    }
                    _ => current.push(char),
                }
            } else {
                current.push(char);
            }
            index += 1;
            continue;
        }

        if char == '\'' || char == '"' {
            quote = Some(char);
            quoted = true;
        } else if char.is_whitespace() {
            if !current.is_empty() || quoted {
                tokens.push(std::mem::take(&mut current));
                quoted = false;
            }
        } else if char == '\\' {
            match chars.get(index + 1) {
                Some(next) if next.is_whitespace() => {
                    current.push(*next);
                    index += 1;
                }
                _ => current.push(char),
            }
        } else {
            current.push(char);
        }
        index += 1;
    }

    if !current.is_empty() || quoted {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_quoted_values_and_escaped_spaces() {
        assert_eq!(
            tokenize_cli_args(
                r#"--config model="gpt 5" --enable foo\ bar --config=profile='work profile'"#
            ),
            vec![
                "--config",
                "model=gpt 5",
                "--enable",
                "foo bar",
                "--config=profile=work profile",
            ]
        );
    }

    #[test]
    fn preserves_literal_backslashes_in_path_values() {
        assert_eq!(
            tokenize_cli_args(r#"--config cacheDir=C:\Users\me --config "quoted=C:\Users\me""#),
            vec![
                "--config",
                r"cacheDir=C:\Users\me",
                "--config",
                r"quoted=C:\Users\me",
            ]
        );
    }

    #[test]
    fn empty_and_whitespace_input_has_no_tokens() {
        assert!(tokenize_cli_args("").is_empty());
        assert!(tokenize_cli_args("   ").is_empty());
    }

    #[test]
    fn empty_quotes_are_a_token() {
        assert_eq!(tokenize_cli_args(r#"a "" b"#), vec!["a", "", "b"]);
    }

    #[test]
    fn double_quote_escapes_only_special_characters() {
        assert_eq!(tokenize_cli_args(r#""a\"b""#), vec![r#"a"b"#]);
        assert_eq!(tokenize_cli_args(r#""a\nb""#), vec![r"a\nb"]);
        assert_eq!(tokenize_cli_args(r#""a\$b""#), vec!["a$b"]);
    }

    #[test]
    fn collapses_extra_whitespace() {
        assert_eq!(
            tokenize_cli_args("  --strict-config   --enable  foo  "),
            vec!["--strict-config", "--enable", "foo"]
        );
    }
}
