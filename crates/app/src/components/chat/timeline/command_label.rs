//! Port of upstream/packages/client-runtime/src/work-log/commandLabel.ts
//!
//! String semantics: upstream indexes JS strings by UTF-16 code unit. The
//! scanners here index `Vec<char>` (Unicode scalar values) or, for
//! `split_first_shell_command`, UTF-8 bytes. Every character the scanners
//! branch on is ASCII, so the only observable gap is that an astral character
//! counts as one position rather than two, which no comparison or slice here
//! depends on. JS `\s`, `trim()` and `trimStart()`
//! use the ECMAScript whitespace set (which includes U+FEFF and excludes
//! U+0085), so they go through `is_js_whitespace` / `js_trim` rather than
//! `char::is_whitespace` / `str::trim`, and regexes spell that set out.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandWrapper {
    Env,
    Sudo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandProgramContext {
    Exec,
    Shell,
}

const MAX_COMMAND_SEGMENTS: usize = 64;

const SHELL_PROGRAMS: &[&str] = &["sh", "bash", "zsh", "dash", "ash", "ksh", "fish"];
const WINDOWS_SHELL_PROGRAMS: &[&str] = &[
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
];
const SHELL_OPTIONS_WITH_VALUE: &[&str] = &["-o", "-O", "--rcfile", "--init-file"];
const SHELL_COMMAND_WRAPPERS: &[&str] = &["builtin", "command", "exec"];
const SHELL_PRECOMMAND_MODIFIERS: &[&str] = &["nocorrect", "noglob", "time"];
const POWERSHELL_SETUP_PROGRAMS: &[&str] = &["pop-location", "push-location", "set-location"];
const POWERSHELL_FLAGS: &[&str] = &["-mta", "-nologo", "-noninteractive", "-noprofile", "-sta"];
const POWERSHELL_OPTIONS_WITH_VALUE: &[&str] = &[
    "-configurationname",
    "-executionpolicy",
    "-inputformat",
    "-outputformat",
    "-version",
    "-windowstyle",
    "-workingdirectory",
];
const START_PROCESS_FLAGS: &[&str] = &[
    "-confirm",
    "-debug",
    "-loaduserprofile",
    "-nonewwindow",
    "-passthru",
    "-usenewenvironment",
    "-verbose",
    "-wait",
    "-whatif",
];
const START_PROCESS_OPTIONS_WITH_VALUE: &[&str] = &[
    "-argumentlist",
    "-credential",
    "-environment",
    "-erroraction",
    "-errorvariable",
    "-informationaction",
    "-informationvariable",
    "-outbuffer",
    "-outvariable",
    "-pipelinevariable",
    "-progressaction",
    "-redirectstandarderror",
    "-redirectstandardinput",
    "-redirectstandardoutput",
    "-verb",
    "-warningaction",
    "-warningvariable",
    "-windowstyle",
    "-workingdirectory",
];
const SKIPPABLE_SUDO_PROBES: &[&str] = &["[", "[[", "test", "true"];
const NON_PROGRAM_PREFIX_CHARACTERS: &str = "<>(){}[];|&$`#!%@:";
const NON_PROGRAM_SUFFIX_CHARACTERS: &str = "){]}`";

// These tokens describe shell syntax or shell-local control flow, not a useful
// executable name. Falling back to "command" is less misleading than labels
// such as "Ran if", "Ran [", or "Ran function".
const NON_DESCRIPTIVE_SHELL_PROGRAMS: &[&str] = &[
    "!",
    "#",
    ".",
    ":",
    "[",
    "[[",
    "alias",
    "and",
    "autoload",
    "begin",
    "bg",
    "bind",
    "bindkey",
    "break",
    "builtin",
    "caller",
    "case",
    "catch",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "continue",
    "coproc",
    "declare",
    "dirs",
    "disown",
    "do",
    "done",
    "elif",
    "else",
    "enable",
    "end",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "fi",
    "finally",
    "for",
    "foreach",
    "function",
    "getopts",
    "history",
    "if",
    "in",
    "jobs",
    "let",
    "local",
    "logout",
    "mapfile",
    "nocorrect",
    "noglob",
    "not",
    "or",
    "popd",
    "pushd",
    "read",
    "readarray",
    "readonly",
    "repeat",
    "return",
    "select",
    "set",
    "setopt",
    "shift",
    "shopt",
    "source",
    "switch",
    "suspend",
    "test",
    "then",
    "time",
    "times",
    "trap",
    "try",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "until",
    "unset",
    "unsetopt",
    "wait",
    "while",
];

// Unlike setup builtins, these can make later segments part of control flow or
// otherwise unreachable, so do not use a later program as the command label.
const TERMINAL_SHELL_PROGRAMS: &[&str] = &[
    "and", "begin", "break", "case", "catch", "continue", "coproc", "do", "done", "elif", "else",
    "end", "esac", "eval", "exec", "exit", "false", "fi", "finally", "for", "foreach", "function",
    "if", "in", "not", "or", "repeat", "return", "select", "switch", "then", "try", "until",
    "while",
];

// ---------------------------------------------------------------------------
// JS string/regex semantics helpers.

/// ECMAScript `WhiteSpace` + `LineTerminator`: what `/\s/` matches and what
/// `String.prototype.trim` removes.
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}'
    ) || ('\u{2000}'..='\u{200A}').contains(&character)
        || matches!(
            character,
            '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
        )
}

/// What JS `.` matches without the `s` flag.
fn is_js_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// `value.split(/[\\/]/).at(-1)`.
fn last_path_segment(value: &str) -> &str {
    value.rsplit(['\\', '/']).next().unwrap_or("")
}

fn has_path_separator(value: &str) -> bool {
    value.contains(['\\', '/'])
}

/// `value.replace(/\.exe$/i, "")`. JS case-insensitive matching only folds
/// ASCII onto `e`/`x`, so an ASCII comparison is exact.
fn strip_exe_suffix_ignore_case(value: &str) -> &str {
    let parts = value
        .len()
        .checked_sub(4)
        .and_then(|split| Some((value.get(..split)?, value.get(split..)?)));
    match parts {
        Some((stem, extension)) if extension.eq_ignore_ascii_case(".exe") => stem,
        _ => value,
    }
}

/// Length of the `[A-Za-z_][A-Za-z0-9_]*` prefix (0 when absent).
fn identifier_prefix_len(value: &str) -> usize {
    value
        .bytes()
        .enumerate()
        .take_while(|&(index, byte)| {
            byte == b'_'
                || if index == 0 {
                    byte.is_ascii_alphabetic()
                } else {
                    byte.is_ascii_alphanumeric()
                }
        })
        .count()
}

/// `/^[A-Za-z_][A-Za-z0-9_]*\+?=/`.
fn is_assignment_word(token: &str) -> bool {
    let name_len = identifier_prefix_len(token);
    if name_len == 0 {
        return false;
    }
    let rest = &token[name_len..];
    rest.strip_prefix('+').unwrap_or(rest).starts_with('=')
}

/// `/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/s`.
fn split_assignment_word(token: &str) -> Option<(&str, &str)> {
    let name_len = identifier_prefix_len(token);
    if name_len == 0 {
        return None;
    }
    let value = token[name_len..].strip_prefix('=')?;
    Some((&token[..name_len], value))
}

/// `/^[A-Za-z][A-Za-z0-9+.-]*:(?![\\/])/` (the `regex` crate has no
/// lookahead, and the scheme run cannot contain `:`, so this cannot backtrack).
fn has_uri_scheme_prefix(value: &str) -> bool {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
    {
        return false;
    }
    let after_scheme = chars.as_str().trim_start_matches(|character: char| {
        character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
    });
    after_scheme
        .strip_prefix(':')
        .is_some_and(|after_colon| !after_colon.starts_with(['\\', '/']))
}

const JS_WS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";
const JS_NWS: &str = r"[^\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";
const JS_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// Compiles an upstream regex. `{WS}`, `{NWS}` and `{DOT}` stand for JS `\s`,
/// `\S` and `.`; `\d` is written `[0-9]` because JS digits are ASCII.
fn js_regex(pattern: &str) -> Regex {
    let pattern = pattern
        .replace("{WS}", JS_WS)
        .replace("{NWS}", JS_NWS)
        .replace("{DOT}", JS_DOT);
    Regex::new(&pattern).expect("command label regexes are valid")
}

static WINDOWS_PATH_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^(?:[A-Za-z]:|\.{1,2})(?:\\{NWS}*)?$"));
static SHELL_REDIRECTION: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^(?:(?:(?:[0-9]+|\*|\{[A-Za-z_][A-Za-z0-9_]*\})?(?:<<<|<<-|<<|<>|>>|>\||<&|>&|<|>))|&>>|&>)({DOT}*)$",
    )
});
static SCRIPT_OPTIONS: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^-[adkpqr]+$"));
static ARCH_FLAG: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^-(?:arm64|arm64e|i386|x86_64)$"));
static TIMEOUT_DURATION: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)[smhd]?$"));
static POWERSHELL_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?i)^{WS}*\$(?:(env|global|local|script):)?[A-Za-z_][A-Za-z0-9_]*{WS}*={WS}*((?s:.)*)$",
    )
});
static POWERSHELL_COLLECTION: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)^(?:\[ordered\]{WS}*)?@{WS}*[{(]"));
// Upstream ends this with `\b`; see `power_shell_direct_command`.
static POWERSHELL_DIRECT_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^(?:@?\({WS}*)?([A-Za-z][A-Za-z0-9_.-]*)"));
static REFERENCED_COMMAND_ALIAS: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(r"^\$(?:([A-Za-z_][A-Za-z0-9_]*)|\{([A-Za-z_][A-Za-z0-9_]*)(?:\[@\])?\})$")
});
static POWERSHELL_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(r"(?i)^(?:catch|finally|for|foreach|function|if|param|switch|try|while){WS}*[{(]")
});
static SHELL_FUNCTION_DEFINITION: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^[A-Za-z_][A-Za-z0-9_]*{WS}*\({WS}*\){WS}*\{"));
static WINDOWS_PATH_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?i)^{WS}*((?:\.{1,2}|%[A-Za-z_][A-Za-z0-9_]*%|\$env:[A-Za-z_][A-Za-z0-9_]*)\\{NWS}+)",
    )
});
static COMPACT_FUNCTION_DEFINITION: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^[A-Za-z_][A-Za-z0-9_]*\(\)\{$"));
static HIGHLIGHT_PROGRAM: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r#"^(?:&{WS}*)?(?:"([^"]*)"|'([^']*)'|({NWS}+))"#));
static POWERSHELL_CMDLET: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^([A-Z][a-z]+(?:[A-Z][a-z]+)?)-[A-Z][A-Za-z]*$"));
static POWERSHELL_SCOPED_VARIABLE: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?i)^\$(?:env|global|local|script):"));
static POWERSHELL_VARIABLE_ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^\$[A-Za-z_][A-Za-z0-9_]*{WS}+="));

// ---------------------------------------------------------------------------

/// `/^-[a-zA-Z]*c[a-zA-Z]*$/`.
fn is_command_flag_cluster(option: &str) -> bool {
    option.strip_prefix('-').is_some_and(|flags| {
        flags.contains('c')
            && flags
                .chars()
                .all(|character| character.is_ascii_alphabetic())
    })
}

