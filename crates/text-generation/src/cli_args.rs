//! Port of `tokenizeCliArgs` from upstream/packages/shared/src/cliArgs.ts.
//!
//! Copied from the T09d `cli_args.rs` branch because `turin-shared` is not on `stage`
//! yet. Dedupe when it lands.

/// Splits a launch-args string into argv, honouring single and double quotes and backslash escapes.
pub fn tokenize_cli_args(args: Option<&str>) -> Vec<String> {
    let Some(input) = args.map(turin_contracts::js_trim) else {
        return Vec::new();
    };
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
        if let Some(active) = quote {
            if char == active {
                quote = None;
                quoted = true;
            } else if char == '\\' && active == '"' {
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
        } else if char == '\'' || char == '"' {
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