fn shell_command_argument_index(tokens: &[String], start: usize) -> Option<usize> {
    let mut index = start;
    while index < tokens.len() {
        let option = tokens[index].as_str();
        if option == "--" || !option.starts_with('-') {
            return None;
        }
        if SHELL_OPTIONS_WITH_VALUE.contains(&option) {
            index += 2;
            continue;
        }
        if option == "--command" || is_command_flag_cluster(option) {
            return Some(index + 1);
        }
        index += 1;
    }
    None
}

fn command_wrapper_options_with_value(wrapper: CommandWrapper) -> &'static [&'static str] {
    match wrapper {
        CommandWrapper::Env => &["-C", "--chdir", "-S", "--split-string", "-u", "--unset"],
        CommandWrapper::Sudo => &[
            "-C",
            "--close-from",
            "-D",
            "--chdir",
            "-g",
            "--group",
            "-u",
            "--user",
        ],
    }
}

fn command_wrapper_flags(wrapper: CommandWrapper) -> &'static [&'static str] {
    match wrapper {
        CommandWrapper::Env => &[
            "-0",
            "--null",
            "-i",
            "--ignore-environment",
            "--debug",
            "-v",
        ],
        CommandWrapper::Sudo => &[
            "-A",
            "--askpass",
            "-b",
            "--background",
            "-E",
            "-H",
            "-i",
            "-n",
            "-S",
        ],
    }
}

fn tokenize_shell_command(command: &str) -> Option<Vec<String>> {
    let input: Vec<char> = js_trim(command).chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let mut in_backticks = false;
    let mut substitution_depth = 0usize;
    let mut parameter_expansion_depth = 0usize;
    let mut token_started = false;

    let mut cursor = 0;
    while cursor < input.len() {
        let index = cursor;
        cursor += 1;
        let character = input[index];
        if escaping {
            current.push(character);
            escaping = false;
            token_started = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            let next_character = input.get(index + 1).copied();
            let is_windows_path = quote.is_none() && WINDOWS_PATH_PREFIX.is_match(&current);
            if (quote == Some('"') || is_windows_path)
                && next_character.is_some_and(|next| !matches!(next, '"' | '\\' | '$' | '`' | '\n'))
            {
                current.push(character);
                token_started = true;
                continue;
            }
            escaping = true;
            token_started = true;
            continue;
        }
        if in_backticks {
            current.push(character);
            if character == '`' {
                in_backticks = false;
            }
            token_started = true;
            continue;
        }
        if let Some(open_quote) = quote {
            if character == open_quote {
                quote = None;
            } else {
                current.push(character);
            }
            token_started = true;
            continue;
        }
        if character == '`' {
            current.push(character);
            in_backticks = true;
            token_started = true;
            continue;
        }
        if character == '$' && input.get(index + 1) == Some(&'{') {
            current.push_str("${");
            parameter_expansion_depth += 1;
            token_started = true;
            cursor += 1;
            continue;
        }
        if character == '{' && parameter_expansion_depth > 0 {
            current.push(character);
            parameter_expansion_depth += 1;
            token_started = true;
            continue;
        }
        if character == '}' && parameter_expansion_depth > 0 {
            current.push(character);
            parameter_expansion_depth -= 1;
            token_started = true;
            continue;
        }
        if character == '$' && input.get(index + 1) == Some(&'(') {
            current.push_str("$(");
            substitution_depth += 1;
            token_started = true;
            cursor += 1;
            continue;
        }
        if character == '(' {
            current.push(character);
            substitution_depth += 1;
            token_started = true;
            continue;
        }
        if character == ')' && substitution_depth > 0 {
            current.push(character);
            substitution_depth -= 1;
            token_started = true;
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            token_started = true;
            continue;
        }
        if is_js_whitespace(character) {
            if substitution_depth > 0 || parameter_expansion_depth > 0 {
                current.push(character);
                token_started = true;
                continue;
            }
            if token_started {
                tokens.push(std::mem::take(&mut current));
                token_started = false;
            }
            continue;
        }
        current.push(character);
        token_started = true;
    }

    if quote.is_some()
        || escaping
        || in_backticks
        || substitution_depth > 0
        || parameter_expansion_depth > 0
    {
        return None;
    }
    if token_started {
        tokens.push(current);
    }
    Some(tokens)
}

#[derive(Debug)]
struct ShellCommandSplit {
    first_command: String,
    remaining_command: Option<String>,
    separator: Option<String>,
}

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
}

#[derive(Clone, Copy)]
struct ShellSeparator {
    index: usize,
    length: usize,
}

struct ShellCommentRange {
    start: usize,
    end: usize,
}

fn command_without_shell_comments(
    command: &str,
    end: usize,
    comments: &[ShellCommentRange],
) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    for comment in comments {
        if comment.start >= end {
            break;
        }
        result.push_str(byte_slice(command, cursor, comment.start));
        cursor = comment.end.min(end);
    }
    result + byte_slice(command, cursor, end)
}

/// `String.prototype.slice(start, end)` over byte offsets, clamped like JS.
/// Callers only pass offsets of ASCII characters (or the end), which are
/// always char boundaries.
fn byte_slice(command: &str, start: usize, end: usize) -> &str {
    let end = end.min(command.len());
    if start >= end {
        return "";
    }
    command.get(start..end).unwrap_or("")
}

fn read_heredoc_delimiter(
    command: &str,
    start: usize,
    strip_tabs: bool,
) -> Option<(Heredoc, usize)> {
    let mut index = start;
    while matches!(command.as_bytes().get(index), Some(b' ' | b'\t')) {
        index += 1;
    }

    let mut delimiter = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let mut end = command.len();
    for (offset, character) in command.get(index..).unwrap_or("").char_indices() {
        if escaping {
            delimiter.push(character);
            escaping = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaping = true;
            continue;
        }
        if let Some(open_quote) = quote {
            if character == open_quote {
                quote = None;
            } else {
                delimiter.push(character);
            }
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            continue;
        }
        if is_js_whitespace(character) || ";&|<>()".contains(character) {
            end = index + offset;
            break;
        }
        delimiter.push(character);
    }

    if delimiter.is_empty() || quote.is_some() || escaping {
        return None;
    }
    Some((
        Heredoc {
            delimiter,
            strip_tabs,
        },
        end,
    ))
}

fn command_after_heredocs(command: &str, start: usize, heredocs: &[Heredoc]) -> Option<String> {
    let mut cursor = start;
    for heredoc in heredocs {
        let mut found_delimiter = false;
        while cursor <= command.len() {
            let newline_index = command
                .get(cursor..)
                .and_then(|rest| rest.find('\n'))
                .map(|offset| cursor + offset);
            let line_end = newline_index.unwrap_or(command.len());
            let line = byte_slice(command, cursor, line_end);
            let line = line.strip_suffix('\r').unwrap_or(line);
            let comparable_line = if heredoc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            cursor = newline_index.map_or(command.len(), |newline| newline + 1);
            if comparable_line == heredoc.delimiter {
                found_delimiter = true;
                break;
            }
            if newline_index.is_none() {
                break;
            }
        }
        if !found_delimiter {
            return None;
        }
    }

    non_empty(js_trim(byte_slice(command, cursor, command.len())).to_owned())
}

// Scans bytes rather than chars: every character this branches on is ASCII,
// and UTF-8 lead/continuation bytes never equal an ASCII byte, so multi-byte
// characters fall through the same paths a JS code unit would. Scanning the
// `&str` in place (rather than collecting chars) keeps the repeated splits of
// long setup chains linear in the first segment, as upstream's are.
fn split_first_shell_command(command: &str) -> ShellCommandSplit {
    let bytes = command.as_bytes();
    let at = |index: usize| bytes.get(index).copied();
    let before = |index: usize| index.checked_sub(1).and_then(at);

    let mut quote: Option<u8> = None;
    let mut power_shell_here_string_quote: Option<u8> = None;
    let mut escaping = false;
    let mut in_backticks = false;
    let mut in_comment = false;
    let mut substitution_depth = 0usize;
    let mut parameter_expansion_depth = 0usize;
    let mut heredocs: Vec<Heredoc> = Vec::new();
    let mut comments: Vec<ShellCommentRange> = Vec::new();
    let mut comment_start = 0;
    let mut separator_before_heredocs: Option<ShellSeparator> = None;

    let mut cursor = 0;
    while cursor < bytes.len() {
        let index = cursor;
        cursor += 1;
        let character = bytes[index];
        if let Some(here_string_quote) = power_shell_here_string_quote {
            if character == here_string_quote
                && at(index + 1) == Some(b'@')
                && (index == 0 || before(index) == Some(b'\n'))
            {
                power_shell_here_string_quote = None;
                cursor += 1;
            }
            continue;
        }
        if in_comment {
            if character != b'\n' {
                continue;
            }
            in_comment = false;
            comments.push(ShellCommentRange {
                start: comment_start,
                end: index,
            });
        }
        if escaping {
            escaping = false;
            continue;
        }
        if character == b'\\' && quote != Some(b'\'') {
            escaping = true;
            continue;
        }
        if in_backticks {
            if character == b'`' {
                in_backticks = false;
            }
            continue;
        }
        if let Some(open_quote) = quote {
            if character == open_quote {
                quote = None;
            }
            continue;
        }
        if character == b'@'
            && matches!(at(index + 1), Some(b'"' | b'\''))
            && (at(index + 2) == Some(b'\n')
                || (at(index + 2) == Some(b'\r') && at(index + 3) == Some(b'\n')))
        {
            power_shell_here_string_quote = at(index + 1);
            cursor += 1;
            continue;
        }
        if character == b'"' || character == b'\'' {
            quote = Some(character);
            continue;
        }
        if character == b'`' {
            in_backticks = true;
            continue;
        }
        if character == b'#'
            && command[..index]
                .chars()
                .next_back()
                .is_none_or(|previous| is_js_whitespace(previous) || ";&|(".contains(previous))
        {
            in_comment = true;
            comment_start = index;
            continue;
        }
        if character == b'$' && at(index + 1) == Some(b'{') {
            parameter_expansion_depth += 1;
            cursor += 1;
            continue;
        }
        if character == b'{' && parameter_expansion_depth > 0 {
            parameter_expansion_depth += 1;
            continue;
        }
        if character == b'}' && parameter_expansion_depth > 0 {
            parameter_expansion_depth -= 1;
            continue;
        }
        if character == b'(' {
            substitution_depth += 1;
            continue;
        }
        if character == b')' && substitution_depth > 0 {
            substitution_depth -= 1;
            continue;
        }
        if substitution_depth > 0 || parameter_expansion_depth > 0 {
            continue;
        }

        if character == b'<' && at(index + 1) == Some(b'<') && at(index + 2) != Some(b'<') {
            let strip_tabs = at(index + 2) == Some(b'-');
            let Some((heredoc, end)) =
                read_heredoc_delimiter(command, index + if strip_tabs { 3 } else { 2 }, strip_tabs)
            else {
                return ShellCommandSplit {
                    first_command: js_trim(command).to_owned(),
                    remaining_command: None,
                    separator: None,
                };
            };
            heredocs.push(heredoc);
            cursor = end;
            continue;
        }

        let is_double_operator = (character == b'&' && at(index + 1) == Some(b'&'))
            || (character == b'|' && matches!(at(index + 1), Some(b'|' | b'&')));
        let is_redirection_ampersand = character == b'&'
            && (matches!(before(index), Some(b'>' | b'<')) || at(index + 1) == Some(b'>'));
        if (!is_double_operator && !b";&|\n".contains(&character)) || is_redirection_ampersand {
            continue;
        }

        if character == b'\n' && !heredocs.is_empty() {
            let separator = separator_before_heredocs;
            let first_command = js_trim_start(&command_without_shell_comments(
                command,
                separator.map_or(index, |separator| separator.index),
                &comments,
            ))
            .to_owned();
            let command_before_heredocs = separator
                .map(|separator| {
                    js_trim(byte_slice(
                        command,
                        separator.index + separator.length,
                        index,
                    ))
                    .to_owned()
                })
                .unwrap_or_default();
            let command_following_heredocs = command_after_heredocs(command, index + 1, &heredocs);
            let remaining_command = [Some(command_before_heredocs), command_following_heredocs]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            return ShellCommandSplit {
                first_command,
                remaining_command: non_empty(remaining_command),
                separator: Some(separator.map_or_else(
                    || "\n".to_owned(),
                    |separator| {
                        byte_slice(command, separator.index, separator.index + separator.length)
                            .to_owned()
                    },
                )),
            };
        }
        if !heredocs.is_empty() {
            separator_before_heredocs.get_or_insert(ShellSeparator {
                index,
                length: if is_double_operator { 2 } else { 1 },
            });
            if is_double_operator {
                cursor += 1;
            }
            continue;
        }

        let first_command =
            js_trim_start(&command_without_shell_comments(command, index, &comments)).to_owned();
        let separator_length = if is_double_operator { 2 } else { 1 };
        // Upstream skips whitespace after the separator before trimming; the
        // trim alone has the same effect.
        let next_command = js_trim(byte_slice(command, index + separator_length, command.len()));
        return ShellCommandSplit {
            first_command,
            remaining_command: non_empty(next_command.to_owned()),
            separator: Some(byte_slice(command, index, index + separator_length).to_owned()),
        };
    }

    if in_comment {
        comments.push(ShellCommentRange {
            start: comment_start,
            end: bytes.len(),
        });
    }
    ShellCommandSplit {
        first_command: js_trim(&command_without_shell_comments(
            command,
            bytes.len(),
            &comments,
        ))
        .to_owned(),
        remaining_command: None,
        separator: None,
    }
}

fn command_without_leading_shell_comments(command: &str) -> Option<&str> {
    let mut remaining_command = js_trim_start(command);
    while remaining_command.starts_with('#') {
        let newline_index = remaining_command.find('\n')?;
        remaining_command = js_trim_start(&remaining_command[newline_index + 1..]);
    }
    (!remaining_command.is_empty()).then_some(remaining_command)
}

fn without_shell_line_continuations(command: &str) -> String {
    let chars: Vec<char> = command.chars().collect();
    let mut normalized_command = String::new();
    let mut quote: Option<char> = None;
    let mut escaping = false;

    let mut cursor = 0;
    while cursor < chars.len() {
        let index = cursor;
        cursor += 1;
        let character = chars[index];
        if escaping {
            normalized_command.push(character);
            escaping = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            if chars.get(index + 1) == Some(&'\n') {
                cursor += 1;
                continue;
            }
            if chars.get(index + 1) == Some(&'\r') && chars.get(index + 2) == Some(&'\n') {
                cursor += 2;
                continue;
            }
            normalized_command.push(character);
            escaping = true;
            continue;
        }
        if let Some(open_quote) = quote {
            if character == open_quote {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        }
        normalized_command.push(character);
    }

    normalized_command
}

fn index_after_shell_redirection(tokens: &[String], index: usize) -> Option<usize> {
    let token = tokens.get(index).map_or("", String::as_str);
    if token.is_empty() || (token.starts_with(['<', '>']) && token[1..].starts_with('(')) {
        return None;
    }
    let captures = SHELL_REDIRECTION.captures(token)?;
    if captures
        .get(1)
        .is_some_and(|target| !target.as_str().is_empty())
    {
        return Some(index + 1);
    }
    Some(if tokens.get(index + 1).is_none() {
        tokens.len() + 1
    } else {
        index + 2
    })
}

fn serialize_shell_tokens(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| format!("'{}'", token.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn transparent_wrapper_command_index(
    wrapper: &str,
    tokens: &[String],
    index: usize,
) -> Option<usize> {
    let token_at = |position: usize| tokens.get(position).map(String::as_str);

    if wrapper == "bundle" {
        return (token_at(index + 1) == Some("exec") && token_at(index + 2).is_some())
            .then_some(index + 2);
    }

    if wrapper == "nohup" {
        let mut target_index = index + 1;
        if token_at(target_index) == Some("--") {
            target_index += 1;
        }
        let target = token_at(target_index);
        return target
            .is_some_and(|target| !target.is_empty() && !target.starts_with('-'))
            .then_some(target_index);
    }

    if wrapper == "script" {
        // BSD `script` takes an output file before the optional command. Requiring
        // an option and both operands avoids guessing about a plain `script file`.
        return (SCRIPT_OPTIONS.is_match(token_at(index + 1).unwrap_or(""))
            && token_at(index + 3).is_some())
        .then_some(index + 3);
    }

    if wrapper == "arch" {
        if ARCH_FLAG.is_match(token_at(index + 1).unwrap_or("")) {
            return token_at(index + 2).is_some().then_some(index + 2);
        }
        return (token_at(index + 1) == Some("-arch") && token_at(index + 3).is_some())
            .then_some(index + 3);
    }

    if wrapper == "timeout" || wrapper == "gtimeout" {
        return (TIMEOUT_DURATION.is_match(token_at(index + 1).unwrap_or(""))
            && token_at(index + 2).is_some())
        .then_some(index + 2);
    }

    None
}

fn static_program_name(value: &str) -> Option<String> {
    let trimmed_value = js_trim(value);
    if trimmed_value.is_empty() || has_uri_scheme_prefix(trimmed_value) {
        return None;
    }
    let program = last_path_segment(trimmed_value);
    if program.is_empty()
        || (program.contains(is_js_whitespace) && !has_path_separator(trimmed_value))
        || program
            .chars()
            .next()
            .is_some_and(|first| NON_PROGRAM_PREFIX_CHARACTERS.contains(first))
        || program
            .chars()
            .next_back()
            .is_some_and(|last| NON_PROGRAM_SUFFIX_CHARACTERS.contains(last))
    {
        return None;
    }
    Some(program.to_owned())
}

fn leading_power_shell_literal(command: &str) -> Option<String> {
    let input = js_trim_start(command);
    let quote = match input.chars().next() {
        Some(quote @ ('"' | '\'')) => quote,
        _ => return static_program_name(input.split(is_js_whitespace).next().unwrap_or("")),
    };

    let chars: Vec<char> = input.chars().collect();
    let mut value = String::new();
    let mut index = 1;
    while index < chars.len() {
        let character = chars[index];
        if character == '`'
            && let Some(&escaped) = chars.get(index + 1)
        {
            value.push(escaped);
            index += 2;
            continue;
        }
        if character == quote {
            if quote == '\'' && chars.get(index + 1) == Some(&'\'') {
                value.push('\'');
                index += 2;
                continue;
            }
            return static_program_name(&value);
        }
        value.push(character);
        index += 1;
    }
    None
}

/// `None` when the command is not a call (`&`) expression (upstream's
/// `undefined`); `Some(None)` when it is one without a literal program.
fn power_shell_call_operator_program_name(command: &str) -> Option<Option<String>> {
    // `/^\s*&\s+([\s\S]*)$/`
    let operand = js_trim_start(command).strip_prefix('&')?;
    if !operand.starts_with(is_js_whitespace) {
        return None;
    }
    Some(leading_power_shell_literal(operand))
}

/// `/^(?:@?\(\s*)?([A-Za-z][A-Za-z0-9_.-]*)\b/`. The `regex` crate's `\b` is
/// Unicode-aware, so match the run greedily and backtrack to a word character
/// by hand, which is where the ASCII `\b` lands.
fn power_shell_direct_command(value: &str) -> Option<&str> {
    let command = POWERSHELL_DIRECT_COMMAND.captures(value)?.get(1)?.as_str();
    Some(command.trim_end_matches(['.', '-']))
}

struct PowerShellAssignment {
    matched: bool,
    program: Option<String>,
}

fn power_shell_assignment_program_name(
    command: &str,
    depth: usize,
    remaining_command: Option<&str>,
    segments_remaining: usize,
) -> PowerShellAssignment {
    let Some(assignment) = POWERSHELL_ASSIGNMENT.captures(command) else {
        return PowerShellAssignment {
            matched: false,
            program: None,
        };
    };

    // Environment assignments are setup. Their right-hand side is a value, not
    // a command, so prefer the next top-level segment when one exists.
    if assignment
        .get(1)
        .is_some_and(|scope| scope.as_str().to_lowercase() == "env")
    {
        return PowerShellAssignment {
            matched: true,
            program: remaining_command.and_then(|remaining_command| {
                command_program_name_internal(
                    remaining_command,
                    depth,
                    CommandProgramContext::Shell,
                    segments_remaining.saturating_sub(1),
                )
            }),
        };
    }

    let value = js_trim(assignment.get(2).map_or("", |value| value.as_str()));
    // The POSIX-oriented segment splitter does not balance PowerShell arrays or
    // hashtables. Do not mistake a key after an internal semicolon for a command.
    if POWERSHELL_COLLECTION.is_match(value) {
        return PowerShellAssignment {
            matched: true,
            program: None,
        };
    }
    if let Some(called_program) = power_shell_call_operator_program_name(value) {
        return PowerShellAssignment {
            matched: true,
            program: called_program,
        };
    }

    if let Some(direct_command) = power_shell_direct_command(value)
        && !NON_DESCRIPTIVE_SHELL_PROGRAMS.contains(&direct_command.to_lowercase().as_str())
    {
        let parsed_command = command_program_name_internal(
            value,
            depth + 1,
            CommandProgramContext::Shell,
            segments_remaining,
        );
        return PowerShellAssignment {
            matched: true,
            program: parsed_command.or_else(|| Some(direct_command.to_owned())),
        };
    }

    PowerShellAssignment {
        matched: true,
        program: remaining_command.and_then(|remaining_command| {
            command_program_name_internal(
                remaining_command,
                depth,
                CommandProgramContext::Shell,
                segments_remaining.saturating_sub(1),
            )
        }),
    }
}

struct WindowsShellPayload {
    matched: bool,
    program: Option<String>,
}

fn windows_shell_payload_program_name(
    shell: &str,
    tokens: &[String],
    start: usize,
    depth: usize,
    remaining_command: Option<&str>,
    separator: Option<&str>,
    segments_remaining: usize,
) -> WindowsShellPayload {
    let parse_payload = |payload: Option<&String>| -> Option<String> {
        let payload = payload.filter(|payload| !payload.is_empty())?;
        let command = match (remaining_command, separator) {
            (Some(remaining_command), Some(separator)) => {
                format!("{payload} {separator} {remaining_command}")
            }
            _ => payload.clone(),
        };
        command_program_name_internal(
            &command,
            depth + 1,
            CommandProgramContext::Shell,
            segments_remaining,
        )
    };

    if shell == "cmd" || shell == "cmd.exe" {
        for (index, token) in tokens.iter().enumerate().skip(start) {
            let option = token.to_lowercase();
            if option != "/c" && option != "/k" {
                continue;
            }
            let payload = tokens.get(index + 1);
            return WindowsShellPayload {
                matched: true,
                program: parse_payload(payload),
            };
        }
        return WindowsShellPayload {
            matched: false,
            program: None,
        };
    }

    let mut index = start;
    while index < tokens.len() {
        let option = tokens[index].to_lowercase();
        if option == "-command" || option == "-c" {
            let payload = tokens.get(index + 1);
            return WindowsShellPayload {
                matched: true,
                program: parse_payload(payload),
            };
        }
        if option == "-file" || option == "-f" {
            return WindowsShellPayload {
                matched: true,
                program: static_program_name(tokens.get(index + 1).map_or("", String::as_str)),
            };
        }
        if option == "-encodedcommand" || option == "-enc" || option == "-e" {
            return WindowsShellPayload {
                matched: true,
                program: None,
            };
        }
        if POWERSHELL_OPTIONS_WITH_VALUE.contains(&option.as_str()) {
            index += 2;
            continue;
        }
        if POWERSHELL_FLAGS.contains(&option.as_str()) {
            index += 1;
            continue;
        }
        if !option.starts_with('-') {
            return WindowsShellPayload {
                matched: true,
                program: static_program_name(&tokens[index]),
            };
        }
        index += 1;
    }
    WindowsShellPayload {
        matched: false,
        program: None,
    }
}

fn start_process_program_name(tokens: &[String], start: usize) -> Option<String> {
    let mut index = start;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        let option = token.to_lowercase();
        if option == "-filepath" {
            return static_program_name(tokens.get(index + 1).map_or("", String::as_str));
        }
        if START_PROCESS_FLAGS.contains(&option.as_str()) {
            index += 1;
            continue;
        }
        if START_PROCESS_OPTIONS_WITH_VALUE.contains(&option.as_str()) {
            tokens.get(index + 1)?;
            index += 2;
            continue;
        }
        if token.starts_with('-') {
            return None;
        }
        return static_program_name(token);
    }
    None
}

struct LiteralAssignment {
    name: String,
    program: Option<String>,
}

fn literal_assignment_program(token: &str) -> Option<LiteralAssignment> {
    let (name, value) = split_assignment_word(token)?;
    let name = name.to_owned();
    let mut value = js_trim(value).to_owned();
    if value.is_empty() || value.contains(['$', '`']) {
        return Some(LiteralAssignment {
            name,
            program: None,
        });
    }

    if let Some(array_body) = value
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    {
        let array_tokens = tokenize_shell_command(array_body);
        value = array_tokens
            .and_then(|tokens| tokens.into_iter().next())
            .unwrap_or_default();
    } else if value.contains(is_js_whitespace) && !has_path_separator(&value) {
        return Some(LiteralAssignment {
            name,
            program: None,
        });
    }

    Some(LiteralAssignment {
        name,
        program: static_program_name(&value),
    })
}

fn referenced_command_alias(token: &str) -> Option<String> {
    let reference = REFERENCED_COMMAND_ALIAS.captures(token)?;
    reference
        .get(1)
        .or_else(|| reference.get(2))
        .map(|name| name.as_str().to_owned())
}

// Recover only literal aliases declared by an earlier top-level shell segment.
// This covers common `SSH=(ssh ...)` and `TOOL=/path/to/tool` forms without
// evaluating expansions or trying to model general shell state.
fn literal_command_alias_program_name(command: &str) -> Option<String> {
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut remaining_command: Option<String> = Some(command.to_owned());
    let mut control_flow_depth = 0usize;

    for _ in 0..64 {
        let Some(current_command) = remaining_command.take().filter(|value| !value.is_empty())
        else {
            break;
        };
        let command_without_comments = command_without_leading_shell_comments(&current_command)?;
        let command_split = split_first_shell_command(command_without_comments);
        let tokens = tokenize_shell_command(&without_shell_line_continuations(
            &command_split.first_command,
        ))?;
        let token_at = |index: usize| tokens.get(index).map_or("", String::as_str);

        let mut command_index = if matches!(token_at(0), "do" | "then") {
            1
        } else {
            0
        };
        while command_index < tokens.len() {
            if let Some(index_after_redirection) =
                index_after_shell_redirection(&tokens, command_index)
                && index_after_redirection <= tokens.len()
            {
                command_index = index_after_redirection;
                continue;
            }
            if is_assignment_word(token_at(command_index)) {
                command_index += 1;
                continue;
            }
            break;
        }

        let alias_name = referenced_command_alias(token_at(command_index));
        if let Some(aliased_program) = alias_name.and_then(|name| aliases.get(&name))
            && !aliased_program.is_empty()
        {
            return Some(aliased_program.clone());
        }

        let leading_token = tokens.first().map(String::as_str);
        if matches!(leading_token, Some("fi" | "done" | "esac")) {
            control_flow_depth = control_flow_depth.saturating_sub(1);
        }
        if matches!(
            leading_token,
            Some("if" | "for" | "while" | "until" | "select" | "case")
        ) {
            control_flow_depth += 1;
        }

        if control_flow_depth == 0 && leading_token == Some("unset") {
            for name in tokens.iter().skip(1) {
                aliases.remove(name);
            }
        }

        let assignment_start = if leading_token == Some("export") {
            1
        } else {
            0
        };
        let assignments: Vec<Option<LiteralAssignment>> = tokens
            .iter()
            .skip(assignment_start)
            .map(|token| literal_assignment_program(token))
            .collect();
        if control_flow_depth == 0
            && !assignments.is_empty()
            && assignments.iter().all(Option::is_some)
        {
            for assignment in assignments.into_iter().flatten() {
                match assignment.program {
                    Some(program) => {
                        aliases.insert(assignment.name, program);
                    }
                    None => {
                        aliases.remove(&assignment.name);
                    }
                }
            }
        }

        remaining_command = command_split.remaining_command;
    }

    None
}

fn wrapped_shell_command_program_name(
    wrapper: &str,
    tokens: &[String],
    start: usize,
    depth: usize,
    remaining_command: Option<&str>,
    segments_remaining: usize,
) -> Option<String> {
    let mut index = start;

    if wrapper == "command" {
        while index < tokens.len() {
            let option = tokens[index].as_str();
            if option == "--" {
                index += 1;
                break;
            }
            if !option.starts_with('-') || option == "-" {
                break;
            }
            if option != "-p" {
                return None;
            }
            index += 1;
        }
    } else if wrapper == "builtin" {
        if tokens.get(index).map(String::as_str) == Some("--") {
            index += 1;
        } else if tokens
            .get(index)
            .is_some_and(|token| token.starts_with('-'))
        {
            return None;
        }
    } else if wrapper == "exec" {
        while index < tokens.len() {
            let option = tokens[index].as_str();
            if option == "--" {
                index += 1;
                break;
            }
            if option == "-a" {
                tokens.get(index + 1)?;
                index += 2;
                continue;
            }
            let is_attached_argv0 = option.strip_prefix("-a").is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_some_and(|character| !is_js_line_terminator(character))
            });
            let is_flag_cluster = option
                .strip_prefix('-')
                .is_some_and(|flags| !flags.is_empty() && flags.chars().all(|c| "cl".contains(c)));
            if is_attached_argv0 || is_flag_cluster {
                index += 1;
                continue;
            }
            if option.starts_with('-') && option != "-" {
                return None;
            }
            break;
        }
    }

    let wrapped_tokens = tokens.get(index..).unwrap_or(&[]);
    if wrapped_tokens.is_empty() {
        return None;
    }
    let mut target_index = 0;
    while target_index < wrapped_tokens.len() {
        match index_after_shell_redirection(wrapped_tokens, target_index) {
            Some(index_after_redirection) if index_after_redirection <= wrapped_tokens.len() => {
                target_index = index_after_redirection;
            }
            _ => break,
        }
    }
    let target = wrapped_tokens
        .get(target_index)
        .map(String::as_str)
        .filter(|target| !target.is_empty());
    if target.is_some_and(is_assignment_word) {
        return None;
    }

    let wrapped_program = command_program_name_internal(
        &serialize_shell_tokens(wrapped_tokens),
        depth + 1,
        if wrapper == "exec" {
            CommandProgramContext::Exec
        } else {
            CommandProgramContext::Shell
        },
        segments_remaining,
    );
    if wrapped_program.is_some() {
        return wrapped_program;
    }

    if wrapper != "exec"
        && let Some(target) = target
        && target == target.to_lowercase()
        && NON_DESCRIPTIVE_SHELL_PROGRAMS.contains(&target)
        && !TERMINAL_SHELL_PROGRAMS.contains(&target)
        && let Some(remaining_command) = remaining_command
    {
        return command_program_name_internal(
            remaining_command,
            depth,
            CommandProgramContext::Shell,
            segments_remaining.saturating_sub(1),
        );
    }
    None
}

/// `/^-[A-Za-z].+/.test(token) && !token.startsWith("--")`.
fn is_short_option_cluster(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next() == Some('-')
        && chars
            .next()
            .is_some_and(|option| option.is_ascii_alphabetic())
        && chars
            .next()
            .is_some_and(|rest| !is_js_line_terminator(rest))
        && !token.starts_with("--")
}

/// `/^@["'](?:\r?\n)/`.
fn starts_power_shell_here_string(command: &str) -> bool {
    command
        .strip_prefix(['@'])
        .and_then(|rest| rest.strip_prefix(['"', '\'']))
        .is_some_and(|rest| rest.starts_with('\n') || rest.starts_with("\r\n"))
}

fn parse_command_program_name(
    command: &str,
    depth: usize,
    context: CommandProgramContext,
    segments_remaining: usize,
) -> Option<String> {
    if depth >= 8 || segments_remaining == 0 {
        return None;
    }
    let command_without_comments = command_without_leading_shell_comments(command)?;
    if POWERSHELL_BLOCK.is_match(command_without_comments) {
        return None;
    }
    if SHELL_FUNCTION_DEFINITION.is_match(command_without_comments) {
        return None;
    }
    // `&&` and `||` inside a `[[ ... ]]` expression are not top-level command
    // separators. Keep the label conservative instead of scanning the test body.
    if command_without_comments.starts_with("[[") {
        return None;
    }
    let command_split = split_first_shell_command(command_without_comments);
    let remaining_command = command_split.remaining_command.as_deref();
    let parse_remaining = |context: CommandProgramContext| {
        remaining_command.and_then(|remaining_command| {
            command_program_name_internal(
                remaining_command,
                depth,
                context,
                segments_remaining.saturating_sub(1),
            )
        })
    };
    if starts_power_shell_here_string(js_trim_start(&command_split.first_command)) {
        return parse_remaining(CommandProgramContext::Shell);
    }
    let power_shell_assignment = power_shell_assignment_program_name(
        &command_split.first_command,
        depth,
        remaining_command,
        segments_remaining,
    );
    if power_shell_assignment.matched {
        return power_shell_assignment.program;
    }
    if let Some(windows_path) = WINDOWS_PATH_COMMAND
        .captures(&command_split.first_command)
        .and_then(|captures| captures.get(1))
    {
        return static_program_name(windows_path.as_str());
    }
    let tokens = tokenize_shell_command(&without_shell_line_continuations(
        &command_split.first_command,
    ))?;
    let first_character = js_trim_start(&command_split.first_command).chars().next();
    if tokens.len() == 1
        && matches!(first_character, Some('"' | '\''))
        && tokens[0].contains(|character: char| {
            is_js_whitespace(character) || matches!(character, '(' | ')' | '=')
        })
        && !has_path_separator(&tokens[0])
    {
        return parse_remaining(context);
    }
    let mut index = 0;
    let mut wrapper: Option<CommandWrapper> = None;
    let mut execution_context = context;
    let mut saw_assignment = false;
    let mut saw_redirection = false;

    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token.is_empty() {
            return None;
        }
        if let Some(index_after_redirection) = index_after_shell_redirection(&tokens, index) {
            if index_after_redirection > tokens.len() {
                return None;
            }
            saw_redirection = true;
            index = index_after_redirection;
            continue;
        }
        if is_assignment_word(token) {
            saw_assignment = true;
            index += 1;
            continue;
        }
        if execution_context == CommandProgramContext::Shell && token == ":" {
            return parse_remaining(execution_context);
        }
        if token
            .chars()
            .next()
            .is_some_and(|first| NON_PROGRAM_PREFIX_CHARACTERS.contains(first))
            && !(token.starts_with('$') && token.contains('/'))
        {
            return if execution_context == CommandProgramContext::Shell && token.starts_with('[') {
                parse_remaining(execution_context)
            } else {
                None
            };
        }
        let token_program = last_path_segment(token);
        let is_unqualified_token = token == token_program;
        if token_program == "env" || token_program == "sudo" {
            wrapper = Some(if token_program == "env" {
                CommandWrapper::Env
            } else {
                CommandWrapper::Sudo
            });
            execution_context = CommandProgramContext::Exec;
            index += 1;
            continue;
        }
        if wrapper.is_some() && token == "--" {
            wrapper = None;
            index += 1;
            continue;
        }
        if let Some(active_wrapper) = wrapper
            && token.starts_with('-')
        {
            if active_wrapper == CommandWrapper::Env && (token == "-S" || token == "--split-string")
            {
                let split_command = tokens.get(index + 1).filter(|value| !value.is_empty());
                return split_command.and_then(|split_command| {
                    command_program_name_internal(
                        split_command,
                        depth + 1,
                        execution_context,
                        segments_remaining,
                    )
                });
            }
            if active_wrapper == CommandWrapper::Env
                && let Some(split_command) = token.strip_prefix("--split-string=")
            {
                return command_program_name_internal(
                    split_command,
                    depth + 1,
                    execution_context,
                    segments_remaining,
                );
            }
            let options_with_value = command_wrapper_options_with_value(active_wrapper);
            let flags = command_wrapper_flags(active_wrapper);
            if options_with_value.contains(&token) {
                tokens.get(index + 1)?;
                index += 2;
                continue;
            }
            if flags.contains(&token) {
                index += 1;
                continue;
            }
            if let Some(equals_index) = token.find('=')
                && token.starts_with("--")
                && equals_index > 2
            {
                if !options_with_value.contains(&&token[..equals_index]) {
                    return None;
                }
                index += 1;
                continue;
            }
            if is_short_option_cluster(token) {
                let mut consumes_next_token = false;
                let token_length = token.chars().count();
                for (option_index, option) in token.chars().skip(1).enumerate() {
                    let short_option = format!("-{option}");
                    if options_with_value.contains(&short_option.as_str()) {
                        consumes_next_token = option_index == token_length - 2;
                        break;
                    }
                    if !flags.contains(&short_option.as_str()) {
                        return None;
                    }
                }
                if consumes_next_token && tokens.get(index + 1).is_none() {
                    return None;
                }
                index += if consumes_next_token { 2 } else { 1 };
                continue;
            }
            return None;
        }
        if SHELL_PROGRAMS.contains(
            &strip_exe_suffix_ignore_case(token_program)
                .to_lowercase()
                .as_str(),
        ) && let Some(script_index) = shell_command_argument_index(&tokens, index + 1)
        {
            let script = tokens.get(script_index).filter(|script| !script.is_empty());
            return script.and_then(|script| {
                command_program_name_internal(
                    script,
                    depth + 1,
                    CommandProgramContext::Shell,
                    segments_remaining,
                )
            });
        }
        let lower_token_program = token_program.to_lowercase();
        if WINDOWS_SHELL_PROGRAMS.contains(&lower_token_program.as_str()) {
            let payload = windows_shell_payload_program_name(
                &lower_token_program,
                &tokens,
                index + 1,
                depth,
                remaining_command,
                command_split.separator.as_deref(),
                segments_remaining,
            );
            if payload.matched {
                return payload.program;
            }
        }
        if lower_token_program == "start-process"
            && let Some(started_program) = start_process_program_name(&tokens, index + 1)
        {
            return Some(started_program);
        }
        if execution_context == CommandProgramContext::Shell
            && is_unqualified_token
            && SHELL_PRECOMMAND_MODIFIERS.contains(&token_program)
        {
            index += 1;
            if token_program == "time" && tokens.get(index).map(String::as_str) == Some("-p") {
                index += 1;
            }
            if tokens.get(index).is_some_and(|next| next.starts_with('-')) {
                return None;
            }
            continue;
        }
        if is_unqualified_token
            && !token_program.is_empty()
            && let Some(target_index) =
                transparent_wrapper_command_index(token_program, &tokens, index)
        {
            let wrapped_program = command_program_name_internal(
                &serialize_shell_tokens(tokens.get(target_index..).unwrap_or(&[])),
                depth + 1,
                CommandProgramContext::Exec,
                segments_remaining,
            );
            if wrapped_program.is_some() {
                return wrapped_program;
            }
        }
        if is_unqualified_token && SHELL_COMMAND_WRAPPERS.contains(&token_program) {
            return wrapped_shell_command_program_name(
                token_program,
                &tokens,
                index + 1,
                depth,
                remaining_command,
                segments_remaining,
            );
        }
        if (execution_context == CommandProgramContext::Shell
            || (wrapper == Some(CommandWrapper::Sudo)
                && SKIPPABLE_SUDO_PROBES.contains(&token_program)))
            && is_unqualified_token
            && NON_DESCRIPTIVE_SHELL_PROGRAMS.contains(&token_program)
            && (!TERMINAL_SHELL_PROGRAMS.contains(&token_program)
                || (token_program == "false" && command_split.separator.as_deref() != Some("&&")))
            && remaining_command.is_some()
        {
            return parse_remaining(CommandProgramContext::Shell);
        }
        if execution_context == CommandProgramContext::Shell
            && is_unqualified_token
            && POWERSHELL_SETUP_PROGRAMS.contains(&lower_token_program.as_str())
            && remaining_command.is_some()
        {
            return parse_remaining(CommandProgramContext::Shell);
        }
        if token_program.is_empty()
            || (is_unqualified_token && NON_DESCRIPTIVE_SHELL_PROGRAMS.contains(&token_program))
            || token_program
                .chars()
                .next()
                .is_some_and(|first| NON_PROGRAM_PREFIX_CHARACTERS.contains(first))
            || token_program
                .chars()
                .next_back()
                .is_some_and(|last| NON_PROGRAM_SUFFIX_CHARACTERS.contains(last))
            || token_program.ends_with("()")
            || COMPACT_FUNCTION_DEFINITION.is_match(token_program)
        {
            return None;
        }
        return Some(token_program.to_owned());
    }

    if (saw_assignment || saw_redirection) && wrapper.is_none() && remaining_command.is_some() {
        return parse_remaining(execution_context);
    }
    None
}

fn command_program_name_internal(
    command: &str,
    depth: usize,
    context: CommandProgramContext,
    segments_remaining: usize,
) -> Option<String> {
    if segments_remaining == 0 {
        return None;
    }
    if let Some(called_program) = power_shell_call_operator_program_name(command) {
        return called_program;
    }
    parse_command_program_name(command, depth, context, segments_remaining).or_else(|| {
        if context == CommandProgramContext::Shell {
            literal_command_alias_program_name(command)
        } else {
            None
        }
    })
}

/// The executable a command runs, for labels such as "Ran git". `None` when
/// the command is shell syntax, a lookup, or otherwise has no useful program.
pub fn command_program_name(command: &str) -> Option<String> {
    command_program_name_internal(
        command,
        0,
        CommandProgramContext::Shell,
        MAX_COMMAND_SEGMENTS,
    )
}

/// The command as rows show it: a plain shell -c wrapper removed and control
/// characters made visible. Callers retain the original for details.
pub fn command_display_text(command: &str) -> String {
    with_visible_control_characters(&without_shell_wrapper(command))
}

// Escape bytes and other control characters render as invisible gaps; display
// shows them as Unicode control pictures (ESC becomes ␛) instead. Tab, newline
// and carriage return lay out as whitespace, so CRLF scripts stay unmarked.
fn is_hidden_control_character(character: char) -> bool {
    matches!(
        character,
        '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{007F}'
    )
}

/// Replaces each control character with its control picture; the length does not change.
pub fn with_visible_control_characters(text: &str) -> String {
    text.chars()
        .map(|character| {
            if !is_hidden_control_character(character) {
                character
            } else if character == '\u{007F}' {
                '␡'
            } else {
                char::from_u32(0x2400 + u32::from(character)).unwrap_or(character)
            }
        })
        .collect()
}

// A wrapper whose quoting does not balance is kept: the script's end cannot be
// known, which happens when Codex's secret redaction removes the backslash of
// an escaped quote inside `-lc "…"`.
fn without_shell_wrapper(command: &str) -> String {
    let trimmed = js_trim(command);
    let split = split_first_shell_command(trimmed);
    if split.remaining_command.is_some() {
        return trimmed.to_owned();
    }
    let Some(tokens) = tokenize_shell_command(trimmed) else {
        return trimmed.to_owned();
    };
    let program = tokens
        .first()
        .map(|token| strip_exe_suffix_ignore_case(last_path_segment(token)));
    if !program.is_some_and(|program| !program.is_empty() && SHELL_PROGRAMS.contains(&program)) {
        return trimmed.to_owned();
    }
    let script_index = shell_command_argument_index(&tokens, 1);
    // Positional arguments can affect the script; keep those invocations intact.
    let Some(script_index) = script_index.filter(|&index| index + 1 == tokens.len()) else {
        return trimmed.to_owned();
    };
    let script = tokens
        .get(script_index)
        .map_or("", |script| js_trim(script));
    if script.is_empty() {
        trimmed.to_owned()
    } else {
        script.to_owned()
    }
}

const POWERSHELL_PROGRAMS: &[&str] = &["powershell", "pwsh"];

// PowerShell's approved verbs (`Get-Verb`), plus the ForEach, Where, Sort and
// Tee of its built-in *-Object cmdlets. Cmdlets match only capitalized, as
// agents write them: lowercase `update-grub` or `install-info` are Linux tools.
const POWERSHELL_VERBS: &str = "Add Approve Assert Backup Block Build Checkpoint Clear Close Compare Complete Compress Confirm Connect Convert ConvertFrom ConvertTo Copy Debug Deny Deploy Disable Disconnect Dismount Edit Enable Enter Exit Expand Export Find ForEach Format Get Grant Group Hide Import Initialize Install Invoke Join Limit Lock Measure Merge Mount Move New Open Optimize Out Ping Pop Protect Publish Push Read Receive Redo Register Remove Rename Repair Request Reset Resize Resolve Restart Restore Resume Revoke Save Search Select Send Set Show Skip Sort Split Start Step Stop Submit Suspend Switch Sync Tee Test Trace Unblock Undo Uninstall Unlock Unprotect Unpublish Unregister Update Use Wait Watch Where Write";

fn is_power_shell_cmdlet(word: &str) -> bool {
    POWERSHELL_CMDLET
        .captures(word)
        .and_then(|cmdlet| cmdlet.get(1))
        .is_some_and(|verb| {
            POWERSHELL_VERBS
                .split(' ')
                .any(|known| known == verb.as_str())
        })
}

/// The grammar a command is highlighted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandHighlightLanguage {
    PowerShell,
    ShellScript,
}

impl CommandHighlightLanguage {
    /// Upstream's grammar name (`"powershell"` / `"shellscript"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PowerShell => "powershell",
            Self::ShellScript => "shellscript",
        }
    }
}

/// The grammar to highlight a command with: PowerShell when pwsh or powershell
/// runs it, or when it is written in PowerShell, as Windows agents run it.
pub fn command_highlight_language(command: &str) -> CommandHighlightLanguage {
    let program = HIGHLIGHT_PROGRAM.captures(js_trim(command));
    let program_name = program
        .as_ref()
        .and_then(|program| {
            program
                .get(1)
                .or_else(|| program.get(2))
                .or_else(|| program.get(3))
        })
        .map_or("", |name| name.as_str());
    let name = last_path_segment(program_name).to_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if !name.is_empty() && POWERSHELL_PROGRAMS.contains(&name) {
        return CommandHighlightLanguage::PowerShell;
    }
    if is_power_shell_script(command) {
        CommandHighlightLanguage::PowerShell
    } else {
        CommandHighlightLanguage::ShellScript
    }
}

/// Whether any statement starts like PowerShell and never like POSIX shell.
fn is_power_shell_script(command: &str) -> bool {
    let trimmed = js_trim(command);
    // A leading call operator; POSIX shell cannot start a command with `&`.
    if trimmed
        .strip_prefix('&')
        .is_some_and(|operand| !js_trim_start(operand).is_empty())
    {
        return true;
    }
    let mut rest: Option<String> = Some(trimmed.to_owned());
    for _ in 0..MAX_COMMAND_SEGMENTS {
        let Some(current) = rest.take().filter(|value| !value.is_empty()) else {
            break;
        };
        let ShellCommandSplit {
            first_command,
            remaining_command,
            ..
        } = split_first_shell_command(&current);
        let statement = js_trim(&first_command);
        // `$env:NAME` or `$name = value`; neither parses as POSIX shell.
        if POWERSHELL_SCOPED_VARIABLE.is_match(statement)
            || POWERSHELL_VARIABLE_ASSIGNMENT.is_match(statement)
            || is_power_shell_cmdlet(statement.split(is_js_whitespace).next().unwrap_or(""))
        {
            return true;
        }
        rest = remaining_command;
    }
    false
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn assert_programs(cases: &[(&str, &str)]) {
        for &(command, program) in cases {
            assert_eq!(
                command_program_name(command).as_deref(),
                Some(program),
                "command: {command:?}"
            );
        }
    }

    fn assert_no_programs(commands: &[&str]) {
        for &command in commands {
            assert_eq!(command_program_name(command), None, "command: {command:?}");
        }
    }

    fn assert_display_texts(cases: &[(&str, &str)]) {
        for &(input, expected) in cases {
            assert_eq!(command_display_text(input), expected, "input: {input:?}");
        }
    }

    // describe("commandProgramName")

    #[test]
    fn unwraps_shell_scripts_without_executing_them() {
        assert_programs(&[
            (
                "/bin/zsh -lc 'vp test run apps/web/src/session-logic.test.ts'",
                "vp",
            ),
            ("/bin/zsh -lc 'git diff --check'", "git"),
            (
                "/bin/zsh -lc \"npx -y react-doctor@latest apps/web\"",
                "npx",
            ),
            (
                "/bin/zsh -lc 'rg -n \"registerHooks|worker\" apps/web/src'",
                "rg",
            ),
            (
                "/bin/bash --noprofile --norc -l -c 'sed -n 1,270p file.ts'",
                "sed",
            ),
            ("/bin/bash -o pipefail -lc 'vp test run'", "vp"),
            ("/bin/bash --rcfile /tmp/config -c 'git status'", "git"),
            ("sh -ec 'node scripts/check.js'", "node"),
            ("fish --command 'rg --files'", "rg"),
            ("zsh -lc 'CI=1 env -u DEBUG sudo -u root vp test run'", "vp"),
            (
                "env CI=1 /bin/zsh -lc '\"/Applications/My Tools/bin/check\" --verbose'",
                "check",
            ),
            ("bash -lc \"zsh -c 'git status'\"", "git"),
            (
                "\"C:\\Program Files\\Git\\bin\\bash.exe\" -lc \"git status\"",
                "git",
            ),
            (
                "/bin/zsh -lc 'git status\nsed -n '\"'1,20p' apps/web/src/components/DiffPanel.tsx\"",
                "git",
            ),
        ]);
    }

    #[test]
    fn preserves_ordinary_programs_and_actual_shell_launches() {
        assert_programs(&[
            ("vp test run", "vp"),
            ("sudo -u root pnpm test", "pnpm"),
            ("env --split-string='CI=1 node scripts/check.js'", "node"),
            (
                "\"C:\\Program Files\\nodejs\\node.exe\" script.js",
                "node.exe",
            ),
            ("/bin/zsh", "zsh"),
            ("/bin/bash -l", "bash"),
            ("zsh script.sh -c 'git status'", "zsh"),
            ("bash -- -c 'git status'", "bash"),
            ("bash --rcfile config.sh", "bash"),
            ("my-shell -c 'git status'", "my-shell"),
            ("$HOME/.bun/bin/bun test", "bun"),
            ("\"$ANDROID_HOME/emulator/emulator\" -list-avds", "emulator"),
            ("${ROOT}/bin/tool --version", "tool"),
        ]);
    }

    #[test]
    fn falls_back_for_missing_or_malformed_scripts() {
        assert_no_programs(&[
            "",
            "zsh -lc",
            "zsh -lc ''",
            "zsh -lc 'git status",
            "zsh -lc 'env'",
            "zsh -lc \"git \\\"",
        ]);
    }

    #[test]
    fn falls_back_for_shell_syntax_and_internal_control_commands() {
        assert_no_programs(&[
            "if test -f package.json; then vp test; fi",
            "[ -f package.json ]",
            "[[ -f package.json ]]",
            "test -f package.json",
            "for file in *; do echo \"$file\"; done",
            "while true; do sleep 1; done",
            "until false; do sleep 1; done",
            "case $name in test) vp test;; esac",
            "select item in one two; do echo \"$item\"; done",
            "function check() { vp test; }",
            "check() { vp test; }",
            "k(){ echo ok; }; k",
            "{ vp test; }",
            "(vp test)",
            "(( count += 1 ))",
            "! vp test",
            ":",
            ". ./script.sh",
            "source ./script.sh",
            "eval 'vp test'",
            "cd packages/client-runtime",
            "export NODE_ENV=test",
            "local name=value",
            "set -e",
            "alias ll='ls -la'",
            "repeat 3 echo ok",
            "and vp test",
            "return 1",
            "break",
            "continue",
            "true",
            "false",
        ]);
    }

    #[test]
    fn uses_the_first_executable_looking_program() {
        assert_programs(&[
            ("rg -n \"if|for|while\" src", "rg"),
            ("printf '%s\\n' 'a;b|c'", "printf"),
            ("node -e \"if (true) console.log('ok')\"", "node"),
            ("echo '$(git status)'", "echo"),
            ("vp test && git status", "vp"),
            ("vp test || git status", "vp"),
            ("rg needle src | head", "rg"),
            ("vp test; git status", "vp"),
            ("vp test &", "vp"),
            ("vp test\ngit status", "vp"),
            ("echo \"$(git status)\"", "echo"),
            ("echo `git status`", "echo"),
            ("cat <(rg needle src)", "cat"),
        ]);
    }

    #[test]
    fn skips_leading_cd_commands_and_uses_the_next_useful_program() {
        assert_programs(&[
            ("cd packages/client-runtime && vp test run", "vp"),
            ("cd \"a path with spaces\"; git status", "git"),
            ("cd apps/web\npnpm test", "pnpm"),
            ("cd first && cd second && bun test", "bun"),
            ("CI=1 cd apps/web && npm test", "npm"),
            ("PATH+=:/tools npm test", "npm"),
            ("PATH+=:/tools && npm test", "npm"),
            ("TMP=$(mktemp -d); cd \"$TMP\"; npm pack ./package", "npm"),
            ("cd $(find . -type d | head -1) && git status", "git"),
            ("cd `find . -type d | head -1` && node script.js", "node"),
            ("cd /tmp 2>&1 && npm test", "npm"),
            ("cd /tmp 2<&0 && pnpm test", "pnpm"),
            ("cd /tmp &>/dev/null && bun test", "bun"),
            ("cd work |& npm test", "npm"),
            ("cd /tmp && # use the selected workspace\nnpm test", "npm"),
            (
                "export CI=1; # first note\n# second note\npnpm test",
                "pnpm",
            ),
            ("cd&&npm test", "npm"),
            ("export CI=1;pnpm test", "pnpm"),
            ("cd ${ROOT:-path;with;semicolons} && bun test", "bun"),
            ("cd ${ROOT:-path&&fallback} && node app.js", "node"),
            ("cd @(first|second) && npm test", "npm"),
            ("cd /tmp \\\n&& npm test", "npm"),
            ("/bin/zsh -lc 'cd apps/web && vp test run'", "vp"),
        ]);
    }

    #[test]
    fn skips_shell_setup_commands_and_uses_the_next_useful_program() {
        assert_programs(&[
            ("source ~/.nvm/nvm.sh && nvm use", "nvm"),
            (". ./.env && pnpm test", "pnpm"),
            ("export CI=1 && vp test run", "vp"),
            ("unset DEBUG; node app.js", "node"),
            ("export CI=1 && cd apps/web && pnpm test", "pnpm"),
            ("/bin/zsh -lc 'source ~/.nvm/nvm.sh && nvm use'", "nvm"),
        ]);
    }

    #[test]
    fn skips_non_descriptive_shell_commands_before_a_useful_program() {
        assert_programs(&[
            ("set -eu; npm test", "npm"),
            (": && npm test", "npm"),
            ("true && npm test", "npm"),
            ("false || npm test", "npm"),
            ("false; npm test", "npm"),
            ("sudo -n true && npm test", "npm"),
            ("sudo -n true; echo checked", "echo"),
            ("test -d node_modules || vp i", "vp"),
            ("[ -d node_modules ] || vp i", "vp"),
        ]);
    }

    #[test]
    fn handles_shell_setup_followed_by_every_command_separator() {
        for setup in [
            "cd /tmp",
            "export CI=1",
            "unset DEBUG",
            "source env.sh",
            ". env.sh",
        ] {
            for operator in ["&&", " || ", ";", "\n", "|", " |& ", " & "] {
                let command = format!("{setup}{operator}npm test");
                assert_eq!(
                    command_program_name(&command).as_deref(),
                    Some("npm"),
                    "command: {command:?}"
                );
            }
        }
    }

    #[test]
    fn unwraps_shell_command_wrappers() {
        assert_programs(&[
            ("command git status", "git"),
            ("command -p git status", "git"),
            ("command -- git status", "git"),
            ("builtin printf ok", "printf"),
            ("builtin -- echo ok", "echo"),
            ("command cd /tmp && npm test", "npm"),
            ("builtin cd /tmp && pnpm test", "pnpm"),
            ("exec node app.js", "node"),
            ("exec -cl -a worker node app.js", "node"),
            ("exec env CI=1 /opt/tools/check --verbose", "check"),
            (
                "exec \"C:\\Program Files\\nodejs\\node.exe\" app.js",
                "node.exe",
            ),
            ("exec sh -c 'cd /tmp && npm test'", "npm"),
            ("exec sh -c 'cd /tmp\nnpm test'", "npm"),
            ("exec bash -c 'set -e\nnpm test'", "npm"),
        ]);
    }

    #[test]
    fn unwraps_process_launch_wrappers() {
        assert_programs(&[
            ("timeout 10 pnpm test", "pnpm"),
            ("timeout 10s python3 script.py", "python3"),
            ("gtimeout 1.5 node app.js", "node"),
            ("nohup npx expo start >/tmp/metro.log 2>&1 &", "npx"),
            ("nohup -- env CI=1 bun test", "bun"),
            ("arch -x86_64 ./build/app-under-test", "app-under-test"),
            ("arch -arch arm64 /opt/tools/check", "check"),
            ("bundle exec pod install", "pod"),
            ("timeout 30 nohup env CI=1 node app.js", "node"),
            (
                "timeout 60 script -q /dev/null env CI=1 node app.js",
                "node",
            ),
        ]);
    }

    #[test]
    fn resolves_literal_power_shell_call_operators() {
        assert_programs(&[
            (
                "& 'C:\\Program Files\\nodejs\\node.exe' script.js",
                "node.exe",
            ),
            (
                "& \"$env:WINDIR\\Microsoft.NET\\Framework64\\v4\\csc.exe\" file.cs",
                "csc.exe",
            ),
        ]);
    }

    #[test]
    fn labels_commands_inside_simple_power_shell_assignments() {
        assert_programs(&[
            ("$env:CI='1'; npm test", "npm"),
            ("$value = 'configured'; node app.js", "node"),
            ("$process = Get-Process node; $process.Id", "Get-Process"),
            (
                "$tmp = Join-Path $env:TEMP repo; git clone example",
                "Join-Path",
            ),
            (
                "$html = (Invoke-WebRequest https://example.com).Content",
                "Invoke-WebRequest",
            ),
            (
                "$process = Start-Process -FilePath .\\app.exe -PassThru; $process.Id",
                "app.exe",
            ),
        ]);
    }

    #[test]
    fn unwraps_windows_shell_launchers() {
        assert_programs(&[
            ("cmd /c bcdedit /enum", "bcdedit"),
            ("cmd /c cd C:\\work && npm test", "npm"),
            ("cmd.exe /d /s /c \"cd C:\\work && npm test\"", "npm"),
            (
                "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"C:\\work\\scripts\\doctor.ps1\"",
                "doctor.ps1",
            ),
            (
                "pwsh -NoProfile -Command \"Set-Location C:\\work; pnpm test\"",
                "pnpm",
            ),
            ("pwsh -Command Set-Location C:\\work; pnpm test", "pnpm"),
        ]);
    }

    #[test]
    fn handles_common_power_shell_setup_and_launch_commands() {
        assert_programs(&[
            ("Set-Location C:\\work; npm test", "npm"),
            ("Push-Location C:\\work; node app.js", "node"),
            (
                "Start-Process -FilePath node -ArgumentList server.js",
                "node",
            ),
            (
                "Start-Process -ArgumentList '-FilePath helper' node",
                "node",
            ),
            ("Start-Process -ErrorAction Stop -FilePath node", "node"),
            (
                "Start-Process -NoNewWindow -WorkingDirectory C:\\work node",
                "node",
            ),
            (
                "Start-Process \"C:\\Program Files\\Example\\app.exe\"",
                "app.exe",
            ),
            (
                "Start-Process -FilePath \".\\dist\\Example App.exe\" -PassThru",
                "Example App.exe",
            ),
            (".\\.venv\\Scripts\\python.exe script.py", "python.exe"),
            (
                "$env:LOCALAPPDATA\\Programs\\tool.exe --version",
                "tool.exe",
            ),
            (
                "\"=== CHECK FILE ===\"; Get-Content file.txt",
                "Get-Content",
            ),
            ("$x = @'\ndata'; Get-Fake\n'@\nGet-Process", "Get-Process"),
            ("@'\nprint('ok; still data')\n'@ | python -", "python"),
        ]);
    }

    #[test]
    fn keeps_process_launch_wrappers_when_no_safe_payload_is_present() {
        assert_programs(&[
            ("timeout --help", "timeout"),
            ("nohup --version", "nohup"),
            ("arch", "arch"),
            ("bundle install", "bundle"),
            ("script output.log", "script"),
            ("/usr/bin/timeout 10 node app.js", "timeout"),
        ]);
    }

    #[test]
    fn does_not_treat_shell_lookup_and_commandless_wrapper_forms_as_executions() {
        assert_no_programs(&[
            "command -v git",
            "command -V git",
            "command -a git",
            "command -pv git",
            "builtin -p",
            "exec",
            "exec > output.log",
            "exec --",
            "exec cd /tmp && npm test",
            "exec env CI=1 cd /tmp && npm test",
            "exec CI=1 npm test",
            "env CI=1 cd /tmp && npm test",
            "env CI=1; npm test",
            "sudo cd /tmp && npm test",
            "command CI=1 npm test",
            "command false && npm test",
            "cmd /c false && npm test",
            "cd /tmp <<EOF\nunterminated heredoc",
            "cd /tmp && >/tmp/log",
            "(xcrun simctl io booted recordVideo /tmp/video.mp4 &) ; wait",
            "export CI=1 && (bundle exec pod install || pod install)",
            "$PY scripts/check.py",
            "${TOOL} --version",
            "%TOOL% --version",
            "!TOOL! --version",
            "& $tool --version",
            "& { Get-Process }",
            "$value = 'configured'",
            "$headers = @{ 'Accept' = 'application/json'; 'Content-Type' = 'application/json' }; Invoke-WebRequest https://example.com",
            "\"sha256(value)=$hash\"",
            "broken{",
            "@echo off",
            ":: comment",
            "time -- npm test",
            "time -v npm test",
            "coproc npm test",
            "coproc worker { npm test; }",
            "cd [first|second] && pnpm test",
            "npm) --version",
            "try { Invoke-WebRequest https://example.com } catch { Write-Error $_ }",
            "for($i=0; $i -lt 2; $i++){ Start-Sleep 1 }",
            "# comment only",
        ]);
    }

    #[test]
    fn does_not_hide_legitimate_or_case_distinct_program_names() {
        assert_programs(&[
            ("parallel -j4", "parallel"),
            ("hash --help", "hash"),
            ("process --help", "process"),
            ("rem comment", "rem"),
            ("Exec node app.js", "Exec"),
            ("CD /tmp && npm test", "CD"),
        ]);
    }

    #[test]
    fn parses_shell_setup_inside_an_explicitly_launched_shell() {
        assert_programs(&[
            ("env sh -c 'cd /tmp && npm test'", "npm"),
            ("sudo zsh -lc 'export CI=1 && pnpm test'", "pnpm"),
        ]);
    }

    #[test]
    fn skips_shell_precommand_modifiers() {
        assert_programs(&[
            ("nocorrect pnpm test", "pnpm"),
            ("noglob bun test", "bun"),
            ("time node app.js", "node"),
            ("time -p deno test", "deno"),
            ("time nocorrect npm test", "npm"),
        ]);
    }

    #[test]
    fn uses_the_command_after_leading_shell_comments() {
        assert_eq!(
            command_program_name("# first comment\n  # second comment\ngit status").as_deref(),
            Some("git")
        );
    }

    #[test]
    fn skips_comments_after_commandless_shell_setup() {
        assert_programs(&[
            ("CI=1 # note\nnpm test", "npm"),
            ("CI=1 # it's configured\nnpm test", "npm"),
            ("CI=1 # \"unterminated quote\nbun test", "bun"),
            (">/tmp/log # note\npnpm test", "pnpm"),
            (">/tmp/log # it's configured\ndeno test", "deno"),
        ]);
    }

    #[test]
    fn does_not_treat_qualified_paths_as_shell_syntax() {
        assert_programs(&[
            ("./cd /tmp && npm test", "cd"),
            ("/opt/exec node app.js", "exec"),
            ("/usr/bin/time npm test", "time"),
            ("/usr/bin/test -f package.json", "test"),
        ]);
    }

    #[test]
    fn skips_a_shell_array_assignment_before_the_command() {
        assert_eq!(
            command_program_name("items=(one two); npm test").as_deref(),
            Some("npm")
        );
    }

    #[test]
    fn resolves_literal_command_aliases_from_earlier_shell_segments() {
        assert_programs(&[
            (
                "EMU=\"/opt/android/emulator\"; \"$EMU\" -list-avds",
                "emulator",
            ),
            (
                "AAPT=/opt/android/aapt2\n$AAPT dump badging app.apk",
                "aapt2",
            ),
            (
                "SSH=(ssh -i /tmp/key -o IdentitiesOnly=yes); HOST=user@example; \"${SSH[@]}\" \"$HOST\" uptime",
                "ssh",
            ),
            (
                "SCP=(/usr/bin/scp -i /tmp/key); if true; then ${SCP[@]} file user@example:/tmp; fi",
                "scp",
            ),
            (
                "TOOL=\"/Applications/My Tool/bin/check\"; \"$TOOL\" --verbose",
                "check",
            ),
        ]);
    }

    #[test]
    fn does_not_evaluate_dynamic_or_non_persistent_command_aliases() {
        assert_no_programs(&[
            "TOOL=$(pick-command); \"$TOOL\" --version",
            "TOOL=\"git status\"; \"$TOOL\"",
            "# TOOL=git\n$TOOL status",
            "TOOL=git true; \"$TOOL\" status",
            "TOOL=git; TOOL=$(pick-command); \"$TOOL\" status",
            "TOOL=git; unset TOOL; \"$TOOL\" status",
        ]);
    }

    #[test]
    fn does_not_retain_aliases_assigned_inside_control_flow() {
        assert_eq!(
            command_program_name("TOOL=git; if false; then\nTOOL=npm\nfi\n\"$TOOL\" status")
                .as_deref(),
            Some("git")
        );
    }

    #[test]
    fn keeps_expansions_inside_assignment_words() {
        assert_programs(&[
            ("ROOT=${BASE:-path with spaces}; npm test", "npm"),
            ("ROOT=`printf 'path with spaces'`; pnpm test", "pnpm"),
        ]);
    }

    #[test]
    fn skips_redirections_before_the_next_command() {
        assert_programs(&[
            (">/tmp/log && npm test", "npm"),
            ("2>/tmp/error.log; pnpm test", "pnpm"),
            ("cd /tmp && >/tmp/log npm test", "npm"),
            ("cd /tmp && > /tmp/log pnpm test", "pnpm"),
            ("cd /tmp && 2>&1 bun test", "bun"),
            ("cd /tmp && 2>& 1 node app.js", "node"),
            ("cd /tmp && &>/tmp/log git status", "git"),
            ("cd /tmp && *>>/tmp/log vp test", "vp"),
            ("cd /tmp && {output}>/tmp/log deno test", "deno"),
            ("cd /tmp && <<<input ruby script.rb", "ruby"),
        ]);
    }

    #[test]
    fn skips_heredoc_bodies_before_finding_the_next_command() {
        assert_programs(&[
            ("cd /tmp <<EOF\nnot-a-command\nEOF\nnpm test", "npm"),
            ("cd /tmp <<'EOF'\nnot-a-command\nEOF\npnpm test", "pnpm"),
            ("cd /tmp <<'EOF'\nnot-a-command\\\nEOF\npnpm test", "pnpm"),
            ("cd /tmp <<-EOF\n\tnot-a-command\n\tEOF\nbun test", "bun"),
            ("cd /tmp <<A <<B\none\nA\ntwo\nB\ngit status", "git"),
            (
                "cd /tmp <<EOF; # setup\nnot-a-command\nEOF\nnpm test",
                "npm",
            ),
            (
                "cd /tmp <<EOF && pnpm test\nnot-a-command\nEOF\nbun test",
                "pnpm",
            ),
        ]);
    }

    #[test]
    fn falls_back_when_no_useful_program_follows_cd() {
        assert_no_programs(&[
            "cd apps/web && [ -f package.json ]",
            "cd apps/web || exit 1",
            "cd one && cd two",
        ]);
    }

    #[test]
    fn does_not_label_commands_inside_multiline_shell_control_flow() {
        assert_no_programs(&[
            "if test -f package.json\nthen\n  npm test\nfi",
            "[[ -d first && -d second ]] && npm test",
            "for file in *\ndo\n  echo \"$file\"\ndone",
            "while true\ndo\n  sleep 1\ndone",
            "cd /tmp; build () { npm test; }",
        ]);
    }

    #[test]
    fn bounds_nested_shell_unwrapping() {
        let mut command = "git status".to_owned();
        for _ in 0..9 {
            command = format!("sh -c '{}'", command.replace('\'', "'\\''"));
        }
        assert_eq!(command_program_name(&command), None);
    }

    #[test]
    fn does_not_spend_the_shell_nesting_budget_on_setup_commands() {
        let command = (0..10)
            .map(|index| format!("export VALUE_{index}=configured"))
            .chain(["npm test".to_owned()])
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(command_program_name(&command).as_deref(), Some("npm"));
    }

    #[test]
    fn bounds_the_number_of_top_level_setup_segments() {
        let command = (0..2_000)
            .map(|index| format!("export VALUE_{index}=configured"))
            .chain(["npm test".to_owned()])
            .collect::<Vec<_>>()
            .join(";");

        assert_eq!(command_program_name(&command), None);
    }

    #[test]
    fn bounds_nested_command_wrappers() {
        assert_eq!(
            command_program_name(&format!("{}git status", "command ".repeat(9))),
            None
        );
    }

    // describe("commandDisplayText")

    #[test]
    fn shows_the_script_without_hiding_meaningful_outer_commands() {
        assert_display_texts(&[
            ("/bin/zsh -lc 'git diff --stat'", "git diff --stat"),
            (
                "bash -c \"printf \\\"%s\\\" hello; git status\"",
                "printf \"%s\" hello; git status",
            ),
            (
                "/usr/bin/fish --command 'rg TODO src | head -20'",
                "rg TODO src | head -20",
            ),
            ("zsh -lc 'pwd\nls'", "pwd\nls"),
            ("git status", "git status"),
            ("zsh script.sh", "zsh script.sh"),
            (
                "zsh -lc 'echo $1' name value",
                "zsh -lc 'echo $1' name value",
            ),
            ("zsh -lc 'pwd'; git status", "zsh -lc 'pwd'; git status"),
            ("zsh -lc 'unterminated", "zsh -lc 'unterminated"),
        ]);
    }

    // describe("commands recorded from each provider")
    //
    // Inputs are copied from the recorded replay transcripts in
    // apps/server/src/orchestration-v2/testkit/fixtures (turn_interrupt_mid_tool,
    // todo_list, grok_background_bash and
    // opencode_running_child_approval). In turn_interrupt_mid_tool every provider
    // ran the same script, and only Codex wraps it in a login shell.

    #[test]
    fn each_provider_shows_the_script_it_ran() {
        let interrupt_script = r#"node -e "console.log('interrupt fixture tool started'); setTimeout(() => {}, 30000)""#;
        let cases = [
            ("Claude", interrupt_script, interrupt_script),
            ("Cursor", interrupt_script, interrupt_script),
            ("Pi", interrupt_script, interrupt_script),
            (
                "Codex",
                r#"/bin/bash -lc "node -e \"console.log('interrupt fixture tool started'); setTimeout(() => {}, 30000)\"""#,
                interrupt_script,
            ),
            (
                "Codex",
                "/bin/bash -lc 'cat package.json && cat tsconfig.json'",
                "cat package.json && cat tsconfig.json",
            ),
            (
                "Grok",
                "for i in 1 2 3; do sleep 8; echo tock $i; done",
                "for i in 1 2 3; do sleep 8; echo tock $i; done",
            ),
            (
                "OpenCode",
                "printf 'running child approval' > running-child.txt",
                "printf 'running child approval' > running-child.txt",
            ),
        ];
        for (provider, input, script) in cases {
            assert_eq!(command_display_text(input), script, "{provider}: {input:?}");
            assert_eq!(
                command_highlight_language(&command_display_text(input)),
                CommandHighlightLanguage::ShellScript,
                "{provider}: {input:?}"
            );
        }
    }

    // describe("commandDisplayText with redacted Codex commands")
    //
    // Codex replaces secrets in command text before sending it
    // (codex-rs/secrets/src/sanitizer.rs). Usually the quoting survives.

    #[test]
    fn unwraps_a_redacted_command_whose_quoting_is_intact() {
        assert_eq!(
            command_display_text(
                r#"/bin/zsh -lc 'curl -H "Authorization: Bearer [REDACTED_SECRET]" https://example.com'"#,
            ),
            r#"curl -H "Authorization: Bearer [REDACTED_SECRET]" https://example.com"#
        );
    }

    // Its secret-assignment pattern can also swallow the backslash of an escaped
    // quote: `token=config[\"apiToken\"]` becomes `token=[REDACTED_SECRET]"apiToken\"]`.
    // The bare quote ends the -lc argument early, so where the script ends is unknown.
    #[test]
    fn keeps_the_wrapper_when_redaction_broke_the_quoting() {
        let input =
            r#"/bin/zsh -lc "python3 -c 'token=[REDACTED_SECRET]"apiToken\"]; print(token)'""#;
        assert_eq!(command_display_text(input), input);
    }

    // describe("commandDisplayText control characters")

    #[test]
    fn shows_control_characters_as_control_pictures() {
        assert_display_texts(&[
            ("printf '\u{1b}[31mred\u{1b}[0m'", "printf '␛[31mred␛[0m'"),
            ("zsh -lc 'printf \"\u{1b}[1m\"'", "printf \"␛[1m\""),
            ("echo bell\u{7} delete\u{7f}", "echo bell␇ delete␡"),
            ("printf 'a\\tb'\r\necho done", "printf 'a\\tb'\r\necho done"),
            (
                "cat <<'EOF'\n\tindented\nEOF",
                "cat <<'EOF'\n\tindented\nEOF",
            ),
        ]);
    }

    // describe("commandHighlightLanguage")

    #[test]
    fn command_highlight_language_cases() {
        let cases = [
            (
                "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoProfile -Command \"Get-ChildItem -Recurse\"",
                "powershell",
            ),
            (
                "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"C:\\work\\scripts\\doctor.ps1\"",
                "powershell",
            ),
            ("& pwsh -c 'Get-Date'", "powershell"),
            ("PWSH.EXE -Command Get-Date", "powershell"),
            ("pwsh-preview -c 'Get-Date'", "shellscript"),
            ("git status; pwsh -c 'Get-Date'", "shellscript"),
            ("cat <<'EOF' > notes.txt\npwsh\nEOF", "shellscript"),
            ("", "shellscript"),
            // Windows agents also run PowerShell directly, without a pwsh wrapper.
            (
                "Get-Content package.json | Select-String version",
                "powershell",
            ),
            ("$env:CI='1'; npm test", "powershell"),
            (
                "$tmp = Join-Path $env:TEMP repo; git clone example",
                "powershell",
            ),
            ("\"=== CHECK FILE ===\"; Get-Content file.txt", "powershell"),
            ("& 'C:\\Python312\\python.exe' -c 'print(1)'", "powershell"),
            ("Install-Module Pester -Scope CurrentUser", "powershell"),
            ("ConvertTo-Json @{ a = 1 }", "powershell"),
            ("update-alternatives --list java", "shellscript"),
            ("install-info --version", "shellscript"),
            ("Make-Thing now", "shellscript"),
            ("echo Get-Content; git status", "shellscript"),
            ("FOO=1 npm test", "shellscript"),
            ("echo $HOME && ls", "shellscript"),
        ];
        for (command, language) in cases {
            assert_eq!(
                command_highlight_language(command).as_str(),
                language,
                "command: {command:?}"
            );
        }
    }

    // Not upstream cases: pin the JS string semantics the port re-creates.

    #[test]
    fn with_visible_control_characters_keeps_the_length() {
        let text = "\u{0}\u{8}\t\n\u{b}\u{c}\r\u{e}\u{1f} \u{7f}";
        let visible = with_visible_control_characters(text);
        assert_eq!(visible, "␀␈\t\n␋␌\r␎␟ ␡");
        assert_eq!(visible.chars().count(), text.chars().count());
    }

    #[test]
    fn uses_js_whitespace_rules() {
        // U+FEFF is JS whitespace (trimmed); U+0085 is not.
        assert_eq!(
            command_display_text("\u{feff}git status\u{feff}"),
            "git status"
        );
        assert_eq!(
            command_program_name("\u{85}git status").as_deref(),
            Some("\u{85}git")
        );
    }
}
